//! Configuration management for the Polymarket bot

use crate::alerts::{AlertBackend, AlertSender};
use crate::error::{BotError, Result};
use rust_decimal::Decimal;
use std::sync::Arc;
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

    /// Polygon JSON-RPC endpoint for on-chain redemptions.
    /// Defaults to the public polygon-rpc.com if not set.
    pub polygon_rpc_url: String,

    /// Whether to attempt on-chain `redeemPositions()` for resolved markets.
    /// Set false to disable redemption (e.g. when the RPC endpoint is unavailable).
    /// Default: true.
    pub redeem_enabled: bool,

    // Operating mode
    pub mode: OperatingMode,
    pub log_level: String,

    // Portfolio / cash tracking
    /// Starting USDC balance for the ledger (read from INITIAL_CASH_USD env var).
    /// Defaults to max_total_exposure_usd when not set.
    /// In live mode, set this to your actual USDC balance so portfolio P&L is accurate.
    pub initial_cash_usd: Decimal,

    // Sizing configuration
    pub max_bet_usd: Decimal,
    pub max_position_per_market_usd: Decimal,
    pub max_total_exposure_usd: Decimal,

    // Risk limits
    pub max_daily_loss_usd: Decimal,
    pub max_open_orders: u32,

    // Core arb strategy toggles
    /// Enable MathArbStrategy (YES + NO combined ask < $1 arbitrage). Default: true.
    pub math_arb_enabled: bool,

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

    // Contrarian (mean-reversion) strategy configuration
    /// Enable ContrarianStrategy (fade sharp price drops expecting mean reversion)
    pub contrarian_enabled: bool,
    /// Min mid-price drop in bps within the lookback window to trigger entry (default 500)
    pub contrarian_min_move_bps: i64,
    /// Lookback window for velocity measurement in seconds (default 60)
    pub contrarian_lookback_secs: u64,
    /// Max notional per contrarian trade in USDC (default: inherits max_bet_usd)
    pub contrarian_max_bet_usd: Decimal,
    /// Time-based exit: close after this many seconds (default 120)
    pub contrarian_max_hold_secs: u64,
    /// Take-profit threshold as fraction of entry price (default 0.08 = 8 %)
    pub contrarian_profit_target_pct: Decimal,
    /// Stop-loss threshold as fraction of entry price (default 0.05 = 5 %)
    pub contrarian_stop_loss_pct: Decimal,
    /// Minimum edge for standard markets (default 0.05)
    pub contrarian_min_edge: Decimal,
    /// Minimum edge for 10 %-fee markets (default 0.12)
    pub contrarian_high_fee_min_edge: Decimal,
    /// Per-market cooldown after a trade fires in ms (default 10 000)
    pub contrarian_cooldown_ms: u64,

    // TBO Trend (Trending Breakout) strategy configuration
    /// Enable TboTrendStrategy (breakout momentum)
    pub tbo_enabled: bool,
    /// Rolling lookback window in book-update ticks (default 50)
    pub tbo_lookback_ticks: usize,
    /// Bps above rolling high required to qualify as a breakout (default 150)
    pub tbo_breakout_threshold_bps: i64,
    /// Consecutive ticks above breakout level before entry (default 3)
    pub tbo_min_confirm_ticks: u32,
    /// Max notional per trade in USDC (default: inherits max_bet_usd)
    pub tbo_max_bet_usd: Decimal,
    /// Time-based exit in seconds (default 180)
    pub tbo_max_hold_secs: u64,
    /// Take-profit threshold (default 0.06)
    pub tbo_profit_target_pct: Decimal,
    /// Stop-loss threshold (default 0.04)
    pub tbo_stop_loss_pct: Decimal,
    /// Minimum ask-side depth in shares (default 10)
    pub tbo_min_ask_depth: Decimal,
    /// Per-market cooldown in ms (default 15 000)
    pub tbo_cooldown_ms: u64,

    // TBT Divergence + Late Entry strategy configuration
    /// Enable TbtDivergenceStrategy (RSI divergence + late entry)
    pub tbt_enabled: bool,
    /// Rolling lookback window in book-update ticks (default 80)
    pub tbt_lookback_ticks: usize,
    /// RSI computation period in price changes (default 14)
    pub tbt_rsi_period: usize,
    /// Minimum price gap between old/recent lows in bps (default 100)
    pub tbt_divergence_min_gap_bps: i64,
    /// Pullback required from detection mid before late entry in bps (default 50)
    pub tbt_pullback_bps: i64,
    /// Ticks to wait for pullback before discarding signal (default 30)
    pub tbt_signal_expiry_ticks: u32,
    /// Max notional per trade in USDC (default: inherits max_bet_usd)
    pub tbt_max_bet_usd: Decimal,
    /// Time-based exit in seconds (default 240)
    pub tbt_max_hold_secs: u64,
    /// Take-profit threshold (default 0.07)
    pub tbt_profit_target_pct: Decimal,
    /// Stop-loss threshold (default 0.05)
    pub tbt_stop_loss_pct: Decimal,
    /// Minimum ask-side depth in shares (default 8)
    pub tbt_min_ask_depth: Decimal,
    /// Per-market cooldown in ms (default 20 000)
    pub tbt_cooldown_ms: u64,

    // Late Entry (High-Confidence Near-Resolution) strategy configuration
    /// Enable LateEntryStrategy (buy near-certain outcomes close to resolution)
    pub late_entry_enabled: bool,
    /// Enter when this fraction of the round remains (e.g. 0.20 = last 20%).
    /// Scales automatically: 15-min × 0.20 = last 3 min; 5-min × 0.20 = last 1 min.
    /// Default: 0.20.
    pub late_entry_window_pct: f64,
    /// Minimum ask price to buy — token must cost ≥ this (default 0.90 = near-certain winner)
    pub late_entry_min_entry_price: Decimal,
    /// Max notional per trade in USDC (default: inherits max_bet_usd)
    pub late_entry_max_bet_usd: Decimal,
    /// Minimum ask-side depth in shares (default 5)
    pub late_entry_min_ask_depth: Decimal,
    /// Per-market cooldown in ms — prevents re-entering same round (default 120 000)
    pub late_entry_cooldown_ms: u64,
    /// Stop-loss price: sell if best bid drops to or below this (default 0.70, set 0 to disable)
    pub late_entry_stop_loss_price: Decimal,

    // Market discovery configuration
    /// Asset filter: only trade these assets, e.g. ["btc"], ["btc", "eth"].
    /// Empty = trade all discovered assets.
    pub market_assets: Vec<String>,

    /// Timeframe for Up/Down markets: "15m", "5m", "1m", etc.
    /// Must match Polymarket slug convention.
    pub market_timeframe: String,

    /// Interval in seconds for the chosen timeframe (900 for 15m, 300 for 5m).
    pub market_interval_secs: u64,

    /// Max number of markets to monitor simultaneously.
    pub market_limit: usize,

    // Alerting configuration
    /// Alert backend: "discord", "telegram", or None
    pub alert_backend: Option<String>,
    /// Discord incoming webhook URL (used when alert_backend = "discord")
    pub discord_webhook_url: Option<String>,
    /// Telegram bot token (used when alert_backend = "telegram")
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID (used when alert_backend = "telegram")
    pub telegram_chat_id: Option<String>,
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
        let polygon_rpc_url = std::env::var("POLYGON_RPC_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| crate::claim::POLYGON_RPC_DEFAULT.to_string());
        let redeem_enabled = std::env::var("REDEEM_ENABLED")
            .map(|v| v.trim().to_lowercase() != "false")
            .unwrap_or(true);

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

        // Initial cash — defaults to max_total_exposure_usd when not explicitly set.
        // Override with INITIAL_CASH_USD to match your actual USDC account balance.
        let initial_cash_usd = std::env::var("INITIAL_CASH_USD")
            .ok()
            .and_then(|v| Decimal::from_str(&v).ok())
            .unwrap_or(max_total_exposure_usd);

        // Risk limits (with defaults)
        let max_daily_loss_usd = env_decimal("MAX_DAILY_LOSS_USD", Decimal::from(100));
        let max_open_orders = std::env::var("MAX_OPEN_ORDERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        // Maker strategy configuration
        let math_arb_enabled = std::env::var("MATH_ARB_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true); // default ON — preserves existing behaviour

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

        // Contrarian strategy configuration
        let contrarian_enabled = std::env::var("CONTRARIAN_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let contrarian_min_move_bps = std::env::var("CONTRARIAN_MIN_MOVE_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500i64);
        let contrarian_lookback_secs = std::env::var("CONTRARIAN_LOOKBACK_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60u64);
        let contrarian_max_bet_usd = std::env::var("CONTRARIAN_MAX_BET_USD")
            .ok()
            .and_then(|v| Decimal::from_str(&v).ok())
            .unwrap_or(max_bet_usd);
        let contrarian_max_hold_secs = std::env::var("CONTRARIAN_MAX_HOLD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120u64);
        let contrarian_profit_target_pct = env_decimal("CONTRARIAN_PROFIT_TARGET_PCT", Decimal::new(8, 2));
        let contrarian_stop_loss_pct = env_decimal("CONTRARIAN_STOP_LOSS_PCT", Decimal::new(5, 2));
        let contrarian_min_edge = env_decimal("CONTRARIAN_MIN_EDGE", Decimal::new(5, 2));
        let contrarian_high_fee_min_edge = env_decimal("CONTRARIAN_HIGH_FEE_MIN_EDGE", Decimal::new(12, 2));
        let contrarian_cooldown_ms = std::env::var("CONTRARIAN_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000u64);

        // TBO Trend strategy configuration
        let tbo_enabled = std::env::var("TBO_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let tbo_lookback_ticks = std::env::var("TBO_LOOKBACK_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50usize);
        let tbo_breakout_threshold_bps = std::env::var("TBO_BREAKOUT_THRESHOLD_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(150i64);
        let tbo_min_confirm_ticks = std::env::var("TBO_MIN_CONFIRM_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3u32);
        let tbo_max_bet_usd = std::env::var("TBO_MAX_BET_USD")
            .ok()
            .and_then(|v| Decimal::from_str(&v).ok())
            .unwrap_or(max_bet_usd);
        let tbo_max_hold_secs = std::env::var("TBO_MAX_HOLD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(180u64);
        let tbo_profit_target_pct = env_decimal("TBO_PROFIT_TARGET_PCT", Decimal::new(6, 2));
        let tbo_stop_loss_pct = env_decimal("TBO_STOP_LOSS_PCT", Decimal::new(4, 2));
        let tbo_min_ask_depth = env_decimal("TBO_MIN_ASK_DEPTH", Decimal::new(10, 0));
        let tbo_cooldown_ms = std::env::var("TBO_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15_000u64);

        // TBT Divergence strategy configuration
        let tbt_enabled = std::env::var("TBT_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let tbt_lookback_ticks = std::env::var("TBT_LOOKBACK_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80usize);
        let tbt_rsi_period = std::env::var("TBT_RSI_PERIOD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(14usize);
        let tbt_divergence_min_gap_bps = std::env::var("TBT_DIVERGENCE_MIN_GAP_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100i64);
        let tbt_pullback_bps = std::env::var("TBT_PULLBACK_BPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50i64);
        let tbt_signal_expiry_ticks = std::env::var("TBT_SIGNAL_EXPIRY_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u32);
        let tbt_max_bet_usd = std::env::var("TBT_MAX_BET_USD")
            .ok()
            .and_then(|v| Decimal::from_str(&v).ok())
            .unwrap_or(max_bet_usd);
        let tbt_max_hold_secs = std::env::var("TBT_MAX_HOLD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(240u64);
        let tbt_profit_target_pct = env_decimal("TBT_PROFIT_TARGET_PCT", Decimal::new(7, 2));
        let tbt_stop_loss_pct = env_decimal("TBT_STOP_LOSS_PCT", Decimal::new(5, 2));
        let tbt_min_ask_depth = env_decimal("TBT_MIN_ASK_DEPTH", Decimal::new(8, 0));
        let tbt_cooldown_ms = std::env::var("TBT_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20_000u64);

        // Late Entry strategy configuration
        let late_entry_enabled = std::env::var("LATE_ENTRY_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let late_entry_window_pct = std::env::var("LATE_ENTRY_WINDOW_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.20);
        let late_entry_min_entry_price =
            env_decimal("LATE_ENTRY_MIN_ENTRY_PRICE", Decimal::new(90, 2));
        let late_entry_max_bet_usd = std::env::var("LATE_ENTRY_MAX_BET_USD")
            .ok()
            .and_then(|v| Decimal::from_str(&v).ok())
            .unwrap_or(max_bet_usd);
        let late_entry_min_ask_depth =
            env_decimal("LATE_ENTRY_MIN_ASK_DEPTH", Decimal::new(5, 0));
        let late_entry_stop_loss_price =
            env_decimal("LATE_ENTRY_STOP_LOSS_PRICE", Decimal::new(70, 2));
        let late_entry_cooldown_ms = std::env::var("LATE_ENTRY_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120_000u64);

        // Market discovery
        // MARKET_ASSETS=BTC,ETH  → ["btc", "eth"]  (empty = all supported assets)
        let market_assets: Vec<String> = std::env::var("MARKET_ASSETS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        // MARKET_TIMEFRAME=15m  (default "15m")
        let market_timeframe = std::env::var("MARKET_TIMEFRAME")
            .unwrap_or_else(|_| "15m".to_string());

        // Derive interval_secs from timeframe string so callers don't have to
        let market_interval_secs = timeframe_to_secs(&market_timeframe);

        // MARKET_LIMIT=3  (default 5)
        let market_limit = std::env::var("MARKET_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5usize);

        // Alerting
        let alert_backend = std::env::var("ALERT_BACKEND").ok();
        let discord_webhook_url = std::env::var("DISCORD_WEBHOOK_URL").ok();
        let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
        let telegram_chat_id = std::env::var("TELEGRAM_CHAT_ID").ok();

        Ok(Config {
            api_key,
            secret_key: secret_key.map(|s| s.trim_matches('"').to_string()),
            passphrase: passphrase.map(|s| s.trim_matches('"').to_string()),
            private_key: private_key.map(|s| s.trim_matches('"').to_string()),
            wallet_address,
            polygon_rpc_url,
            redeem_enabled,
            mode,
            log_level,
            initial_cash_usd,
            max_bet_usd,
            max_position_per_market_usd,
            max_total_exposure_usd,
            max_daily_loss_usd,
            max_open_orders,
            math_arb_enabled,
            use_maker_mode,
            maker_price_offset,
            maker_order_ttl_secs,
            maker_rebate_enabled,
            temporal_arb_enabled,
            temporal_arb_threshold_bps,
            temporal_arb_sensitivity_bps,
            contrarian_enabled,
            contrarian_min_move_bps,
            contrarian_lookback_secs,
            contrarian_max_bet_usd,
            contrarian_max_hold_secs,
            contrarian_profit_target_pct,
            contrarian_stop_loss_pct,
            contrarian_min_edge,
            contrarian_high_fee_min_edge,
            contrarian_cooldown_ms,
            tbo_enabled,
            tbo_lookback_ticks,
            tbo_breakout_threshold_bps,
            tbo_min_confirm_ticks,
            tbo_max_bet_usd,
            tbo_max_hold_secs,
            tbo_profit_target_pct,
            tbo_stop_loss_pct,
            tbo_min_ask_depth,
            tbo_cooldown_ms,
            tbt_enabled,
            tbt_lookback_ticks,
            tbt_rsi_period,
            tbt_divergence_min_gap_bps,
            tbt_pullback_bps,
            tbt_signal_expiry_ticks,
            tbt_max_bet_usd,
            tbt_max_hold_secs,
            tbt_profit_target_pct,
            tbt_stop_loss_pct,
            tbt_min_ask_depth,
            tbt_cooldown_ms,
            late_entry_enabled,
            late_entry_window_pct,
            late_entry_min_entry_price,
            late_entry_max_bet_usd,
            late_entry_min_ask_depth,
            late_entry_stop_loss_price,
            late_entry_cooldown_ms,
            market_assets,
            market_timeframe,
            market_interval_secs,
            market_limit,
            alert_backend,
            discord_webhook_url,
            telegram_bot_token,
            telegram_chat_id,
        })
    }

    /// Build an [`AlertSender`] from the current configuration, if a valid
    /// backend is configured.  Returns `None` when alerting is disabled.
    pub fn alert_sender(&self) -> Option<Arc<AlertSender>> {
        match self.alert_backend.as_deref() {
            Some("discord") => {
                let url = self.discord_webhook_url.clone()?;
                Some(AlertSender::new(AlertBackend::Discord { url }))
            }
            Some("telegram") => {
                let token = self.telegram_bot_token.as_ref()?;
                let chat_id = self.telegram_chat_id.clone()?;
                let endpoint = format!(
                    "https://api.telegram.org/bot{}/sendMessage",
                    token
                );
                Some(AlertSender::new(AlertBackend::Telegram { endpoint, chat_id }))
            }
            _ => None,
        }
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

        let has_wallet = is_valid(&self.wallet_address);

        // Full live mode: private key present — credentials are derived automatically.
        let has_signer = is_valid(&self.private_key);

        // Simulation/monitoring mode: explicit API key env vars provided without a
        // private key (e.g. for connecting the user WebSocket without placing orders).
        let has_manual_creds = is_valid(&self.api_key)
            && is_valid(&self.secret_key)
            && is_valid(&self.passphrase);

        has_wallet && (has_signer || has_manual_creds)
    }

    /// Whether the bot can derive L2 credentials from its private key.
    /// False in simulation mode where only manual API credentials are provided.
    pub fn has_private_key(&self) -> bool {
        self.private_key.as_deref().map_or(false, |s| {
            let s = s.trim();
            !s.is_empty() && s != "0x" && s != "0x0" && s.len() > 4
        })
    }
}

/// Convert a human-readable timeframe string to seconds.
/// Supports formats like "15m", "5m", "1h", "1d".
/// Unknown formats fall back to 900 (15 minutes).
fn timeframe_to_secs(tf: &str) -> u64 {
    let tf = tf.trim().to_lowercase();
    let (digits, unit) = tf.split_at(tf.find(|c: char| !c.is_ascii_digit()).unwrap_or(tf.len()));
    let n: u64 = digits.parse().unwrap_or(15);
    match unit {
        "s" => n,
        "m" | "min" => n * 60,
        "h" | "hr" => n * 3600,
        "d" | "day" => n * 86400,
        _ => 900,
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
