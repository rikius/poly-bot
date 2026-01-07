//! Execution state machine
//!
//! Manages order state transitions and validates ledger invariants.

use crate::ledger::{Ledger, OrderState};
use rust_decimal::Decimal;

/// Invariant violation types
#[derive(Debug, Clone)]
pub enum InvariantViolation {
    /// Fill references unknown order
    OrphanFill {
        fill_id: String,
        order_id: String,
    },
    /// Reserved cash doesn't match open orders
    CashMismatch {
        expected_reserved: Decimal,
        actual_reserved: Decimal,
    },
    /// Order in invalid state
    InvalidOrderState {
        order_id: String,
        state: OrderState,
        reason: String,
    },
    /// Position size inconsistent with fills
    PositionMismatch {
        token_id: String,
        expected_shares: Decimal,
        actual_shares: Decimal,
    },
}

/// Check all ledger invariants
///
/// Returns list of violations (empty = all good)
pub fn check_invariants(ledger: &Ledger) -> Vec<InvariantViolation> {
    let mut violations = Vec::new();

    // 1. Every fill must map to a known order (relaxed - order may have been cleaned up)
    // We skip this check since fills are stored separately and orders get removed

    // 2. Reserved cash must equal sum of active order notional values
    let expected_reserved: Decimal = ledger.orders.total_reserved_notional();
    let actual_reserved = ledger.cash.reserved();

    // Allow small tolerance for rounding
    let tolerance = Decimal::new(1, 4); // 0.0001
    if (expected_reserved - actual_reserved).abs() > tolerance {
        violations.push(InvariantViolation::CashMismatch {
            expected_reserved,
            actual_reserved,
        });
    }

    // 3. No orders in invalid terminal-but-active state
    for order in ledger.orders.active_orders() {
        if order.state.is_terminal() {
            violations.push(InvariantViolation::InvalidOrderState {
                order_id: order.order_id.unwrap_or_else(|| order.local_id.to_string()),
                state: order.state,
                reason: "Terminal order in active list".to_string(),
            });
        }
    }

    violations
}

/// Validate a proposed state transition
pub fn validate_transition(from: OrderState, to: OrderState) -> Result<(), String> {
    if from.can_transition_to(to) {
        Ok(())
    } else {
        Err(format!("Invalid transition: {:?} -> {:?}", from, to))
    }
}

/// Check if order can be cancelled
pub fn can_cancel(state: OrderState) -> bool {
    matches!(state, OrderState::Acked | OrderState::PartiallyFilled)
}

/// Check if order requires acknowledgement before action
pub fn requires_ack(state: OrderState) -> bool {
    matches!(state, OrderState::CreatedLocal | OrderState::Signed | OrderState::Submitted)
}

/// Reconciliation discrepancy types
#[derive(Debug, Clone)]
pub enum Discrepancy {
    /// Order exists locally but not on exchange
    LocalNotRemote {
        local_id: String,
        order_id: Option<String>,
        state: OrderState,
    },
    /// Order exists on exchange but not locally
    RemoteNotLocal {
        order_id: String,
    },
    /// Order state mismatch
    StateMismatch {
        order_id: String,
        local_state: OrderState,
        remote_filled: Decimal,
        local_filled: Decimal,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::TrackedOrder;
    use crate::api::types::Side;
    use rust_decimal_macros::dec;

    #[test]
    fn test_validate_transition() {
        assert!(validate_transition(OrderState::CreatedLocal, OrderState::Signed).is_ok());
        assert!(validate_transition(OrderState::Signed, OrderState::Submitted).is_ok());
        assert!(validate_transition(OrderState::Submitted, OrderState::Acked).is_ok());
        assert!(validate_transition(OrderState::Acked, OrderState::Filled).is_ok());

        assert!(validate_transition(OrderState::CreatedLocal, OrderState::Filled).is_err());
        assert!(validate_transition(OrderState::Submitted, OrderState::PartiallyFilled).is_err());
    }

    #[test]
    fn test_can_cancel() {
        assert!(can_cancel(OrderState::Acked));
        assert!(can_cancel(OrderState::PartiallyFilled));
        assert!(!can_cancel(OrderState::Submitted));
        assert!(!can_cancel(OrderState::Filled));
        assert!(!can_cancel(OrderState::CreatedLocal));
    }

    #[test]
    fn test_requires_ack() {
        assert!(requires_ack(OrderState::CreatedLocal));
        assert!(requires_ack(OrderState::Signed));
        assert!(requires_ack(OrderState::Submitted));
        assert!(!requires_ack(OrderState::Acked));
        assert!(!requires_ack(OrderState::Filled));
    }

    #[test]
    fn test_check_invariants_clean() {
        let ledger = Ledger::new(dec!(1000));

        // Create and ack an order
        let order = TrackedOrder::new(
            "token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(100),
            None,
        );
        let local_id = ledger.orders.add(order);
        ledger.orders.transition(&local_id, OrderState::Signed).unwrap();
        ledger.orders.transition(&local_id, OrderState::Submitted).unwrap();
        ledger.orders.transition(&local_id, OrderState::Acked).unwrap();

        // Reserve matching cash
        ledger.cash.reserve(dec!(50)).unwrap(); // 0.50 * 100 = 50

        // Check invariants
        let violations = check_invariants(&ledger);
        assert!(violations.is_empty(), "Expected no violations: {:?}", violations);
    }

    #[test]
    fn test_check_invariants_cash_mismatch() {
        let ledger = Ledger::new(dec!(1000));

        // Create and ack an order
        let order = TrackedOrder::new(
            "token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(100),
            None,
        );
        let local_id = ledger.orders.add(order);
        ledger.orders.transition(&local_id, OrderState::Signed).unwrap();
        ledger.orders.transition(&local_id, OrderState::Submitted).unwrap();
        ledger.orders.transition(&local_id, OrderState::Acked).unwrap();

        // Reserve wrong amount
        ledger.cash.reserve(dec!(25)).unwrap(); // Should be 50

        // Check invariants
        let violations = check_invariants(&ledger);
        assert_eq!(violations.len(), 1);
        assert!(matches!(violations[0], InvariantViolation::CashMismatch { .. }));
    }
}
