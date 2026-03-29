//! Intent processing — paper simulation and live order execution.

use super::Bot;
use crate::config::OperatingMode;
use crate::execution::{ExecutionResult, ExecutionStatus, OrderExecutor};
use crate::risk::CircuitBreaker;
use crate::strategy::{OrderIntent, Urgency};
use crate::websocket::types::Side;
use crate::ledger::Fill;
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

impl Bot {
    /// Process order intents from strategies.
    pub(crate) fn process_intents(&mut self, intents: Vec<OrderIntent>) {
        self.total_intents += intents.len() as u64;

        for intent in &intents {
            let exec_mode = match intent.urgency {
                Urgency::Immediate => "TAKER/FOK",
                Urgency::Normal => "TAKER/FAK",
                Urgency::Passive => "MAKER/GTC",
            };
            info!(
                "📝 Intent: {} {} {} @ ${:.4} x {} [{}] → {}",
                intent.strategy_name,
                format!("{:?}", intent.side),
                &intent.token_id[..intent.token_id.len().min(12)],
                intent.price,
                intent.size,
                intent.reason,
                exec_mode
            );
        }

        if !self.circuit_breaker.is_trading_allowed() {
            warn!(
                "⚠️ Circuit breaker OPEN - not executing {} intent(s)",
                intents.len()
            );
            return;
        }

        match self.config.mode {
            OperatingMode::Paper => {
                info!("📋 PAPER MODE: Simulating {} order(s)", intents.len());
                for intent in &intents {
                    info!(
                        "  [PAPER] {} {} @ ${:.4} x {}",
                        format!("{:?}", intent.side),
                        &intent.token_id[..intent.token_id.len().min(16)],
                        intent.price,
                        intent.size
                    );
                }
                self.simulate_paper_fills(&intents);
            }
            OperatingMode::Live => {
                let Some(ref executor) = self.executor else {
                    error!("Live mode but no executor configured — missing credentials?");
                    return;
                };
                let executor = executor.clone();
                let circuit_breaker = self.circuit_breaker.clone();
                let intents_owned = intents.clone();

                tokio::spawn(async move {
                    Self::execute_intents(executor, circuit_breaker, intents_owned).await;
                });

                self.total_executions += intents.len() as u64;
            }
        }
    }

    /// Simulate order fills in paper mode against the live order book.
    ///
    /// For each intent:
    /// - BUY: fills at the current best ask if ask ≤ limit price
    /// - SELL: fills at the current best bid if bid ≥ limit price
    fn simulate_paper_fills(&self, intents: &[OrderIntent]) {
        for intent in intents {
            let fill_price = match intent.side {
                Side::Buy => self
                    .order_book_state
                    .best_ask(&intent.token_id)
                    .filter(|&ask| ask <= intent.price),
                Side::Sell => self
                    .order_book_state
                    .best_bid(&intent.token_id)
                    .filter(|&bid| bid >= intent.price),
            };

            let Some(fill_price) = fill_price else {
                debug!(
                    side = ?intent.side,
                    token = %&intent.token_id[..intent.token_id.len().min(12)],
                    limit = %intent.price,
                    "Paper: no counterside at limit price, no fill"
                );
                continue;
            };

            let notional = fill_price * intent.size;
            let fee_rate_bps = self
                .market_registry
                .get_by_token(&intent.token_id)
                .map(|p| p.fee_rate_bps)
                .unwrap_or(0);
            let fee = notional * Decimal::new(fee_rate_bps as i64, 4);

            // For BUY: pre-deduct notional so process_fill's settle_buy doesn't double-count.
            if intent.side == Side::Buy {
                let total_cost = notional + fee;
                if !self.ledger.cash.can_afford(total_cost) {
                    warn!(
                        token = %&intent.token_id[..intent.token_id.len().min(12)],
                        needed = %total_cost,
                        available = %self.ledger.cash.available(),
                        "Paper: insufficient cash, skipping fill"
                    );
                    continue;
                }
                let _ = self.ledger.cash.withdraw(notional);
            }

            let fill = Fill {
                fill_id: format!("paper_{}", Uuid::new_v4()),
                order_id: format!("paper_{}", Uuid::new_v4()),
                token_id: intent.token_id.clone(),
                side: intent.side,
                price: fill_price,
                size: intent.size,
                fee,
                expected_price: Some(intent.price),
                slippage_cost: Decimal::ZERO,
                timestamp: Utc::now(),
            };

            info!(
                side = ?fill.side,
                token = %&fill.token_id[..fill.token_id.len().min(12)],
                fill_price = %fill_price,
                limit_price = %intent.price,
                size = %fill.size,
                fee = %fee,
                "Paper fill simulated"
            );

            self.ledger.process_fill(fill);
        }
    }

    /// Execute intents asynchronously (called from a spawned task).
    pub(crate) async fn execute_intents(
        executor: Arc<OrderExecutor>,
        circuit_breaker: Arc<CircuitBreaker>,
        intents: Vec<OrderIntent>,
    ) {
        info!("🚀 LIVE: Executing {} order(s)...", intents.len());

        let has_group = intents.first().and_then(|i| i.group_id.as_ref()).is_some();
        let results = if has_group {
            executor.execute_grouped(&intents).await
        } else {
            executor.execute_batch(&intents).await
        };

        for (intent, result) in intents.iter().zip(results.iter()) {
            Self::handle_execution_result(intent, result, &circuit_breaker);
        }
    }

    /// Handle the result of a single order execution.
    fn handle_execution_result(
        intent: &OrderIntent,
        result: &ExecutionResult,
        circuit_breaker: &CircuitBreaker,
    ) {
        match result.status {
            ExecutionStatus::FullyFilled => {
                info!(
                    "✅ FILLED: {} {} @ {} x {} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.filled_size,
                    result.order_id.as_deref().unwrap_or("?")
                );
                circuit_breaker.record_order_result(None);
            }
            ExecutionStatus::PartialFill => {
                warn!(
                    "⚠️ PARTIAL: {} {} @ {} - filled {}/{} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.filled_size,
                    result.requested_size,
                    result.order_id.as_deref().unwrap_or("?")
                );
                circuit_breaker.record_order_result(None);
            }
            ExecutionStatus::Pending => {
                info!(
                    "⏳ PENDING: {} {} @ {} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.order_id.as_deref().unwrap_or("?")
                );
            }
            ExecutionStatus::Rejected => {
                error!(
                    "❌ REJECTED: {} {} @ {} - {}",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.error.as_deref().unwrap_or("unknown error")
                );
                circuit_breaker.record_order_result(Some(crate::error::ErrorType::Expected));
            }
            ExecutionStatus::Cancelled => {
                info!(
                    "🚫 CANCELLED: {} {} @ {} (FOK not filled)",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                );
            }
            ExecutionStatus::SubmissionFailed => {
                error!(
                    "💥 FAILED: {} {} @ {} - {}",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.error.as_deref().unwrap_or("submission failed")
                );
                circuit_breaker.record_order_result(Some(crate::error::ErrorType::Retryable));
            }
            ExecutionStatus::CircuitOpen => {
                warn!(
                    "🔴 CIRCUIT OPEN: {} {} @ {} - trading halted",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                );
            }
        }
    }
}
