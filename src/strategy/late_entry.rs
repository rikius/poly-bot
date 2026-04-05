//! Late Entry (High-Confidence Near-Resolution) strategy for Polymarket binary markets.
//!
//! ## Hypothesis
//!
//! When a binary market is approaching resolution and one side is already
//! priced at near-certainty (e.g. YES at 97¢, NO at 3¢), the implied
//! probability is very high.  Buying the near-certain side yields a small
//! but extremely high-probability return in a short time window.
//!
//! Example: "Will BTC close above $X in the next 15 min?" — with 2 minutes
//! remaining and YES trading at 0.96, buying YES yields ≈4% in ≤2 minutes
//! if the outcome holds.
//!
//! ## Signal logic
//!
//! Entry fires when **all** of the following are true:
//!
//! 1. **Time gate** — `secs_to_resolution ≤ entry_window_secs` (default: last
//!    300 s = 5 min of the round).
//! 2. **Price gate** — `best_ask ≥ min_entry_price` (default: 0.90, meaning
//!    the token costs ≥ 90¢ — i.e. this side is ≥ 90% likely to win).
//! 3. **Spread gate** — the token has a two-sided book and spread is tight
//!    enough that the trade is executable.
//! 4. **Liquidity gate** — ask-side depth ≥ `min_ask_depth`.
//! 5. **Position guard** — flat on this token and its complement.
//! 6. **Cooldown** — per condition_id, to avoid multiple entries in the same round.
//!
//! ## Time-to-resolution
//!
//! For periodic markets (15-min, 5-min crypto Up/Down rounds), the resolution
//! time is the next multiple of `market_interval_secs` after UTC midnight.
//! `secs_to_resolution = interval_secs - (utc_now_secs % interval_secs)`
//!
//! ## Exit
//!
//! The position is held to resolution — the contract pays $1 if correct.
//! The strategy does NOT generate early exits.  The sync task handles
//! on-chain redemption after market resolves.
//!
//! ## Enable
//!
//! Set `LATE_ENTRY_ENABLED=true` in `.env`.
//! Conservative start: `LATE_ENTRY_MAX_ENTRY_PRICE=0.03`, `LATE_ENTRY_MAX_BET_USD=50`,
//! `LATE_ENTRY_ENTRY_WINDOW_SECS=180`.

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

/// Configuration for [`LateEntryStrategy`].
#[derive(Debug, Clone)]
pub struct LateEntryConfig {
    /// Enter when the fraction of the round remaining is ≤ this value.
    ///
    /// Computed as: `entry_window_secs = market_interval_secs × entry_window_pct`
    /// - 15-min market (900 s) at 0.20 → activates in the last 180 s (3 min)
    /// - 5-min market  (300 s) at 0.20 → activates in the last  60 s (1 min)
    ///
    /// Default: 0.20 (last 20% of the round).
    pub entry_window_pct: f64,

    /// Minimum ask price of the winning token to enter.
    ///
    /// We buy the token that is near certain to win, i.e. ask ≥ this value.
    /// A token at 0.90 costs 90¢ and pays $1 → 11% gross return.
    /// With 10% fees the net is ~1¢ profit — use 0.92+ for positive expectancy.
    ///
    /// Default: 0.90.
    pub min_entry_price: Decimal,

    /// Market interval in seconds — used to compute time-to-resolution.
    ///
    /// For 15-min Up/Down markets this is 900; for 5-min markets 300.
    /// Should match `Config::market_interval_secs`.
    /// Default: 900 (15 min).
    pub market_interval_secs: u64,

    /// Minimum ask-side depth (shares) at time of entry.
    ///
    /// Default: 5 shares.
    pub min_ask_depth: Decimal,

    /// Maximum notional per trade (USDC).
    ///
    /// Default: 100.
    pub max_bet_usd: Decimal,

    /// Per-market cooldown after a trade fires (ms).
    ///
    /// Prevents re-entering the same market during the same resolution window.
    /// Default: 120 000 ms (2 min).
    pub cooldown_ms: u64,

    /// Stop-loss price: if the held token's best bid falls to or below this
    /// value, sell immediately to cut losses.
    ///
    /// e.g. 0.70 means sell if the token drops to 70¢ (loss of ≥20¢ from a 90¢ entry).
    /// Set to 0 to disable the stop-loss (hold to resolution unconditionally).
    /// Default: 0.70.
    pub stop_loss_price: Decimal,
}

impl Default for LateEntryConfig {
    fn default() -> Self {
        Self {
            entry_window_pct: 0.20,
            min_entry_price: dec!(0.90),
            market_interval_secs: 900,
            min_ask_depth: dec!(5),
            max_bet_usd: dec!(100),
            cooldown_ms: 120_000,
            stop_loss_price: dec!(0.70),
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy struct
// ---------------------------------------------------------------------------

/// Late Entry (High-Confidence Near-Resolution) strategy.
///
/// See the module-level documentation for the signal logic and configuration.
pub struct LateEntryStrategy {
    name: String,
    config: LateEntryConfig,
    registry: Arc<MarketPairRegistry>,
    enabled: AtomicBool,

    /// Tracks entered positions: `token_id → entry_price`.
    /// Used to avoid double-entry and for shutdown reporting.
    entry_tracking: DashMap<TokenId, Decimal>,

    /// Per-market cooldown tracker.
    last_trade: DashMap<ConditionId, Instant>,

    trade_count: AtomicU64,
}

impl LateEntryStrategy {
    /// Create with default config.
    pub fn new(registry: Arc<MarketPairRegistry>) -> Self {
        Self::with_config(registry, LateEntryConfig::default())
    }

    /// Create with a specific config.
    pub fn with_config(registry: Arc<MarketPairRegistry>, config: LateEntryConfig) -> Self {
        Self {
            name: "late_entry".to_string(),
            config,
            registry,
            enabled: AtomicBool::new(true),
            entry_tracking: DashMap::new(),
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


    /// Compute seconds remaining until the next market resolution boundary.
    ///
    /// For periodic markets the resolution is the next multiple of
    /// `market_interval_secs` past UTC midnight.
    fn secs_to_resolution(&self, ctx: &StrategyContext) -> u64 {
        let interval = self.config.market_interval_secs;
        if interval == 0 {
            return u64::MAX;
        }
        let utc_secs = ctx.utc_now.timestamp() as u64;
        let secs_into_interval = utc_secs % interval;
        interval - secs_into_interval
    }

    /// Compute the entry window in seconds from the configured percentage.
    ///
    /// e.g. 15-min market × 0.20 = 180 s; 5-min market × 0.20 = 60 s.
    fn entry_window_secs(&self) -> u64 {
        let window = (self.config.market_interval_secs as f64 * self.config.entry_window_pct)
            .round() as u64;
        window.max(1)
    }

    /// Attempt to build a buy intent for `token_id` when all entry conditions are met.
    fn check_entry(
        &self,
        token_id: &TokenId,
        secs_remaining: u64,
        ctx: &StrategyContext,
    ) -> Option<OrderIntent> {
        let pair = self.registry.get_by_token(token_id)?;

        // Cooldown guard — use entry() to atomically claim the slot.
        // This prevents two concurrent on_book_update calls (YES + NO) from
        // both slipping through the cooldown check at the same instant.
        {
            use dashmap::mapref::entry::Entry;
            let cooldown_ms = self.config.cooldown_ms as u128;
            match self.last_trade.entry(pair.condition_id.clone()) {
                Entry::Occupied(e) if e.get().elapsed().as_millis() < cooldown_ms => {
                    return None;
                }
                Entry::Occupied(mut e) => {
                    // Expired — claim it now to block concurrent callers.
                    *e.get_mut() = Instant::now();
                }
                Entry::Vacant(e) => {
                    e.insert(Instant::now());
                }
            }
        }

        // Position guards.
        if !ctx.position(token_id).is_flat() {
            return None;
        }
        let complement_id = pair.complement(token_id)?;
        if !ctx.position(complement_id).is_flat() {
            return None;
        }

        // Already have a confirmed position in this token.
        if self.entry_tracking.contains_key(token_id) {
            return None;
        }

        // Price gate: token must be the near-certain winner (ask ≥ min_entry_price).
        // e.g. min_entry_price=0.90 means the token costs ≥90¢ → pays $1 → ≥11% gross.
        let best_ask = ctx.best_ask(token_id)?;
        if best_ask < self.config.min_entry_price {
            return None;
        }
        // Also cap at 0.99 — at exactly $1 there is no upside.
        if best_ask >= dec!(0.99) {
            return None;
        }

        // Spread gate: must have a two-sided book.
        if ctx.best_bid(token_id).is_none() {
            return None;
        }

        // Liquidity gate.
        let ask_depth = ctx.books.ask_depth(token_id).unwrap_or(Decimal::ZERO);
        if ask_depth < self.config.min_ask_depth {
            debug!(
                token = %&token_id[..token_id.len().min(12)],
                ask_depth = %ask_depth,
                "LateEntry: insufficient ask depth"
            );
            return None;
        }

        // Sizing: round shares DOWN (ToZero) to avoid spending over max_bet_usd.
        // Then ensure notional meets Polymarket's $1.00 minimum by rounding UP
        // if the floored shares produce notional < $1.00.
        let shares_floor = (self.config.max_bet_usd / best_ask)
            .round_dp_with_strategy(2, RoundingStrategy::ToZero);
        let shares = if shares_floor * best_ask < dec!(1.00) {
            // Notional is below Polymarket's $1 minimum — round shares UP (ceiling)
            // to meet it. This may exceed max_bet_usd by at most 1 cent.
            (self.config.max_bet_usd / best_ask)
                .round_dp_with_strategy(2, RoundingStrategy::AwayFromZero)
                .max(dec!(0.01))
        } else {
            shares_floor
        };
        if shares <= Decimal::ZERO {
            return None;
        }

        Some(
            OrderIntent::new(
                pair.condition_id.clone(),
                token_id.clone(),
                Side::Buy,
                best_ask,
                shares,
                Urgency::Normal,
                format!(
                    "late_entry: ask={:.4} win_prob={:.1}% secs_left={}",
                    best_ask,
                    best_ask * dec!(100),
                    secs_remaining,
                ),
                self.name.clone(),
            )
            .with_tick_size(pair.tick_size)
            .with_priority(70), // Higher than other strategies — high-conviction signal
        )
    }
}

// ---------------------------------------------------------------------------
// Strategy trait
// ---------------------------------------------------------------------------

impl Strategy for LateEntryStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        70
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

        // 1. Time gate — only active near resolution.
        let secs_remaining = self.secs_to_resolution(ctx);
        if secs_remaining > self.entry_window_secs() {
            return vec![];
        }
        // Stop entering in the final 5 seconds — market makers pull their asks
        // as the outcome becomes certain, leaving no counterparty for FAK orders.
        if secs_remaining < 5 {
            return vec![];
        }

        // 2. Check both tokens of the pair — lowest ask (near-certainty side) first.
        // This covers both uptrend (YES cheap) and downtrend (NO cheap) without
        // duplicating logic: whichever token is near 0¢ is the entry candidate.
        let pair = match self.registry.get_by_token(token_id) {
            Some(p) => p,
            None => return vec![],
        };

        let yes_ask = ctx.best_ask(&pair.yes_token_id);
        let no_ask = ctx.best_ask(&pair.no_token_id);
        debug!(
            secs_remaining,
            yes_ask = %yes_ask.map(|a| a.to_string()).unwrap_or_else(|| "-".into()),
            no_ask = %no_ask.map(|a| a.to_string()).unwrap_or_else(|| "-".into()),
            min_entry_price = %self.config.min_entry_price,
            "LateEntry: in resolution window"
        );

        // Check both tokens — whichever one is the near-certain winner (highest ask).
        // YES at 92¢ → buy YES.  NO at 92¢ (downtrend) → buy NO.
        // tokens_by_ask_asc returns [lowest, highest]; we want the highest first.
        let mut intents = Vec::new();
        let ordered = ctx.tokens_by_ask_asc(&pair);
        // Reverse: highest ask (winner) first.
        for &tid in ordered.iter().rev() {
            let best_ask = match ctx.best_ask(tid) {
                Some(a) if a >= self.config.min_entry_price && a < dec!(0.99) => a,
                _ => continue,
            };
            if let Some(intent) = self.check_entry(tid, secs_remaining, ctx) {
                // Don't insert into entry_tracking here — wait for the confirmed
                // fill in on_fill(). Inserting here causes on_tick to immediately
                // discard the entry (position is still flat until the fill arrives).
                self.trade_count.fetch_add(1, Ordering::Relaxed);
                info!(
                    token = %&tid[..tid.len().min(12)],
                    ask = %best_ask,
                    shares = %intent.size,
                    secs_remaining,
                    "LateEntry: high-confidence entry intent generated"
                );
                intents.push(intent);
            }
        }

        intents
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        if fill.side == Side::Buy {
            // Record entry price now that the fill is confirmed — this is when
            // the position actually exists and on_tick can reliably check it.
            self.entry_tracking
                .entry(fill.token_id.clone())
                .or_insert(fill.price);
            debug!(
                token = %&fill.token_id[..fill.token_id.len().min(12)],
                price = %fill.price,
                "LateEntry: entry confirmed — tracking for stop-loss"
            );
        } else if fill.side == Side::Sell {
            // Stop-loss sell filled — clear tracking.
            self.entry_tracking.remove(&fill.token_id);
            debug!(
                token = %&fill.token_id[..fill.token_id.len().min(12)],
                "LateEntry: stop-loss sell confirmed — position closed"
            );
        }
        vec![]
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        if !self.is_enabled() {
            return vec![];
        }

        // Clean up entry_tracking for positions that have been closed
        // (e.g. by the sync/claim task after resolution).
        self.entry_tracking
            .retain(|token_id, _| !ctx.position(token_id).is_flat());

        // Stop-loss: if the held token's best bid has dropped to or below
        // stop_loss_price, sell immediately to cap the loss.
        if self.config.stop_loss_price.is_zero() {
            return vec![];
        }

        let mut exits = Vec::new();
        for entry in self.entry_tracking.iter() {
            let token_id = entry.key();
            let entry_price = *entry.value();

            // Use best bid as the exit signal — it's what we'd actually get filled at.
            let best_bid = match ctx.best_bid(token_id) {
                Some(b) => b,
                None => continue,
            };

            if best_bid > self.config.stop_loss_price {
                continue;
            }

            let position = ctx.position(token_id);
            if position.is_flat() {
                continue;
            }

            let shares = position.shares.abs();
            let pair = match self.registry.get_by_token(token_id) {
                Some(p) => p,
                None => continue,
            };

            warn!(
                token = %&token_id[..token_id.len().min(12)],
                entry_price = %entry_price,
                best_bid = %best_bid,
                stop_loss = %self.config.stop_loss_price,
                shares = %shares,
                "LateEntry: stop-loss triggered — selling position"
            );

            exits.push(
                OrderIntent::new(
                    pair.condition_id.clone(),
                    token_id.clone(),
                    Side::Sell,
                    best_bid,
                    shares,
                    Urgency::Immediate,
                    format!(
                        "late_entry stop-loss: bid={:.4} entry={:.4} stop={:.4}",
                        best_bid, entry_price, self.config.stop_loss_price,
                    ),
                    self.name.clone(),
                )
                .with_tick_size(pair.tick_size)
                .with_priority(80), // Higher priority than entry to ensure execution
            );
        }

        exits
    }

    fn on_shutdown(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        let count = self.trade_count();
        if count > 0 {
            warn!(
                trades = count,
                open_positions = self.entry_tracking.len(),
                "LateEntryStrategy shutting down — open positions will be claimed on next startup"
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
            MarketPair::new(
                "0xmkt".to_string(),
                "yes_tok".to_string(),
                "no_tok".to_string(),
            )
            .with_fee_rate(1000),
        );
        r
    }

    /// Build a book where YES=`yes_ask` and NO=`no_ask`.
    fn make_books(yes_ask: &str, no_ask: &str) -> OrderBookState {
        let b = OrderBookState::new();
        let bid_yes = format!("{:.2}", yes_ask.parse::<f64>().unwrap() - 0.01);
        let bid_no  = format!("{:.2}", no_ask.parse::<f64>().unwrap() - 0.01);
        b.update_book(
            "yes_tok".to_string(), "0xmkt".to_string(),
            vec![PriceLevel { price: bid_yes, size: "100".to_string() }],
            vec![PriceLevel { price: yes_ask.to_string(), size: "100".to_string() }],
            None, None,
        );
        b.update_book(
            "no_tok".to_string(), "0xmkt".to_string(),
            vec![PriceLevel { price: bid_no, size: "100".to_string() }],
            vec![PriceLevel { price: no_ask.to_string(), size: "100".to_string() }],
            None, None,
        );
        b
    }

    fn make_strategy(min_entry_price: &str, window_pct: f64) -> LateEntryStrategy {
        LateEntryStrategy::with_config(
            make_registry(),
            LateEntryConfig {
                min_entry_price: min_entry_price.parse().unwrap(),
                entry_window_pct: window_pct,
                market_interval_secs: 300, // 5-min market
                min_ask_depth: dec!(1),
                max_bet_usd: dec!(10),
                cooldown_ms: 60_000,
                stop_loss_price: dec!(0.70),
            },
        )
    }

    /// Build a StrategyContext with a fixed UTC time that is `secs_before_end`
    /// seconds before the next 300-second boundary.
    fn make_ctx_with_time<'a>(
        books: &'a OrderBookState,
        ledger: &'a Ledger,
        secs_before_end: u64,
    ) -> StrategyContext<'a> {
        // Find a UTC timestamp that puts us `secs_before_end` before the next
        // 5-min boundary.  current_secs % 300 = 300 - secs_before_end.
        let interval = 300u64;
        let target_secs_into = interval - secs_before_end;
        // Pick an arbitrary base that satisfies the constraint.
        let base = 1_700_000_000u64; // some fixed past timestamp
        let secs_into = base % interval;
        let adjustment = if secs_into <= target_secs_into {
            target_secs_into - secs_into
        } else {
            interval - secs_into + target_secs_into
        };
        let ts = base + adjustment;
        let utc = chrono::DateTime::from_timestamp(ts as i64, 0).unwrap();
        StrategyContext {
            books,
            ledger,
            now: std::time::Instant::now(),
            utc_now: utc,
        }
    }

    #[test]
    fn test_entry_window_scaling() {
        // 300s × 0.20 = 60s; 300s × 0.40 = 120s
        assert_eq!((300.0f64 * 0.20).round() as u64, 60);
        assert_eq!((300.0f64 * 0.40).round() as u64, 120);
    }

    #[test]
    fn test_no_intent_outside_window() {
        let s = make_strategy("0.90", 0.20);
        let books = make_books("0.08", "0.93"); // NO qualifies price-wise
        let ledger = Ledger::new(dec!(1000));
        // 120s before end — outside 60s window
        let ctx = make_ctx_with_time(&books, &ledger, 120);
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert!(intents.is_empty(), "should not fire outside window");
    }

    #[test]
    fn test_buys_winning_no_token() {
        let s = make_strategy("0.90", 0.40);
        // NO at 0.92 is near-certain winner; YES at 0.08
        let books = make_books("0.08", "0.92");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50); // inside window
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].token_id, "no_tok");
        assert_eq!(intents[0].side, crate::websocket::types::Side::Buy);
        assert_eq!(intents[0].price, dec!(0.92));
    }

    #[test]
    fn test_buys_winning_yes_token() {
        let s = make_strategy("0.90", 0.40);
        // YES at 0.91 is near-certain winner
        let books = make_books("0.91", "0.11");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50);
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].token_id, "yes_tok");
    }

    #[test]
    fn test_below_min_entry_price_no_intent() {
        let s = make_strategy("0.90", 0.40);
        // Both tokens below threshold (market still contested)
        let books = make_books("0.55", "0.46");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50);
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_cooldown_prevents_duplicate_orders() {
        let s = make_strategy("0.90", 0.40);
        let books = make_books("0.08", "0.92");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50);

        // First call — should fire
        let r1 = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert_eq!(r1.len(), 1, "first call should produce one intent");

        // Second call immediately after — cooldown blocks it
        let r2 = s.on_book_update(&"0xmkt".to_string(), &"no_tok".to_string(), &ctx);
        assert!(r2.is_empty(), "cooldown must block duplicate order");
    }

    #[test]
    fn test_disabled_strategy_no_intent() {
        let s = make_strategy("0.90", 0.40);
        s.set_enabled(false);
        let books = make_books("0.08", "0.92");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50);
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_no_intent_when_ask_at_ceiling() {
        // ask ≥ 0.99 should be rejected (no upside)
        let s = make_strategy("0.90", 0.40);
        let books = make_books("0.005", "0.995");
        let ledger = Ledger::new(dec!(1000));
        let ctx = make_ctx_with_time(&books, &ledger, 50);
        let intents = s.on_book_update(&"0xmkt".to_string(), &"yes_tok".to_string(), &ctx);
        assert!(intents.is_empty(), "ask ≥ 0.99 has no upside, should not fire");
    }
}
