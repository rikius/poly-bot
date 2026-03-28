//! Position tracking
//!
//! Tracks all positions (shares held) per token. Updates ONLY from confirmed fills.
//! Never updates from expected or unconfirmed events.

use crate::websocket::types::{Side, TokenId};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rust_decimal::prelude::Signed;
use rust_decimal::Decimal;

/// A position in a single token
#[derive(Debug, Clone)]
pub struct Position {
    /// Token ID
    pub token_id: TokenId,
    /// Net shares held (positive = long, negative = short)
    pub shares: Decimal,
    /// Average cost basis per share
    pub avg_cost: Decimal,
    /// Total cost basis (shares * avg_cost when opened)
    pub cost_basis: Decimal,
    /// Realized P&L from closed trades
    pub realized_pnl: Decimal,
    /// Unrealized P&L (mark-to-market)
    pub unrealized_pnl: Decimal,
    /// Total fees paid on this position
    pub total_fees: Decimal,
    /// When position was opened
    pub opened_at: DateTime<Utc>,
    /// Last update time
    pub last_update: DateTime<Utc>,
}

impl Position {
    /// Create a new empty position
    pub fn new(token_id: TokenId) -> Self {
        let now = Utc::now();
        Self {
            token_id,
            shares: Decimal::ZERO,
            avg_cost: Decimal::ZERO,
            cost_basis: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            total_fees: Decimal::ZERO,
            opened_at: now,
            last_update: now,
        }
    }

    /// Is this position flat (no shares)?
    pub fn is_flat(&self) -> bool {
        self.shares == Decimal::ZERO
    }

    /// Is this a long position?
    pub fn is_long(&self) -> bool {
        self.shares > Decimal::ZERO
    }

    /// Is this a short position?
    pub fn is_short(&self) -> bool {
        self.shares < Decimal::ZERO
    }

    /// Absolute size of position
    pub fn abs_shares(&self) -> Decimal {
        self.shares.abs()
    }

    /// Calculate unrealized P&L at given market price
    pub fn mark_to_market(&mut self, market_price: Decimal) {
        if self.is_flat() {
            self.unrealized_pnl = Decimal::ZERO;
        } else {
            // For long: unrealized = (market_price - avg_cost) * shares
            // For short: unrealized = (avg_cost - market_price) * abs(shares)
            self.unrealized_pnl = (market_price - self.avg_cost) * self.shares;
        }
        self.last_update = Utc::now();
    }

    /// Total P&L (realized + unrealized)
    pub fn total_pnl(&self) -> Decimal {
        self.realized_pnl + self.unrealized_pnl
    }

    /// Notional value of position at current avg_cost
    pub fn notional(&self) -> Decimal {
        self.shares.abs() * self.avg_cost
    }
}

/// A confirmed fill from the exchange
#[derive(Debug, Clone)]
pub struct Fill {
    /// Exchange fill ID
    pub fill_id: String,
    /// Order ID that was filled
    pub order_id: String,
    /// Token that was traded
    pub token_id: TokenId,
    /// Buy or Sell
    pub side: Side,
    /// Price of the fill
    pub price: Decimal,
    /// Size of the fill
    pub size: Decimal,
    /// Fee charged (in USDC)
    pub fee: Decimal,
    /// When the fill occurred
    pub timestamp: DateTime<Utc>,
}

impl Fill {
    /// Notional value of this fill
    pub fn notional(&self) -> Decimal {
        self.price * self.size
    }

    /// Signed size (positive for buy, negative for sell)
    pub fn signed_size(&self) -> Decimal {
        match self.side {
            Side::Buy => self.size,
            Side::Sell => -self.size,
        }
    }
}

/// Collection of positions indexed by token_id
pub struct Positions {
    positions: DashMap<TokenId, Position>,
}

impl Default for Positions {
    fn default() -> Self {
        Self::new()
    }
}

impl Positions {
    /// Create a new positions tracker
    pub fn new() -> Self {
        Self {
            positions: DashMap::new(),
        }
    }

    /// Get a position (returns empty position if not found)
    pub fn get(&self, token_id: &TokenId) -> Position {
        self.positions
            .get(token_id)
            .map(|r| r.clone())
            .unwrap_or_else(|| Position::new(token_id.clone()))
    }

    /// Check if we have any position in a token
    pub fn has_position(&self, token_id: &TokenId) -> bool {
        self.positions
            .get(token_id)
            .map(|p| !p.is_flat())
            .unwrap_or(false)
    }

    /// Apply a confirmed fill to update position
    /// This is the ONLY way positions should be updated
    pub fn apply_fill(&self, fill: &Fill) {
        let mut position = self
            .positions
            .entry(fill.token_id.clone())
            .or_insert_with(|| Position::new(fill.token_id.clone()));

        let old_shares = position.shares;
        let fill_shares = fill.signed_size();
        let new_shares = old_shares + fill_shares;

        // Track fees
        position.total_fees += fill.fee;

        if old_shares == Decimal::ZERO {
            // Opening a new position
            position.shares = new_shares;
            position.avg_cost = fill.price;
            position.cost_basis = fill.price * new_shares.abs();
            position.opened_at = fill.timestamp;
        } else if (old_shares > Decimal::ZERO) == (fill_shares > Decimal::ZERO) {
            // Adding to existing position (same direction)
            let old_cost = position.avg_cost * old_shares.abs();
            let new_cost = fill.price * fill_shares.abs();
            let total_cost = old_cost + new_cost;
            let total_shares = new_shares.abs();

            position.shares = new_shares;
            position.avg_cost = if total_shares > Decimal::ZERO {
                total_cost / total_shares
            } else {
                Decimal::ZERO
            };
            position.cost_basis = total_cost;
        } else {
            // Reducing or flipping position (opposite direction)
            let closing_shares = fill_shares.abs().min(old_shares.abs());

            // Realize P&L on closed portion
            let realized = (fill.price - position.avg_cost) * closing_shares * old_shares.signum();
            position.realized_pnl += realized;

            // Check if we crossed zero (flipped sides)
            let flipped = (old_shares > Decimal::ZERO && new_shares < Decimal::ZERO)
                || (old_shares < Decimal::ZERO && new_shares > Decimal::ZERO);

            if new_shares == Decimal::ZERO {
                // Full close
                position.shares = Decimal::ZERO;
                position.avg_cost = Decimal::ZERO;
                position.cost_basis = Decimal::ZERO;
            } else if flipped {
                // Flip position - new direction
                let remaining_fill = fill_shares.abs() - old_shares.abs();
                position.shares = new_shares;
                position.avg_cost = fill.price;
                position.cost_basis = fill.price * remaining_fill;
                position.opened_at = fill.timestamp;
            } else {
                // Partial close - keep same avg_cost
                position.shares = new_shares;
                position.cost_basis = position.avg_cost * new_shares.abs();
            }
        }

        position.last_update = fill.timestamp;
    }

    /// Get all non-flat positions
    pub fn all_positions(&self) -> Vec<Position> {
        self.positions
            .iter()
            .filter(|r| !r.is_flat())
            .map(|r| r.clone())
            .collect()
    }

    /// Total realized P&L across all positions
    pub fn total_realized_pnl(&self) -> Decimal {
        self.positions.iter().map(|r| r.realized_pnl).sum()
    }

    /// Total unrealized P&L across all positions
    pub fn total_unrealized_pnl(&self) -> Decimal {
        self.positions.iter().map(|r| r.unrealized_pnl).sum()
    }

    /// Total fees paid
    pub fn total_fees(&self) -> Decimal {
        self.positions.iter().map(|r| r.total_fees).sum()
    }

    /// Total notional exposure (sum of abs position values)
    pub fn total_notional(&self) -> Decimal {
        self.positions.iter().map(|r| r.notional()).sum()
    }

    /// Number of open positions
    pub fn count(&self) -> usize {
        self.positions.iter().filter(|r| !r.is_flat()).count()
    }

    /// Mark all positions to market
    pub fn mark_all_to_market(&self, prices: &[(TokenId, Decimal)]) {
        for (token_id, price) in prices {
            if let Some(mut position) = self.positions.get_mut(token_id) {
                position.mark_to_market(*price);
            }
        }
    }

    /// Clear all positions (for testing/reset)
    pub fn clear(&self) {
        self.positions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_fill(
        token_id: &str,
        side: Side,
        price: Decimal,
        size: Decimal,
        fee: Decimal,
    ) -> Fill {
        Fill {
            fill_id: "fill_1".to_string(),
            order_id: "order_1".to_string(),
            token_id: token_id.to_string(),
            side,
            price,
            size,
            fee,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_open_long_position() {
        let positions = Positions::new();

        // Buy 100 shares at $0.50
        let fill = make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0.50));
        positions.apply_fill(&fill);

        let pos = positions.get(&"token1".to_string());
        assert_eq!(pos.shares, dec!(100));
        assert_eq!(pos.avg_cost, dec!(0.50));
        assert!(pos.is_long());
    }

    #[test]
    fn test_add_to_long_position() {
        let positions = Positions::new();

        // Buy 100 at $0.50
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0)));

        // Buy 100 more at $0.60
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.60), dec!(100), dec!(0)));

        let pos = positions.get(&"token1".to_string());
        assert_eq!(pos.shares, dec!(200));
        // Average cost = (100*0.50 + 100*0.60) / 200 = 0.55
        assert_eq!(pos.avg_cost, dec!(0.55));
    }

    #[test]
    fn test_partial_close_with_profit() {
        let positions = Positions::new();

        // Buy 100 at $0.50
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0)));

        // Sell 50 at $0.70 (profit)
        positions.apply_fill(&make_fill("token1", Side::Sell, dec!(0.70), dec!(50), dec!(0)));

        let pos = positions.get(&"token1".to_string());
        assert_eq!(pos.shares, dec!(50));
        assert_eq!(pos.avg_cost, dec!(0.50)); // unchanged
        // Realized P&L = (0.70 - 0.50) * 50 = 10
        assert_eq!(pos.realized_pnl, dec!(10));
    }

    #[test]
    fn test_full_close() {
        let positions = Positions::new();

        // Buy 100 at $0.40
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.40), dec!(100), dec!(0)));

        // Sell 100 at $0.60 (full close)
        positions.apply_fill(&make_fill("token1", Side::Sell, dec!(0.60), dec!(100), dec!(0)));

        let pos = positions.get(&"token1".to_string());
        assert!(pos.is_flat());
        // Realized P&L = (0.60 - 0.40) * 100 = 20
        assert_eq!(pos.realized_pnl, dec!(20));
    }

    #[test]
    fn test_flip_position() {
        let positions = Positions::new();

        // Buy 100 at $0.50
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0)));

        // Sell 150 at $0.60 (close 100, open short 50)
        positions.apply_fill(&make_fill("token1", Side::Sell, dec!(0.60), dec!(150), dec!(0)));

        let pos = positions.get(&"token1".to_string());
        assert_eq!(pos.shares, dec!(-50));
        assert!(pos.is_short());
        assert_eq!(pos.avg_cost, dec!(0.60)); // new cost for short
        // Realized P&L from closing 100 shares = (0.60 - 0.50) * 100 = 10
        assert_eq!(pos.realized_pnl, dec!(10));
    }

    #[test]
    fn test_mark_to_market() {
        let positions = Positions::new();

        // Buy 100 at $0.50
        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0)));

        // Mark at $0.70
        positions.mark_all_to_market(&[("token1".to_string(), dec!(0.70))]);

        let pos = positions.get(&"token1".to_string());
        // Unrealized = (0.70 - 0.50) * 100 = 20
        assert_eq!(pos.unrealized_pnl, dec!(20));
    }

    #[test]
    fn test_fee_tracking() {
        let positions = Positions::new();

        positions.apply_fill(&make_fill("token1", Side::Buy, dec!(0.50), dec!(100), dec!(0.50)));
        positions.apply_fill(&make_fill("token1", Side::Sell, dec!(0.60), dec!(50), dec!(0.30)));

        let pos = positions.get(&"token1".to_string());
        assert_eq!(pos.total_fees, dec!(0.80));
    }
}
