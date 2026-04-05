//! TBO Trend (Trending Breakout) strategy for Polymarket binary markets.
//!
//! ## Hypothesis
//!
//! When a token's mid-price breaks above its recent rolling high by a meaningful
//! threshold and sustains that level for multiple consecutive book updates, it
//! signals building crowd conviction — a momentum breakout rather than noise.
//! The strategy enters in the direction of the breakout and exits on a time
//! limit, profit target, or stop-loss.
//!
//! ## Entry signal (gates must all pass)
//!
//! 1. **Warm-up** — at least `lookback_ticks` book updates recorded for the token.
//! 2. **Breakout** — current mid > (rolling high × (1 + breakout_threshold_bps/10000)).
//! 3. **Confirmation** — breakout level sustained for ≥ `min_confirm_ticks` consecutive updates.
//! 4. **Liquidity** — ask-side depth ≥ `min_ask_depth`.
//! 5. **Price cap** — best ask < 0.92 (don't chase near-certain outcomes).
//! 6. **Position guard** — flat on this token and its complement.
//! 7. **Cooldown** — per condition_id.
//!
//! ## Exit (checked every tick, ~100 ms)
//!
//! - **Time** — position held ≥ `max_hold_secs`
//! - **Profit** — unrealized gain ≥ `profit_target_pct`
//! - **Stop-loss** — unrealized loss ≥ `stop_loss_pct`
//!
//! ## Enable
//!
//! Set `TBO_ENABLED=true` in `.env`.  Conservative starting parameters:
//! `TBO_BREAKOUT_THRESHOLD_BPS=200`, `TBO_MIN_CONFIRM_TICKS=4`, `TBO_MAX_BET_USD=5`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use crate::ledger::Fill;
use crate::strategy::market_pair::MarketPairRegistry;
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use crate::websocket::types::{ConditionId, Side, TokenId};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for [`TboTrendStrategy`].
#[derive(Debug, Clone)]
pub struct TboConfig {
    /// Number of book update ticks forming the rolling lookback window.
    ///
    /// Strategy is inactive until this many ticks have accumulated (warm-up).
    /// Default: 50.
    pub lookback_ticks: usize,

    /// How many basis points above the rolling high the current mid must be
    /// to qualify as a breakout.
    ///
    /// Default: 150 bps (1.5%).
    pub breakout_threshold_bps: i64,

    /// Number of consecutive ticks the price must remain above the breakout
    /// level before an entry intent is generated.
    ///
    /// Filters single-tick spikes. Default: 3.
    pub min_confirm_ticks: u32,

    /// Minimum ask-side depth (shares) at time of entry.
    ///
    /// Default: 10 shares.
    pub min_ask_depth: Decimal,

    /// Maximum notional per trade (USDC).
    ///
    /// Default: 100.
    pub max_bet_usd: Decimal,

    /// Skip if `shares × ask < min_bet_usd`.
    ///
    /// Default: 5.
    pub min_bet_usd: Decimal,

    /// Time-based exit: close the position after this many seconds.
    ///
    /// Default: 180 s.
    pub max_hold_secs: u64,

    /// Profit-target exit: close when unrealized gain / entry_price ≥ this.
    ///
    /// Default: 0.06 (6%).
    pub profit_target_pct: Decimal,

    /// Stop-loss exit: close when unrealized loss / entry_price ≥ this.
    ///
    /// Default: 0.04 (4%).
    pub stop_loss_pct: Decimal,

    /// Per-market cooldown after a trade fires (ms).
    ///
    /// Default: 15 000 ms (15 s).
    pub cooldown_ms: u64,
}

impl Default for TboConfig {
    fn default() -> Self {
        Self {
            lookback_ticks: 50,
            breakout_threshold_bps: 150,
            min_confirm_ticks: 3,
            min_ask_depth: dec!(10),
            max_bet_usd: dec!(100),
            min_bet_usd: dec!(5),
            max_hold_secs: 180,
            profit_target_pct: dec!(0.06),
            stop_loss_pct: dec!(0.04),
            cooldown_ms: 15_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy struct
// ---------------------------------------------------------------------------

/// TBO Trend (Trending Breakout) strategy.
///
/// See the module-level documentation for the signal logic and configuration.
pub struct TboTrendStrategy {
    name: String,
    config: TboConfig,
    registry: Arc<MarketPairRegistry>,
    enabled: AtomicBool,

    /// Rolling mid-price history per token (newest at front, tick-based).
    price_history: DashMap<TokenId, VecDeque<Decimal>>,

    /// Consecutive ticks each token has been above its breakout level.
    breakout_confirm: DashMap<TokenId, u32>,

    /// Open position tracking: `token_id → (entry_price, entry_time)`.
    entry_tracking: DashMap<TokenId, (Decimal, Instant)>,

    /// Tokens for which a close order is in flight — prevents duplicate exits.
    pending_exit: DashMap<TokenId, Instant>,

    /// Per-market cooldown tracker.
    last_trade: DashMap<ConditionId, Instant>,

    trade_count: AtomicU64,
}

impl TboTrendStrategy {
    /// Create with default config.
    pub fn new(registry: Arc<MarketPairRegistry>) -> Self {
        Self::with_config(registry, TboConfig::default())
    }

    /// Create with a specific config.
    pub fn with_config(registry: Arc<MarketPairRegistry>, config: TboConfig) -> Self {
        Self {
            name: "tbo_trend".to_string(),
            config,
            registry,
            enabled: AtomicBool::new(true),
            price_history: DashMap::new(),
            breakout_confirm: DashMap::new(),
            entry_tracking: DashMap::new(),
            pending_exit: DashMap::new(),
            last_trade: DashMap::new(),
            trade_count: AtomicU64::new(0),
        }
    }

    /// Enable or disable at runtime.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Trades generated since startup.
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

    /// Push a new mid-price and cap the history at `lookback_ticks`.
    fn update_price_history(&self, token_id: &TokenId, mid: Decimal) {
        let mut entry = self.price_history
            .entry(token_id.clone())
            .or_insert_with(VecDeque::new);
        entry.push_front(mid);
        while entry.len() > self.config.lookback_ticks {
            entry.pop_back();
        }
    }

    /// Attempt to build a buy intent when all entry conditions are met.
    fn check_entry(
        &self,
        token_id: &TokenId,
        mid_now: Decimal,
        ctx: &StrategyContext,
    ) -> Option<OrderIntent> {
        let pair = self.registry.get_by_token(token_id)?;

        // Cooldown guard.
        if self.is_on_cooldown(&pair.condition_id) {
            return None;
        }

        // Already have an open or pending position.
        if !ctx.position(token_id).is_flat() {
            return None;
        }
        if self.pending_exit.contains_key(token_id) {
            return None;
        }

        // Complement guard — avoid double-entry.
        let complement_id = pair.complement(token_id)?;
        if !ctx.position(complement_id).is_flat() {
            return None;
        }

        // Liquidity gate.
        let ask_depth = ctx.books.ask_depth(token_id).unwrap_or(Decimal::ZERO);
        if ask_depth < self.config.min_ask_depth {
            debug!(
                token = %&token_id[..token_id.len().min(12)],
                ask_depth = %ask_depth,
                "TBO: insufficient ask depth"
            );
            return None;
        }

        // Price cap — don't buy near-certainty tokens.
        let best_ask = ctx.best_ask(token_id)?;
        if best_ask >= dec!(0.92) {
            return None;
        }

        // Sizing.
        let shares = (self.config.max_bet_usd / best_ask)
            .round_dp_with_strategy(2, RoundingStrategy::ToZero);
        if shares <= Decimal::ZERO {
            return None;
        }
        let notional = shares * best_ask;
        if notional < self.config.min_bet_usd {
            return None;
        }

        // Compute the rolling high for the reason string.
        let high_n = self.price_history
            .get(token_id)
            .and_then(|h| h.iter().copied().reduce(Decimal::max))
            .unwrap_or(mid_now);

        Some(
            OrderIntent::new(
                pair.condition_id.clone(),
                token_id.clone(),
                Side::Buy,
                best_ask,
                shares,
                Urgency::Normal,
                format!(
                    "tbo_entry: mid={:.4} high_n={:.4} ask={:.4}",
                    mid_now, high_n, best_ask
                ),
                self.name.clone(),
            )
            .with_tick_size(pair.tick_size)
            .with_priority(55),
        )
    }

    /// Build exit intents for all tracked open positions.
    fn check_exits(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let mut intents = Vec::new();

        for entry in self.entry_tracking.iter() {
            let token_id = entry.key();
            let &(entry_price, entry_time) = entry.value();

            let pos = ctx.position(token_id);
            if pos.is_flat() {
                continue;
            }
            if self.pending_exit.contains_key(token_id) {
                continue;
            }

            let elapsed_secs = entry_time.elapsed().as_secs();
            let mid = match ctx.mid_price(token_id) {
                Some(m) => m,
                None => continue,
            };

            let unrealized_pct = if entry_price > Decimal::ZERO {
                (mid - entry_price) / entry_price
            } else {
                Decimal::ZERO
            };

            let exit_reason = if elapsed_secs >= self.config.max_hold_secs {
                Some("time")
            } else if unrealized_pct >= self.config.profit_target_pct {
                Some("profit")
            } else if unrealized_pct <= -self.config.stop_loss_pct {
                Some("stop_loss")
            } else {
                None
            };

            if let Some(reason) = exit_reason {
                let best_bid = match ctx.best_bid(token_id) {
                    Some(b) => b,
                    None => continue,
                };
                let sell_size = pos.shares
                    .round_dp_with_strategy(2, RoundingStrategy::ToZero);
                if sell_size <= Decimal::ZERO {
                    continue;
                }
                let pair = match self.registry.get_by_token(token_id) {
                    Some(p) => p,
                    None => continue,
                };

                info!(
                    token = %&token_id[..token_id.len().min(12)],
                    reason = %reason,
                    elapsed_secs,
                    unrealized_pct = %unrealized_pct,
                    "TBO: exit triggered"
                );

                let intent = OrderIntent::new(
                    pair.condition_id.clone(),
                    token_id.clone(),
                    Side::Sell,
                    best_bid,
                    sell_size,
                    Urgency::Normal,
                    format!("tbo_exit:{}", reason),
                    self.name.clone(),
                )
                .with_tick_size(pair.tick_size)
                .with_priority(55);

                intents.push(intent);
                self.pending_exit.insert(token_id.clone(), Instant::now());
            }
        }

        intents
    }
}

// ---------------------------------------------------------------------------
// Strategy trait
// ---------------------------------------------------------------------------

impl Strategy for TboTrendStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        55
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn on_book_update(
        &self,
        _market_id: &ConditionId,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        if !self.is_enabled() {
            return vec![];
        }

        let mid = match ctx.mid_price(token_id) {
            Some(m) => m,
            None => return vec![],
        };

        // 1. Update rolling history for this token.
        self.update_price_history(token_id, mid);

        // Also update the complement's history using its current mid — this ensures
        // the complement builds history even when its book updates lag behind.
        // When YES drops, NO rises: we want NO's history to reflect that immediately.
        if let Some(pair) = self.registry.get_by_token(token_id) {
            if let Some(complement_id) = pair.complement(token_id) {
                if let Some(complement_mid) = ctx.mid_price(complement_id) {
                    self.update_price_history(complement_id, complement_mid);
                }
            }
        }

        // 2. Warm-up: need a full window before signalling.
        let history_len = self.price_history
            .get(token_id)
            .map(|h| h.len())
            .unwrap_or(0);
        if history_len < self.config.lookback_ticks {
            return vec![];
        }

        // 3. Compute rolling high (excluding the current tick to measure breakout above history).
        let high_n = {
            let h = match self.price_history.get(token_id) {
                Some(h) => h,
                None => return vec![],
            };
            // Skip the front (current mid) when computing the historical high.
            h.iter().skip(1).copied().reduce(Decimal::max).unwrap_or(mid)
        };

        // 4. Breakout check.
        let threshold = high_n
            * (Decimal::ONE + Decimal::from(self.config.breakout_threshold_bps) / dec!(10_000));

        if mid > threshold {
            // Increment confirmation counter.
            let count = {
                let mut c = self.breakout_confirm
                    .entry(token_id.clone())
                    .or_insert(0);
                *c += 1;
                *c
            };

            // 5. Require min_confirm_ticks consecutive ticks above breakout.
            if count < self.config.min_confirm_ticks {
                debug!(
                    token = %&token_id[..token_id.len().min(12)],
                    count,
                    needed = self.config.min_confirm_ticks,
                    "TBO: breakout confirm building"
                );
                return vec![];
            }

            // 6. All gates passed — try to emit an entry.
            match self.check_entry(token_id, mid, ctx) {
                Some(intent) => {
                    let market_id = intent.market_id.clone();
                    self.last_trade.insert(market_id, Instant::now());
                    // Reset confirmation so we don't re-enter the same breakout.
                    self.breakout_confirm.insert(token_id.clone(), 0);
                    self.trade_count.fetch_add(1, Ordering::Relaxed);
                    info!(
                        token = %&token_id[..token_id.len().min(12)],
                        mid = %mid,
                        high_n = %high_n,
                        price = %intent.price,
                        size = %intent.size,
                        "TBO: breakout entry intent generated"
                    );
                    vec![intent]
                }
                None => vec![],
            }
        } else {
            // Price fell back below breakout level — reset counter.
            self.breakout_confirm.insert(token_id.clone(), 0);
            vec![]
        }
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        match fill.side {
            Side::Buy => {
                if !self.entry_tracking.contains_key(&fill.token_id) {
                    self.entry_tracking
                        .insert(fill.token_id.clone(), (fill.price, Instant::now()));
                    debug!(
                        token = %&fill.token_id[..fill.token_id.len().min(12)],
                        price = %fill.price,
                        "TBO: entry recorded"
                    );
                }
            }
            Side::Sell => {
                self.entry_tracking.remove(&fill.token_id);
                self.pending_exit.remove(&fill.token_id);
                debug!(
                    token = %&fill.token_id[..fill.token_id.len().min(12)],
                    "TBO: exit confirmed"
                );
            }
        }
        vec![]
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        if !self.is_enabled() {
            return vec![];
        }

        // Clean up stale pending_exit entries (> 60 s without a fill → assume failed).
        self.pending_exit.retain(|_, t| t.elapsed().as_secs() < 60);

        // Clean up entry_tracking for positions closed externally.
        self.entry_tracking
            .retain(|token_id, _| !ctx.position(token_id).is_flat());

        self.check_exits(ctx)
    }

    fn on_shutdown(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        let count = self.trade_count();
        if count > 0 {
            warn!(
                trades = count,
                open_positions = self.entry_tracking.len(),
                "TboTrendStrategy shutting down"
            );
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::state::OrderBookState;
    use crate::strategy::market_pair::{MarketPair, MarketPairRegistry};
    use crate::strategy::traits::StrategyContext;
    use crate::websocket::types::PriceLevel;
    use rust_decimal_macros::dec;

    fn make_registry() -> Arc<MarketPairRegistry> {
        let r = Arc::new(MarketPairRegistry::new());
        r.register(
            MarketPair::new("0xmkt".to_string(), "yes_tok".to_string(), "no_tok".to_string())
                .with_fee_rate(1000),
        );
        r
    }

    fn make_books(yes_ask: &str, no_ask: &str, depth: &str) -> OrderBookState {
        let b = OrderBookState::new();
        let yes_bid = format!("{:.2}", yes_ask.parse::<f64>().unwrap() - 0.01);
        let no_bid  = format!("{:.2}", no_ask.parse::<f64>().unwrap() - 0.01);
        b.update_book(
            "yes_tok".to_string(), "0xmkt".to_string(),
            vec![PriceLevel { price: yes_bid, size: depth.to_string() }],
            vec![PriceLevel { price: yes_ask.to_string(), size: depth.to_string() }],
            None, None,
        );
        b.update_book(
            "no_tok".to_string(), "0xmkt".to_string(),
            vec![PriceLevel { price: no_bid, size: depth.to_string() }],
            vec![PriceLevel { price: no_ask.to_string(), size: depth.to_string() }],
            None, None,
        );
        b
    }

    fn make_strategy(lookback: usize, threshold_bps: i64, confirm: u32) -> TboTrendStrategy {
        TboTrendStrategy::with_config(
            make_registry(),
            TboConfig {
                lookback_ticks: lookback,
                breakout_threshold_bps: threshold_bps,
                min_confirm_ticks: confirm,
                min_ask_depth: dec!(1),
                max_bet_usd: dec!(10),
                min_bet_usd: dec!(1),
                max_hold_secs: 180,
                profit_target_pct: dec!(0.06),
                stop_loss_pct: dec!(0.04),
                cooldown_ms: 1_000,
            },
        )
    }

    fn ctx<'a>(books: &'a OrderBookState, ledger: &'a Ledger) -> StrategyContext<'a> {
        StrategyContext::new(books, ledger)
    }

    #[test]
    fn test_no_signal_during_warmup() {
        let s = make_strategy(5, 100, 1);
        let books = make_books("0.60", "0.41", "100");
        let ledger = Ledger::new(dec!(1000));
        let c = ctx(&books, &ledger);
        // Only 3 updates — below lookback=5
        for _ in 0..3 {
            let r = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c);
            assert!(r.is_empty(), "should not signal during warm-up");
        }
    }

    #[test]
    fn test_breakout_fires_after_confirmation() {
        // lookback=3, threshold=500bps (5%), confirm=2
        // Use a large threshold so the price must be clearly above the rolling high,
        // and use a high enough breakout price that even after it's added to history
        // (becoming the new high) the next tick also still qualifies.
        let s = make_strategy(3, 500, 2);
        let ledger = Ledger::new(dec!(1000));

        // Feed 3 warm-up ticks at 0.50
        let books_base = make_books("0.50", "0.51", "100");
        let c = ctx(&books_base, &ledger);
        for _ in 0..3 {
            s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c);
        }
        // history = [0.50, 0.50, 0.50]; high_n (skip 1) = 0.50
        // threshold = 0.50 * 1.05 = 0.525

        // Breakout at 0.70: well above threshold, and after 0.70 is added to history,
        // [0.70, 0.50, 0.50] → high_n (skip first) = 0.50 → threshold still 0.525.
        let books_break = make_books("0.70", "0.31", "100");
        let c2 = ctx(&books_break, &ledger);

        let r1 = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c2);
        assert!(r1.is_empty(), "need 2 confirm ticks, got 1");

        // history = [0.70, 0.70, 0.50]; high_n (skip first) = 0.70; threshold = 0.735
        // 0.70 < 0.735 → counter resets! Use an even higher price for the second tick.
        let books_break2 = make_books("0.80", "0.21", "100");
        let c3 = ctx(&books_break2, &ledger);
        // history = [0.80, 0.70, 0.70]; high_n (skip first) = 0.70; threshold = 0.735
        // 0.80 > 0.735 → counter = 2 → fires!
        let r2 = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c3);
        assert_eq!(r2.len(), 1, "should fire after min_confirm_ticks=2");
        assert_eq!(r2[0].token_id, "yes_tok");
        assert_eq!(r2[0].side, crate::websocket::types::Side::Buy);
    }

    #[test]
    fn test_confirm_counter_resets_on_price_drop() {
        let s = make_strategy(3, 50, 3);
        let ledger = Ledger::new(dec!(1000));

        let books_base = make_books("0.50", "0.51", "100");
        let c = ctx(&books_base, &ledger);
        for _ in 0..3 {
            s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c);
        }

        let books_break = make_books("0.55", "0.46", "100");
        let c2 = ctx(&books_break, &ledger);
        // 2 ticks above breakout
        s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c2);
        s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c2);

        // Price drops back — counter should reset
        let c3 = ctx(&books_base, &ledger);
        s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c3);

        // Back to breakout — counter restarts at 1, needs 3 total
        let r = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c2);
        assert!(r.is_empty(), "counter should have reset, not enough confirms");
    }

    #[test]
    fn test_disabled_strategy_no_signal() {
        let s = make_strategy(3, 50, 1);
        s.set_enabled(false);
        let books = make_books("0.60", "0.41", "100");
        let ledger = Ledger::new(dec!(1000));
        let c = ctx(&books, &ledger);
        for _ in 0..5 {
            assert!(s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &c).is_empty());
        }
    }
}
