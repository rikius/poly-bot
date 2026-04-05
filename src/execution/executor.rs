//! Order executor - converts intents to orders and submits them
//!
//! The executor is the bridge between strategy decisions (OrderIntent)
//! and actual order submission. It:
//! 1. Applies ExecutionPolicy to convert intent → OrderParams
//! 2. Builds orders using the SDK OrderBuilder (cryptographic salt, tick-size validation)
//! 3. Signs orders via EIP-712 (neg-risk auto-detected by SDK)
//! 4. Submits orders to the exchange
//! 5. Handles partial fills per policy rules
//! 6. Tracks execution results

use std::str::FromStr as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use alloy_signer_local::PrivateKeySigner;
use futures_util::future::join_all;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::clob::types::{
    OrderType as SdkOrderType,
    Side as SdkSide,
};
use polymarket_client_sdk::clob::types::response::PostOrderResponse;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
// use rust_decimal::RoundingStrategy;
use rust_decimal_macros::dec;

use crate::alerts::AlertSender;
use crate::constants::{PARTIAL_FILL_UNWIND_MS, UNWIND_SETTLE_DELAY_MS};
use crate::error::ErrorType;
use crate::execution::policy::{ExecutionPolicy, IntentRef, OrderParams};
use crate::metrics::BotLatency;
use crate::risk::circuit_breaker::CircuitBreaker;
use crate::risk::rate_limiter::RateLimiter;
use crate::strategy::{OrderIntent, Urgency};
use crate::websocket::types::{OrderType, Side};


// ============================================================================
// EXECUTION RESULT
// ============================================================================

/// Result of executing an order intent
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Original intent
    pub intent_token_id: String,

    /// Order ID if submission succeeded
    pub order_id: Option<String>,

    /// Whether the order was filled (any amount)
    pub filled: bool,

    /// Amount filled (if any)
    pub filled_size: Decimal,

    /// Original requested size
    pub requested_size: Decimal,

    /// Execution status
    pub status: ExecutionStatus,

    /// Error message if failed
    pub error: Option<String>,
}

/// Status of order execution
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Order fully filled
    FullyFilled,

    /// Order partially filled
    PartialFill,

    /// Order accepted, pending fill
    Pending,

    /// Order rejected by exchange
    Rejected,

    /// Order cancelled (FOK that didn't fill)
    Cancelled,

    /// Failed to submit
    SubmissionFailed,

    /// Circuit breaker prevented submission
    CircuitOpen,
}

// ============================================================================
// ORDER EXECUTOR
// ============================================================================

/// Executes order intents by converting them to orders and submitting
///
/// Uses the `polymarket-client-sdk` to:
/// - Build orders with cryptographically random salt (not timestamp-derived)
/// - Auto-detect neg-risk markets (no manual `is_neg_risk` flag)
/// - Sign via EIP-712 with the correct exchange contract per chain
pub struct OrderExecutor {
    /// SDK CLOB client (authenticated, Clone-cheap via inner Arc)
    clob_client: ClobClient<Authenticated<Normal>>,

    /// EIP-712 signer (private key)
    signer: Arc<PrivateKeySigner>,

    /// Execution policy (determines order type, partial fill handling)
    policy: Arc<dyn ExecutionPolicy>,

    /// Circuit breaker to check before submission
    circuit_breaker: Arc<CircuitBreaker>,

    /// Rate limiter for order submission (POST /order)
    rate_limiter: RateLimiter,

    /// Shared latency histograms
    latency: Arc<BotLatency>,

    /// Optional alert sender — fires on unwind failure
    alerts: Option<Arc<AlertSender>>,
}

impl OrderExecutor {
    /// Create a new order executor
    pub fn new(
        clob_client: ClobClient<Authenticated<Normal>>,
        signer: Arc<PrivateKeySigner>,
        policy: Arc<dyn ExecutionPolicy>,
        circuit_breaker: Arc<CircuitBreaker>,
        latency: Arc<BotLatency>,
        alerts: Option<Arc<AlertSender>>,
    ) -> Self {
        Self {
            clob_client,
            signer,
            policy,
            circuit_breaker,
            rate_limiter: RateLimiter::for_order_submission(),
            latency,
            alerts,
        }
    }

    /// Execute a single order intent
    pub async fn execute(&self, intent: &OrderIntent) -> ExecutionResult {
        // Check circuit breaker
        if !self.circuit_breaker.is_trading_allowed() {
            warn!(
                strategy = %intent.strategy_name,
                token = %intent.token_id,
                "Circuit breaker open, rejecting order"
            );
            return ExecutionResult {
                intent_token_id: intent.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: intent.size,
                status: ExecutionStatus::CircuitOpen,
                error: Some("Circuit breaker open".to_string()),
            };
        }

        // Check rate limiter
        if !self.rate_limiter.try_acquire() {
            warn!(
                strategy = %intent.strategy_name,
                token = %intent.token_id,
                stats = %self.rate_limiter.stats(),
                "Rate limit reached, rejecting order"
            );
            return ExecutionResult {
                intent_token_id: intent.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: intent.size,
                status: ExecutionStatus::SubmissionFailed,
                error: Some("Rate limit exceeded".to_string()),
            };
        }

        // Convert intent to order params using policy
        let intent_ref = IntentRef::from_intent(intent);
        let mut params = self.policy.to_order_params(&intent_ref);
        // Quantize size here so params.size reflects the actual submitted size.
        // build_sign_submit also calls this, but params.size is used by
        // process_response for fill comparison — if they disagree a fully-filled
        // quantized order is misclassified as PartialFill, spuriously triggering
        // unwind logic.
        params.size = quantize_size_for_usdc_precision(params.price, params.size);

        debug!(
            token = %params.token_id,
            side = ?params.side,
            price = %params.price,
            size = %params.size,
            order_type = ?params.order_type,
            policy = %self.policy.name(),
            "Executing order"
        );

        // Build → sign → submit via SDK
        match self.build_sign_submit(&params).await {
            Ok(response) => self.process_response(&params, response),
            Err(e) => {
                error!(error = %e, "Order submission failed");
                self.circuit_breaker
                    .record_order_result(Some(ErrorType::Retryable));
                ExecutionResult {
                    intent_token_id: intent.token_id.clone(),
                    order_id: None,
                    filled: false,
                    filled_size: Decimal::ZERO,
                    requested_size: intent.size,
                    status: ExecutionStatus::SubmissionFailed,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    /// Execute multiple intents concurrently (for multi-leg orders like arb)
    ///
    /// This uses tokio::join! to submit all orders at once, minimizing
    /// the time window between leg submissions.
    pub async fn execute_batch(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        match intents.len() {
            0 => vec![],
            1 => vec![self.execute(&intents[0]).await],
            2 => {
                // Common case: two-leg arb
                let (r1, r2) = tokio::join!(self.execute(&intents[0]), self.execute(&intents[1]));
                vec![r1, r2]
            }
            3 => {
                let (r1, r2, r3) = tokio::join!(
                    self.execute(&intents[0]),
                    self.execute(&intents[1]),
                    self.execute(&intents[2])
                );
                vec![r1, r2, r3]
            }
            _ => {
                // For arbitrary batch sizes run all concurrently
                join_all(intents.iter().map(|i| self.execute(i))).await
            }
        }
    }

    /// Execute a grouped set of intents and handle partial fills.
    ///
    /// For multi-leg arb orders all legs must stay balanced. After concurrent
    /// submission we compare filled amounts:
    ///
    /// - If every leg filled equally → nothing to do
    /// - If some legs filled more than others → unwind the excess on the
    ///   over-filled legs so positions stay delta-neutral
    ///
    /// The unwind attempt runs within `PARTIAL_FILL_UNWIND_MS` milliseconds.
    pub async fn execute_grouped(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        if intents.is_empty() {
            return vec![];
        }

        // Submit all legs concurrently
        let results = self.execute_batch(intents).await;

        // Only groups need balancing
        if intents.first().and_then(|i| i.group_id.as_ref()).is_none() {
            return results;
        }

        // Minimum filled across all legs – the balanced target
        let min_filled: Decimal = results
            .iter()
            .map(|r| r.filled_size)
            .fold(Decimal::MAX, |m, s| m.min(s));

        // Build unwind intents for any leg that exceeded the minimum
        let unwind_intents: Vec<OrderIntent> = results
            .iter()
            .zip(intents.iter())
            .filter_map(|(result, original)| {
                let excess = result.filled_size - min_filled;
                if excess <= Decimal::ZERO {
                    return None;
                }

                // Inverse side to close the excess position
                let unwind_side = match original.side {
                    Side::Buy => Side::Sell,
                    Side::Sell => Side::Buy,
                };

                // Aggressive limit price so the order fills against any
                // available counter-party (FAK semantics via Immediate urgency)
                let limit_price = match unwind_side {
                    Side::Sell => Decimal::new(1, 2),   // 0.01 – sell at any bid
                    Side::Buy  => Decimal::new(99, 2),  // 0.99 – buy at any ask
                };

                info!(
                    token    = %original.token_id,
                    excess   = %excess,
                    side     = ?unwind_side,
                    "Unwinding excess fill from grouped order"
                );

                Some(OrderIntent::new(
                    original.market_id.clone(),
                    original.token_id.clone(),
                    unwind_side,
                    limit_price,
                    excess,
                    Urgency::Immediate,
                    "unwind: partial fill imbalance",
                    format!("unwind:{}", original.strategy_name),
                ))
            })
            .collect();

        if unwind_intents.is_empty() {
            return results;
        }

        // Wait for the exchange to credit freshly-filled tokens before selling.
        // Without this the sell gets "balance: 0" because the fill hasn't settled.
        tokio::time::sleep(Duration::from_millis(UNWIND_SETTLE_DELAY_MS)).await;

        // Run all unwinds within the deadline
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(PARTIAL_FILL_UNWIND_MS);

        for unwind in &unwind_intents {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(
                    token = %unwind.token_id,
                    "Unwind deadline exceeded – residual exposure remains"
                );
                break;
            }

            match tokio::time::timeout(remaining, self.execute(unwind)).await {
                Ok(r) if r.status == ExecutionStatus::FullyFilled => {
                    info!(token = %unwind.token_id, "Unwind fully filled");
                }
                Ok(r) => {
                    let msg = format!(
                        "Unwind partial fill on {} — residual {} (filled {})",
                        unwind.token_id, unwind.size, r.filled_size,
                    );
                    warn!(
                        token    = %unwind.token_id,
                        status   = ?r.status,
                        excess   = %unwind.size,
                        filled   = %r.filled_size,
                        "Unwind did not fully fill – residual exposure remains"
                    );
                    self.circuit_breaker.manual_open();
                    if let Some(ref alerts) = self.alerts {
                        alerts.send_circuit_breaker_trip(&msg).await;
                    }
                }
                Err(_) => {
                    let msg = format!(
                        "Unwind timed out on {} — residual {} unhedged",
                        unwind.token_id, unwind.size,
                    );
                    warn!(
                        token  = %unwind.token_id,
                        budget = ?Duration::from_millis(PARTIAL_FILL_UNWIND_MS),
                        "Unwind timed out"
                    );
                    self.circuit_breaker.manual_open();
                    if let Some(ref alerts) = self.alerts {
                        alerts.send_circuit_breaker_trip(&msg).await;
                    }
                }
            }
        }

        results
    }

    /// Build, sign, and submit an order via the SDK
    ///
    /// The SDK handles:
    /// - Cryptographic salt generation (random u64, not timestamp-based)
    /// - Tick-size validation (fetches and caches per-market tick size)
    /// - Neg-risk auto-detection (fetches and caches per-token)
    /// - EIP-712 domain separator with correct exchange contract
    async fn build_sign_submit(
        &self,
        params: &OrderParams,
    ) -> std::result::Result<PostOrderResponse, Box<dyn std::error::Error + Send + Sync>> {
        let sdk_token_id = U256::from_str(&params.token_id)?;

        let sdk_side = match params.side {
            crate::websocket::types::Side::Buy => SdkSide::Buy,
            crate::websocket::types::Side::Sell => SdkSide::Sell,
        };

        let sdk_order_type = match params.order_type {
            OrderType::GTC => SdkOrderType::GTC,
            OrderType::FOK => SdkOrderType::FOK,
            OrderType::FAK => SdkOrderType::FAK,
        };

        // --- NEW FIX START ---
        // Strictly truncate decimals to meet Polymarket API requirements.
        // Maker amount (USDC) supports max 2 decimals.
        // Price usually supports 4 decimals on crypto markets.
        // let sanitized_price = params.price.round_dp_with_strategy(4, RoundingStrategy::ToZero);
        // let sanitized_size = params.size.round_dp_with_strategy(2, RoundingStrategy::ToZero);
        // --- NEW FIX END ---

        let t0 = Instant::now();

        // Build order — SDK validates tick-size and lot-size automatically.
        // params.size is already quantized for USDC precision by execute().
        let signable = self
            .clob_client
            .limit_order()
            .token_id(sdk_token_id)
            .side(sdk_side)
            .price(params.price)
            .size(params.size)
            .order_type(sdk_order_type)
            .build()
            .await?;

        // Sign — SDK auto-detects neg-risk and picks the right exchange contract
        let signed = self.clob_client.sign(self.signer.as_ref(), signable).await?;

        // Submit
        let response = self.clob_client.post_order(signed).await?;

        self.latency
            .submit_to_ack
            .record_us(t0.elapsed().as_micros() as u64);

        Ok(response)
    }

    /// Process order response into execution result
    fn process_response(&self, params: &OrderParams, response: PostOrderResponse) -> ExecutionResult {
        if response.success {
            // Clamp taking_amount to the requested size.  A value larger than
            // requested would be an exchange or SDK bug; silently accepting it
            // would corrupt position tracking.
            let filled_size = response.taking_amount.min(params.size);
            if response.taking_amount > params.size {
                warn!(
                    order_id = %response.order_id,
                    taking   = %response.taking_amount,
                    requested = %params.size,
                    "Exchange returned taking_amount > requested; clamping to requested size"
                );
            }
            let filled = filled_size > Decimal::ZERO;

            let status = if filled_size >= params.size {
                ExecutionStatus::FullyFilled
            } else if filled {
                ExecutionStatus::PartialFill
            } else if params.order_type == OrderType::FOK {
                ExecutionStatus::Cancelled
            } else {
                ExecutionStatus::Pending
            };

            info!(
                order_id = %response.order_id,
                status = ?status,
                filled = %filled_size,
                requested = %params.size,
                "Order executed"
            );

            // Record success for circuit breaker
            self.circuit_breaker.record_order_result(None);

            ExecutionResult {
                intent_token_id: params.token_id.clone(),
                order_id: Some(response.order_id),
                filled,
                filled_size,
                requested_size: params.size,
                status,
                error: None,
            }
        } else {
            let error_msg = response.error_msg.unwrap_or_default();

            warn!(
                error = %error_msg,
                token = %params.token_id,
                "Order rejected"
            );

            // Classify error and record for circuit breaker
            let error_type = ErrorType::from_error_msg(&error_msg);
            self.circuit_breaker.record_order_result(Some(error_type));

            ExecutionResult {
                intent_token_id: params.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: params.size,
                status: ExecutionStatus::Rejected,
                error: Some(error_msg),
            }
        }
    }

    /// Cancel all live orders via the exchange bulk-cancel endpoint.
    ///
    /// Called from the API server's `POST /api/orders/cancel-all` handler.
    /// Returns the number of orders the exchange confirmed as cancelled.
    pub async fn cancel_all_orders(&self) -> usize {
        warn!("Cancelling all orders via API request");
        match self.clob_client.cancel_all_orders().await {
            Ok(response) => {
                let count = response.canceled.len();
                warn!(count = count, "All orders cancelled via API");
                count
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("401") || msg.contains("Unauthorized") {
                    error!(
                        error = %msg,
                        "Bulk cancel-all failed: 401 Unauthorized — L2 credentials are invalid. \
                         Check POLYMARKET_API_KEY / POLYMARKET_SECRET / POLYMARKET_PASSPHRASE."
                    );
                } else {
                    error!(error = %msg, "Bulk cancel-all failed");
                }
                0
            }
        }
    }
}

// ============================================================================
// AMOUNT PRECISION HELPERS
// ============================================================================

/// Quantize `size` so that `price × size` (the USDC maker_amount) has ≤ 2dp.
///
/// For prices like $0.51 (where gcd(51, 100) = 1) this rounds size down to
/// the nearest whole number. For $0.50 it rounds to the nearest 0.02, etc.
fn quantize_size_for_usdc_precision(price: Decimal, size: Decimal) -> Decimal {
    use rust_decimal::prelude::ToPrimitive as _;
    if price.is_zero() || size.is_zero() {
        return size;
    }
    // Price expressed as integer hundredths (round to suppress sub-cent noise).
    let price_cents = (price * dec!(100)).round().to_i64().unwrap_or(1).max(1) as u64;
    // lot_size = (100 / gcd(price_cents, 100)) / 100
    // e.g. P=0.51 → gcd=1 → lot=100/100=1.00 (integers only)
    //      P=0.50 → gcd=50 → lot=2/100=0.02
    //      P=0.58 → gcd=2  → lot=50/100=0.50
    let g = gcd_u64(price_cents, 100);
    let lot = Decimal::new((100 / g) as i64, 2);
    let quantized = (size / lot).floor() * lot;
    // Polymarket requires notional (price × size) ≥ $1.00.
    // If flooring dropped us below that, add one lot to meet the minimum.
    if price * quantized < dec!(1.00) {
        quantized + lot
    } else {
        quantized
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_size_for_usdc_precision() {
        use rust_decimal_macros::dec;
        // P=0.51: gcd(51,100)=1 → lot=1.00
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.51), dec!(5.55)), dec!(5));
        // 0.51 × 1 = $0.51 < $1 minimum → bumps to 2 (next lot)
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.51), dec!(1.99)), dec!(2));
        // P=0.50: gcd(50,100)=50 → lot=0.02
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.50), dec!(5.55)), dec!(5.54));
        // P=0.58: gcd(58,100)=2 → lot=0.50
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.58), dec!(5.55)), dec!(5.50));
        // P=0.29: gcd(29,100)=1 → lot=1.00
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.29), dec!(5.25)), dec!(5));
        // Already valid
        assert_eq!(quantize_size_for_usdc_precision(dec!(0.51), dec!(3.00)), dec!(3));
        // Verify resulting USDC amounts are multiples of $0.01 (≤ 2dp)
        for (price, size) in [(dec!(0.51), dec!(5.55)), (dec!(0.29), dec!(5.25)), (dec!(0.58), dec!(5.55))] {
            let s = quantize_size_for_usdc_precision(price, size);
            let usdc = price * s;
            assert_eq!((usdc * dec!(100)).fract(), Decimal::ZERO, "USDC not 2dp for price={price}, size={size}");
        }
        // Minimum notional: P=0.90, size=1.12 → lot=0.10, floor→1.10, notional=0.99 < $1 → bump to 1.20
        let s = quantize_size_for_usdc_precision(dec!(0.90), dec!(1.12));
        assert_eq!(s, dec!(1.20), "should bump to 1.20 to meet $1 minimum");
        assert!(dec!(0.90) * s >= dec!(1.00), "notional must be ≥ $1");
    }

    #[test]
    fn test_execution_status_eq() {
        assert_eq!(ExecutionStatus::FullyFilled, ExecutionStatus::FullyFilled);
        assert_ne!(ExecutionStatus::FullyFilled, ExecutionStatus::PartialFill);
    }

    #[test]
    fn test_execution_result_creation() {
        let result = ExecutionResult {
            intent_token_id: "token123".to_string(),
            order_id: Some("order456".to_string()),
            filled: true,
            filled_size: Decimal::from(100),
            requested_size: Decimal::from(100),
            status: ExecutionStatus::FullyFilled,
            error: None,
        };

        assert!(result.filled);
        assert_eq!(result.filled_size, result.requested_size);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_partial_fill_detection() {
        let result = ExecutionResult {
            intent_token_id: "token".to_string(),
            order_id: Some("order".to_string()),
            filled: true,
            filled_size: Decimal::from(50),
            requested_size: Decimal::from(100),
            status: ExecutionStatus::PartialFill,
            error: None,
        };

        assert!(result.filled_size < result.requested_size);
        assert_eq!(result.status, ExecutionStatus::PartialFill);
    }
}
