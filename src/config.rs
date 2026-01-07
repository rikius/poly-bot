//! Configuration management for the Polymarket bot

use crate::error::{BotError, Result};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Bot configuration loaded from environment
#[derive(Debug, Clone)]
pub struct Config {
    // API credentials
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
    pub private_key: String,
    pub wallet_address: String,

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

        let api_key = std::env::var("POLYMARKET_API_KEY")
            .or_else(|_| std::env::var("api_key"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_API_KEY or api_key".into()))?;

        let secret_key = std::env::var("POLYMARKET_SECRET")
            .or_else(|_| std::env::var("secret_key"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_SECRET or secret_key".into()))?;

        let passphrase = std::env::var("POLYMARKET_PASSPHRASE")
            .or_else(|_| std::env::var("passphrase"))
            .map_err(|_| BotError::Config("Missing POLYMARKET_PASSPHRASE or passphrase".into()))?;

        let private_key = std::env::var("PRIVATE_KEY")
            .or_else(|_| std::env::var("private_key"))
            .map_err(|_| BotError::Config("Missing PRIVATE_KEY or private_key".into()))?;

        let wallet_address = std::env::var("WALLET_ADDRESS")
            .or_else(|_| std::env::var("builder_address"))
            .map_err(|_| BotError::Config("Missing WALLET_ADDRESS or builder_address".into()))?;

        // Operating mode (default to paper)
        let mode = match std::env::var("BOT_MODE").as_deref() {
            Ok("live") => OperatingMode::Live,
            _ => OperatingMode::Paper,
        };

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

        Ok(Config {
            api_key,
            secret_key: secret_key.trim_matches('"').to_string(),
            passphrase: passphrase.trim_matches('"').to_string(),
            private_key: private_key.trim_matches('"').to_string(),
            wallet_address,
            mode,
            log_level,
            max_bet_usd,
            max_position_per_market_usd,
            max_total_exposure_usd,
            max_daily_loss_usd,
            max_open_orders,
        })
    }

    /// Check if running in paper trading mode
    pub fn is_paper_mode(&self) -> bool {
        self.mode == OperatingMode::Paper
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
    use super::*;

    #[test]
    fn test_operating_mode_default_paper() {
        // Without BOT_MODE set, should default to Paper
        std::env::remove_var("BOT_MODE");
        // This would need full env setup to test properly
    }
}
