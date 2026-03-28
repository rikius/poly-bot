//! Temporal Arbitrage Strategy
//!
//! Detects when Polymarket's binary market price has **not yet priced in** a
//! significant move on the underlying asset (BTC, ETH, SOL) as observed on
//! Binance.  This is the temporal arb played by top traders like
//! JaneStreetIndia ($324 K / 25 days, 99.5 % win rate).
//!
//! ## How it works
//!
//! 1. An external Binance feed maintains rolling 30 s price-change data in a
//!    shared [`ExternalPriceStore`].
//! 2. On every `on_book_update`, the strategy checks whether the external
//!    price has moved more than `threshold_bps` in the rolling window.
//! 3. If yes, it estimates a **fair probability** for the "Up" outcome using a
//!    simple linear model:
//!    ```text
//!    fair_yes = 0.5 + change_bps / sensitivity_bps
//!    ```
//! 4. If the Polymarket ask for the UP token is below `fair_yes − min_edge`,
//!    the book is stale — we buy before it reprices.
//! 5. Symmetrically for the DOWN token.
//!
//! ## Limitations of the simple model
//!
//! The linear model is intentionally crude.  For production use, replace
//! `estimate_fair_yes` with a log-normal model using realised 15-minute
//! volatility.  The architecture is unchanged — only the estimator needs
//! updating.
//!
//! ## Enable
//!
//! Set `TEMPORAL_ARB_ENABLED=true` in `.env`.  Also set
//! `TEMPORAL_ARB_THRESHOLD_BPS` (default: 100 bps = 1 %) and
//! `TEMPORAL_ARB_SENSITIVITY_BPS` (default: 2000 bps).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::feeds::ExternalPriceStore;
use crate::ledger::Fill;
use crate::strategy::market_pair::{MarketPairRegistry};
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use crate::websocket::types::{ConditionId, Side, TokenId};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for [`TemporalArbStrategy`].
#[derive(Debug, Clone)]
pub struct TemporalArbConfig {
    /// Minimum external price move (in bps) over the rolling window to
    /// consider the Polymarket book potentially stale.
    ///
    /// Default: 100 bps (1 %).
    pub threshold_bps: i64,

    /// Sensitivity parameter for the fair-probability model.
    ///
    /// `fair_yes = 0.5 + change_bps / sensitivity_bps`
    ///
    /// Default: 2000 (a 2 % move → fair_yes = 0.6 / fair_no = 0.4).
    pub sensitivity_bps: i64,

    /// Minimum edge between fair price and current ask to trade.
    pub min_edge: Decimal,

    /// Maximum position size per trade (USDC notional).
    pub max_position_size: Decimal,

    /// Minimum position size per trade.
    pub min_position_size: Decimal,

    /// Maximum total exposure.
    pub max_total_exposure: Decimal,

    /// Cooldown between trades on the same market (ms).
    pub cooldown_ms: u64,

    /// Maximum age of an external price record before it is ignored.
    ///
    /// If the Binance feed is down (> max_feed_age_secs stale), we skip.
    pub max_feed_age_secs: u64,
}

impl Default for TemporalArbConfig {
    fn default() -> Self {
        Self {
            threshold_bps: 100,         // 1 % move required
            sensitivity_bps: 2000,      // 2 % move → 50 % probability shift
            min_edge: dec!(0.03),        // 3-cent edge
            max_position_size: dec!(200),
            min_position_size: dec!(10),
            max_total_exposure: dec!(1000),
            cooldown_ms: 5_000,
            max_feed_age_secs: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// Temporal arbitrage strategy — trades Polymarket binary books that are
/// lagging the underlying Binance spot price.
pub struct TemporalArbStrategy {
    name: String,
    config: TemporalArbConfig,
    registry: Arc<MarketPairRegistry>,
    external_prices: ExternalPriceStore,
    enabled: AtomicBool,
    last_trade: dashmap::DashMap<ConditionId, Instant>,
    trade_count: AtomicU64,
    current_exposure: std::sync::RwLock<Decimal>,
}

impl TemporalArbStrategy {
    /// Create with default config.
    pub fn new(registry: Arc<MarketPairRegistry>, prices: ExternalPriceStore) -> Self {
        Self::with_config(registry, prices, TemporalArbConfig::default())
    }

    /// Create with custom config.
    pub fn with_config(
        registry: Arc<MarketPairRegistry>,
        prices: ExternalPriceStore,
        config: TemporalArbConfig,
    ) -> Self {
        Self {
            name: "TemporalArbStrategy".to_string(),
            config,
            registry,
            external_prices: prices,
            enabled: AtomicBool::new(true),
            last_trade: dashmap::DashMap::new(),
            trade_count: AtomicU64::new(0),
            current_exposure: std::sync::RwLock::new(Decimal::ZERO),
        }
    }

    /// Enable or disable the strategy at runtime.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Number of trades generated since startup.
    pub fn trade_count(&self) -> u64 {
        self.trade_count.load(Ordering::Relaxed)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn is_on_cooldown(&self, cid: &ConditionId) -> bool {
        self.last_trade
            .get(cid)
            .map(|t| t.elapsed().as_millis() < self.config.cooldown_ms as u128)
            .unwrap_or(false)
    }

    /// Simple linear model: `fair_yes = 0.5 + change_bps / sensitivity_bps`.
    ///
    /// Clamped to `[0.05, 0.95]` to avoid degenerate orders.
    fn estimate_fair_yes(&self, change_bps: i64) -> Decimal {
        let adjustment = Decimal::from(change_bps) / Decimal::from(self.config.sensitivity_bps);
        (dec!(0.5) + adjustment)
            .max(dec!(0.05))
            .min(dec!(0.95))
    }

    fn check_opportunity(
        &self,
        pair: &crate::strategy::market_pair::MarketPair,
        ctx: &StrategyContext,
    ) -> Option<Vec<OrderIntent>> {
        // Only process markets with a known asset symbol
        let symbol = pair.asset_symbol.as_ref()?;

        // Get external price record
        let ext = self.external_prices.get(symbol.as_str())?;

        // Reject stale feed data
        if !ext.is_fresh(self.config.max_feed_age_secs) {
            debug!(
                market = %pair.condition_id,
                symbol = %symbol,
                "External feed stale — skipping"
            );
            return None;
        }

        let change_bps = ext.change_bps;

        // Only act on significant moves
        if change_bps.abs() < self.config.threshold_bps {
            return None;
        }

        // Estimate fair probability
        let fair_yes = self.estimate_fair_yes(change_bps);
        let fair_no = dec!(1) - fair_yes;

        let yes_ask = ctx.best_ask(&pair.yes_token_id)?;
        let no_ask = ctx.best_ask(&pair.no_token_id)?;

        // Check cooldown
        if self.is_on_cooldown(&pair.condition_id) {
            return None;
        }

        // Determine trade direction
        let (token_id, ask, fair_val, side_label) = if change_bps > 0 {
            // Price going UP → UP token should reprice higher
            if yes_ask >= fair_yes - self.config.min_edge {
                debug!(
                    market = %pair.condition_id,
                    yes_ask = %yes_ask,
                    fair_yes = %fair_yes,
                    "UP token already priced in move"
                );
                return None;
            }
            (&pair.yes_token_id, yes_ask, fair_yes, "UP")
        } else {
            // Price going DOWN → DOWN token should reprice higher
            if no_ask >= fair_no - self.config.min_edge {
                debug!(
                    market = %pair.condition_id,
                    no_ask = %no_ask,
                    fair_no = %fair_no,
                    "DOWN token already priced in move"
                );
                return None;
            }
            (&pair.no_token_id, no_ask, fair_no, "DOWN")
        };

        // Size: buy up to the estimated fair value (cap by config)
        let max_by_exposure = {
            let current = *self.current_exposure.read().unwrap();
            (self.config.max_total_exposure - current).max(Decimal::ZERO)
        };
        let trade_size = (self.config.max_position_size / ask)
            .min(max_by_exposure / ask)
            .max(Decimal::ZERO)
            .min(dec!(10_000)); // sanity cap

        if trade_size * ask < self.config.min_position_size {
            debug!(
                market = %pair.condition_id,
                "Trade size too small for temporal arb"
            );
            return None;
        }

        info!(
            market = %pair.condition_id,
            symbol = %symbol,
            side = %side_label,
            ask = %ask,
            fair = %fair_val,
            change_bps = %change_bps,
            trade_size = %trade_size,
            "⚡ Temporal arb: {} stale, {} moved {}bps",
            side_label, symbol, change_bps
        );

        self.last_trade.insert(pair.condition_id.clone(), Instant::now());
        self.trade_count.fetch_add(1, Ordering::Relaxed);
        {
            let mut exp = self.current_exposure.write().unwrap();
            *exp += ask * trade_size;
        }

        let intent = OrderIntent::new(
            pair.condition_id.clone(),
            token_id.clone(),
            Side::Buy,
            ask, // FOK at current ask — we want immediate fill
            trade_size,
            Urgency::Immediate,
            format!(
                "Temporal arb: {} {}bps, fair={:.2} ask={:.2}",
                symbol, change_bps, fair_val, ask
            ),
            self.name.clone(),
        )
        .with_priority(95); // High — speed is critical

        Some(vec![intent])
    }
}

// ---------------------------------------------------------------------------
// Strategy trait
// ---------------------------------------------------------------------------

impl Strategy for TemporalArbStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        95 // High — temporal arb is time-critical
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        self.registry.all_condition_ids()
    }

    fn on_book_update(
        &self,
        market_id: &ConditionId,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        if !self.is_enabled() {
            return vec![];
        }

        let pair = match self.registry.get_by_token(token_id) {
            Some(p) => p,
            None => return vec![],
        };

        if &pair.condition_id != market_id {
            return vec![];
        }

        // Skip markets without an asset symbol (not a tracked crypto market)
        if pair.asset_symbol.is_none() {
            return vec![];
        }

        self.check_opportunity(&pair, ctx).unwrap_or_default()
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Reduce tracked exposure on any fill
        if fill.side == Side::Buy {
            // Bought in — exposure already tracked
        } else {
            let mut exp = self.current_exposure.write().unwrap();
            *exp = (*exp - fill.notional()).max(Decimal::ZERO);
        }
        vec![]
    }

    fn on_shutdown(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        info!(
            trades = self.trade_count(),
            "TemporalArbStrategy shutting down"
        );
        // Temporal arb is taker-only (Immediate/FOK) — no open GTC orders to cancel
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::{new_price_store, ExternalPriceRecord};
    use crate::ledger::Ledger;
    use crate::state::OrderBookState;
    use crate::strategy::market_pair::{MarketPair, MarketPairRegistry};
    use crate::websocket::types::PriceLevel;

    fn make_registry() -> Arc<MarketPairRegistry> {
        let r = Arc::new(MarketPairRegistry::new());
        r.register(
            MarketPair::new(
                "0xbtc_market".to_string(),
                "yes_tok".to_string(),
                "no_tok".to_string(),
            )
            .with_fee_rate(1000)
            .with_asset_symbol("BTC"),
        );
        r
    }

    fn books_balanced() -> OrderBookState {
        let b = OrderBookState::new();
        // Balanced 50/50 book — no obvious arb signal
        b.update_book(
            "yes_tok".to_string(),
            "0xbtc_market".to_string(),
            vec![PriceLevel { price: "0.49".to_string(), size: "500".to_string() }],
            vec![PriceLevel { price: "0.50".to_string(), size: "500".to_string() }],
            None,
            None,
        );
        b.update_book(
            "no_tok".to_string(),
            "0xbtc_market".to_string(),
            vec![PriceLevel { price: "0.49".to_string(), size: "500".to_string() }],
            vec![PriceLevel { price: "0.50".to_string(), size: "500".to_string() }],
            None,
            None,
        );
        b
    }

    #[test]
    fn test_no_signal_when_no_feed() {
        let strategy = TemporalArbStrategy::new(make_registry(), new_price_store());
        let books = books_balanced();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        // No external price → no trade
        assert!(intents.is_empty());
    }

    #[test]
    fn test_no_signal_below_threshold() {
        let prices = new_price_store();
        // BTC price up only 50 bps (below 100 bps threshold)
        let mut rec = ExternalPriceRecord::new("BTC", dec!(50_000));
        rec.update(dec!(50_250), 30); // +50 bps
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(make_registry(), prices);
        let books = books_balanced();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert!(intents.is_empty());
    }

    #[test]
    fn test_signal_buy_up_on_btc_surge() {
        let prices = new_price_store();
        // BTC up 200 bps → fair_yes = 0.5 + 200/2000 = 0.60
        // Current ask = 0.50 → edge = 0.10 → trade
        let mut rec = ExternalPriceRecord::new("BTC", dec!(50_000));
        rec.update(dec!(51_000), 30); // +200 bps
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(make_registry(), prices);
        let books = books_balanced(); // YES ask = 0.50
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].token_id, "yes_tok");
        assert_eq!(intents[0].side, Side::Buy);
        assert_eq!(intents[0].urgency, Urgency::Immediate);
    }

    #[test]
    fn test_signal_buy_down_on_btc_drop() {
        let prices = new_price_store();
        // BTC down 200 bps → fair_no = 0.5 + 200/2000 = 0.60
        // NO ask = 0.50 → edge = 0.10 → trade
        let mut rec = ExternalPriceRecord::new("BTC", dec!(50_000));
        rec.update(dec!(49_000), 30); // −200 bps
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(make_registry(), prices);
        let books = books_balanced();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].token_id, "no_tok");
        assert_eq!(intents[0].side, Side::Buy);
    }

    #[test]
    fn test_no_trade_when_already_priced_in() {
        let prices = new_price_store();
        // BTC up 200 bps → fair_yes = 0.60
        // But YES ask is already 0.58 — only 2 cent gap, below 3-cent min_edge
        let mut rec = ExternalPriceRecord::new("BTC", dec!(50_000));
        rec.update(dec!(51_000), 30);
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(make_registry(), prices);

        let books = OrderBookState::new();
        books.update_book(
            "yes_tok".to_string(),
            "0xbtc_market".to_string(),
            vec![PriceLevel { price: "0.57".to_string(), size: "500".to_string() }],
            vec![PriceLevel { price: "0.58".to_string(), size: "500".to_string() }],
            None,
            None,
        );
        books.update_book(
            "no_tok".to_string(),
            "0xbtc_market".to_string(),
            vec![PriceLevel { price: "0.41".to_string(), size: "500".to_string() }],
            vec![PriceLevel { price: "0.42".to_string(), size: "500".to_string() }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        // fair_yes = 0.60, ask = 0.58 → gap = 0.02 < min_edge 0.03
        assert!(intents.is_empty());
    }

    #[test]
    fn test_disabled_strategy() {
        let prices = new_price_store();
        let mut rec = ExternalPriceRecord::new("BTC", dec!(50_000));
        rec.update(dec!(51_000), 30);
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(make_registry(), prices);
        strategy.set_enabled(false);

        let books = books_balanced();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xbtc_market".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert!(intents.is_empty());
    }

    #[test]
    fn test_no_symbol_market_skipped() {
        // Market without asset_symbol should be ignored
        let r = Arc::new(MarketPairRegistry::new());
        r.register(
            MarketPair::new(
                "0xother".to_string(),
                "yes2".to_string(),
                "no2".to_string(),
            )
            // No with_asset_symbol call
        );

        let prices = new_price_store();
        let mut rec = ExternalPriceRecord::new("BTC", dec!(51_000));
        rec.update(dec!(52_000), 30);
        prices.insert("BTC".to_string(), rec);

        let strategy = TemporalArbStrategy::new(r, prices);

        let books = OrderBookState::new();
        books.update_book(
            "yes2".to_string(),
            "0xother".to_string(),
            vec![PriceLevel { price: "0.49".to_string(), size: "100".to_string() }],
            vec![PriceLevel { price: "0.50".to_string(), size: "100".to_string() }],
            None,
            None,
        );
        books.update_book(
            "no2".to_string(),
            "0xother".to_string(),
            vec![PriceLevel { price: "0.49".to_string(), size: "100".to_string() }],
            vec![PriceLevel { price: "0.50".to_string(), size: "100".to_string() }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(&"0xother".to_string(), &"yes2".to_string(), &ctx);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_fair_yes_estimation() {
        let strategy = TemporalArbStrategy::new(make_registry(), new_price_store());
        // +200 bps move → fair_yes = 0.5 + 200/2000 = 0.60
        assert_eq!(strategy.estimate_fair_yes(200), dec!(0.60));
        // −200 bps → 0.40
        assert_eq!(strategy.estimate_fair_yes(-200), dec!(0.40));
        // 0 bps → 0.50
        assert_eq!(strategy.estimate_fair_yes(0), dec!(0.50));
        // Clamp upper
        assert_eq!(strategy.estimate_fair_yes(10_000), dec!(0.95));
        // Clamp lower
        assert_eq!(strategy.estimate_fair_yes(-10_000), dec!(0.05));
    }
}
