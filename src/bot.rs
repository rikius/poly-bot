//! Bot orchestration - connects all components together
//!
//! This module handles the main event loop and coordinates between:
//! - WebSocket market data (order book updates)
//! - WebSocket user data (fill notifications)
//! - Strategy execution
//! - Risk management
//!
//! ## Event-Driven Architecture
//!
//! The bot uses `tokio::select!` for zero-latency event handling:
//! - Market WS messages processed instantly (<1ms)
//! - User WS fills processed instantly
//! - Periodic tick for strategy logic (100ms)
//! - Heartbeat for logging (10s)
//! - Async kill signal for shutdown

use crate::config::Config;
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::state::OrderBookState;
use crate::strategy::{
    MarketPair, MarketPairRegistry, MathArbStrategy, OrderIntent, StrategyContext, StrategyRouter,
};
use crate::websocket::{MarketMessage, MarketWebSocket};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

/// Main bot struct that orchestrates all components
pub struct Bot {
    /// Configuration
    config: Arc<Config>,
    /// Kill switch for emergency stop
    kill_switch: Arc<KillSwitch>,
    /// Order book state (shared across components)
    order_book_state: Arc<OrderBookState>,
    /// Ledger for tracking orders, positions, cash
    ledger: Arc<Ledger>,
    /// Market pair registry (YES/NO token mappings)
    market_registry: Arc<MarketPairRegistry>,
    /// Strategy router
    strategy_router: Arc<StrategyRouter>,
    /// Market WebSocket message receiver
    market_ws_rx: mpsc::UnboundedReceiver<MarketMessage>,
    /// Market WebSocket task handle
    market_ws_task: JoinHandle<()>,
    /// Last log time per token (for rate limiting)
    last_log_time: HashMap<String, Instant>,
    /// Message counter per token
    message_counts: HashMap<String, u64>,
    /// Total messages processed
    total_messages: u64,
    /// Total order intents generated
    total_intents: u64,
}

impl Bot {
    /// Create a new bot instance
    ///
    /// # Arguments
    /// * `config` - Bot configuration
    /// * `kill_switch` - Kill switch for emergency stop
    /// * `token_ids` - Token IDs to subscribe to (alternating YES/NO pairs)
    /// * `market_pairs` - Market pair definitions for arb detection
    pub async fn new(
        config: Config,
        kill_switch: Arc<KillSwitch>,
        token_ids: Vec<String>,
        market_pairs: Vec<MarketPair>,
    ) -> Self {
        let config = Arc::new(config);
        let order_book_state = Arc::new(OrderBookState::new());
        let ledger = Arc::new(Ledger::new(config.max_bet_usd));

        // Set up market pair registry
        let market_registry = Arc::new(MarketPairRegistry::new());
        for pair in market_pairs {
            market_registry.register(pair);
        }

        // Set up strategy router
        let strategy_router = Arc::new(StrategyRouter::new());

        // Register MathArbStrategy
        let math_arb = Arc::new(MathArbStrategy::new(market_registry.clone()));
        if let Err(e) = strategy_router.register(math_arb) {
            warn!("Failed to register MathArbStrategy: {}", e);
        }

        // Set up Market WebSocket for order book data
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), market_ws_tx));

        // Spawn Market WebSocket task
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
            market_ws_rx,
            market_ws_task,
            last_log_time: HashMap::new(),
            message_counts: HashMap::new(),
            total_messages: 0,
            total_intents: 0,
        }
    }

    /// Run the main event loop (event-driven architecture)
    ///
    /// Uses `tokio::select!` for zero-latency event handling:
    /// - Market WS: Processed instantly when received
    /// - Tick: Every 100ms for strategy periodic logic
    /// - Heartbeat: Every 10s for logging/monitoring
    /// - Kill signal: Async shutdown trigger
    pub async fn run(&mut self) {
        info!("Starting bot main loop (event-driven)...");

        // Periodic tick for strategy logic (100ms)
        let mut tick_interval = interval(Duration::from_millis(100));
        
        // Heartbeat for logging (10s)
        let mut heartbeat_interval = interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                // Bias toward market data - process first if multiple ready
                biased;

                // Market WebSocket messages - highest priority, zero latency
                Some(msg) = self.market_ws_rx.recv() => {
                    self.handle_market_message(msg).await;
                }

                // Strategy tick - 100ms periodic
                _ = tick_interval.tick() => {
                    self.handle_tick().await;
                }

                // Heartbeat - 10s periodic logging
                _ = heartbeat_interval.tick() => {
                    self.log_heartbeat();
                }

                // Kill signal - graceful shutdown
                _ = self.kill_switch.wait_for_kill() => {
                    warn!("Kill signal received - shutting down");
                    break;
                }
            }
        }

        // Cleanup
        self.shutdown().await;
    }

    /// Handle a market WebSocket message
    async fn handle_market_message(&mut self, msg: MarketMessage) {
        self.total_messages += 1;

        match msg {
            MarketMessage::Connected => {
                info!("WebSocket connected to market data stream");
            }
            MarketMessage::Reconnecting => {
                warn!("WebSocket reconnecting...");
            }
            MarketMessage::BookSnapshot(book_msg) => {
                self.handle_book_snapshot(book_msg).await;
            }
            MarketMessage::LevelUpdate(level_msg) => {
                self.handle_level_update(level_msg).await;
            }
        }
    }

    /// Handle periodic tick (100ms)
    async fn handle_tick(&mut self) {
        // Create strategy context
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);

        // Run strategy on_tick() callbacks
        let intents = self.strategy_router.on_tick(&ctx);

        // Process any generated intents
        if !intents.is_empty() {
            self.process_intents(intents);
        }
    }

    /// Log heartbeat with current stats
    fn log_heartbeat(&mut self) {
        info!(
            "Heartbeat: {} markets | {} msgs | {:.1} msg/s | {} intents",
            self.order_book_state.num_markets(),
            self.total_messages,
            self.total_messages as f64 / 10.0,  // msgs per second (over 10s window)
            self.total_intents
        );
        // Reset counter for next interval
        self.total_messages = 0;
    }

    /// Handle a full book snapshot message
    async fn handle_book_snapshot(&mut self, book_msg: crate::websocket::BookUpdateMessage) {
        // Full book replacement
        self.order_book_state.update_book(
            book_msg.token_id.clone(),
            book_msg.market.clone(),
            book_msg.bids,
            book_msg.asks,
            book_msg.timestamp,
            book_msg.hash,
        );

        // Route to strategies
        self.route_book_update(&book_msg.market, &book_msg.token_id);

        self.log_book_state(&book_msg.token_id);
    }

    /// Handle an incremental level update
    async fn handle_level_update(&mut self, level_msg: crate::websocket::LevelUpdateMessage) {
        // Update single price level
        self.order_book_state.update_level(
            &level_msg.token_id,
            level_msg.market.clone(),
            &level_msg.side,
            &level_msg.price,
            &level_msg.size,
            level_msg.timestamp,
            level_msg.hash,
        );

        // Route to strategies
        self.route_book_update(&level_msg.market, &level_msg.token_id);

        self.log_book_state(&level_msg.token_id);
    }

    /// Route a book update to strategies and process intents
    fn route_book_update(&mut self, market_id: &str, token_id: &str) {
        // Create strategy context
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);

        // Route to strategies
        let intents = self.strategy_router.on_book_update(
            &market_id.to_string(),
            &token_id.to_string(),
            &ctx,
        );

        // Process any generated intents
        if !intents.is_empty() {
            self.process_intents(intents);
        }
    }

    /// Process order intents from strategies
    fn process_intents(&mut self, intents: Vec<OrderIntent>) {
        self.total_intents += intents.len() as u64;

        for intent in &intents {
            info!(
                "📝 Intent: {} {} {} @ ${:.4} x {} [{}]",
                intent.strategy_name,
                format!("{:?}", intent.side),
                &intent.token_id[..intent.token_id.len().min(12)],
                intent.price,
                intent.size,
                intent.reason
            );
        }

        // TODO: In Phase 7 full integration:
        // 1. Check circuit breaker
        // 2. Apply execution policy (TakerPolicy/MakerPolicy)
        // 3. Sign and submit orders via executor
        // 4. Track in ledger
        //
        // For now, we just log the intents (paper trading mode)
        debug!(
            "Generated {} intent(s) - paper trading mode, not executing",
            intents.len()
        );
    }

    /// Log current book state (rate limited - max 1 per second per token)
    fn log_book_state(&mut self, token_id: &str) {
        // Increment message count
        *self.message_counts.entry(token_id.to_string()).or_insert(0) += 1;

        // Rate limit: only log once per second per token
        let now = Instant::now();
        let should_log = self
            .last_log_time
            .get(token_id)
            .map(|last| now.duration_since(*last).as_secs() >= 1)
            .unwrap_or(true);

        if !should_log {
            return;
        }

        let token_id_string = token_id.to_string();

        // Log significant updates (for debugging)
        if let (Some(bid), Some(ask)) = (
            self.order_book_state.best_bid(&token_id_string),
            self.order_book_state.best_ask(&token_id_string),
        ) {
            // Only log if spread is reasonable (< 50%)
            let spread_bps = self.order_book_state.spread_bps(&token_id_string).unwrap_or(0);
            if spread_bps < 5000 {
                let msg_count = self.message_counts.get(token_id).copied().unwrap_or(0);
                debug!(
                    "Book: {} | Bid: ${:.4} | Ask: ${:.4} | Spread: {} bps | msgs: {}",
                    &token_id[..token_id.len().min(12)],
                    bid,
                    ask,
                    spread_bps,
                    msg_count
                );
                // Update last log time
                self.last_log_time.insert(token_id.to_string(), now);
            }
        }
    }

    /// Graceful shutdown
    async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        // Get shutdown intents from strategies
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);
        let shutdown_intents = self.strategy_router.on_shutdown(&ctx);
        if !shutdown_intents.is_empty() {
            info!("Processing {} shutdown intent(s)", shutdown_intents.len());
            self.process_intents(shutdown_intents);
        }

        // Abort Market WebSocket task
        self.market_ws_task.abort();

        // Log final stats
        info!(
            "Final stats: {} total intents generated",
            self.total_intents
        );

        info!("Bot shutdown complete");
    }

    /// Get reference to order book state (for external access)
    pub fn order_book_state(&self) -> &Arc<OrderBookState> {
        &self.order_book_state
    }

    /// Get reference to config
    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Get reference to ledger
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Get reference to market registry
    pub fn market_registry(&self) -> &Arc<MarketPairRegistry> {
        &self.market_registry
    }

    /// Get reference to strategy router
    pub fn strategy_router(&self) -> &Arc<StrategyRouter> {
        &self.strategy_router
    }
}
