//! Polymarket Trading Bot - Entry Point
//!
//! High-frequency trading bot for Polymarket prediction markets.

use polymarket_bot::api::{run_api_server, ApiState};
use polymarket_bot::websocket::MarketDiscovery;
use polymarket_bot::latency;
use polymarket_bot::strategy::MarketPair;
use polymarket_bot::{Bot, Config, KillSwitch};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install TLS crypto provider before any network calls
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

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
    info!("  Wallet: {}", config.wallet_address.as_deref().unwrap_or("(none - paper mode)"));
    info!("  Mode: {:?}", config.mode);
    info!("  Max bet: ${}", config.max_bet_usd);
    info!("  Max daily loss: ${}", config.max_daily_loss_usd);

    if config.is_paper_mode() {
        if config.has_credentials() {
            warn!(">>> PAPER TRADING MODE - No real orders will be placed <<<");
        } else {
            warn!(">>> PAPER TRADING MODE (no credentials) - Simulation only <<<");
        }
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
    // LATENCY PROBE — pick lowest-RTT endpoint
    // ===========================================================================
    let selected = latency::probe_best_endpoint().await;
    info!(
        endpoint = %selected.url,
        rtt_ms = selected.rtt_ms,
        "Using endpoint"
    );

    // ===========================================================================
    // GEOBLOCK CHECK — verify trading is allowed from this IP
    // ===========================================================================
    {
        let clob_client = ClobClient::new(&selected.url, ClobConfig::default())
            .expect("Failed to create CLOB client for geoblock check");

        match clob_client.check_geoblock().await {
            Ok(geo) => {
                if geo.blocked {
                    error!(
                        "Trading not available from {} (IP: {}, region: {})",
                        geo.country, geo.ip, geo.region
                    );
                    return Err(anyhow::anyhow!(
                        "Geoblocked: trading not available in {}/{}",
                        geo.country,
                        geo.region
                    ));
                }
                info!(
                    "Geoblock check passed (IP: {}, country: {}, region: {})",
                    geo.ip, geo.country, geo.region
                );
            }
            Err(e) => {
                warn!("Geoblock check failed (proceeding anyway): {}", e);
            }
        }
    }

    // ===========================================================================
    // MARKET DISCOVERY
    // ===========================================================================
    // Discover 15-minute crypto markets from Gamma API
    // Uses slug-based discovery: btc-updown-15m-{timestamp}, etc.
    // ===========================================================================

    info!("Discovering 15-min crypto markets from Gamma API...");
    
    let discovery = MarketDiscovery::new();
    
    // Discover 15-min crypto markets (Up/Down) using slug pattern discovery
    let discovered = match discovery.discover_crypto_15min().await {
        Ok(markets) => {
            // Limit to first 5 markets
            markets.into_iter().take(5).collect::<Vec<_>>()
        }
        Err(e) => {
            warn!("Failed to discover markets from API: {}", e);
            warn!("Falling back to hardcoded test market...");
            vec![]
        }
    };
    
    let (market_pairs, token_ids): (Vec<MarketPair>, Vec<String>) = if !discovered.is_empty() {
        info!("Discovered {} tradeable 15-min crypto markets", discovered.len());
        
        let mut pairs = Vec::new();
        let mut tokens = Vec::new();
        
        for dm in &discovered {
            info!(
                "  Market: {} ({}/{})",
                dm.condition_id, dm.first_outcome, dm.second_outcome
            );
            info!("    Question: {}", dm.question);
            info!("    First token: {}", dm.first_token_id);
            info!("    Second token: {}", dm.second_token_id);
            info!("    Fee rate: {} bps", dm.fee_rate_bps);
            
            pairs.push(dm.to_market_pair());
            tokens.push(dm.first_token_id.clone());
            tokens.push(dm.second_token_id.clone());
        }
        
        (pairs, tokens)
    } else {
        // Fallback to hardcoded market for testing when API fails
        warn!("No markets discovered - using hardcoded fallback");
        
        let yes_token = "91146426612524606788185897426983484145854573836093539884347307480474597236733".to_string();
        let no_token = "42146376778762047477642266233020835044794863565048464944940190870964136665187".to_string();
        let market_id = "0x_fallback_test".to_string();
        
        let pairs = vec![
            MarketPair::new_up_down(
                market_id,
                yes_token.clone(),
                no_token.clone(),
            )
            .with_description("FALLBACK TEST MARKET"),
        ];
        
        let tokens = vec![yes_token, no_token];
        
        (pairs, tokens)
    };

    info!("Registered {} market pair(s) with {} token(s)", market_pairs.len(), token_ids.len());

    // Create the bot
    let mut bot = Bot::new(config, kill_switch.clone(), token_ids, market_pairs, &selected.url).await;

    // Start API server (shares read-only views of bot state)
    {
        let (ledger, obs, cfg) = bot.shared_state();
        let api_state = Arc::new(ApiState::new(ledger, obs, cfg));
        let api_port = std::env::var("API_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3001);
        tokio::spawn(run_api_server(api_state, api_port));
    }

    bot.run().await;

    // Graceful shutdown
    info!("Performing graceful shutdown...");
    polymarket_bot::kill_switch::graceful_shutdown(kill_switch).await;

    info!("Bot shutdown complete");
    Ok(())
}
