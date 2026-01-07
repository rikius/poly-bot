//! Bot orchestration - connects all components together
//!
//! This module handles the main event loop and coordinates between:
//! - WebSocket market data
//! - Order book state
//! - Strategy execution (Phase 6+)
//! - Risk management (Phase 5+)

use crate::config::Config;
use crate::kill_switch::KillSwitch;
use crate::state::OrderBookState;
use crate::websocket::{MarketMessage, MarketWebSocket};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Main bot struct that orchestrates all components
pub struct Bot {
    /// Configuration
    config: Arc<Config>,
    /// Kill switch for emergency stop
    kill_switch: Arc<KillSwitch>,
    /// Order book state (shared across components)
    order_book_state: Arc<OrderBookState>,
    /// WebSocket message receiver
    ws_rx: mpsc::UnboundedReceiver<MarketMessage>,
    /// WebSocket task handle
    ws_task: JoinHandle<()>,
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

        // Set up WebSocket for market data
        let (ws_tx, ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), ws_tx));

        // Spawn WebSocket task
        let ws_clone = market_ws.clone();
        let ws_task = tokio::spawn(async move {
            ws_clone.run().await;
        });

        info!("Bot initialized with {} token(s)", token_ids.len());

        Self {
            config,
            kill_switch,
            order_book_state,
            ws_rx,
            ws_task,
        }
    }

    /// Run the main event loop
    pub async fn run(&mut self) {
        info!("Starting bot main loop...");

        let mut iteration = 0u64;
        let mut last_heartbeat = tokio::time::Instant::now();

        loop {
            // Check kill switch
            if self.kill_switch.is_killed() {
                warn!("Kill switch activated - shutting down");
                break;
            }

            // Process WebSocket messages
            self.process_ws_messages().await;

            // TODO: Phase 5+ - Check circuit breaker
            // TODO: Phase 6+ - Run strategy evaluation
            // TODO: Phase 4+ - Execute pending orders

            iteration += 1;

            // Heartbeat every 10 seconds
            if last_heartbeat.elapsed() >= tokio::time::Duration::from_secs(10) {
                info!(
                    "Heartbeat: {} markets tracked, {} iterations",
                    self.order_book_state.num_markets(),
                    iteration
                );
                last_heartbeat = tokio::time::Instant::now();
            }

            // Brief sleep to prevent busy loop
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        // Cleanup
        self.shutdown().await;
    }

    /// Process incoming WebSocket messages
    async fn process_ws_messages(&mut self) {
        while let Ok(msg) = self.ws_rx.try_recv() {
            match msg {
                MarketMessage::Connected => {
                    info!("WebSocket connected to market data stream");
                }
                MarketMessage::Reconnecting => {
                    warn!("WebSocket reconnecting...");
                }
                MarketMessage::BookUpdate(book_msg) => {
                    self.handle_book_update(book_msg).await;
                }
            }
        }
    }

    /// Handle a book update message
    async fn handle_book_update(&self, book_msg: crate::websocket::BookUpdateMessage) {
        // Update order book state
        self.order_book_state.update_book(
            book_msg.token_id.clone(),
            book_msg.market.clone(),
            book_msg.bids,
            book_msg.asks,
            book_msg.timestamp,
            book_msg.hash,
        );

        // Log significant updates (for debugging)
        if let (Some(bid), Some(ask)) = (
            self.order_book_state.best_bid(&book_msg.token_id),
            self.order_book_state.best_ask(&book_msg.token_id),
        ) {
            // Only log if spread is reasonable (< 50%)
            let spread_bps = self.order_book_state.spread_bps(&book_msg.token_id).unwrap_or(0);
            if spread_bps < 5000 {
                info!(
                    "Book: {} | Bid: ${:.4} | Ask: ${:.4} | Spread: {} bps",
                    &book_msg.token_id[..12],
                    bid,
                    ask,
                    spread_bps
                );
            }
        }

        // TODO: Phase 6+ - Notify strategies of book update
    }

    /// Graceful shutdown
    async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        // Abort WebSocket task
        self.ws_task.abort();

        // TODO: Phase 4+ - Cancel all open orders
        // TODO: Phase 4+ - Save ledger state

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
