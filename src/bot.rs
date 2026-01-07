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
use crate::state::OrderBookState;
use crate::websocket::{MarketMessage, MarketWebSocket};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tracing::{info, warn};

/// Main bot struct that orchestrates all components
pub struct Bot {
    /// Configuration
    config: Arc<Config>,
    /// Kill switch for emergency stop
    kill_switch: Arc<KillSwitch>,
    /// Order book state (shared across components)
    order_book_state: Arc<OrderBookState>,
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
}

impl Bot {
    /// Create a new bot instance
    pub async fn new(
        config: Config,
        kill_switch: Arc<KillSwitch>,
        token_ids: Vec<String>,
    ) -> Self {
        let config = Arc::new(config);
        let order_book_state = Arc::new(OrderBookState::new());

        // Set up Market WebSocket for order book data
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), market_ws_tx));

        // Spawn Market WebSocket task
        let market_ws_clone = market_ws.clone();
        let market_ws_task = tokio::spawn(async move {
            market_ws_clone.run().await;
        });

        info!("Bot initialized with {} token(s)", token_ids.len());

        Self {
            config,
            kill_switch,
            order_book_state,
            market_ws_rx,
            market_ws_task,
            last_log_time: HashMap::new(),
            message_counts: HashMap::new(),
            total_messages: 0,
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
        // TODO: Check circuit breaker status
        // TODO: Run strategy on_tick() callbacks
        // TODO: Process pending order actions
        // TODO: Check for stale orders to cancel
    }

    /// Log heartbeat with current stats
    fn log_heartbeat(&mut self) {
        info!(
            "Heartbeat: {} markets | {} total msgs | {:.1} msg/s",
            self.order_book_state.num_markets(),
            self.total_messages,
            self.total_messages as f64 / 10.0  // msgs per second (over 10s window)
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

        self.log_book_state(&level_msg.token_id);
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
                info!(
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

        // TODO: Notify strategies of book update
    }

    /// Graceful shutdown
    async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        // Abort Market WebSocket task
        self.market_ws_task.abort();

        // TODO: Cancel all open orders
        // TODO: Save ledger state

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
}
