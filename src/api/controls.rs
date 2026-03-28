//! Runtime control state — shared between the API server and the bot.
//!
//! `ControlState` is created once in `Bot::new()` and Arc-shared into both
//! the bot event loop and the API server so either side can read/write it.
//!
//! ## What is immediately effective
//! - `trading_paused` — checked on every book-update; setting it true stops
//!   the bot from generating new intents without killing the process.
//!
//! ## What takes effect on the next trade decision
//! - `runtime_config` fields — read by the strategy router and executor on
//!   each evaluation, so changes propagate within the next book-update cycle.

use crate::config::Config;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Strategy and risk parameters that can be changed at runtime via the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Maximum USD size per trade leg
    pub max_bet_usd: Decimal,
    /// Maximum USD exposure per market
    pub max_position_per_market_usd: Decimal,
    /// Maximum total exposure across all markets
    pub max_total_exposure_usd: Decimal,
    /// Daily loss limit in USD before trading is halted
    pub max_daily_loss_usd: Decimal,
    /// Maximum concurrent open orders
    pub max_open_orders: u32,
    /// Use maker execution (GTC, 0% fees) instead of taker (FOK)
    pub use_maker_mode: bool,
    /// Enable TemporalArbStrategy (momentum vs Binance feed)
    pub temporal_arb_enabled: bool,
    /// Minimum external price move in bps to trigger temporal arb
    pub temporal_arb_threshold_bps: i64,
    /// Sensitivity: bps_move / sensitivity = probability shift
    pub temporal_arb_sensitivity_bps: i64,
}

impl RuntimeConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_bet_usd: config.max_bet_usd,
            max_position_per_market_usd: config.max_position_per_market_usd,
            max_total_exposure_usd: config.max_total_exposure_usd,
            max_daily_loss_usd: config.max_daily_loss_usd,
            max_open_orders: config.max_open_orders,
            use_maker_mode: config.use_maker_mode,
            temporal_arb_enabled: config.temporal_arb_enabled,
            temporal_arb_threshold_bps: config.temporal_arb_threshold_bps,
            temporal_arb_sensitivity_bps: config.temporal_arb_sensitivity_bps,
        }
    }
}

/// Partial update payload for `PATCH /api/config`.
/// All fields are optional — only provided fields are updated.
#[derive(Debug, Deserialize)]
pub struct ConfigPatch {
    pub max_bet_usd: Option<f64>,
    pub max_position_per_market_usd: Option<f64>,
    pub max_total_exposure_usd: Option<f64>,
    pub max_daily_loss_usd: Option<f64>,
    pub max_open_orders: Option<u32>,
    pub use_maker_mode: Option<bool>,
    pub temporal_arb_enabled: Option<bool>,
    pub temporal_arb_threshold_bps: Option<i64>,
    pub temporal_arb_sensitivity_bps: Option<i64>,
}

/// Mutable control state shared between the API server and the bot event loop.
pub struct ControlState {
    /// When true the bot generates no new intents (checked every book update).
    pub trading_paused: AtomicBool,
    /// Tunable strategy and risk parameters.
    pub runtime_config: RwLock<RuntimeConfig>,
}

impl ControlState {
    /// Build from static Config at startup.
    pub fn new(config: &Config) -> Arc<Self> {
        Arc::new(Self {
            trading_paused: AtomicBool::new(false),
            runtime_config: RwLock::new(RuntimeConfig::from_config(config)),
        })
    }

    pub fn is_paused(&self) -> bool {
        self.trading_paused.load(Ordering::Relaxed)
    }

    pub fn pause(&self) {
        self.trading_paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.trading_paused.store(false, Ordering::Relaxed);
    }

    /// Apply a partial patch to the runtime config.
    pub fn apply_patch(&self, patch: ConfigPatch) {
        let mut cfg = self.runtime_config.write().unwrap();
        if let Some(v) = patch.max_bet_usd {
            cfg.max_bet_usd = Decimal::try_from(v).unwrap_or(cfg.max_bet_usd);
        }
        if let Some(v) = patch.max_position_per_market_usd {
            cfg.max_position_per_market_usd =
                Decimal::try_from(v).unwrap_or(cfg.max_position_per_market_usd);
        }
        if let Some(v) = patch.max_total_exposure_usd {
            cfg.max_total_exposure_usd =
                Decimal::try_from(v).unwrap_or(cfg.max_total_exposure_usd);
        }
        if let Some(v) = patch.max_daily_loss_usd {
            cfg.max_daily_loss_usd = Decimal::try_from(v).unwrap_or(cfg.max_daily_loss_usd);
        }
        if let Some(v) = patch.max_open_orders {
            cfg.max_open_orders = v;
        }
        if let Some(v) = patch.use_maker_mode {
            cfg.use_maker_mode = v;
        }
        if let Some(v) = patch.temporal_arb_enabled {
            cfg.temporal_arb_enabled = v;
        }
        if let Some(v) = patch.temporal_arb_threshold_bps {
            cfg.temporal_arb_threshold_bps = v;
        }
        if let Some(v) = patch.temporal_arb_sensitivity_bps {
            cfg.temporal_arb_sensitivity_bps = v;
        }
    }
}
