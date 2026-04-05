//! Bot construction — authentication, strategy registration, WebSocket setup.

use super::auth::AuthComponents;
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
    ContrarianConfig, ContrarianStrategy, LateEntryConfig, LateEntryStrategy,
    MakerRebateArbStrategy, MakerRebateConfig, MarketPair, MarketPairRegistry, MathArbStrategy,
    StrategyRouter, TboConfig, TboTrendStrategy, TbtConfig, TbtDivergenceStrategy,
    TemporalArbConfig, TemporalArbStrategy,
};
use crate::websocket::{MarketWebSocket, UserWebSocket};
use polymarket_client_sdk::auth::Credentials;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

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

        // Optionally register MathArbStrategy (MATH_ARB_ENABLED, default true)
        if config.math_arb_enabled {
            let arb_config = if config.use_maker_mode {
                info!("Using MAKER mode for arb strategy (1% min edge, GTC orders, 0% fees)");
                crate::strategy::MathArbConfig::maker()
            } else {
                info!("Using TAKER mode for arb strategy (3% min edge, FOK orders)");
                crate::strategy::MathArbConfig::taker()
            };
            let arb_config = crate::strategy::MathArbConfig {
                max_bet_usd: config.max_bet_usd,
                ..arb_config
            };
            let math_arb = Arc::new(MathArbStrategy::with_config(market_registry.clone(), arb_config));
            if let Err(e) = strategy_router.register(math_arb) {
                warn!("Failed to register MathArbStrategy: {}", e);
            }
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
                max_bet_usd: config.max_bet_usd,
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

        // Optionally register ContrarianStrategy (mean-reversion / fade-the-crowd)
        if config.contrarian_enabled {
            info!(
                min_move_bps = config.contrarian_min_move_bps,
                lookback_secs = config.contrarian_lookback_secs,
                "Registering ContrarianStrategy"
            );
            let contrarian_config = ContrarianConfig {
                min_move_bps: config.contrarian_min_move_bps,
                lookback_secs: config.contrarian_lookback_secs,
                max_bet_usd: config.contrarian_max_bet_usd,
                max_hold_secs: config.contrarian_max_hold_secs,
                profit_target_pct: config.contrarian_profit_target_pct,
                stop_loss_pct: config.contrarian_stop_loss_pct,
                min_edge: config.contrarian_min_edge,
                high_fee_min_edge: config.contrarian_high_fee_min_edge,
                cooldown_ms: config.contrarian_cooldown_ms,
                ..ContrarianConfig::default()
            };
            let contrarian = Arc::new(ContrarianStrategy::with_config(
                market_registry.clone(),
                contrarian_config,
            ));
            if let Err(e) = strategy_router.register(contrarian) {
                warn!("Failed to register ContrarianStrategy: {}", e);
            }
        }

        // Optionally register TboTrendStrategy (trending breakout / momentum)
        if config.tbo_enabled {
            info!(
                lookback_ticks = config.tbo_lookback_ticks,
                breakout_threshold_bps = config.tbo_breakout_threshold_bps,
                min_confirm_ticks = config.tbo_min_confirm_ticks,
                "Registering TboTrendStrategy"
            );
            let tbo_config = TboConfig {
                lookback_ticks: config.tbo_lookback_ticks,
                breakout_threshold_bps: config.tbo_breakout_threshold_bps,
                min_confirm_ticks: config.tbo_min_confirm_ticks,
                max_bet_usd: config.tbo_max_bet_usd,
                max_hold_secs: config.tbo_max_hold_secs,
                profit_target_pct: config.tbo_profit_target_pct,
                stop_loss_pct: config.tbo_stop_loss_pct,
                min_ask_depth: config.tbo_min_ask_depth,
                cooldown_ms: config.tbo_cooldown_ms,
                ..TboConfig::default()
            };
            let tbo = Arc::new(TboTrendStrategy::with_config(
                market_registry.clone(),
                tbo_config,
            ));
            if let Err(e) = strategy_router.register(tbo) {
                warn!("Failed to register TboTrendStrategy: {}", e);
            }
        }

        // Optionally register TbtDivergenceStrategy (RSI divergence + late entry)
        if config.tbt_enabled {
            info!(
                lookback_ticks = config.tbt_lookback_ticks,
                rsi_period = config.tbt_rsi_period,
                divergence_min_gap_bps = config.tbt_divergence_min_gap_bps,
                pullback_bps = config.tbt_pullback_bps,
                "Registering TbtDivergenceStrategy"
            );
            let tbt_config = TbtConfig {
                lookback_ticks: config.tbt_lookback_ticks,
                rsi_period: config.tbt_rsi_period,
                divergence_min_gap_bps: config.tbt_divergence_min_gap_bps,
                pullback_bps: config.tbt_pullback_bps,
                signal_expiry_ticks: config.tbt_signal_expiry_ticks,
                max_bet_usd: config.tbt_max_bet_usd,
                max_hold_secs: config.tbt_max_hold_secs,
                profit_target_pct: config.tbt_profit_target_pct,
                stop_loss_pct: config.tbt_stop_loss_pct,
                min_ask_depth: config.tbt_min_ask_depth,
                cooldown_ms: config.tbt_cooldown_ms,
                ..TbtConfig::default()
            };
            let tbt = Arc::new(TbtDivergenceStrategy::with_config(
                market_registry.clone(),
                tbt_config,
            ));
            if let Err(e) = strategy_router.register(tbt) {
                warn!("Failed to register TbtDivergenceStrategy: {}", e);
            }
        }

        // Optionally register LateEntryStrategy (high-confidence near-resolution betting)
        if config.late_entry_enabled {
            info!(
                entry_window_pct = config.late_entry_window_pct,
                entry_window_secs = (config.market_interval_secs as f64 * config.late_entry_window_pct) as u64,
                min_entry_price = %config.late_entry_min_entry_price,
                "Registering LateEntryStrategy"
            );
            let late_config = LateEntryConfig {
                entry_window_pct: config.late_entry_window_pct,
                min_entry_price: config.late_entry_min_entry_price,
                market_interval_secs: config.market_interval_secs,
                max_bet_usd: config.late_entry_max_bet_usd,
                min_ask_depth: config.late_entry_min_ask_depth,
                cooldown_ms: config.late_entry_cooldown_ms,
                stop_loss_price: config.late_entry_stop_loss_price,
                ..LateEntryConfig::default()
            };
            let late = Arc::new(LateEntryStrategy::with_config(
                market_registry.clone(),
                late_config,
            ));
            if let Err(e) = strategy_router.register(late) {
                warn!("Failed to register LateEntryStrategy: {}", e);
            }
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
                let AuthComponents { clob_client, sync_client, signer, creds, portfolio_usdc, recent_fills } = auth
                    .expect("AuthComponents must be provided when PRIVATE_KEY is set");

                // Seed the ledger with the real CLOB balance when available,
                // otherwise fall back to INITIAL_CASH_USD from config.
                if let Some(balance) = portfolio_usdc {
                    ledger.sync_cash(balance);
                    info!(balance_usdc = %balance, "Ledger seeded from CLOB portfolio balance");
                } else {
                    info!(
                        fallback_usd = %config.initial_cash_usd,
                        "CLOB balance unavailable — ledger seeded from INITIAL_CASH_USD"
                    );
                }

                // Replay recent fills to restore positions after a restart.
                // process_fill deduplicates by fill_id so WS replay is harmless.
                let fill_count = recent_fills.len();
                for fill in recent_fills {
                    ledger.process_fill(fill);
                }
                if fill_count > 0 {
                    let pos_count = ledger.positions.count();
                    info!(fills = fill_count, positions = pos_count, "Positions restored from recent trade history");
                }

                let policy = Arc::new(DualPolicy::new().with_maker_offset(config.maker_price_offset));
                info!(
                    "Execution policy: DualPolicy (Taker=FOK/FAK, Maker=GTC offset={} cents)",
                    config.maker_price_offset
                );

                // Clone signer before the executor takes ownership.
                let sync_signer = Some(Arc::clone(&signer));

                let exec = Arc::new(OrderExecutor::new(
                    clob_client,
                    signer,
                    policy,
                    circuit_breaker.clone(),
                    Arc::clone(&latency),
                    alerts.clone(),
                ));

                // Spawn the background sync task on its own connection.
                let sync_ledger = Arc::clone(&ledger);
                let sync_registry = Arc::clone(&market_registry);
                let sync_rpc_url = config.polygon_rpc_url.clone();
                let sync_redeem_enabled = config.redeem_enabled;
                tokio::spawn(async move {
                    crate::sync::SyncTask::new(sync_client, sync_ledger, sync_registry, sync_signer, sync_rpc_url, sync_redeem_enabled)
                        .run()
                        .await;
                });

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
