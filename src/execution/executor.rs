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
use tracing::{debug, error, info, warn};

use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::clob::types::{
    OrderType as SdkOrderType,
    Side as SdkSide,
};
use polymarket_client_sdk::clob::types::response::PostOrderResponse;
use polymarket_client_sdk::types::U256;

use crate::websocket::types::OrderType;
use crate::error::ErrorType;
use crate::execution::policy::{ExecutionPolicy, IntentRef, OrderParams, PartialFillAction};
use crate::risk::circuit_breaker::CircuitBreaker;
use crate::risk::rate_limiter::RateLimiter;
use crate::strategy::OrderIntent;
use rust_decimal::Decimal;


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
}

impl OrderExecutor {
    /// Create a new order executor
    pub fn new(
        clob_client: ClobClient<Authenticated<Normal>>,
        signer: Arc<PrivateKeySigner>,
        policy: Arc<dyn ExecutionPolicy>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Self {
        Self {
            clob_client,
            signer,
            policy,
            circuit_breaker,
            rate_limiter: RateLimiter::for_order_submission(),
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
        let params = self.policy.to_order_params(&intent_ref);

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
                // For larger batches, execute sequentially
                let mut results = Vec::with_capacity(intents.len());
                for intent in intents {
                    results.push(self.execute(intent).await);
                }
                results
            }
        }
    }

    /// Execute a grouped set of intents and handle partial fills
    ///
    /// For grouped orders (like arb legs), we need to ensure both legs
    /// fill equally. If they don't, the policy determines how to handle it.
    pub async fn execute_grouped(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        // First, submit all orders concurrently
        let results = self.execute_batch(intents).await;

        // Check if any need partial fill handling
        let group_id = intents.first().and_then(|i| i.group_id.clone());
        if group_id.is_none() {
            return results;
        }

        // Collect indices of filled orders
        let filled_indices: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.filled && r.filled_size > Decimal::ZERO)
            .map(|(i, _)| i)
            .collect();

        let unfilled_count = results.len() - filled_indices.len();

        // If all filled or all unfilled, nothing to do
        if filled_indices.is_empty() || unfilled_count == 0 {
            return results;
        }

        // Handle imbalanced fills based on policy
        for idx in filled_indices {
            let result = &results[idx];
            if result.filled_size < result.requested_size {
                // Partial fill - check policy
                let intent_ref = IntentRef::from_intent(&intents[idx]);
                let action = self.policy.on_partial_fill(&intent_ref, result.filled_size);

                match action {
                    PartialFillAction::UnwindFilled => {
                        info!(
                            token = %result.intent_token_id,
                            filled = %result.filled_size,
                            "Unwinding partial fill for grouped order"
                        );
                        // TODO: Submit unwind order
                        // For now, just log - actual unwind requires inverse order
                    }
                    PartialFillAction::CancelRemainder => {
                        // FAK already does this
                        debug!(token = %result.intent_token_id, "Cancelling remainder");
                    }
                    PartialFillAction::KeepRemainder => {
                        // GTC keeps working
                        debug!(token = %result.intent_token_id, "Keeping remainder on book");
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

        // Build order — SDK validates tick-size and lot-size automatically
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

        Ok(response)
    }

    /// Process order response into execution result
    fn process_response(&self, params: &OrderParams, response: PostOrderResponse) -> ExecutionResult {
        if response.success {
            let filled_size = response.taking_amount;
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
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
