//! Reconciliation - REST sync loop to detect state drift
//!
//! Periodically fetches open orders from the server and compares with local ledger.
//! Detects discrepancies and triggers circuit breaker if needed.

use crate::api::client::ApiClient;
use crate::constants::RECONCILIATION_INTERVAL;
use crate::execution::Discrepancy;
use crate::ledger::{OpenOrders, OrderState};
use crate::risk::CircuitBreaker;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{debug, error, warn};

/// Reconciliation result
#[derive(Debug, Clone)]
pub struct ReconciliationResult {
    /// Server-reported open orders
    pub server_order_count: usize,
    /// Local tracked open orders
    pub local_order_count: usize,
    /// Discrepancies found
    pub discrepancies: Vec<Discrepancy>,
    /// Whether reconciliation was successful (no critical issues)
    pub success: bool,
}

/// Message types from reconciliation loop
#[derive(Debug, Clone)]
pub enum ReconciliationMessage {
    /// Reconciliation completed
    Completed(ReconciliationResult),
    /// Reconciliation failed (network error, etc.)
    Failed(String),
    /// Discrepancy detected
    DiscrepancyDetected(Discrepancy),
}

/// Reconciliation loop - runs in background
pub struct ReconciliationLoop {
    /// API client for REST calls
    client: Arc<ApiClient>,
    /// Local orders state
    orders: Arc<OpenOrders>,
    /// Circuit breaker to trigger on failures
    circuit_breaker: Arc<CircuitBreaker>,
    /// Interval between reconciliations
    interval: Duration,
    /// Channel to send results
    tx: mpsc::UnboundedSender<ReconciliationMessage>,
}

impl ReconciliationLoop {
    /// Create a new reconciliation loop
    pub fn new(
        client: Arc<ApiClient>,
        orders: Arc<OpenOrders>,
        circuit_breaker: Arc<CircuitBreaker>,
        tx: mpsc::UnboundedSender<ReconciliationMessage>,
    ) -> Self {
        Self {
            client,
            orders,
            circuit_breaker,
            interval: RECONCILIATION_INTERVAL,
            tx,
        }
    }

    /// Create with custom interval (for testing)
    pub fn with_interval(
        client: Arc<ApiClient>,
        orders: Arc<OpenOrders>,
        circuit_breaker: Arc<CircuitBreaker>,
        tx: mpsc::UnboundedSender<ReconciliationMessage>,
        interval: Duration,
    ) -> Self {
        Self {
            client,
            orders,
            circuit_breaker,
            interval,
            tx,
        }
    }

    /// Run the reconciliation loop (call in tokio::spawn)
    pub async fn run(&self) {
        let mut ticker = interval(self.interval);

        loop {
            ticker.tick().await;

            match self.reconcile_once().await {
                Ok(result) => {
                    if result.success {
                        debug!(
                            server = result.server_order_count,
                            local = result.local_order_count,
                            discrepancies = result.discrepancies.len(),
                            "Reconciliation completed"
                        );
                    } else {
                        warn!(
                            discrepancies = result.discrepancies.len(),
                            "Reconciliation found issues"
                        );
                    }

                    // Report discrepancies
                    for discrepancy in &result.discrepancies {
                        let _ = self
                            .tx
                            .send(ReconciliationMessage::DiscrepancyDetected(
                                discrepancy.clone(),
                            ));
                    }

                    let _ = self.tx.send(ReconciliationMessage::Completed(result));
                }
                Err(e) => {
                    error!(error = %e, "Reconciliation failed");
                    self.circuit_breaker
                        .record_reconciliation_failure(e.clone());
                    let _ = self.tx.send(ReconciliationMessage::Failed(e));
                }
            }
        }
    }

    /// Run a single reconciliation check
    pub async fn reconcile_once(&self) -> Result<ReconciliationResult, String> {
        // Fetch server orders
        let server_orders = self
            .client
            .get_orders()
            .await
            .map_err(|e| format!("Failed to fetch orders: {}", e))?;

        let server_order_count = server_orders.len();

        // Get local orders
        let local_orders = self.orders.all_open();
        let local_order_count = local_orders.len();

        // Build sets for comparison
        let server_ids: HashSet<String> = server_orders.iter().map(|o| o.id.clone()).collect();

        let local_ids: HashSet<String> = local_orders
            .iter()
            .filter_map(|o| o.order_id.clone())
            .collect();

        let mut discrepancies = Vec::new();

        // Check for orders on server that we don't have locally
        for server_order in &server_orders {
            if !local_ids.contains(&server_order.id) {
                discrepancies.push(Discrepancy::RemoteNotLocal {
                    order_id: server_order.id.clone(),
                });
            }
        }

        // Check for local orders not on server (could be filled/cancelled)
        for local_order in &local_orders {
            if let Some(ref order_id) = local_order.order_id {
                if !server_ids.contains(order_id) {
                    // Only flag if we think order is still open
                    if local_order.state.is_open() {
                        discrepancies.push(Discrepancy::LocalNotRemote {
                            local_id: local_order.local_id.to_string(),
                            order_id: Some(order_id.clone()),
                            state: local_order.state,
                        });
                    }
                }
            }
        }

        // Check for state mismatches (server says filled, we think open)
        for server_order in &server_orders {
            if let Some(local_order) = local_orders
                .iter()
                .find(|o| o.order_id.as_ref() == Some(&server_order.id))
            {
                let server_state = parse_server_status(&server_order.status);
                
                // If server says filled but we don't, that's a mismatch
                if server_state == OrderState::Filled && local_order.state != OrderState::Filled {
                    let server_filled = parse_filled_size(&server_order.size_matched);
                    discrepancies.push(Discrepancy::StateMismatch {
                        order_id: server_order.id.clone(),
                        local_state: local_order.state,
                        remote_filled: server_filled,
                        local_filled: local_order.filled_size,
                    });
                }
            }
        }

        let success = discrepancies.is_empty()
            || discrepancies.iter().all(|d| !d.is_critical());

        Ok(ReconciliationResult {
            server_order_count,
            local_order_count,
            discrepancies,
            success,
        })
    }
}

/// Parse server status string to OrderState
fn parse_server_status(status: &str) -> OrderState {
    match status.to_lowercase().as_str() {
        "live" | "open" => OrderState::Acked,
        "matched" | "filled" => OrderState::Filled,
        "cancelled" | "canceled" => OrderState::Cancelled,
        "expired" => OrderState::Expired,
        "rejected" => OrderState::Rejected,
        _ => OrderState::Unknown,
    }
}

/// Parse filled size from string
fn parse_filled_size(size_str: &str) -> Decimal {
    size_str.parse().unwrap_or(Decimal::ZERO)
}

impl Discrepancy {
    /// Is this a critical discrepancy that should halt trading?
    pub fn is_critical(&self) -> bool {
        match self {
            // Order on server we don't know about - critical (possible double-spend)
            Discrepancy::RemoteNotLocal { .. } => true,
            // Local order not on server - could just be filled, check state
            Discrepancy::LocalNotRemote { state, .. } => {
                // Only critical if we think it's still live
                matches!(state, OrderState::Acked | OrderState::PartiallyFilled)
            }
            // State mismatch with fills - critical if we missed fills
            Discrepancy::StateMismatch {
                local_state,
                remote_filled,
                local_filled,
                ..
            } => {
                // Critical if server has more fills than we tracked
                remote_filled > local_filled
                    && matches!(local_state, OrderState::Acked | OrderState::PartiallyFilled)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_parse_server_status() {
        assert_eq!(parse_server_status("live"), OrderState::Acked);
        assert_eq!(parse_server_status("LIVE"), OrderState::Acked);
        assert_eq!(parse_server_status("matched"), OrderState::Filled);
        assert_eq!(parse_server_status("cancelled"), OrderState::Cancelled);
        assert_eq!(parse_server_status("expired"), OrderState::Expired);
        assert_eq!(parse_server_status("unknown_status"), OrderState::Unknown);
    }

    #[test]
    fn test_discrepancy_criticality() {
        let disc1 = Discrepancy::RemoteNotLocal {
            order_id: "123".to_string(),
        };
        assert!(disc1.is_critical());

        let disc2 = Discrepancy::LocalNotRemote {
            local_id: "abc".to_string(),
            order_id: Some("123".to_string()),
            state: OrderState::Acked,
        };
        assert!(disc2.is_critical());

        let disc3 = Discrepancy::LocalNotRemote {
            local_id: "abc".to_string(),
            order_id: Some("123".to_string()),
            state: OrderState::Filled,
        };
        assert!(!disc3.is_critical()); // Already filled, not critical

        let disc4 = Discrepancy::StateMismatch {
            order_id: "123".to_string(),
            local_state: OrderState::Acked,
            remote_filled: dec!(100),
            local_filled: dec!(0),
        };
        assert!(disc4.is_critical()); // Missed fill!

        let disc5 = Discrepancy::StateMismatch {
            order_id: "123".to_string(),
            local_state: OrderState::Filled,
            remote_filled: dec!(100),
            local_filled: dec!(100),
        };
        assert!(!disc5.is_critical()); // Fills match
    }
}
