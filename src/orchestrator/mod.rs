//! Bot orchestration — connects all components together
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

pub mod auth;
mod execution;
mod handlers;
mod heartbeat;
mod setup;
mod shutdown;

pub use auth::AuthComponents;

use crate::alerts::AlertSender;
use crate::api::ControlState;
use crate::config::Config;
use crate::execution::{OrderExecutor, OrderTracker};
use crate::feeds::ExternalPriceStore;
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::metrics::BotLatency;
use crate::risk::CircuitBreaker;
use crate::state::OrderBookState;
use crate::strategy::{MarketPairRegistry, StrategyRouter};
use crate::websocket::{MarketMessage, UserMessage};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tracing::{info, warn};

/// Main bot struct that orchestrates all components.
pub struct Bot {
    pub(crate) config: Arc<Config>,
    pub(crate) kill_switch: Arc<KillSwitch>,
    pub(crate) order_book_state: Arc<OrderBookState>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) market_registry: Arc<MarketPairRegistry>,
    pub(crate) strategy_router: Arc<StrategyRouter>,
    pub(crate) circuit_breaker: Arc<CircuitBreaker>,
    pub(crate) executor: Option<Arc<OrderExecutor>>,
    pub(crate) order_tracker: Arc<OrderTracker>,
    pub(crate) market_ws_rx: mpsc::UnboundedReceiver<MarketMessage>,
    pub(crate) market_ws_task: JoinHandle<()>,
    pub(crate) user_ws_rx: Option<mpsc::UnboundedReceiver<UserMessage>>,
    pub(crate) user_ws_task: Option<JoinHandle<()>>,
    pub(crate) last_log_time: HashMap<String, Instant>,
    pub(crate) message_counts: HashMap<String, u64>,
    pub(crate) total_messages: u64,
    pub(crate) total_intents: u64,
    pub(crate) total_executions: u64,
    pub(crate) total_fills: u64,
    pub(crate) latency: Arc<BotLatency>,
    pub(crate) heartbeat_count: u32,
    pub(crate) _external_prices: ExternalPriceStore,
    pub(crate) alerts: Option<Arc<AlertSender>>,
    pub(crate) circuit_was_open: bool,
    pub(crate) controls: Arc<ControlState>,
}

impl Bot {
    /// Expose shared state for the API server.
    ///
    /// Call this *after* `Bot::new` but *before* `Bot::run` to obtain
    /// Arc handles that the API server needs.
    pub fn shared_state(
        &self,
    ) -> (
        Arc<Ledger>,
        Arc<OrderBookState>,
        Arc<Config>,
        Arc<BotLatency>,
        Arc<ControlState>,
        Option<Arc<OrderExecutor>>,
        Arc<MarketPairRegistry>,
    ) {
        (
            Arc::clone(&self.ledger),
            Arc::clone(&self.order_book_state),
            Arc::clone(&self.config),
            Arc::clone(&self.latency),
            Arc::clone(&self.controls),
            self.executor.clone(),
            Arc::clone(&self.market_registry),
        )
    }

    /// Run the main event loop (event-driven architecture).
    ///
    /// Uses `tokio::select!` for zero-latency event handling:
    /// - Market WS: Processed instantly when received
    /// - User WS: Fill notifications processed instantly
    /// - Tick: Every 100ms for strategy periodic logic
    /// - Heartbeat: Every 10s for logging/monitoring
    /// - Kill signal: Async shutdown trigger
    pub async fn run(&mut self) {
        info!("Starting bot main loop (event-driven)...");

        let mut tick_interval = interval(Duration::from_millis(100));
        let mut heartbeat_interval = interval(Duration::from_secs(10));
        // Re-discover 15-min markets every 5 minutes so the bot picks up new
        // rounds automatically as old markets expire.
        let mut market_refresh_interval = interval(Duration::from_secs(300));
        market_refresh_interval.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                biased;

                Some(msg) = self.market_ws_rx.recv() => {
                    self.handle_market_message(msg).await;
                }

                Some(msg) = async {
                    match self.user_ws_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_user_message(msg).await;
                }

                _ = tick_interval.tick() => {
                    self.handle_tick().await;
                }

                _ = heartbeat_interval.tick() => {
                    self.log_heartbeat().await;
                }

                _ = market_refresh_interval.tick() => {
                    self.refresh_markets().await;
                }

                _ = self.kill_switch.wait_for_kill() => {
                    warn!("Kill signal received - shutting down");
                    break;
                }
            }
        }

        self.shutdown().await;
    }

    /// Get reference to order book state (for external access).
    pub fn order_book_state(&self) -> &Arc<OrderBookState> {
        &self.order_book_state
    }

    /// Get reference to config.
    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Get reference to ledger.
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Get reference to market registry.
    pub fn market_registry(&self) -> &Arc<MarketPairRegistry> {
        &self.market_registry
    }

    /// Get reference to strategy router.
    pub fn strategy_router(&self) -> &Arc<StrategyRouter> {
        &self.strategy_router
    }
}
