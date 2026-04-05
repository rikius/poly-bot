//! Contrarian (mean-reversion) strategy for Polymarket binary markets.
//!
//! ## Hypothesis
//!
//! Short-horizon price dislocations on prediction markets are often temporary.
//! When a token's mid-price drops sharply — driven by a wave of market-sell
//! orders — the fair probability implied by the *complement* token frequently
//! diverges from the dislodged price.  The contrarian strategy buys the
//! oversold side and exits on a time limit, profit target, or stop-loss.
//!
//! ## Entry signal (three gates, all must pass)
//!
//! 1. **Velocity** — mid-price fell ≥ `min_move_bps` within the last
//!    `lookback_secs` seconds.
//! 2. **Edge** — complement-implied fair value exceeds current ask by at least
//!    `min_edge` (or `high_fee_min_edge` for 10 %-fee markets).
//! 3. **Liquidity** — bid-side depth on the entry token ≥ `min_bid_depth`.
//!
//! ## Exit (checked every tick, ~100 ms)
//!
//! - **Time** — position held ≥ `max_hold_secs`
//! - **Profit** — unrealized gain ≥ `profit_target_pct`
//! - **Stop-loss** — unrealized loss ≥ `stop_loss_pct`
//!
//! ## Enable
//!
//! Set `CONTRARIAN_ENABLED=true` in `.env`.  Conservative starting parameters:
//! `CONTRARIAN_MIN_MOVE_BPS=800`, `CONTRARIAN_MAX_BET_USD=5`.

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

/// Configuration for [`ContrarianStrategy`].
#[derive(Debug, Clone)]
pub struct ContrarianConfig {
    /// Minimum mid-price drop (in bps) within `lookback_secs` to trigger entry.
    ///
    /// Default: 500 bps (5 %).
    pub min_move_bps: i64,

    /// Rolling time window for velocity measurement (seconds).
    ///
    /// Default: 60 s.
    pub lookback_secs: u64,

    /// Minimum bid-side depth on the entry token (shares).
    ///
    /// Prevents chasing a book that has been flushed to zero.
    /// Default: 10 shares.
    pub min_bid_depth: Decimal,

    /// Minimum edge between complement-implied fair value and current ask
    /// for standard markets (0 % or low fee).
    ///
    /// Default: 0.05 ($0.05).
    pub min_edge: Decimal,

    /// Minimum edge for high-fee markets (fee_rate_bps ≥ 100, e.g. 10 % fee
    /// on 15-min crypto markets).
    ///
    /// Default: 0.12 ($0.12).
    pub high_fee_min_edge: Decimal,

    /// Maximum notional per trade (USDC).
    ///
    /// Default: 100.
    pub max_bet_usd: Decimal,

    /// Skip the trade if `shares × ask < min_bet_usd`.
    ///
    /// Default: 5.
    pub min_bet_usd: Decimal,

    /// Time-based exit: close the position after this many seconds.
    ///
    /// Default: 120 s.
    pub max_hold_secs: u64,

    /// Profit-target exit: close when unrealized gain / entry_price ≥ this.
    ///
    /// Default: 0.08 (8 %).
    pub profit_target_pct: Decimal,

    /// Stop-loss exit: close when unrealized loss / entry_price ≥ this.
    ///
    /// Default: 0.05 (5 %).
    pub stop_loss_pct: Decimal,

    /// Per-market cooldown after a trade fires (ms).
    ///
    /// Default: 10 000 ms (10 s).
    pub cooldown_ms: u64,

    /// Maximum number of mid-price entries kept per token (memory cap).
    ///
    /// Default: 500.
    pub max_history_len: usize,
}

impl Default for ContrarianConfig {
    fn default() -> Self {
        Self {
            min_move_bps: 500,
            lookback_secs: 60,
            min_bid_depth: dec!(10),
            min_edge: dec!(0.05),
            high_fee_min_edge: dec!(0.12),
            max_bet_usd: dec!(100),
            min_bet_usd: dec!(5),
            max_hold_secs: 120,
            profit_target_pct: dec!(0.08),
            stop_loss_pct: dec!(0.05),
            cooldown_ms: 10_000,
            max_history_len: 500,
        }
    }
}

// ---------------------------------------------------------------------------
// Strategy struct
// ---------------------------------------------------------------------------

/// Contrarian (mean-reversion) strategy.
///
/// See the module-level documentation for the signal logic and configuration.
pub struct ContrarianStrategy {
    name: String,
    config: ContrarianConfig,
    registry: Arc<MarketPairRegistry>,
    enabled: AtomicBool,

    /// Rolling mid-price history per token: `token_id → [(time, mid)]`.
    /// Newest entries are at the *front* of the deque.
    price_history: DashMap<TokenId, VecDeque<(Instant, Decimal)>>,

    /// Open position tracking: `token_id → (entry_price, entry_time)`.
    /// Populated on fill confirmation (`on_fill`), cleared when the sell fills.
    entry_tracking: DashMap<TokenId, (Decimal, Instant)>,

    /// Tokens for which a close order has been submitted but not yet filled.
    /// Prevents generating duplicate exit intents on consecutive ticks.
    pending_exit: DashMap<TokenId, Instant>,

    /// Per-market cooldown tracker.
    last_trade: DashMap<ConditionId, Instant>,

    trade_count: AtomicU64,
}

impl ContrarianStrategy {
    /// Create with default config.
    pub fn new(registry: Arc<MarketPairRegistry>) -> Self {
        Self::with_config(registry, ContrarianConfig::default())
    }

    /// Create with a specific config.
    pub fn with_config(registry: Arc<MarketPairRegistry>, config: ContrarianConfig) -> Self {
        Self {
            name: "contrarian".to_string(),
            config,
            registry,
            enabled: AtomicBool::new(true),
            price_history: DashMap::new(),
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

    fn effective_min_edge(&self, pair: &crate::strategy::market_pair::MarketPair) -> Decimal {
        if pair.is_crypto_15min() {
            self.config.high_fee_min_edge
        } else {
            self.config.min_edge
        }
    }

    /// Push a new mid-price observation and trim stale entries.
    fn update_price_history(&self, token_id: &TokenId, mid: Decimal, now: Instant) {
        let mut entry = self.price_history
            .entry(token_id.clone())
            .or_insert_with(VecDeque::new);

        // Push newest to front.
        entry.push_front((now, mid));

        // Trim entries older than lookback_secs from the back.
        let cutoff_millis = self.config.lookback_secs * 1_000;
        while let Some(&(t, _)) = entry.back() {
            if t.elapsed().as_millis() > cutoff_millis as u128 {
                entry.pop_back();
            } else {
                break;
            }
        }

        // Hard cap to prevent unbounded growth.
        while entry.len() > self.config.max_history_len {
            entry.pop_back();
        }
    }

    /// Compute mid-price velocity in bps over the history window.
    ///
    /// Returns `Some(bps)` where negative = drop, positive = rise.
    /// Returns `None` when there are fewer than 2 data points.
    fn compute_velocity(&self, token_id: &TokenId) -> Option<Decimal> {
        let history = self.price_history.get(token_id)?;
        if history.len() < 2 {
            return None;
        }
        let (_, mid_now) = history.front().copied()?;
        let (_, mid_old) = history.back().copied()?;
        if mid_old == Decimal::ZERO {
            return None;
        }
        let bps = (mid_now - mid_old) / mid_old * Decimal::from(10_000i64);
        Some(bps)
    }

    /// Attempt to build a buy intent for `token_id` when entry conditions are met.
    fn check_entry(
        &self,
        token_id: &TokenId,
        ctx: &StrategyContext,
    ) -> Option<OrderIntent> {
        let pair = self.registry.get_by_token(token_id)?;

        // Cooldown guard.
        if self.is_on_cooldown(&pair.condition_id) {
            return None;
        }

        // Already have an open or pending position on this token.
        if !ctx.position(token_id).is_flat() {
            return None;
        }
        if self.pending_exit.contains_key(token_id) {
            return None;
        }

        // Gate 1: velocity (must be a drop ≥ min_move_bps).
        let velocity_bps = self.compute_velocity(token_id)?;
        let min_move = Decimal::from(self.config.min_move_bps);
        if velocity_bps > -min_move {
            return None; // No significant drop.
        }

        // Gate 2: complement-implied edge.
        let complement_id = pair.complement(token_id)?;
        let yes_ask = ctx.best_ask(token_id)?;
        let no_mid = ctx.mid_price(complement_id)?;

        // fair for this token = 1.0 - complement_mid, clamped to [0.05, 0.95].
        let fair = (Decimal::ONE - no_mid)
            .max(dec!(0.05))
            .min(dec!(0.95));
        let edge = fair - yes_ask;
        let required_edge = self.effective_min_edge(&pair);
        if edge < required_edge {
            debug!(
                token = %&token_id[..token_id.len().min(12)],
                edge = %edge,
                required = %required_edge,
                "Contrarian: edge too small"
            );
            return None;
        }

        // Gate 3: liquidity — must have meaningful bid support.
        let bid_depth = ctx.books.bid_depth(token_id).unwrap_or(Decimal::ZERO);
        if bid_depth < self.config.min_bid_depth {
            debug!(
                token = %&token_id[..token_id.len().min(12)],
                bid_depth = %bid_depth,
                "Contrarian: insufficient bid depth"
            );
            return None;
        }

        // Guard: ensure the complement token also has no open position
        // (avoid double-entry if both YES and NO tokens trigger the same tick).
        if !ctx.position(complement_id).is_flat() {
            return None;
        }

        // Sizing.
        let shares = (self.config.max_bet_usd / yes_ask)
            .round_dp_with_strategy(2, RoundingStrategy::ToZero);
        if shares <= Decimal::ZERO {
            return None;
        }
        let notional = shares * yes_ask;
        if notional < self.config.min_bet_usd {
            return None;
        }

        Some(
            OrderIntent::new(
                pair.condition_id.clone(),
                token_id.clone(),
                Side::Buy,
                yes_ask,
                shares,
                Urgency::Normal,
                format!(
                    "contrarian_entry: drop={:.0}bps edge={:.3} fair={:.3}",
                    velocity_bps, edge, fair
                ),
                self.name.clone(),
            )
            .with_tick_size(pair.tick_size)
            .with_priority(60),
        )
    }

    /// Build exit intents for all tracked open positions.
    fn check_exits(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let mut intents = Vec::new();

        for entry in self.entry_tracking.iter() {
            let token_id = entry.key();
            let &(entry_price, entry_time) = entry.value();

            // Stale guard: position closed externally (sync, other strategy, etc.).
            let pos = ctx.position(token_id);
            if pos.is_flat() {
                // Will be cleaned up by on_fill or on the next check.
                continue;
            }

            // Already have a pending exit for this token.
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
                    "Contrarian: exit triggered"
                );

                let intent = OrderIntent::new(
                    pair.condition_id.clone(),
                    token_id.clone(),
                    Side::Sell,
                    best_bid,
                    sell_size,
                    Urgency::Normal,
                    format!("contrarian_exit:{}", reason),
                    self.name.clone(),
                )
                .with_tick_size(pair.tick_size)
                .with_priority(60);

                intents.push(intent);
                // Mark pending to avoid duplicate exits on the next tick.
                self.pending_exit.insert(token_id.clone(), Instant::now());
            }
        }

        intents
    }
}

// ---------------------------------------------------------------------------
// Strategy trait
// ---------------------------------------------------------------------------

impl Strategy for ContrarianStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        60
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

        // Update rolling mid-price history first (even if we skip entry).
        let mid = match ctx.mid_price(token_id) {
            Some(m) => m,
            None => return vec![],
        };
        self.update_price_history(token_id, mid, ctx.now);

        // Try to generate an entry intent.
        match self.check_entry(token_id, ctx) {
            Some(intent) => {
                let market_id = intent.market_id.clone();
                self.last_trade.insert(market_id, Instant::now());
                self.trade_count.fetch_add(1, Ordering::Relaxed);
                info!(
                    token = %&token_id[..token_id.len().min(12)],
                    price = %intent.price,
                    size = %intent.size,
                    reason = %intent.reason,
                    "Contrarian: entry intent generated"
                );
                vec![intent]
            }
            None => vec![],
        }
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        match fill.side {
            Side::Buy => {
                // Record entry only on the first fill (avoid overwriting on partials).
                if !self.entry_tracking.contains_key(&fill.token_id) {
                    self.entry_tracking
                        .insert(fill.token_id.clone(), (fill.price, Instant::now()));
                    debug!(
                        token = %&fill.token_id[..fill.token_id.len().min(12)],
                        price = %fill.price,
                        "Contrarian: entry recorded"
                    );
                }
            }
            Side::Sell => {
                self.entry_tracking.remove(&fill.token_id);
                self.pending_exit.remove(&fill.token_id);
                debug!(
                    token = %&fill.token_id[..fill.token_id.len().min(12)],
                    "Contrarian: exit confirmed"
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

        // Clean up entry_tracking for positions that were closed externally.
        self.entry_tracking.retain(|token_id, _| {
            !ctx.position(token_id).is_flat()
        });

        self.check_exits(ctx)
    }

    fn on_shutdown(&self, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        let count = self.trade_count();
        if count > 0 {
            warn!(
                trades = count,
                open_positions = self.entry_tracking.len(),
                "ContrarianStrategy shutting down"
            );
        }
        vec![]
    }
}
