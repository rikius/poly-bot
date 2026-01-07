//! Dynamic edge calculator for arbitrage strategies
//!
//! Instead of a static threshold (e.g., YES + NO < $0.97), we calculate
//! the required edge dynamically based on:
//! - Trading fees
//! - Expected slippage from order book depth
//! - Partial fill risk
//! - Spread penalty
//!
//! This prevents both:
//! - Missing opportunities (threshold too high)
//! - Unprofitable trades (threshold too low)

use crate::api::types::TokenId;
use crate::constants::{DEFAULT_MIN_EDGE, MAKER_MIN_EDGE, THIN_BOOK_EXTRA_MARGIN};
use crate::state::order_book::BookSnapshot;
use crate::state::OrderBookState;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

/// Edge calculation result
#[derive(Debug, Clone)]
pub struct EdgeCalculation {
    /// Total required edge to execute profitably
    pub required_edge: Decimal,

    /// Actual edge available (1 - yes_ask - no_ask)
    pub actual_edge: Decimal,

    /// Is the opportunity profitable?
    pub is_profitable: bool,

    /// Maximum executable size (limited by book depth)
    pub max_size: Decimal,

    /// Breakdown of edge components
    pub components: EdgeComponents,
}

/// Breakdown of edge calculation components
#[derive(Debug, Clone)]
pub struct EdgeComponents {
    /// Fee component (taker fees if applicable)
    pub fees: Decimal,

    /// Estimated slippage based on size vs depth
    pub slippage: Decimal,

    /// Extra margin for partial fill risk
    pub partial_fill_risk: Decimal,

    /// Penalty for wide spreads
    pub spread_penalty: Decimal,
}

/// Configuration for edge calculation
#[derive(Debug, Clone)]
pub struct EdgeConfig {
    /// Minimum edge even in ideal conditions
    pub min_edge: Decimal,

    /// Extra margin for thin order books
    pub thin_book_margin: Decimal,

    /// Minimum book depth to consider (in shares)
    pub min_depth_threshold: Decimal,

    /// Slippage multiplier (higher = more conservative)
    pub slippage_factor: Decimal,

    /// Whether we're acting as maker (no fees)
    pub is_maker: bool,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            min_edge: DEFAULT_MIN_EDGE,
            thin_book_margin: THIN_BOOK_EXTRA_MARGIN,
            min_depth_threshold: dec!(100), // Minimum 100 shares at top of book
            slippage_factor: dec!(0.5),     // 50% of spread as slippage estimate
            is_maker: false,
        }
    }
}

impl EdgeConfig {
    /// Create config for taker execution
    pub fn taker() -> Self {
        Self::default()
    }

    /// Create config for maker execution (lower edge requirement)
    pub fn maker() -> Self {
        Self {
            min_edge: MAKER_MIN_EDGE,
            is_maker: true,
            ..Self::default()
        }
    }

    /// Set minimum edge
    pub fn with_min_edge(mut self, min_edge: Decimal) -> Self {
        self.min_edge = min_edge;
        self
    }
}

/// Calculator for dynamic arbitrage edge requirements
pub struct EdgeCalculator {
    config: EdgeConfig,
}

impl EdgeCalculator {
    /// Create a new edge calculator with default config
    pub fn new() -> Self {
        Self {
            config: EdgeConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: EdgeConfig) -> Self {
        Self { config }
    }

    /// Calculate required edge for an arbitrage opportunity
    ///
    /// # Arguments
    /// * `yes_book` - Order book snapshot for YES token
    /// * `no_book` - Order book snapshot for NO token
    /// * `fee_rate_bps` - Fee rate in basis points (0 for standard, 1000 for 15-min crypto)
    /// * `intended_size` - How many shares we want to trade
    ///
    /// # Returns
    /// EdgeCalculation with profitability assessment
    pub fn calculate(
        &self,
        yes_book: &BookSnapshot,
        no_book: &BookSnapshot,
        fee_rate_bps: u32,
        intended_size: Decimal,
    ) -> EdgeCalculation {
        // Get best asks (we're buying both sides)
        let yes_ask = yes_book.best_ask().unwrap_or(Decimal::ONE);
        let no_ask = no_book.best_ask().unwrap_or(Decimal::ONE);

        // Calculate actual edge available
        let combined_cost = yes_ask + no_ask;
        let actual_edge = Decimal::ONE - combined_cost;

        // Calculate fee component
        let fees = self.calculate_fees(fee_rate_bps, combined_cost);

        // Calculate slippage estimate
        let slippage = self.estimate_slippage(yes_book, no_book, intended_size);

        // Calculate partial fill risk margin
        let partial_fill_risk = self.partial_fill_margin(yes_book, no_book);

        // Calculate spread penalty
        let spread_penalty = self.spread_penalty(yes_book, no_book);

        // Total required edge
        let required_edge = (fees + slippage + partial_fill_risk + spread_penalty)
            .max(self.config.min_edge);

        // Maximum executable size (minimum of both sides' depth)
        let yes_depth = yes_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        let no_depth = no_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);
        let max_size = yes_depth.min(no_depth);

        let is_profitable = actual_edge >= required_edge && max_size >= intended_size;

        debug!(
            yes_ask = %yes_ask,
            no_ask = %no_ask,
            actual_edge = %actual_edge,
            required_edge = %required_edge,
            max_size = %max_size,
            is_profitable = is_profitable,
            "Edge calculation"
        );

        EdgeCalculation {
            required_edge,
            actual_edge,
            is_profitable,
            max_size,
            components: EdgeComponents {
                fees,
                slippage,
                partial_fill_risk,
                spread_penalty,
            },
        }
    }

    /// Quick check if arbitrage is possible without full calculation
    ///
    /// Returns Some((yes_ask, no_ask, edge)) if potentially profitable
    pub fn quick_check(
        &self,
        books: &OrderBookState,
        yes_token: &TokenId,
        no_token: &TokenId,
    ) -> Option<(Decimal, Decimal, Decimal)> {
        let yes_ask = books.best_ask(yes_token)?;
        let no_ask = books.best_ask(no_token)?;

        let combined = yes_ask + no_ask;
        let edge = Decimal::ONE - combined;

        // Quick filter: if combined cost >= 1, no arb possible
        if combined >= Decimal::ONE {
            return None;
        }

        // Quick filter: if edge < minimum, skip detailed calculation
        if edge < self.config.min_edge {
            return None;
        }

        Some((yes_ask, no_ask, edge))
    }

    /// Calculate fee component based on rate and combined cost
    fn calculate_fees(&self, fee_rate_bps: u32, combined_cost: Decimal) -> Decimal {
        if self.config.is_maker {
            // Makers pay no fees
            Decimal::ZERO
        } else {
            // Takers pay fee on the trade
            // Fee rate is in bps (1000 = 10%)
            // For 15-min crypto at 50/50 odds, fee is ~3% per side
            // We're buying both sides, so fee applies to both
            let fee_rate = Decimal::from(fee_rate_bps) / dec!(10000);

            // Fee is based on the trade amount
            // Approximate: fee_rate * combined_cost
            // This is conservative (actual fee may be slightly less at extreme odds)
            fee_rate * combined_cost
        }
    }

    /// Estimate slippage based on intended size vs book depth
    fn estimate_slippage(
        &self,
        yes_book: &BookSnapshot,
        no_book: &BookSnapshot,
        intended_size: Decimal,
    ) -> Decimal {
        // Get depth at top of book
        let yes_depth = yes_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        let no_depth = no_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        let min_depth = yes_depth.min(no_depth);

        if min_depth.is_zero() {
            // No liquidity - maximum slippage
            return dec!(0.10); // 10% penalty
        }

        // If we need more than available at top, we'll face slippage
        if intended_size > min_depth {
            // Estimate: proportional to how much we exceed depth
            let excess_ratio = (intended_size - min_depth) / min_depth;

            // Get spread to estimate price impact
            let yes_spread = yes_book.spread().unwrap_or(dec!(0.05));
            let no_spread = no_book.spread().unwrap_or(dec!(0.05));
            let avg_spread = (yes_spread + no_spread) / dec!(2);

            // Slippage increases with excess and spread
            (excess_ratio * avg_spread * self.config.slippage_factor).min(dec!(0.10))
        } else {
            // Can fill at top of book
            Decimal::ZERO
        }
    }

    /// Calculate extra margin for partial fill risk
    fn partial_fill_margin(
        &self,
        yes_book: &BookSnapshot,
        no_book: &BookSnapshot,
    ) -> Decimal {
        let yes_depth = yes_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        let no_depth = no_book
            .asks
            .first()
            .and_then(|l| l.size.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO);

        let min_depth = yes_depth.min(no_depth);

        // Thin books get extra margin for partial fill risk
        if min_depth < self.config.min_depth_threshold {
            self.config.thin_book_margin
        } else if min_depth < self.config.min_depth_threshold * dec!(2) {
            self.config.thin_book_margin / dec!(2)
        } else {
            Decimal::ZERO
        }
    }

    /// Calculate spread penalty (wider spreads = more risk)
    fn spread_penalty(&self, yes_book: &BookSnapshot, no_book: &BookSnapshot) -> Decimal {
        let yes_spread = yes_book.spread().unwrap_or(dec!(0.10));
        let no_spread = no_book.spread().unwrap_or(dec!(0.10));

        let avg_spread = (yes_spread + no_spread) / dec!(2);

        // Penalty for spreads wider than 5 cents
        if avg_spread > dec!(0.05) {
            (avg_spread - dec!(0.05)) * dec!(0.5)
        } else {
            Decimal::ZERO
        }
    }
}

impl Default for EdgeCalculator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::PriceLevel;

    fn make_book(token_id: &str, best_bid: &str, best_ask: &str, depth: &str) -> BookSnapshot {
        BookSnapshot {
            token_id: token_id.to_string(),
            market: "market123".to_string(),
            bids: vec![PriceLevel {
                price: best_bid.to_string(),
                size: depth.to_string(),
            }],
            asks: vec![PriceLevel {
                price: best_ask.to_string(),
                size: depth.to_string(),
            }],
            last_update: Some(1234567890),
            hash: None,
        }
    }

    #[test]
    fn test_profitable_arb() {
        let calc = EdgeCalculator::new();

        // YES ask = 0.48, NO ask = 0.49, combined = 0.97
        // Edge = 0.03 (3 cents)
        let yes_book = make_book("yes", "0.47", "0.48", "1000");
        let no_book = make_book("no", "0.48", "0.49", "1000");

        let result = calc.calculate(&yes_book, &no_book, 0, dec!(100));

        assert_eq!(result.actual_edge, dec!(0.03));
        assert!(result.is_profitable);
        assert!(result.max_size >= dec!(100));
    }

    #[test]
    fn test_no_arb_when_sum_equals_one() {
        let calc = EdgeCalculator::new();

        // YES ask = 0.50, NO ask = 0.50, combined = 1.00
        let yes_book = make_book("yes", "0.49", "0.50", "1000");
        let no_book = make_book("no", "0.49", "0.50", "1000");

        let result = calc.calculate(&yes_book, &no_book, 0, dec!(100));

        assert_eq!(result.actual_edge, Decimal::ZERO);
        assert!(!result.is_profitable);
    }

    #[test]
    fn test_no_arb_when_sum_exceeds_one() {
        let calc = EdgeCalculator::new();

        // YES ask = 0.52, NO ask = 0.52, combined = 1.04
        let yes_book = make_book("yes", "0.51", "0.52", "1000");
        let no_book = make_book("no", "0.51", "0.52", "1000");

        let result = calc.calculate(&yes_book, &no_book, 0, dec!(100));

        assert_eq!(result.actual_edge, dec!(-0.04));
        assert!(!result.is_profitable);
    }

    #[test]
    fn test_fees_reduce_profitability() {
        let calc = EdgeCalculator::new();

        // YES ask = 0.48, NO ask = 0.49, combined = 0.97
        // Edge = 0.03, but with 10% fee rate (1000 bps), fees eat the edge
        let yes_book = make_book("yes", "0.47", "0.48", "1000");
        let no_book = make_book("no", "0.48", "0.49", "1000");

        // With 1000 bps (10%) fees
        let result = calc.calculate(&yes_book, &no_book, 1000, dec!(100));

        // Fee = 10% * 0.97 = 0.097, which exceeds the 0.03 edge
        assert!(!result.is_profitable);
    }

    #[test]
    fn test_thin_book_margin() {
        let calc = EdgeCalculator::new();

        // Good edge but thin book (only 50 shares)
        let yes_book = make_book("yes", "0.45", "0.46", "50");
        let no_book = make_book("no", "0.47", "0.48", "50");

        let result = calc.calculate(&yes_book, &no_book, 0, dec!(100));

        // Can't fill 100 shares when only 50 available
        assert!(!result.is_profitable);
        assert_eq!(result.max_size, dec!(50));
    }

    #[test]
    fn test_maker_lower_threshold() {
        let calc = EdgeCalculator::with_config(EdgeConfig::maker());

        // Smaller edge that works for maker but not taker
        let yes_book = make_book("yes", "0.49", "0.495", "1000");
        let no_book = make_book("no", "0.49", "0.495", "1000");

        // Combined = 0.99, edge = 0.01
        let result = calc.calculate(&yes_book, &no_book, 0, dec!(100));

        // Maker min edge is 0.01, so this should be profitable
        assert!(result.is_profitable);
    }

    #[test]
    fn test_quick_check() {
        let calc = EdgeCalculator::new();
        let books = OrderBookState::new();

        books.update_book(
            "yes".to_string(),
            "market".to_string(),
            vec![PriceLevel {
                price: "0.47".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.48".to_string(),
                size: "1000".to_string(),
            }],
            None,
            None,
        );

        books.update_book(
            "no".to_string(),
            "market".to_string(),
            vec![PriceLevel {
                price: "0.48".to_string(),
                size: "1000".to_string(),
            }],
            vec![PriceLevel {
                price: "0.49".to_string(),
                size: "1000".to_string(),
            }],
            None,
            None,
        );

        let result = calc.quick_check(&books, &"yes".to_string(), &"no".to_string());

        assert!(result.is_some());
        let (yes_ask, no_ask, edge) = result.unwrap();
        assert_eq!(yes_ask, dec!(0.48));
        assert_eq!(no_ask, dec!(0.49));
        assert_eq!(edge, dec!(0.03));
    }
}
