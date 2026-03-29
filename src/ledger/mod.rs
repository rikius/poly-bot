//! Ledger module - authoritative state tracking
//!
//! The ledger is the single source of truth for portfolio state.
//! All positions and cash updates flow through here.

pub mod cash;
pub mod orders;
pub mod positions;

pub use cash::{CashBalance, CashSnapshot};
pub use orders::{OpenOrders, OrderState, OrderStats, TrackedOrder};
pub use positions::{Fill, Position, Positions};

use crate::websocket::types::{Side, TokenId};
use rust_decimal::Decimal;
use std::sync::Arc;

/// Authoritative portfolio state
///
/// This is the single source of truth. All reads/writes go through here.
pub struct Ledger {
    /// Open orders tracker
    pub orders: OpenOrders,
    /// Position tracker
    pub positions: Positions,
    /// Cash balance tracker
    pub cash: CashBalance,
    /// Fill history (for audit/replay)
    fills: std::sync::RwLock<Vec<Fill>>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(Decimal::ZERO)
    }
}

impl Ledger {
    /// Sync the cash balance to a known on-chain amount.
    ///
    /// Called at startup after querying Polymarket's `balance-allowance` endpoint
    /// so the ledger reflects the wallet's actual deposited USDC.
    pub fn sync_cash(&self, on_chain_balance: Decimal) {
        self.cash.reset(on_chain_balance);
    }

    /// Create a new ledger with initial cash balance
    pub fn new(initial_cash: Decimal) -> Self {
        Self {
            orders: OpenOrders::new(),
            positions: Positions::new(),
            cash: CashBalance::new(initial_cash),
            fills: std::sync::RwLock::new(Vec::new()),
        }
    }

    /// Process a confirmed fill
    ///
    /// Updates: positions, cash, order state.
    /// Automatically resolves `expected_price` from the tracked order (if not
    /// already set) so that slippage is computed against the original limit price.
    pub fn process_fill(&self, mut fill: Fill) {
        // 1. Resolve expected price from the tracked order when not explicitly set.
        //    This allows callers (e.g., the WebSocket user handler) to pass a bare
        //    fill and have slippage computed automatically.
        if fill.expected_price.is_none() {
            fill.expected_price = self
                .orders
                .get_by_order_id(&fill.order_id)
                .map(|o| o.price);
        }

        // 2. Compute slippage cost now that expected_price is resolved.
        fill.slippage_cost = fill.expected_price.map(|exp| {
            match fill.side {
                Side::Buy  => (fill.price - exp) * fill.size,
                Side::Sell => (exp - fill.price) * fill.size,
            }
        }).unwrap_or(Decimal::ZERO);

        // 3. Update position
        self.positions.apply_fill(&fill);

        // 4. Update cash
        match fill.side {
            Side::Buy => {
                // Bought shares - remove cash from reserved
                let _ = self.cash.settle_buy(fill.notional());
            }
            Side::Sell => {
                // Sold shares - receive USDC
                self.cash.settle_sell(fill.notional());
            }
        }

        // 5. Deduct fees
        self.cash.deduct_fee(fill.fee);

        // 6. Store fill for history (cap at 10 000 entries to bound memory use;
        //    drain the oldest half when the limit is reached).
        {
            const MAX_FILLS: usize = 10_000;
            let mut fills = self.fills.write().unwrap();
            fills.push(fill);
            if fills.len() > MAX_FILLS {
                fills.drain(0..MAX_FILLS / 2);
            }
        }
    }

    /// Get all fills
    pub fn fills(&self) -> Vec<Fill> {
        self.fills.read().unwrap().clone()
    }

    /// Get fills for a specific order
    pub fn fills_for_order(&self, order_id: &str) -> Vec<Fill> {
        self.fills
            .read()
            .unwrap()
            .iter()
            .filter(|f| f.order_id == order_id)
            .cloned()
            .collect()
    }

    /// Get a complete snapshot of the ledger state
    pub fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            cash: self.cash.snapshot(),
            positions: self.positions.all_positions(),
            order_stats: self.orders.stats(),
            realized_pnl: self.positions.total_realized_pnl(),
            unrealized_pnl: self.positions.total_unrealized_pnl(),
            total_fees: self.positions.total_fees(),
            total_slippage_cost: self.positions.total_slippage_cost(),
            fill_count: self.fills.read().unwrap().len(),
        }
    }

    /// Calculate net exposure for a token (position + pending orders)
    pub fn net_exposure(&self, token_id: &TokenId) -> Decimal {
        let position_exposure = self.positions.get(token_id).shares;
        let pending_buy: Decimal = self
            .orders
            .orders_for_token(token_id)
            .iter()
            .filter(|o| o.state.is_active() && o.side == Side::Buy)
            .map(|o| o.remaining_size)
            .sum();
        let pending_sell: Decimal = self
            .orders
            .orders_for_token(token_id)
            .iter()
            .filter(|o| o.state.is_active() && o.side == Side::Sell)
            .map(|o| o.remaining_size)
            .sum();

        position_exposure + pending_buy - pending_sell
    }

    /// Total notional exposure (positions + pending orders)
    pub fn total_exposure(&self) -> Decimal {
        let position_notional = self.positions.total_notional();
        let order_notional = self.orders.total_reserved_notional();
        position_notional + order_notional
    }

    /// Get count of open orders
    pub fn open_orders_count(&self) -> u32 {
        self.orders.stats().active_count as u32
    }

    /// Get cash balance snapshot
    pub fn cash_snapshot(&self) -> CashSnapshot {
        self.cash.snapshot()
    }

    /// Get position for a token (returns default if none)
    pub fn get_position(&self, token_id: &TokenId) -> Position {
        self.positions.get(token_id)
    }
}

/// Complete ledger snapshot for logging/persistence
#[derive(Debug, Clone)]
pub struct LedgerSnapshot {
    pub cash: CashSnapshot,
    pub positions: Vec<Position>,
    pub order_stats: OrderStats,
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_fees: Decimal,
    /// Cumulative slippage cost across all positions (positive = unfavourable)
    pub total_slippage_cost: Decimal,
    pub fill_count: usize,
}

/// Create a shared ledger wrapped in Arc
pub fn create_ledger(initial_cash: Decimal) -> Arc<Ledger> {
    Arc::new(Ledger::new(initial_cash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    #[test]
    fn test_ledger_creation() {
        let ledger = Ledger::new(dec!(1000));
        assert_eq!(ledger.cash.available(), dec!(1000));
        assert_eq!(ledger.positions.count(), 0);
        assert_eq!(ledger.orders.active_count(), 0);
    }

    #[test]
    fn test_process_buy_fill() {
        let ledger = Ledger::new(dec!(1000));

        // Reserve cash for order
        ledger.cash.reserve(dec!(100)).unwrap();

        // Process fill
        let fill = Fill {
            fill_id: "f1".to_string(),
            order_id: "o1".to_string(),
            token_id: "token1".to_string(),
            side: Side::Buy,
            price: dec!(0.50),
            size: dec!(200),
            fee: dec!(0.50),
            expected_price: None,
            slippage_cost: Decimal::ZERO,
            timestamp: Utc::now(),
        };
        ledger.process_fill(fill);

        // Check position
        let pos = ledger.positions.get(&"token1".to_string());
        assert_eq!(pos.shares, dec!(200));
        assert_eq!(pos.avg_cost, dec!(0.50));

        // Check cash: 1000 - 100 reserved, then 100 spent on fill, 0.50 fee
        // available = 900 - 0.50 = 899.50
        assert_eq!(ledger.cash.available(), dec!(899.50));
        assert_eq!(ledger.cash.reserved(), dec!(0));
    }

    #[test]
    fn test_process_sell_fill() {
        let ledger = Ledger::new(dec!(100));

        // First buy some shares
        let buy_fill = Fill {
            fill_id: "f1".to_string(),
            order_id: "o1".to_string(),
            token_id: "token1".to_string(),
            side: Side::Buy,
            price: dec!(0.40),
            size: dec!(100),
            fee: dec!(0),
            expected_price: None,
            slippage_cost: Decimal::ZERO,
            timestamp: Utc::now(),
        };
        // Reserve and process
        ledger.cash.reserve(dec!(40)).unwrap();
        ledger.process_fill(buy_fill);

        // Now sell at higher price
        let sell_fill = Fill {
            fill_id: "f2".to_string(),
            order_id: "o2".to_string(),
            token_id: "token1".to_string(),
            side: Side::Sell,
            price: dec!(0.60),
            size: dec!(100),
            fee: dec!(0.30),
            expected_price: None,
            slippage_cost: Decimal::ZERO,
            timestamp: Utc::now(),
        };
        ledger.process_fill(sell_fill);

        // Check position closed
        let pos = ledger.positions.get(&"token1".to_string());
        assert!(pos.is_flat());
        // Realized P&L = (0.60 - 0.40) * 100 = 20
        assert_eq!(pos.realized_pnl, dec!(20));

        // Cash: started 100, spent 40, received 60, paid 0.30 fee
        // = 100 - 40 + 60 - 0.30 = 119.70
        assert_eq!(ledger.cash.available(), dec!(119.70));
    }

    #[test]
    fn test_snapshot() {
        let ledger = Ledger::new(dec!(500));
        let snap = ledger.snapshot();

        assert_eq!(snap.cash.total, dec!(500));
        assert_eq!(snap.positions.len(), 0);
        assert_eq!(snap.fill_count, 0);
    }
}
