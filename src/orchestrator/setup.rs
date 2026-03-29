//! Bot construction — authentication, strategy registration, WebSocket setup.

use super::Bot;
use crate::api::ControlState;
use crate::config::Config;
use crate::execution::{DualPolicy, OrderExecutor, OrderTracker};
use crate::feeds::{new_price_store, ExternalPriceStore};
use crate::feeds::binance::BinanceFeed;
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::metrics::BotLatency;
use crate::risk::CircuitBreaker;
use crate::state::OrderBookState;
use crate::strategy::{
    MakerRebateArbStrategy, MakerRebateConfig, MarketPair, MarketPairRegistry, MathArbStrategy,
    StrategyRouter, TemporalArbConfig, TemporalArbStrategy,
};
use crate::websocket::{MarketWebSocket, UserWebSocket};
use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::auth::{Credentials, Normal};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::clob::types::AssetType;
use polymarket_client_sdk::clob::types::request::{BalanceAllowanceRequest, UpdateBalanceAllowanceRequest};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

/// Pre-built authentication components created in `main.rs` before `Bot::new()`.
///
/// Passing these in avoids a second round-trip to derive API credentials —
/// `main.rs` already creates an unauthenticated client for the geoblock check,
/// so we reuse that same connection for auth and hand the result here.
pub struct AuthComponents {
    /// Fully authenticated CLOB client ready for order submission.
    pub clob_client: ClobClient<Authenticated<Normal>>,
    /// EIP-712 signer derived from the private key.
    pub signer: Arc<PrivateKeySigner>,
    /// L2 HMAC credentials derived from the private key.
    pub creds: Credentials,
}

impl Bot {
    /// Create a new bot instance.
    ///
    /// # Arguments
    /// * `config` - Bot configuration
    /// * `kill_switch` - Kill switch for emergency stop
    /// * `token_ids` - Token IDs to subscribe to (alternating YES/NO pairs)
    /// * `market_pairs` - Market pair definitions for arb detection
    /// * `auth` - Pre-built auth components from `main.rs` (Some in live/private-key mode,
    ///            None in simulation/paper mode without a private key)
    pub async fn new(
        config: Config,
        kill_switch: Arc<KillSwitch>,
        token_ids: Vec<String>,
        market_pairs: Vec<MarketPair>,
        auth: Option<AuthComponents>,
    ) -> Self {
        let config = Arc::new(config);
        let order_book_state = Arc::new(OrderBookState::new());
        let ledger = Arc::new(Ledger::new(config.initial_cash_usd));

        // Collect condition IDs before consuming market_pairs
        let condition_ids: Vec<String> = market_pairs
            .iter()
            .map(|p| p.condition_id.clone())
            .collect();

        // Set up market pair registry
        let market_registry = Arc::new(MarketPairRegistry::new());
        for pair in market_pairs {
            market_registry.register(pair);
        }

        // Set up strategy router
        let strategy_router = Arc::new(StrategyRouter::new());

        // Register MathArbStrategy
        let arb_config = if config.use_maker_mode {
            info!("Using MAKER mode for arb strategy (1% min edge, GTC orders, 0% fees)");
            crate::strategy::MathArbConfig::maker()
        } else {
            info!("Using TAKER mode for arb strategy (3% min edge, FOK orders)");
            crate::strategy::MathArbConfig::taker()
        };
        let math_arb = Arc::new(MathArbStrategy::with_config(market_registry.clone(), arb_config));
        if let Err(e) = strategy_router.register(math_arb) {
            warn!("Failed to register MathArbStrategy: {}", e);
        }

        // Shared external price store (populated by Binance feed when enabled)
        let external_prices: ExternalPriceStore = new_price_store();

        // Optionally register MakerRebateArbStrategy (MAKER_REBATE_ENABLED=true)
        if config.maker_rebate_enabled {
            info!(
                ttl_secs = config.maker_order_ttl_secs,
                "Registering MakerRebateArbStrategy (passive GTC arb, earns maker rebates)"
            );
            let rebate_config = MakerRebateConfig {
                ttl_secs: config.maker_order_ttl_secs,
                ..MakerRebateConfig::default()
            };
            let maker_rebate = Arc::new(MakerRebateArbStrategy::with_config(
                market_registry.clone(),
                rebate_config,
            ));
            if let Err(e) = strategy_router.register(maker_rebate) {
                warn!("Failed to register MakerRebateArbStrategy: {}", e);
            }
        }

        // Optionally register TemporalArbStrategy + start Binance feed
        if config.temporal_arb_enabled {
            info!(
                threshold_bps = config.temporal_arb_threshold_bps,
                sensitivity_bps = config.temporal_arb_sensitivity_bps,
                "Registering TemporalArbStrategy + starting Binance price feed"
            );
            let temporal_config = TemporalArbConfig {
                threshold_bps: config.temporal_arb_threshold_bps,
                sensitivity_bps: config.temporal_arb_sensitivity_bps,
                ..TemporalArbConfig::default()
            };
            let temporal = Arc::new(TemporalArbStrategy::with_config(
                market_registry.clone(),
                Arc::clone(&external_prices),
                temporal_config,
            ));
            if let Err(e) = strategy_router.register(temporal) {
                warn!("Failed to register TemporalArbStrategy: {}", e);
            }
            let feed = BinanceFeed::new(Arc::clone(&external_prices));
            tokio::spawn(async move {
                feed.run().await;
            });
        }

        let circuit_breaker = Arc::new(CircuitBreaker::new());
        let order_tracker = Arc::new(OrderTracker::new());
        let latency = BotLatency::new();
        let controls = ControlState::new(&config);
        let alerts = config.alert_sender();

        // Authenticate and set up executor + user WS only if credentials are available
        let (executor, user_ws_rx, user_ws_task) = if config.has_credentials() {
            // Resolve L2 credentials:
            //   Live mode   — pre-built AuthComponents passed in from main.rs
            //   Sim mode    — use manually-provided POLYMARKET_API_KEY / SECRET / PASSPHRASE
            let (sdk_credentials, maybe_executor) = if config.has_private_key() {
                // Unpack pre-built components — no extra round-trips needed.
                let AuthComponents { clob_client, signer, creds } = auth
                    .expect("AuthComponents must be provided when PRIVATE_KEY is set");

                info!(api_key = %creds.key(), "L2 credentials received");

                // Step 1: Tell the CLOB to refresh its on-chain balance cache.
                // Without this the API reports balance=0 and rejects every order.
                if let Err(e) = clob_client
                    .update_balance_allowance(
                        UpdateBalanceAllowanceRequest::builder()
                            .asset_type(AssetType::Collateral)
                            .build(),
                    )
                    .await
                {
                    warn!(error = %e, "Could not refresh CLOB balance cache — orders may be rejected");
                }

                // Step 2: Read back the refreshed balance and sync the ledger.
                // This seeds the internal ledger with the wallet's actual deposited
                // USDC so strategy size caps reflect real available funds.
                match clob_client
                    .balance_allowance(
                        BalanceAllowanceRequest::builder()
                            .asset_type(AssetType::Collateral)
                            .build(),
                    )
                    .await
                {
                    Ok(resp) => {
                        info!(
                            balance_usdc = %resp.balance,
                            "Portfolio balance loaded from Polymarket CLOB"
                        );
                        ledger.sync_cash(resp.balance);
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            fallback_usd = %config.initial_cash_usd,
                            "Could not read CLOB balance — using INITIAL_CASH_USD as fallback"
                        );
                    }
                }

                let policy = Arc::new(DualPolicy::new().with_maker_offset(config.maker_price_offset));
                info!(
                    "Execution policy: DualPolicy (Taker=FOK/FAK, Maker=GTC offset={} cents)",
                    config.maker_price_offset
                );

                let exec = Arc::new(OrderExecutor::new(
                    clob_client,
                    signer,
                    policy,
                    circuit_breaker.clone(),
                    Arc::clone(&latency),
                    alerts.clone(),
                ));

                (creds, Some(exec))
            } else {
                // Simulation mode: manual API credentials, no private key → no executor.
                info!(
                    "Simulation mode: using manual API credentials \
                     (no private key — order execution disabled)"
                );
                let api_key = Uuid::parse_str(config.api_key.as_ref().unwrap())
                    .expect("POLYMARKET_API_KEY must be a valid UUID");
                let creds = Credentials::new(
                    api_key,
                    config.secret_key.clone().unwrap(),
                    config.passphrase.clone().unwrap(),
                );
                (creds, None)
            };

            // User WebSocket — works in both live and simulation mode.
            let wallet_address = config
                .wallet_address
                .as_ref()
                .unwrap()
                .parse()
                .expect("WALLET_ADDRESS must be a valid Ethereum address");
            let (user_ws_tx, user_ws_rx) = mpsc::unbounded_channel();
            let user_ws = Arc::new(UserWebSocket::new(
                sdk_credentials,
                wallet_address,
                condition_ids,
                user_ws_tx,
            ));
            let user_ws_clone = user_ws.clone();
            let user_ws_task = tokio::spawn(async move {
                user_ws_clone.run().await;
            });

            (maybe_executor, Some(user_ws_rx), Some(user_ws_task))
        } else {
            info!("Paper mode without credentials — no executor or user WebSocket");
            (None, None, None)
        };

        // Set up Market WebSocket for order book data (always needed)
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), market_ws_tx));
        let market_ws_clone = market_ws.clone();
        let market_ws_task = tokio::spawn(async move {
            market_ws_clone.run().await;
        });

        info!(
            "Bot initialized: {} token(s), {} market pair(s), {} strateg(ies)",
            token_ids.len(),
            market_registry.len(),
            strategy_router.strategy_names().len()
        );

        Self {
            config,
            kill_switch,
            order_book_state,
            ledger,
            market_registry,
            strategy_router,
            circuit_breaker,
            executor,
            order_tracker,
            market_ws_rx,
            market_ws_task,
            user_ws_rx,
            user_ws_task,
            last_log_time: HashMap::new(),
            message_counts: HashMap::new(),
            total_messages: 0,
            total_intents: 0,
            total_executions: 0,
            total_fills: 0,
            latency,
            heartbeat_count: 0,
            _external_prices: external_prices,
            alerts,
            circuit_was_open: false,
            controls,
        }
    }
}
