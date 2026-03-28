//! Risk Limits - Hard guardrails to prevent catastrophic losses
//!
//! Pre-trade and ongoing checks to enforce position limits, loss limits,
//! and exposure constraints.

use crate::websocket::types::{Side, TokenId};
use crate::constants::*;
use crate::ledger::Ledger;
use crate::strategy::market_pair::MarketPairRegistry;
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use tracing::error;

/// Risk limit violation types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitViolation {
    /// Daily loss limit exceeded
    DailyLossExceeded {
        current_loss: Decimal,
        limit: Decimal,
    },
    /// Too many open orders
    MaxOpenOrdersExceeded { current: u32, limit: u32 },
    /// Single position too large
    MaxPositionExceeded {
        token_id: TokenId,
        current_exposure: Decimal,
        limit: Decimal,
    },
    /// Unhedged exposure too high
    MaxUnhedgedExposureExceeded {
        current: Decimal,
        limit: Decimal,
    },
    /// Order size exceeds maximum
    OrderSizeTooLarge { size: Decimal, limit: Decimal },
    /// Order size below minimum
    OrderSizeTooSmall { size: Decimal, minimum: Decimal },
    /// Insufficient available cash
    InsufficientCash { needed: Decimal, available: Decimal },
}

impl std::fmt::Display for LimitViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitViolation::DailyLossExceeded {
                current_loss,
                limit,
            } => {
                write!(
                    f,
                    "Daily loss ${} exceeds limit ${}",
                    current_loss, limit
                )
            }
            LimitViolation::MaxOpenOrdersExceeded { current, limit } => {
                write!(f, "Open orders {} exceeds limit {}", current, limit)
            }
            LimitViolation::MaxPositionExceeded {
                token_id,
                current_exposure,
                limit,
            } => {
                write!(
                    f,
                    "Position exposure ${} in {} exceeds limit ${}",
                    current_exposure, token_id, limit
                )
            }
            LimitViolation::MaxUnhedgedExposureExceeded { current, limit } => {
                write!(
                    f,
                    "Unhedged exposure ${} exceeds limit ${}",
                    current, limit
                )
            }
            LimitViolation::OrderSizeTooLarge { size, limit } => {
                write!(f, "Order size ${} exceeds max ${}", size, limit)
            }
            LimitViolation::OrderSizeTooSmall { size, minimum } => {
                write!(f, "Order size ${} below minimum ${}", size, minimum)
            }
            LimitViolation::InsufficientCash { needed, available } => {
                write!(
                    f,
                    "Insufficient cash: need ${}, have ${}",
                    needed, available
                )
            }
        }
    }
}

/// Risk limits configuration
#[derive(Debug, Clone)]
pub struct RiskLimitsConfig {
    /// Maximum daily loss before stopping (USD)
    pub max_daily_loss: Decimal,
    /// Maximum concurrent open orders
    pub max_open_orders: u32,
    /// Maximum exposure per position (USD)
    pub max_position_exposure: Decimal,
    /// Maximum unhedged directional exposure (USD)
    pub max_unhedged_exposure: Decimal,
    /// Maximum order size (USD)
    pub max_order_size: Decimal,
    /// Minimum order size (USD)
    pub min_order_size: Decimal,
}

impl Default for RiskLimitsConfig {
    fn default() -> Self {
        Self {
            max_daily_loss: MAX_DAILY_LOSS_USD,
            max_open_orders: MAX_OPEN_ORDERS,
            max_position_exposure: MAX_BET_SIZE_USD,
            max_unhedged_exposure: MAX_UNHEDGED_EXPOSURE,
            max_order_size: MAX_BET_SIZE_USD,
            min_order_size: MIN_BET_SIZE_USD,
        }
    }
}

/// Risk limits enforcer
pub struct RiskLimits {
    /// Configuration
    config: RiskLimitsConfig,
    /// Daily realized P&L tracking
    daily_realized_pnl: RwLock<Decimal>,
    /// Daily starting balance (for loss calculation)
    daily_start_balance: RwLock<Decimal>,
    /// Whether trading is halted due to limits
    trading_halted: AtomicBool,
    /// Reason for halt
    halt_reason: RwLock<Option<LimitViolation>>,
}

impl RiskLimits {
    /// Create with default limits
    pub fn new() -> Self {
        Self {
            config: RiskLimitsConfig::default(),
            daily_realized_pnl: RwLock::new(Decimal::ZERO),
            daily_start_balance: RwLock::new(Decimal::ZERO),
            trading_halted: AtomicBool::new(false),
            halt_reason: RwLock::new(None),
        }
    }

    /// Create with custom limits
    pub fn with_config(config: RiskLimitsConfig) -> Self {
        Self {
            config,
            daily_realized_pnl: RwLock::new(Decimal::ZERO),
            daily_start_balance: RwLock::new(Decimal::ZERO),
            trading_halted: AtomicBool::new(false),
            halt_reason: RwLock::new(None),
        }
    }

    /// Set daily start balance (call at start of trading day)
    pub fn set_daily_start_balance(&self, balance: Decimal) {
        *self.daily_start_balance.write().unwrap() = balance;
        *self.daily_realized_pnl.write().unwrap() = Decimal::ZERO;
    }

    /// Update daily P&L from a fill and re-check the limit.
    ///
    /// Pass `current_unrealized` (from the ledger's position tracker) so the
    /// check includes mark-to-market losses on open positions.
    pub fn record_realized_pnl(&self, pnl: Decimal, current_unrealized: Decimal) {
        let mut daily_realized = self.daily_realized_pnl.write().unwrap();
        *daily_realized += pnl;

        // Check realized + unrealized combined so that a deeply underwater
        // open position halts trading before it is actually closed.
        let total = *daily_realized + current_unrealized;
        if total < -self.config.max_daily_loss {
            self.halt_trading(LimitViolation::DailyLossExceeded {
                current_loss: -total,
                limit: self.config.max_daily_loss,
            });
        }
    }

    /// Check if trading is allowed
    pub fn is_trading_allowed(&self) -> bool {
        !self.trading_halted.load(Ordering::Relaxed)
    }

    /// Get halt reason if halted
    pub fn halt_reason(&self) -> Option<LimitViolation> {
        self.halt_reason.read().unwrap().clone()
    }

    /// Pre-trade check for a proposed order
    pub fn check_order(
        &self,
        token_id: &TokenId,
        side: Side,
        price: Decimal,
        size: Decimal,
        ledger: &Ledger,
    ) -> Result<(), LimitViolation> {
        let order_value = price * size;

        // Check order size bounds
        if order_value > self.config.max_order_size {
            return Err(LimitViolation::OrderSizeTooLarge {
                size: order_value,
                limit: self.config.max_order_size,
            });
        }

        if order_value < self.config.min_order_size {
            return Err(LimitViolation::OrderSizeTooSmall {
                size: order_value,
                minimum: self.config.min_order_size,
            });
        }

        // Check open orders count
        let open_orders = ledger.open_orders_count();
        if open_orders >= self.config.max_open_orders {
            return Err(LimitViolation::MaxOpenOrdersExceeded {
                current: open_orders,
                limit: self.config.max_open_orders,
            });
        }

        // Check available cash for buy orders
        if side == Side::Buy {
            let cash = ledger.cash_snapshot();
            if cash.available < order_value {
                return Err(LimitViolation::InsufficientCash {
                    needed: order_value,
                    available: cash.available,
                });
            }
        }

        // Check position exposure
        let current_position = ledger.get_position(token_id);
        let current_exposure = current_position.shares.abs() * current_position.avg_cost;
        let new_exposure = if side == Side::Buy {
            // Buying always increases or opens a long position → exposure grows.
            current_exposure + order_value
        } else {
            // Selling: closing a long reduces exposure; opening/extending a short
            // increases it.  Use the actual share count to determine direction.
            if current_position.is_long() {
                // Closing long: exposure decreases (floor at zero)
                (current_exposure - order_value).max(Decimal::ZERO)
            } else {
                // Flat or already short: selling opens/extends a short position
                current_exposure + order_value
            }
        };

        if new_exposure > self.config.max_position_exposure {
            return Err(LimitViolation::MaxPositionExceeded {
                token_id: token_id.clone(),
                current_exposure: new_exposure,
                limit: self.config.max_position_exposure,
            });
        }

        Ok(())
    }

    /// Check all limits against current ledger state
    pub fn check_all(&self, ledger: &Ledger, registry: Option<&MarketPairRegistry>) -> Vec<LimitViolation> {
        let mut violations = Vec::new();

        // Check daily loss — include unrealized P&L so that an open position
        // that is deeply underwater triggers the halt before it is closed.
        let daily_realized = *self.daily_realized_pnl.read().unwrap();
        let daily_unrealized = ledger.positions.total_unrealized_pnl();
        let daily_pnl = daily_realized + daily_unrealized;
        if daily_pnl < -self.config.max_daily_loss {
            violations.push(LimitViolation::DailyLossExceeded {
                current_loss: -daily_pnl,
                limit: self.config.max_daily_loss,
            });
        }

        // Check open orders — use >= to match check_order's pre-trade gate
        // (both block at exactly max_open_orders, not one order past it).
        let open_orders = ledger.open_orders_count();
        if open_orders >= self.config.max_open_orders {
            violations.push(LimitViolation::MaxOpenOrdersExceeded {
                current: open_orders,
                limit: self.config.max_open_orders,
            });
        }

        // Check unhedged exposure
        let unhedged = self.calculate_unhedged_exposure(ledger, registry);
        if unhedged > self.config.max_unhedged_exposure {
            violations.push(LimitViolation::MaxUnhedgedExposureExceeded {
                current: unhedged,
                limit: self.config.max_unhedged_exposure,
            });
        }

        violations
    }

    /// Calculate unhedged directional exposure
    ///
    /// For binary markets (YES/NO or Up/Down), a perfectly hedged position has
    /// equal notional on both legs. Unhedged exposure per market pair is:
    ///
    ///   |yes_notional - no_notional|
    ///
    /// When registry is `None` (not yet initialised), falls back to summing all
    /// absolute position notionals — a conservative over-estimate that keeps
    /// trading safe during startup.
    pub fn calculate_unhedged_exposure(
        &self,
        ledger: &Ledger,
        registry: Option<&MarketPairRegistry>,
    ) -> Decimal {
        let snapshot = ledger.snapshot();

        let Some(registry) = registry else {
            // Conservative fallback: sum all absolute position notionals
            return snapshot
                .positions
                .iter()
                .map(|pos| pos.shares.abs() * pos.avg_cost)
                .fold(Decimal::ZERO, |acc, v| acc + v);
        };

        let mut total_unhedged = Decimal::ZERO;
        // Track which condition IDs we've already accounted for
        let mut seen: HashSet<String> = HashSet::new();

        for pos in &snapshot.positions {
            if let Some(pair) = registry.get_by_token(&pos.token_id) {
                if !seen.insert(pair.condition_id.clone()) {
                    // Already processed this market pair via its complement
                    continue;
                }

                // Find the paired position (complement token)
                let complement_id = pair.complement(&pos.token_id).cloned();
                let complement_notional = complement_id
                    .and_then(|cid| {
                        snapshot.positions.iter().find(|p| p.token_id == cid)
                    })
                    .map(|p| p.shares.abs() * p.avg_cost)
                    .unwrap_or(Decimal::ZERO);

                let this_notional = pos.shares.abs() * pos.avg_cost;
                // Net directional exposure for this market pair
                total_unhedged += (this_notional - complement_notional).abs();
            } else {
                // Token not registered in any pair — treat as fully unhedged
                total_unhedged += pos.shares.abs() * pos.avg_cost;
            }
        }

        total_unhedged
    }

    /// Get current daily P&L
    pub fn daily_pnl(&self) -> Decimal {
        *self.daily_realized_pnl.read().unwrap()
    }

    /// Get current stats
    pub fn stats(&self) -> RiskLimitsStats {
        RiskLimitsStats {
            daily_pnl: self.daily_pnl(),
            max_daily_loss: self.config.max_daily_loss,
            trading_halted: self.trading_halted.load(Ordering::Relaxed),
            halt_reason: self.halt_reason(),
        }
    }

    /// Reset for new trading day
    pub fn reset_daily(&self, starting_balance: Decimal) {
        self.set_daily_start_balance(starting_balance);
        self.trading_halted.store(false, Ordering::Relaxed);
        *self.halt_reason.write().unwrap() = None;
    }

    // --- Private methods ---

    fn halt_trading(&self, reason: LimitViolation) {
        error!("Risk limits HALT: {}", reason);
        self.trading_halted.store(true, Ordering::Relaxed);
        *self.halt_reason.write().unwrap() = Some(reason);
    }
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for risk limits
#[derive(Debug, Clone)]
pub struct RiskLimitsStats {
    pub daily_pnl: Decimal,
    pub max_daily_loss: Decimal,
    pub trading_halted: bool,
    pub halt_reason: Option<LimitViolation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use rust_decimal_macros::dec;

    fn make_config(max_loss: Decimal, max_orders: u32) -> RiskLimitsConfig {
        RiskLimitsConfig {
            max_daily_loss: max_loss,
            max_open_orders: max_orders,
            max_position_exposure: dec!(1000),
            max_unhedged_exposure: dec!(500),
            max_order_size: dec!(500),
            min_order_size: dec!(10),
        }
    }

    #[test]
    fn test_initial_state() {
        let limits = RiskLimits::new();
        assert!(limits.is_trading_allowed());
        assert_eq!(limits.daily_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_daily_loss_halt() {
        let config = make_config(dec!(100), 10);
        let limits = RiskLimits::with_config(config);

        // Record loss within limit
        limits.record_realized_pnl(dec!(-50), Decimal::ZERO);
        assert!(limits.is_trading_allowed());

        // Record loss exceeding limit
        limits.record_realized_pnl(dec!(-60), Decimal::ZERO);
        assert!(!limits.is_trading_allowed());
        assert!(matches!(
            limits.halt_reason(),
            Some(LimitViolation::DailyLossExceeded { .. })
        ));
    }

    #[test]
    fn test_order_size_validation() {
        let config = make_config(dec!(1000), 10);
        let limits = RiskLimits::with_config(config);
        let ledger = Ledger::new(dec!(10000));

        // Order too large
        let result = limits.check_order(
            &"token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(2000), // $1000 > $500 limit
            &ledger,
        );
        assert!(matches!(
            result,
            Err(LimitViolation::OrderSizeTooLarge { .. })
        ));

        // Order too small
        let result = limits.check_order(
            &"token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(10), // $5 < $10 minimum
            &ledger,
        );
        assert!(matches!(
            result,
            Err(LimitViolation::OrderSizeTooSmall { .. })
        ));

        // Valid order
        let result = limits.check_order(
            &"token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(100), // $50 is valid
            &ledger,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_insufficient_cash() {
        let config = make_config(dec!(1000), 10);
        let limits = RiskLimits::with_config(config);
        let ledger = Ledger::new(dec!(50)); // Only $50 available

        let result = limits.check_order(
            &"token1".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(200), // $100 > $50 available
            &ledger,
        );
        assert!(matches!(
            result,
            Err(LimitViolation::InsufficientCash { .. })
        ));
    }

    #[test]
    fn test_daily_reset() {
        let config = make_config(dec!(100), 10);
        let limits = RiskLimits::with_config(config);

        // Trigger halt
        limits.record_realized_pnl(dec!(-150), Decimal::ZERO);
        assert!(!limits.is_trading_allowed());

        // Reset for new day
        limits.reset_daily(dec!(10000));
        assert!(limits.is_trading_allowed());
        assert_eq!(limits.daily_pnl(), Decimal::ZERO);
    }

    #[test]
    fn test_stats() {
        let limits = RiskLimits::new();
        limits.record_realized_pnl(dec!(100), Decimal::ZERO);
        limits.record_realized_pnl(dec!(-30), Decimal::ZERO);

        let stats = limits.stats();
        assert_eq!(stats.daily_pnl, dec!(70));
        assert!(!stats.trading_halted);
    }
}
