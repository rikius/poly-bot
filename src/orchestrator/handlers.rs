//! Market and user WebSocket message handlers + periodic tick.

use super::Bot;
use crate::strategy::StrategyContext;
use crate::websocket::{MarketDiscovery, MarketMessage, MarketWebSocket, UserMessage};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

impl Bot {
    /// Handle a market WebSocket message.
    pub(crate) async fn handle_market_message(&mut self, msg: MarketMessage) {
        self.total_messages += 1;

        match msg {
            MarketMessage::Connected => {
                info!("WebSocket connected to market data stream");
            }
            MarketMessage::Reconnecting => {
                warn!("WebSocket reconnecting...");
                if let Some(ref alerts) = self.alerts {
                    alerts.send_ws_reconnect().await;
                }
            }
            MarketMessage::BookSnapshot(book_msg) => {
                self.handle_book_snapshot(book_msg).await;
            }
            MarketMessage::LevelUpdate(level_msg) => {
                self.handle_level_update(level_msg).await;
            }
        }
    }

    /// Handle a user WebSocket message (fills, order updates).
    pub(crate) async fn handle_user_message(&mut self, msg: UserMessage) {
        match msg {
            UserMessage::Connected => {
                info!("User WebSocket connected - receiving fill notifications");
            }
            UserMessage::Reconnecting => {
                warn!("User WebSocket reconnecting...");
            }
            UserMessage::Trade(trade) => {
                self.handle_trade_notification(trade).await;
            }
            UserMessage::OrderUpdate(update) => {
                self.handle_order_update(update).await;
            }
        }
    }

    /// Handle a trade/fill notification.
    async fn handle_trade_notification(
        &mut self,
        trade: crate::websocket::TradeNotification,
    ) {
        self.total_fills += 1;

        match trade.to_fill() {
            Ok(fill) => {
                info!(
                    "💰 Fill: {} {} {} @ ${} (fee: ${})",
                    format!("{:?}", fill.side),
                    fill.size,
                    &fill.token_id[..fill.token_id.len().min(12)],
                    fill.price,
                    fill.fee
                );

                self.ledger.process_fill(fill.clone());

                if let Some(remaining) = self.order_tracker.on_fill(&fill.order_id, fill.size) {
                    if remaining.is_zero() {
                        info!(
                            "Order {} fully filled",
                            &fill.order_id[..fill.order_id.len().min(12)]
                        );
                    } else {
                        debug!(
                            "Order {} partial fill, {} remaining",
                            &fill.order_id[..fill.order_id.len().min(12)],
                            remaining
                        );
                    }
                }
            }
            Err(e) => {
                error!("Failed to parse trade notification: {}", e);
            }
        }
    }

    /// Handle an order update (ack, cancel, etc.).
    async fn handle_order_update(&mut self, update: crate::websocket::OrderUpdate) {
        debug!(
            "Order update: {} -> {}",
            &update.order_id[..update.order_id.len().min(12)],
            update.status
        );

        match update.status.to_lowercase().as_str() {
            "cancelled" | "canceled" => {
                self.order_tracker.remove(&update.order_id);
                info!(
                    "Order {} cancelled",
                    &update.order_id[..update.order_id.len().min(12)]
                );
            }
            "expired" => {
                self.order_tracker.remove(&update.order_id);
                info!(
                    "Order {} expired",
                    &update.order_id[..update.order_id.len().min(12)]
                );
            }
            _ => {}
        }
    }

    /// Handle a full book snapshot message.
    pub(crate) async fn handle_book_snapshot(
        &mut self,
        book_msg: crate::websocket::BookUpdateMessage,
    ) {
        self.order_book_state.update_book(
            book_msg.token_id.clone(),
            book_msg.market.clone(),
            book_msg.bids,
            book_msg.asks,
            book_msg.timestamp,
            book_msg.hash,
        );
        self.route_book_update(&book_msg.market, &book_msg.token_id);
        self.log_book_state(&book_msg.token_id);
    }

    /// Handle an incremental level update.
    pub(crate) async fn handle_level_update(
        &mut self,
        level_msg: crate::websocket::LevelUpdateMessage,
    ) {
        self.order_book_state.update_level(
            &level_msg.token_id,
            level_msg.market.clone(),
            &level_msg.side,
            &level_msg.price,
            &level_msg.size,
            level_msg.timestamp,
            level_msg.hash,
        );
        self.route_book_update(&level_msg.market, &level_msg.token_id);
        self.log_book_state(&level_msg.token_id);
    }

    /// Route a book update to strategies and process any resulting intents.
    pub(crate) fn route_book_update(&mut self, market_id: &str, token_id: &str) {
        // Honour API pause — drop all intents without touching the circuit breaker
        if self.controls.is_paused() {
            return;
        }

        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);

        let t0 = std::time::Instant::now();
        let intents = self.strategy_router.on_book_update(
            &market_id.to_string(),
            &token_id.to_string(),
            &ctx,
        );
        self.latency
            .book_to_intent
            .record_us(t0.elapsed().as_micros() as u64);

        if !intents.is_empty() {
            self.process_intents(intents);
        }
    }

    /// Handle periodic tick (100ms) — runs strategy `on_tick` callbacks.
    pub(crate) async fn handle_tick(&mut self) {
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);
        let intents = self.strategy_router.on_tick(&ctx);
        if !intents.is_empty() {
            self.process_intents(intents);
        }
    }

    /// Refresh market subscriptions — discovers new 15-min markets and drops expired ones.
    ///
    /// Called every 5 minutes from the main run loop.  When the market set changes the
    /// `MarketWebSocket` task is restarted so the new token IDs are subscribed immediately.
    pub(crate) async fn refresh_markets(&mut self) {
        info!("Refreshing 15-min market subscriptions...");

        let discovery = MarketDiscovery::new();
        let discovered = match discovery.discover_crypto_15min().await {
            Ok(markets) => markets.into_iter().take(5).collect::<Vec<_>>(),
            Err(e) => {
                warn!(error = %e, "Market refresh discovery failed — keeping current subscriptions");
                return;
            }
        };

        let new_ids: HashSet<String> = discovered.iter().map(|d| d.condition_id.clone()).collect();
        let existing_ids: HashSet<String> = self.market_registry.all_condition_ids().into_iter().collect();

        let added_count = new_ids.difference(&existing_ids).count();
        let removed_count = existing_ids.difference(&new_ids).count();

        if added_count == 0 && removed_count == 0 {
            info!("Market refresh: no changes ({} markets active)", existing_ids.len());
            return;
        }

        // Register newly-discovered markets
        for dm in discovered.iter().filter(|d| !existing_ids.contains(&d.condition_id)) {
            info!(
                condition_id = %dm.condition_id,
                question = %dm.question,
                "New market registered"
            );
            self.market_registry.register(dm.to_market_pair());
        }

        // Unregister expired markets (condition IDs no longer in the fresh discovery)
        for condition_id in existing_ids.iter().filter(|id| !new_ids.contains(*id)) {
            info!(condition_id = %condition_id, "Expired market unregistered");
            self.market_registry.unregister(condition_id);
        }

        info!(
            added = added_count,
            removed = removed_count,
            total = self.market_registry.len(),
            "Market set updated — restarting WebSocket"
        );

        // Build the new full token list from the updated registry
        let token_ids: Vec<String> = self
            .market_registry
            .all_pairs()
            .iter()
            .flat_map(|p| [p.yes_token_id.clone(), p.no_token_id.clone()])
            .collect();

        // Abort the old subscription and spawn a new one with the updated token set
        self.market_ws_task.abort();
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), market_ws_tx));
        let market_ws_clone = market_ws.clone();
        self.market_ws_task = tokio::spawn(async move {
            market_ws_clone.run().await;
        });
        self.market_ws_rx = market_ws_rx;

        info!(token_count = token_ids.len(), "Market WebSocket restarted with updated token set");
    }

    /// Log current book state (rate limited — max once per second per token).
    pub(crate) fn log_book_state(&mut self, token_id: &str) {
        *self.message_counts.entry(token_id.to_string()).or_insert(0) += 1;

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
        if let (Some(bid), Some(ask)) = (
            self.order_book_state.best_bid(&token_id_string),
            self.order_book_state.best_ask(&token_id_string),
        ) {
            let spread_bps = self
                .order_book_state
                .spread_bps(&token_id_string)
                .unwrap_or(0);
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
                self.last_log_time.insert(token_id.to_string(), now);
            }
        }
    }
}
