//! Polymarket Trading Bot - Entry Point
//!
//! High-frequency trading bot for Polymarket prediction markets.

use polymarket_bot::strategy::MarketPair;
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

    // ===========================================================================
    // MARKET DISCOVERY
    // ===========================================================================
    // In production, markets are discovered via REST API:
    //
    //   let client = ApiClient::new(&config)?;
    //   let markets = client.get_markets().await?;
    //   for market in markets {
    //       if market.active && !market.closed && market.tokens.len() == 2 {
    //           let yes_token = market.tokens.iter().find(|t| t.outcome == "Yes");
    //           let no_token = market.tokens.iter().find(|t| t.outcome == "No");
    //           if let (Some(yes), Some(no)) = (yes_token, no_token) {
    //               let pair = MarketPair::new(
    //                   market.condition_id,
    //                   yes.token_id.clone(),
    //                   no.token_id.clone(),
    //               );
    //               registry.register(pair);
    //           }
    //       }
    //   }
    //
    // For Phase 7 MVP, we use hardcoded test tokens.
    // TODO: Replace with API discovery in Phase 8+
    // ===========================================================================

    // HARDCODED TEST MARKET - Replace with API discovery!
    // These are example token IDs from a BTC 15-minute market
    let yes_token = "91146426612524606788185897426983484145854573836093539884347307480474597236733".to_string();
    let no_token = "42146376778762047477642266233020835044794863565048464944940190870964136665187".to_string();
    let market_id = "91146426612524606788185897426983484145854573836093539884347307480474597236733".to_string();

    let market_pairs = vec![
        MarketPair::new(
            market_id.clone(),
            yes_token.clone(),
            no_token.clone(),
        )
        .with_fee_rate(1000) // 15-min crypto has 10% max fee
        .with_description("BTC 15-minute Up/Down [TEST]"),
    ];

    let token_ids = vec![yes_token, no_token];

    warn!("Using {} HARDCODED test market pair(s) - implement API discovery for production!", market_pairs.len());

    // Create and run the bot
    let mut bot = Bot::new(config, kill_switch.clone(), token_ids, market_pairs).await;
    bot.run().await;

    // Graceful shutdown
    info!("Performing graceful shutdown...");
    polymarket_bot::kill_switch::graceful_shutdown(kill_switch).await;

    info!("Bot shutdown complete");
    Ok(())
}
