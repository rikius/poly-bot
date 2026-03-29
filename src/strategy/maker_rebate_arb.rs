//! Maker Rebate Arbitrage Strategy
//!
//! Same YES+NO < $1.00 arb logic as [`MathArbStrategy`], but posts passive
//! GTC limit orders at `best_ask − 1 tick` on both legs.  This earns maker
//! rebates (~50 bps) instead of paying taker fees (~100 bps) — a net fee
//! improvement of ~150 bps per round-trip.
//!
//! ## How it works
//!
//! 1. Detect arb: `(yes_ask − tick) + (no_ask − tick) < $1.00 − min_edge`
//! 2. Post GTC BUY orders at those maker prices on both legs.
//! 3. Wait up to `ttl_secs` for both legs to fill.
//! 4. If TTL expires with only one leg filled, `on_tick` generates a FOK sell
//!    to flatten the imbalanced position immediately.
//!
//! ## Enable
//!
//! Set `MAKER_REBATE_ENABLED=true` in `.env`.  Configure `MAKER_ORDER_TTL`
//! (default 120 s) to control how long to wait before triggering unwind.
//!
//! [`MathArbStrategy`]: crate::strategy::MathArbStrategy

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::ledger::Fill;
use crate::strategy::edge_calculator::{EdgeCalculator, EdgeConfig};
use crate::strategy::market_pair::{MarketPair, MarketPairRegistry};
use crate::strategy::traits::{OrderIntent, Strategy, StrategyContext, Urgency};
use crate::websocket::types::{ConditionId, Side, TokenId};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for [`MakerRebateArbStrategy`].
#[derive(Debug, Clone)]
pub struct MakerRebateConfig {
    /// Minimum arb edge at our *posted* prices (lower than taker because we
    /// earn rebates instead of paying fees — default 0.5 cents).
    pub min_edge: Decimal,

    /// Maximum position size per leg (shares).
    pub max_position_size: Decimal,

    /// Minimum position size per leg (below this, skip the trade).
    pub min_position_size: Decimal,

    /// Maximum total cost per bet in USD (both legs combined).
    /// Maps directly to MAX_BET_USD from the global config.
    pub max_bet_usd: Decimal,

    /// Maximum total exposure across all open maker arbs.
    pub max_total_exposure: Decimal,

    /// Cooldown between new arbs on the *same* market (ms).
    /// Longer than taker because GTC orders linger in the book.
    pub cooldown_ms: u64,

    /// Seconds to wait for both legs to fill before the unwind check fires.
    pub ttl_secs: u64,
}

impl Default for MakerRebateConfig {
    fn default() -> Self {
        Self {
            min_edge: dec!(0.005),          // 0.5 cents
            max_position_size: dec!(500),
            min_position_size: dec!(1),
            max_bet_usd: dec!(5),           // $5 per bet — overridden from config
            max_total_exposure: dec!(2000),
            cooldown_ms: 5_000,             // 5 s — GTC orders linger
            ttl_secs: 120,                  // matches MAKER_ORDER_TTL default
        }
    }
}

// ---------------------------------------------------------------------------
// Internal state: one entry per active maker arb
// ---------------------------------------------------------------------------

struct PendingArb {
    condition_id: ConditionId,
    yes_token_id: TokenId,
    no_token_id: TokenId,
    /// Expected fill size on each leg.
    size: Decimal,
    /// USDC exposure reserved when this arb was posted (used to release on expiry).
    reserved_notional: Decimal,
    posted_at: Instant,
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// Maker rebate arbitrage strategy.
///
/// See module-level docs for full description.
pub struct MakerRebateArbStrategy {
    name: String,
    config: MakerRebateConfig,
    registry: Arc<MarketPairRegistry>,
    edge_calculator: EdgeCalculator,
    enabled: AtomicBool,
    last_trade: dashmap::DashMap<ConditionId, Instant>,
    trade_count: AtomicU64,
    current_exposure: std::sync::RwLock<Decimal>,
    /// Active arb orders awaiting fill (one per condition_id).
    pending_arbs: dashmap::DashMap<ConditionId, PendingArb>,
}

impl MakerRebateArbStrategy {
    /// Create with default config.
    pub fn new(registry: Arc<MarketPairRegistry>) -> Self {
        Self::with_config(registry, MakerRebateConfig::default())
    }

    /// Create with custom config.
    pub fn with_config(registry: Arc<MarketPairRegistry>, config: MakerRebateConfig) -> Self {
        Self {
            name: "MakerRebateArbStrategy".to_string(),
            edge_calculator: EdgeCalculator::with_config(EdgeConfig::maker()),
            config,
            registry,
            enabled: AtomicBool::new(true),
            last_trade: dashmap::DashMap::new(),
            trade_count: AtomicU64::new(0),
            current_exposure: std::sync::RwLock::new(Decimal::ZERO),
            pending_arbs: dashmap::DashMap::new(),
        }
    }

    /// Enable or disable the strategy at runtime.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Number of arb trades posted since startup.
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

    fn has_pending_arb(&self, cid: &ConditionId) -> bool {
        self.pending_arbs.contains_key(cid)
    }

    /// Compute maker BUY posting price: one tick below the best ask, but
    /// never lower than the current best bid (to stay competitive).
    /// Result is always aligned to `tick_size`.
    fn maker_price(best_ask: Decimal, best_bid: Decimal, tick_size: Decimal) -> Decimal {
        let one_tick_below = best_ask - tick_size;
        let raw = one_tick_below.max(best_bid);
        // Floor to tick boundary — best_bid may also have sub-tick precision
        if tick_size.is_zero() {
            raw
        } else {
            (raw / tick_size).floor() * tick_size
        }
    }

    // -----------------------------------------------------------------------
    // Core opportunity check
    // -----------------------------------------------------------------------

    fn check_arb_opportunity(
        &self,
        pair: &MarketPair,
        ctx: &StrategyContext,
    ) -> Option<Vec<OrderIntent>> {
        // Need both sides of both books
        let yes_book = ctx.books.get_book(&pair.yes_token_id)?;
        let no_book = ctx.books.get_book(&pair.no_token_id)?;

        if !yes_book.is_two_sided() || !no_book.is_two_sided() {
            return None;
        }

        let yes_ask = yes_book.best_ask()?;
        let yes_bid = yes_book.best_bid()?;
        let no_ask = no_book.best_ask()?;
        let no_bid = no_book.best_bid()?;

        // Prices we will actually post (tick-aligned)
        let yes_post = Self::maker_price(yes_ask, yes_bid, pair.tick_size);
        let no_post = Self::maker_price(no_ask, no_bid, pair.tick_size);
        let combined_cost = yes_post + no_post;
        let edge = dec!(1) - combined_cost;

        if edge < self.config.min_edge {
            debug!(
                market = %pair.condition_id,
                yes_post = %yes_post,
                no_post = %no_post,
                edge = %edge,
                "Maker edge insufficient"
            );
            return None;
        }

        // Full edge calculation (uses book depth for slippage + size limits)
        let calc = self.edge_calculator.calculate(
            &yes_book,
            &no_book,
            pair.fee_rate_bps,
            self.config.min_position_size,
        );

        if !calc.is_profitable {
            debug!(
                market = %pair.condition_id,
                "Not profitable after full edge calculation"
            );
            return None;
        }

        // Determine trade size
        let max_by_exposure = {
            let current = *self.current_exposure.read().unwrap();
            (self.config.max_total_exposure - current) / dec!(2)
        };
        let max_by_balance = if combined_cost > Decimal::ZERO {
            ctx.available_cash() / combined_cost
        } else {
            Decimal::ZERO
        };
        // Cap total cost per bet to max_bet_usd (both legs combined).
        let max_by_bet = if combined_cost > Decimal::ZERO {
            self.config.max_bet_usd / combined_cost
        } else {
            Decimal::ZERO
        };
        let trade_size = calc
            .max_size
            .min(self.config.max_position_size)
            .min(max_by_exposure)
            .min(max_by_balance)
            .min(max_by_bet)
            .max(Decimal::ZERO)
            // Polymarket requires size to have at most 2 decimal places.
            // Truncate (floor) so we never exceed the balance cap.
            .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero);

        if trade_size < self.config.min_position_size {
            debug!(market = %pair.condition_id, trade_size = %trade_size, "Size below minimum");
            return None;
        }

        let total_notional = combined_cost * trade_size;
        {
            let current = *self.current_exposure.read().unwrap();
            if current + total_notional > self.config.max_total_exposure {
                debug!(market = %pair.condition_id, "Exposure limit would be exceeded");
                return None;
            }
        }

        info!(
            market = %pair.condition_id,
            yes_post = %yes_post,
            no_post = %no_post,
            edge_cents = %((edge * dec!(100)).round()),
            trade_size = %trade_size,
            ttl_secs = %self.config.ttl_secs,
            "📌 Maker rebate arb: posting both legs"
        );

        let group_id = format!("maker-arb-{}", Uuid::new_v4());

        let yes_intent = OrderIntent::new(
            pair.condition_id.clone(),
            pair.yes_token_id.clone(),
            Side::Buy,
            yes_post,
            trade_size,
            Urgency::Passive,
            format!(
                "Maker arb YES @ {:.2}, edge {:.1}¢",
                yes_post,
                edge * dec!(100)
            ),
            self.name.clone(),
        )
        .with_group(group_id.clone())
        .with_priority(90)
        .with_tick_size(pair.tick_size);

        let no_intent = OrderIntent::new(
            pair.condition_id.clone(),
            pair.no_token_id.clone(),
            Side::Buy,
            no_post,
            trade_size,
            Urgency::Passive,
            format!(
                "Maker arb NO @ {:.2}, edge {:.1}¢",
                no_post,
                edge * dec!(100)
            ),
            self.name.clone(),
        )
        .with_group(group_id)
        .with_priority(90)
        .with_tick_size(pair.tick_size);

        // Register pending arb for TTL tracking
        self.pending_arbs.insert(
            pair.condition_id.clone(),
            PendingArb {
                condition_id: pair.condition_id.clone(),
                yes_token_id: pair.yes_token_id.clone(),
                no_token_id: pair.no_token_id.clone(),
                size: trade_size,
                reserved_notional: total_notional,
                posted_at: Instant::now(),
            },
        );
        self.last_trade.insert(pair.condition_id.clone(), Instant::now());
        self.trade_count.fetch_add(1, Ordering::Relaxed);
        {
            let mut exp = self.current_exposure.write().unwrap();
            *exp += total_notional;
        }

        Some(vec![yes_intent, no_intent])
    }
}

// ---------------------------------------------------------------------------
// Strategy trait implementation
// ---------------------------------------------------------------------------

impl Strategy for MakerRebateArbStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> u8 {
        90 // Just below MathArbStrategy (100)
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
            None => {
                debug!(token = %token_id, "Token not in registry");
                return vec![];
            }
        };

        if &pair.condition_id != market_id {
            return vec![];
        }

        // Don't stack arbs: wait for existing maker orders to resolve
        if self.has_pending_arb(market_id) {
            return vec![];
        }

        if self.is_on_cooldown(market_id) {
            return vec![];
        }

        self.check_arb_opportunity(&pair, ctx).unwrap_or_default()
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Reduce tracked exposure when we sell (unwind)
        if fill.side == Side::Sell {
            let mut exp = self.current_exposure.write().unwrap();
            *exp = (*exp - fill.notional()).max(Decimal::ZERO);
        }
        vec![]
    }

    /// Check pending arbs for TTL expiry and generate unwind intents if needed.
    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        if !self.is_enabled() {
            return vec![];
        }

        let mut unwinds: Vec<OrderIntent> = Vec::new();
        let mut expired: Vec<ConditionId> = Vec::new();

        for entry in self.pending_arbs.iter() {
            let p = entry.value();
            if p.posted_at.elapsed().as_secs() < self.config.ttl_secs {
                continue; // Still within TTL window
            }

            let yes_pos = ctx.position(&p.yes_token_id).shares;
            let no_pos = ctx.position(&p.no_token_id).shares;
            // 50 % fill threshold — if we have ≥ half the expected size, treat leg as filled
            let threshold = p.size / dec!(2);
            let yes_filled = yes_pos >= threshold;
            let no_filled = no_pos >= threshold;

            match (yes_filled, no_filled) {
                (true, false) => {
                    // YES leg filled, NO didn't — unwind YES
                    if let Some(bid) = ctx.best_bid(&p.yes_token_id) {
                        warn!(
                            market = %p.condition_id,
                            yes_pos = %yes_pos,
                            "⚠️ Maker arb TTL: YES filled, NO didn't — unwinding YES"
                        );
                        unwinds.push(
                            OrderIntent::new(
                                p.condition_id.clone(),
                                p.yes_token_id.clone(),
                                Side::Sell,
                                bid,
                                yes_pos,
                                Urgency::Immediate,
                                "Maker arb unwind: YES (NO unfilled)".to_string(),
                                self.name.clone(),
                            )
                            .with_priority(255),
                        );
                    }
                }
                (false, true) => {
                    // NO leg filled, YES didn't — unwind NO
                    if let Some(bid) = ctx.best_bid(&p.no_token_id) {
                        warn!(
                            market = %p.condition_id,
                            no_pos = %no_pos,
                            "⚠️ Maker arb TTL: NO filled, YES didn't — unwinding NO"
                        );
                        unwinds.push(
                            OrderIntent::new(
                                p.condition_id.clone(),
                                p.no_token_id.clone(),
                                Side::Sell,
                                bid,
                                no_pos,
                                Urgency::Immediate,
                                "Maker arb unwind: NO (YES unfilled)".to_string(),
                                self.name.clone(),
                            )
                            .with_priority(255),
                        );
                    }
                }
                (true, true) => {
                    info!(market = %p.condition_id, "✅ Maker arb: both legs filled");
                }
                (false, false) => {
                    // Neither leg filled — release the exposure that was reserved
                    // when these orders were posted.
                    let mut exp = self.current_exposure.write().unwrap();
                    *exp = (*exp - p.reserved_notional).max(Decimal::ZERO);
                    debug!(
                        market = %p.condition_id,
                        "Maker arb TTL expired, neither leg filled — cleaning up"
                    );
                }
            }

            expired.push(p.condition_id.clone());
        }

        for key in expired {
            self.pending_arbs.remove(&key);
        }

        unwinds
    }

    fn on_shutdown(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        let mut unwinds: Vec<OrderIntent> = Vec::new();

        for entry in self.pending_arbs.iter() {
            let p = entry.value();
            let yes_pos = ctx.position(&p.yes_token_id).shares;
            let no_pos = ctx.position(&p.no_token_id).shares;

            if yes_pos > Decimal::ZERO {
                if let Some(bid) = ctx.best_bid(&p.yes_token_id) {
                    unwinds.push(
                        OrderIntent::new(
                            p.condition_id.clone(),
                            p.yes_token_id.clone(),
                            Side::Sell,
                            bid,
                            yes_pos,
                            Urgency::Immediate,
                            "Shutdown unwind: maker arb YES".to_string(),
                            self.name.clone(),
                        )
                        .with_priority(255),
                    );
                }
            }

            if no_pos > Decimal::ZERO {
                if let Some(bid) = ctx.best_bid(&p.no_token_id) {
                    unwinds.push(
                        OrderIntent::new(
                            p.condition_id.clone(),
                            p.no_token_id.clone(),
                            Side::Sell,
                            bid,
                            no_pos,
                            Urgency::Immediate,
                            "Shutdown unwind: maker arb NO".to_string(),
                            self.name.clone(),
                        )
                        .with_priority(255),
                    );
                }
            }
        }

        info!(
            trades = self.trade_count(),
            "MakerRebateArbStrategy shutting down"
        );
        unwinds
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::state::OrderBookState;
    use crate::strategy::market_pair::MarketPair;
    use crate::websocket::types::PriceLevel;

    fn make_registry() -> Arc<MarketPairRegistry> {
        let r = Arc::new(MarketPairRegistry::new());
        r.register(
            MarketPair::new(
                "0xmarket".to_string(),
                "yes_tok".to_string(),
                "no_tok".to_string(),
            )
            .with_fee_rate(0),
        );
        r
    }

    /// YES ask=0.48 bid=0.47 | NO ask=0.49 bid=0.48
    /// Maker prices: YES=0.47, NO=0.48 → combined=0.95, edge=0.05 ✓
    fn books_with_arb() -> OrderBookState {
        let b = OrderBookState::new();
        b.update_book(
            "yes_tok".to_string(),
            "0xmarket".to_string(),
            vec![PriceLevel { price: "0.47".to_string(), size: "1000".to_string() }],
            vec![PriceLevel { price: "0.48".to_string(), size: "1000".to_string() }],
            None,
            None,
        );
        b.update_book(
            "no_tok".to_string(),
            "0xmarket".to_string(),
            vec![PriceLevel { price: "0.48".to_string(), size: "1000".to_string() }],
            vec![PriceLevel { price: "0.49".to_string(), size: "1000".to_string() }],
            None,
            None,
        );
        b
    }

    /// YES ask=0.52 | NO ask=0.52 → combined taker=1.04, maker=1.02 → no edge
    fn books_no_arb() -> OrderBookState {
        let b = OrderBookState::new();
        b.update_book(
            "yes_tok".to_string(),
            "0xmarket".to_string(),
            vec![PriceLevel { price: "0.51".to_string(), size: "1000".to_string() }],
            vec![PriceLevel { price: "0.52".to_string(), size: "1000".to_string() }],
            None,
            None,
        );
        b.update_book(
            "no_tok".to_string(),
            "0xmarket".to_string(),
            vec![PriceLevel { price: "0.51".to_string(), size: "1000".to_string() }],
            vec![PriceLevel { price: "0.52".to_string(), size: "1000".to_string() }],
            None,
            None,
        );
        b
    }

    #[test]
    fn test_detects_maker_arb_opportunity() {
        let strategy = MakerRebateArbStrategy::new(make_registry());
        let books = books_with_arb();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );

        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].urgency, Urgency::Passive);
        assert_eq!(intents[1].urgency, Urgency::Passive);
        // Both legs should share a group_id
        assert!(intents[0].group_id.is_some());
        assert_eq!(intents[0].group_id, intents[1].group_id);
    }

    #[test]
    fn test_maker_prices_are_below_ask() {
        let strategy = MakerRebateArbStrategy::new(make_registry());
        let books = books_with_arb();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert_eq!(intents.len(), 2);

        // YES posted price should be 0.47 (ask 0.48 − 0.01), NO at 0.48 (ask 0.49 − 0.01)
        assert_eq!(intents[0].price, dec!(0.47));
        assert_eq!(intents[1].price, dec!(0.48));
    }

    #[test]
    fn test_no_arb_when_unprofitable() {
        let strategy = MakerRebateArbStrategy::new(make_registry());
        let books = books_no_arb();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert!(intents.is_empty());
    }

    #[test]
    fn test_no_double_arb_while_pending() {
        let strategy = MakerRebateArbStrategy::new(make_registry());
        let books = books_with_arb();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let first = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert_eq!(first.len(), 2);

        // Second call should be blocked by pending_arb guard
        let second = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert!(second.is_empty());
    }

    #[test]
    fn test_disabled_strategy() {
        let strategy = MakerRebateArbStrategy::new(make_registry());
        strategy.set_enabled(false);

        let books = books_with_arb();
        let ledger = Ledger::new(dec!(10_000));
        let ctx = StrategyContext::new(&books, &ledger);

        let intents = strategy.on_book_update(
            &"0xmarket".to_string(),
            &"yes_tok".to_string(),
            &ctx,
        );
        assert!(intents.is_empty());
    }

    #[test]
    fn test_maker_price_helper() {
        // Standard case: 1-cent spread
        assert_eq!(
            MakerRebateArbStrategy::maker_price(dec!(0.48), dec!(0.47), dec!(0.01)),
            dec!(0.47)
        );
        // Tight spread (ask - tick would be below bid) → clamp to bid
        assert_eq!(
            MakerRebateArbStrategy::maker_price(dec!(0.50), dec!(0.50), dec!(0.01)),
            dec!(0.50) // ask - tick = 0.49 < bid 0.50, clamped
        );
    }
}
