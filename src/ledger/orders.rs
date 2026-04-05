//! Order tracking and lifecycle management
//!
//! Tracks all orders from creation through completion with state machine transitions.
//! Uses DashMap for lock-free concurrent access.

use crate::websocket::types::{OrderId, Side, TokenId};
use crate::error::{BotError, Result};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

/// Order lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderState {
    /// Built locally, not yet signed
    CreatedLocal,
    /// Signed, ready to submit
    Signed,
    /// POST sent, awaiting ack
    Submitted,
    /// Got order_id back from exchange
    Acked,
    /// Some fills received but not complete
    PartiallyFilled,
    /// Fully filled
    Filled,
    /// Cancel request sent
    CancelPending,
    /// Confirmed cancelled
    Cancelled,
    /// TTL expired
    Expired,
    /// API rejected the order
    Rejected,
    /// Requires reconciliation (state unknown)
    Unknown,
}

impl OrderState {
    /// Check if transition to new state is valid
    pub fn can_transition_to(&self, next: OrderState) -> bool {
        use OrderState::*;
        match (self, next) {
            // Normal flow
            (CreatedLocal, Signed) => true,
            (Signed, Submitted) => true,
            (Submitted, Acked) => true,
            (Submitted, Rejected) => true,
            (Acked, PartiallyFilled) => true,
            (Acked, Filled) => true,
            (Acked, CancelPending) => true,
            (PartiallyFilled, Filled) => true,
            (PartiallyFilled, CancelPending) => true,
            (CancelPending, Cancelled) => true,
            (CancelPending, Filled) => true, // Fill came in before cancel processed
            (Acked, Cancelled) => true,          // server-side cancel without our request
            (PartiallyFilled, Cancelled) => true, // server-side cancel of a partial

            // Error states - can always transition to these
            (_, Unknown) => true,
            (_, Expired) => true,
            (_, Rejected) => true,

            _ => false,
        }
    }

    /// Is this a terminal state (order is done)?
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderState::Filled
                | OrderState::Cancelled
                | OrderState::Expired
                | OrderState::Rejected
        )
    }

    /// Is this order still active on the exchange?
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderState::Acked | OrderState::PartiallyFilled | OrderState::CancelPending
        )
    }

    /// Is this order considered "open" from exchange perspective?
    /// Same as is_active but more explicit name for reconciliation
    pub fn is_open(&self) -> bool {
        self.is_active()
    }
}

/// A tracked order with full lifecycle info
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    /// Exchange-assigned order ID (set after ack)
    pub order_id: Option<OrderId>,
    /// Our internal ID (set on creation, before ack)
    pub local_id: Uuid,
    /// Current state in lifecycle
    pub state: OrderState,
    /// Token being traded
    pub token_id: TokenId,
    /// Buy or sell
    pub side: Side,
    /// Order price
    pub price: Decimal,
    /// Original order size
    pub original_size: Decimal,
    /// Amount filled so far
    pub filled_size: Decimal,
    /// Amount remaining (original - filled)
    pub remaining_size: Decimal,
    /// When order was created locally
    pub created_at: DateTime<Utc>,
    /// Last state update time
    pub last_update: DateTime<Utc>,
    /// Strategy that placed this order (for attribution)
    pub strategy_id: Option<String>,
}

impl TrackedOrder {
    /// Create a new tracked order in CreatedLocal state
    pub fn new(
        token_id: TokenId,
        side: Side,
        price: Decimal,
        size: Decimal,
        strategy_id: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            order_id: None,
            local_id: Uuid::new_v4(),
            state: OrderState::CreatedLocal,
            token_id,
            side,
            price,
            original_size: size,
            filled_size: Decimal::ZERO,
            remaining_size: size,
            created_at: now,
            last_update: now,
            strategy_id,
        }
    }

    /// Calculate notional value of remaining order
    pub fn remaining_notional(&self) -> Decimal {
        self.remaining_size * self.price
    }

    /// Calculate notional value of filled portion
    pub fn filled_notional(&self) -> Decimal {
        self.filled_size * self.price
    }
}

/// Open orders tracker with dual indexing
pub struct OpenOrders {
    /// Orders indexed by exchange order_id
    by_order_id: DashMap<OrderId, TrackedOrder>,
    /// Orders indexed by our local UUID
    by_local_id: DashMap<Uuid, TrackedOrder>,
    /// Counter for stats
    total_created: AtomicU64,
    total_filled: AtomicU64,
    total_cancelled: AtomicU64,
    total_rejected: AtomicU64,
}

impl Default for OpenOrders {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenOrders {
    /// Create a new open orders tracker
    pub fn new() -> Self {
        Self {
            by_order_id: DashMap::new(),
            by_local_id: DashMap::new(),
            total_created: AtomicU64::new(0),
            total_filled: AtomicU64::new(0),
            total_cancelled: AtomicU64::new(0),
            total_rejected: AtomicU64::new(0),
        }
    }

    /// Add a new order (in CreatedLocal state)
    pub fn add(&self, order: TrackedOrder) -> Uuid {
        let local_id = order.local_id;
        self.by_local_id.insert(local_id, order);
        self.total_created.fetch_add(1, Ordering::Relaxed);
        local_id
    }

    /// Get order by exchange order_id
    pub fn get_by_order_id(&self, order_id: &OrderId) -> Option<TrackedOrder> {
        self.by_order_id.get(order_id).map(|r| r.clone())
    }

    /// Get order by local UUID
    pub fn get_by_local_id(&self, local_id: &Uuid) -> Option<TrackedOrder> {
        self.by_local_id.get(local_id).map(|r| r.clone())
    }

    /// Link exchange order_id to local order (called on ack)
    pub fn link_order_id(&self, local_id: &Uuid, order_id: OrderId) -> Result<()> {
        let mut order = self
            .by_local_id
            .get_mut(local_id)
            .ok_or_else(|| BotError::Order(format!("Unknown local_id: {}", local_id)))?;

        order.order_id = Some(order_id.clone());
        order.last_update = Utc::now();

        // Also add to order_id index
        self.by_order_id.insert(order_id, order.clone());
        Ok(())
    }

    /// Transition order to new state (validates transition)
    pub fn transition(&self, local_id: &Uuid, new_state: OrderState) -> Result<OrderState> {
        let mut order = self
            .by_local_id
            .get_mut(local_id)
            .ok_or_else(|| BotError::Order(format!("Unknown local_id: {}", local_id)))?;

        let old_state = order.state;

        if !old_state.can_transition_to(new_state) {
            return Err(BotError::Order(format!(
                "Invalid transition: {:?} -> {:?}",
                old_state, new_state
            )));
        }

        order.state = new_state;
        order.last_update = Utc::now();

        // Update order_id index if present
        if let Some(order_id) = &order.order_id {
            if let Some(mut by_id) = self.by_order_id.get_mut(order_id) {
                by_id.state = new_state;
                by_id.last_update = order.last_update;
            }
        }

        // Update counters for terminal states
        match new_state {
            OrderState::Filled => {
                self.total_filled.fetch_add(1, Ordering::Relaxed);
            }
            OrderState::Cancelled => {
                self.total_cancelled.fetch_add(1, Ordering::Relaxed);
            }
            OrderState::Rejected => {
                self.total_rejected.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok(old_state)
    }

    /// Update filled size (returns new remaining size)
    pub fn apply_fill(&self, local_id: &Uuid, fill_size: Decimal) -> Result<Decimal> {
        let mut order = self
            .by_local_id
            .get_mut(local_id)
            .ok_or_else(|| BotError::Order(format!("Unknown local_id: {}", local_id)))?;

        order.filled_size += fill_size;
        order.remaining_size = order.original_size - order.filled_size;
        order.last_update = Utc::now();

        // Auto-transition state based on fill
        if order.remaining_size <= Decimal::ZERO {
            order.state = OrderState::Filled;
            self.total_filled.fetch_add(1, Ordering::Relaxed);
        } else if order.state == OrderState::Acked {
            order.state = OrderState::PartiallyFilled;
        }

        // Update order_id index
        if let Some(order_id) = &order.order_id {
            if let Some(mut by_id) = self.by_order_id.get_mut(order_id) {
                by_id.filled_size = order.filled_size;
                by_id.remaining_size = order.remaining_size;
                by_id.state = order.state;
                by_id.last_update = order.last_update;
            }
        }

        Ok(order.remaining_size)
    }

    /// Get all active orders (Acked, PartiallyFilled, CancelPending)
    pub fn active_orders(&self) -> Vec<TrackedOrder> {
        self.by_local_id
            .iter()
            .filter(|r| r.state.is_active())
            .map(|r| r.clone())
            .collect()
    }

    /// Alias for active_orders - returns all open orders
    /// Used by reconciliation to compare with server state
    pub fn all_open(&self) -> Vec<TrackedOrder> {
        self.active_orders()
    }

    /// Get all orders for a specific token
    pub fn orders_for_token(&self, token_id: &TokenId) -> Vec<TrackedOrder> {
        self.by_local_id
            .iter()
            .filter(|r| &r.token_id == token_id)
            .map(|r| r.clone())
            .collect()
    }

    /// Count of currently active orders
    pub fn active_count(&self) -> usize {
        self.by_local_id
            .iter()
            .filter(|r| r.state.is_active())
            .count()
    }

    /// Remove terminal orders (cleanup)
    pub fn remove_terminal(&self) {
        let terminal_ids: Vec<Uuid> = self
            .by_local_id
            .iter()
            .filter(|r| r.state.is_terminal())
            .map(|r| r.local_id)
            .collect();

        for local_id in terminal_ids {
            if let Some((_, order)) = self.by_local_id.remove(&local_id) {
                if let Some(order_id) = order.order_id {
                    self.by_order_id.remove(&order_id);
                }
            }
        }
    }

    /// Total reserved notional across all active orders
    pub fn total_reserved_notional(&self) -> Decimal {
        self.by_local_id
            .iter()
            .filter(|r| r.state.is_active())
            .map(|r| r.remaining_notional())
            .sum()
    }

    /// Stats
    pub fn stats(&self) -> OrderStats {
        OrderStats {
            total_created: self.total_created.load(Ordering::Relaxed),
            total_filled: self.total_filled.load(Ordering::Relaxed),
            total_cancelled: self.total_cancelled.load(Ordering::Relaxed),
            total_rejected: self.total_rejected.load(Ordering::Relaxed),
            active_count: self.active_count(),
        }
    }
}

/// Order statistics
#[derive(Debug, Clone)]
pub struct OrderStats {
    pub total_created: u64,
    pub total_filled: u64,
    pub total_cancelled: u64,
    pub total_rejected: u64,
    pub active_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_state_transitions() {
        // Valid transitions
        assert!(OrderState::CreatedLocal.can_transition_to(OrderState::Signed));
        assert!(OrderState::Signed.can_transition_to(OrderState::Submitted));
        assert!(OrderState::Submitted.can_transition_to(OrderState::Acked));
        assert!(OrderState::Submitted.can_transition_to(OrderState::Rejected));
        assert!(OrderState::Acked.can_transition_to(OrderState::PartiallyFilled));
        assert!(OrderState::Acked.can_transition_to(OrderState::Filled));
        assert!(OrderState::PartiallyFilled.can_transition_to(OrderState::Filled));
        assert!(OrderState::CancelPending.can_transition_to(OrderState::Cancelled));

        // Invalid transitions
        assert!(!OrderState::CreatedLocal.can_transition_to(OrderState::Filled));
        assert!(!OrderState::Submitted.can_transition_to(OrderState::Filled));
        assert!(!OrderState::Filled.can_transition_to(OrderState::Acked));

        // Can always go to Unknown
        assert!(OrderState::Acked.can_transition_to(OrderState::Unknown));
        assert!(OrderState::Filled.can_transition_to(OrderState::Unknown));
    }

    #[test]
    fn test_order_lifecycle() {
        let orders = OpenOrders::new();

        // Create order
        let order = TrackedOrder::new(
            "token123".to_string(),
            Side::Buy,
            dec!(0.55),
            dec!(100),
            None,
        );
        let local_id = orders.add(order);

        // Verify initial state
        let o = orders.get_by_local_id(&local_id).unwrap();
        assert_eq!(o.state, OrderState::CreatedLocal);

        // Transition through lifecycle
        orders.transition(&local_id, OrderState::Signed).unwrap();
        orders.transition(&local_id, OrderState::Submitted).unwrap();
        orders.transition(&local_id, OrderState::Acked).unwrap();

        // Link order_id
        orders
            .link_order_id(&local_id, "exchange_order_123".to_string())
            .unwrap();

        // Verify linked
        let o = orders.get_by_order_id(&"exchange_order_123".to_string());
        assert!(o.is_some());
        assert_eq!(o.unwrap().state, OrderState::Acked);

        // Apply partial fill
        orders.apply_fill(&local_id, dec!(50)).unwrap();
        let o = orders.get_by_local_id(&local_id).unwrap();
        assert_eq!(o.state, OrderState::PartiallyFilled);
        assert_eq!(o.filled_size, dec!(50));
        assert_eq!(o.remaining_size, dec!(50));

        // Complete fill
        orders.apply_fill(&local_id, dec!(50)).unwrap();
        let o = orders.get_by_local_id(&local_id).unwrap();
        assert_eq!(o.state, OrderState::Filled);
        assert_eq!(o.remaining_size, dec!(0));

        // Check stats
        let stats = orders.stats();
        assert_eq!(stats.total_created, 1);
        assert_eq!(stats.total_filled, 1);
    }

    #[test]
    fn test_invalid_transition() {
        let orders = OpenOrders::new();

        let order = TrackedOrder::new(
            "token123".to_string(),
            Side::Sell,
            dec!(0.45),
            dec!(200),
            None,
        );
        let local_id = orders.add(order);

        // Can't go from CreatedLocal directly to Acked
        let result = orders.transition(&local_id, OrderState::Acked);
        assert!(result.is_err());
    }

    #[test]
    fn test_active_orders() {
        let orders = OpenOrders::new();

        // Create and ack two orders
        let order1 = TrackedOrder::new("token1".to_string(), Side::Buy, dec!(0.5), dec!(100), None);
        let id1 = orders.add(order1);
        orders.transition(&id1, OrderState::Signed).unwrap();
        orders.transition(&id1, OrderState::Submitted).unwrap();
        orders.transition(&id1, OrderState::Acked).unwrap();

        let order2 = TrackedOrder::new("token2".to_string(), Side::Sell, dec!(0.6), dec!(50), None);
        let id2 = orders.add(order2);
        orders.transition(&id2, OrderState::Signed).unwrap();
        orders.transition(&id2, OrderState::Submitted).unwrap();
        orders.transition(&id2, OrderState::Rejected).unwrap();

        // Only order1 should be active
        let active = orders.active_orders();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].local_id, id1);

        assert_eq!(orders.active_count(), 1);
    }
}
