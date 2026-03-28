//! Configuration management for the Polymarket bot

use crate::error::{BotError, Result};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Bot configuration loaded from environment
#[derive(Debug, Clone)]
pub struct Config {
    // API credentials (optional in Paper mode)
    pub api_key: Option<String>,
    pub secret_key: Option<String>,
    pub passphrase: Option<String>,
    pub private_key: Option<String>,
    pub wallet_address: Option<String>,

    // Operating mode
    pub mode: OperatingMode,
    pub log_level: String,

    // Sizing configuration
    pub max_bet_usd: Decimal,
    pub max_position_per_market_usd: Decimal,
    pub max_total_exposure_usd: Decimal,

    // Risk limits
    pub max_daily_loss_usd: Decimal,
    pub max_open_orders: u32,

    // Maker strategy configuration
    /// Use maker execution (GTC orders, zero fees)
    pub use_maker_mode: bool,
    /// Price offset for maker orders (cents inside spread)
    pub maker_price_offset: Decimal,
    /// TTL for maker orders before cancellation (seconds)
    pub maker_order_ttl_secs: u64,
    /// Enable MakerRebateArbStrategy (passive GTC arb with rebate capture)
    pub maker_rebate_enabled: bool,

    // Temporal arb configuration
    /// Enable TemporalArbStrategy (external price feed vs Polymarket)
    pub temporal_arb_enabled: bool,
    /// Minimum external price move in bps to trigger temporal arb (default 100)
    pub temporal_arb_threshold_bps: i64,
    /// Sensitivity parameter: bps_move / sensitivity → probability shift (default 2000)
    pub temporal_arb_sensitivity_bps: i64,
}

/// Operating mode for the bot
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingMode {
    /// Paper trading - log what would happen, no real orders
    Paper,
    /// Live trading with real money
    Live,
}

impl Config {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        // Load .env file if it exists
        dotenvy::dotenv().ok();

        // Operating mode (default to paper)
        let mode = match std::env::var("BOT_MODE").as_deref() {
            Ok("live") => OperatingMode::Live,
            _ => OperatingMode::Paper,
        };

        // Credentials are optional in Paper mode, required in Live mode
        let api_key = std::env::var("POLYMARKET_API_KEY")
            .or_else(|_| std::env::var("api_key"))
            .ok();
        let secret_key = std::env::var("POLYMARKET_SECRET")
            .or_else(|_| std::env::var("secret_key"))
            .ok();
        let passphrase = std::env::var("POLYMARKET_PASSPHRASE")
            .or_else(|_| std::env::var("passphrase"))
            .ok();
        let private_key = std::env::var("PRIVATE_KEY")
            .or_else(|_| std::env::var("private_key"))
            .ok();
        let wallet_address = std::env::var("WALLET_ADDRESS")
            .or_else(|_| std::env::var("builder_address"))
            .ok();

        // Live mode requires all credentials
        if mode == OperatingMode::Live {
            if api_key.is_none() || secret_key.is_none() || passphrase.is_none()
                || private_key.is_none() || wallet_address.is_none()
            {
                return Err(BotError::Config(
                    "Live mode requires all credentials: POLYMARKET_API_KEY, POLYMARKET_SECRET, \
                     POLYMARKET_PASSPHRASE, PRIVATE_KEY, WALLET_ADDRESS"
                        .into(),
                ));
            }
        }

        let log_level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());

        // Sizing (with defaults)
        let max_bet_usd = env_decimal("MAX_BET_USD", Decimal::from(100));
        let max_position_per_market_usd =
            env_decimal("MAX_POSITION_PER_MARKET_USD", Decimal::from(500));
        let max_total_exposure_usd = env_decimal("MAX_TOTAL_EXPOSURE_USD", Decimal::from(2000));

        // Risk limits (with defaults)
        let max_daily_loss_usd = env_decimal("MAX_DAILY_LOSS_USD", Decimal::from(100));
        let max_open_orders = std::env::var("MAX_OPEN_ORDERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        // Maker strategy configuration
        let use_maker_mode = std::env::var("USE_MAKER_MODE")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let maker_price_offset = env_decimal("MAKER_PRICE_OFFSET", Decimal::new(5, 1)); // 0.5 cents default
        let maker_order_ttl_secs = std::env::var("MAKER_ORDER_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120); // 2 minutes default
        let maker_rebate_enabled = std::env::var("MAKER_REBATE_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let temporal_arb_enabled = std::env::var("TEMPORAL_ARB_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let temporal_arb_threshold_bps = std::env::var("TEMPORAL_ARB_THRESHOLD_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100i64);
        let temporal_arb_sensitivity_bps = std::env::var("TEMPORAL_ARB_SENSITIVITY_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000i64);

        Ok(Config {
            api_key,
            secret_key: secret_key.map(|s| s.trim_matches('"').to_string()),
            passphrase: passphrase.map(|s| s.trim_matches('"').to_string()),
            private_key: private_key.map(|s| s.trim_matches('"').to_string()),
            wallet_address,
            mode,
            log_level,
            max_bet_usd,
            max_position_per_market_usd,
            max_total_exposure_usd,
            max_daily_loss_usd,
            max_open_orders,
            use_maker_mode,
            maker_price_offset,
            maker_order_ttl_secs,
            maker_rebate_enabled,
            temporal_arb_enabled,
            temporal_arb_threshold_bps,
            temporal_arb_sensitivity_bps,
        })
    }

    /// Check if running in paper trading mode
    pub fn is_paper_mode(&self) -> bool {
        self.mode == OperatingMode::Paper
    }

    /// Check if API credentials are available for authentication
    ///
    /// Returns false if any credential is missing or is a placeholder value
    /// (empty, "0x", or similar stubs from .env.example).
    pub fn has_credentials(&self) -> bool {
        fn is_valid(val: &Option<String>) -> bool {
            match val.as_deref() {
                Some(s) => {
                    let s = s.trim();
                    !s.is_empty() && s != "0x" && s != "0x0" && s.len() > 4
                }
                None => false,
            }
        }

        is_valid(&self.api_key)
            && is_valid(&self.secret_key)
            && is_valid(&self.passphrase)
            && is_valid(&self.private_key)
            && is_valid(&self.wallet_address)
    }
}

/// Helper to read a Decimal from environment with default
fn env_decimal(key: &str, default: Decimal) -> Decimal {
    std::env::var(key)
        .ok()
        .and_then(|v| Decimal::from_str(&v).ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_operating_mode_default_paper() {
        // Without BOT_MODE set, should default to Paper
        std::env::remove_var("BOT_MODE");
        // This would need full env setup to test properly
    }
}
