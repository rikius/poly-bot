//! Polymarket Trading Bot - Entry Point
//!
//! High-frequency trading bot for Polymarket prediction markets.

use polymarket_bot::{Bot, Config, KillSwitch};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("polymarket_bot=info".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    info!("===========================================");
    info!("  Polymarket Trading Bot v{}", env!("CARGO_PKG_VERSION"));
    info!("===========================================");

    // Load configuration
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            error!("Make sure your .env file contains:");
            error!("  - api_key or POLYMARKET_API_KEY");
            error!("  - secret_key or POLYMARKET_SECRET");
            error!("  - passphrase or POLYMARKET_PASSPHRASE");
            error!("  - private_key or PRIVATE_KEY");
            error!("  - builder_address or WALLET_ADDRESS");
            return Err(e.into());
        }
    };

    info!("Configuration loaded successfully");
    info!("  Wallet: {}", config.wallet_address);
    info!("  Mode: {:?}", config.mode);
    info!("  Max bet: ${}", config.max_bet_usd);
    info!("  Max daily loss: ${}", config.max_daily_loss_usd);

    if config.is_paper_mode() {
        warn!(">>> PAPER TRADING MODE - No real orders will be placed <<<");
    } else {
        warn!(">>> LIVE TRADING MODE - Real money at risk! <<<");
    }

    // Initialize kill switch
    let kill_switch = Arc::new(KillSwitch::new());
    let ks_clone = kill_switch.clone();

    // Set up Ctrl+C handler
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            warn!("Ctrl+C received - initiating shutdown");
            ks_clone.kill();
        }
    });

    info!("Kill switch initialized");
    info!("  - Set POLYBOT_KILL=1 to stop");
    info!("  - Or create /tmp/polybot_kill");
    info!("  - Or press Ctrl+C");

    // Token IDs to subscribe to
    // TODO: This should come from config or market discovery in Phase 3+
    let token_ids = vec![
        "64216051096597192974559951503874795452539828700419399952430572386874662003148".to_string(),
    ];

    // Create and run the bot
    let mut bot = Bot::new(config, kill_switch.clone(), token_ids).await;
    bot.run().await;

    // Graceful shutdown
    info!("Performing graceful shutdown...");
    polymarket_bot::kill_switch::graceful_shutdown(kill_switch).await;

    info!("Bot shutdown complete");
    Ok(())
}
