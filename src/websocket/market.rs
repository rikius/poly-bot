//! Market WebSocket - real-time order book updates
//!
//! Connects to wss://ws-subscriptions-clob.polymarket.com/ws/market
//! Subscribes to order book updates for specified token IDs

use crate::api::types::{PriceLevel, TokenId};
use crate::constants::*;
use crate::error::{BotError, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Market WebSocket connection
pub struct MarketWebSocket {
    /// Token IDs to subscribe to
    token_ids: Vec<TokenId>,
    /// Channel to send parsed messages
    message_tx: mpsc::UnboundedSender<MarketMessage>,
}

/// Message types from market WebSocket
#[derive(Debug, Clone)]
pub enum MarketMessage {
    /// Book update for a specific token
    BookUpdate(BookUpdateMessage),
    /// Connection established
    Connected,
    /// Connection lost, reconnecting
    Reconnecting,
}

/// Order book update message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookUpdateMessage {
    /// Token ID
    pub token_id: TokenId,
    /// Market ID (condition ID)
    pub market: String,
    /// Asset ID (same as token_id for binary markets)
    pub asset: String,
    /// Timestamp of update
    #[serde(default)]
    pub timestamp: Option<i64>,
    /// Hash of book state (for deduplication)
    #[serde(default)]
    pub hash: Option<String>,
    /// Buy side levels (bids)
    pub bids: Vec<PriceLevel>,
    /// Sell side levels (asks)
    pub asks: Vec<PriceLevel>,
}

/// Subscription request message
#[derive(Debug, Serialize)]
struct SubscribeRequest {
    /// Must be "market"
    #[serde(rename = "type")]
    msg_type: String,
    /// List of token IDs to subscribe to
    assets_ids: Vec<TokenId>,
}

impl MarketWebSocket {
    /// Create a new market WebSocket connection
    pub fn new(
        token_ids: Vec<TokenId>,
        message_tx: mpsc::UnboundedSender<MarketMessage>,
    ) -> Self {
        Self {
            token_ids,
            message_tx,
        }
    }

    /// Start the WebSocket connection with automatic reconnection
    pub async fn run(self: Arc<Self>) {
        let mut reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
        const MAX_BACKOFF: Duration = Duration::from_secs(30);

        loop {
            info!("Connecting to market WebSocket: {}", MARKET_WS_URL);

            match self.connect_and_run().await {
                Ok(_) => {
                    warn!("Market WebSocket connection closed normally");
                    reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
                }
                Err(e) => {
                    error!("Market WebSocket error: {}", e);
                    let _ = self.message_tx.send(MarketMessage::Reconnecting);

                    // Exponential backoff
                    warn!("Reconnecting in {:?}", reconnect_delay);
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Connect and run the WebSocket (returns on disconnect)
    async fn connect_and_run(&self) -> Result<()> {
        // Connect to WebSocket
        let (ws_stream, _) = connect_async(MARKET_WS_URL)
            .await
            .map_err(|e| BotError::WebSocket(format!("Connection failed: {}", e)))?;

        info!("Market WebSocket connected");
        let _ = self.message_tx.send(MarketMessage::Connected);

        let (mut write, mut read) = ws_stream.split();

        // Send subscription message
        let subscribe_msg = SubscribeRequest {
            msg_type: "market".to_string(),
            assets_ids: self.token_ids.clone(),
        };

        let subscribe_json = serde_json::to_string(&subscribe_msg)
            .map_err(|e| BotError::Json(e.to_string()))?;

        write
            .send(Message::Text(subscribe_json))
            .await
            .map_err(|e| BotError::WebSocket(format!("Failed to subscribe: {}", e)))?;

        info!(
            "Subscribed to {} tokens: {:?}",
            self.token_ids.len(),
            self.token_ids
        );

        // Keepalive ping interval (5 seconds)
        let mut ping_interval = interval(Duration::from_secs(WEBSOCKET_PING_INTERVAL_SEC));

        loop {
            tokio::select! {
                // Receive messages
                Some(msg_result) = read.next() => {
                    match msg_result {
                        Ok(msg) => {
                            if let Err(e) = self.handle_message(msg).await {
                                error!("Failed to handle message: {}", e);
                            }
                        }
                        Err(e) => {
                            return Err(BotError::WebSocket(format!("Read error: {}", e)));
                        }
                    }
                }

                // Send periodic ping
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        return Err(BotError::WebSocket(format!("Ping failed: {}", e)));
                    }
                    debug!("Sent WebSocket ping");
                }

                else => {
                    return Err(BotError::WebSocket("Stream ended unexpectedly".to_string()));
                }
            }
        }
    }

    /// Handle incoming WebSocket message
    async fn handle_message(&self, msg: Message) -> Result<()> {
        match msg {
            Message::Text(text) => {
                // Parse as generic JSON to see what we have
                let json_value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| BotError::Json(format!("Invalid JSON: {}", e)))?;

                // Polymarket book messages have this structure:
                // { "event_type": "book", "asset_id": "...", "market": "...", "timestamp": ..., "hash": "...", "bids": [...], "asks": [...] }
                // OR sometimes: { "asset_id": "...", ...other fields... }

                if let Some(event_type) = json_value.get("event_type").and_then(|v| v.as_str()) {
                    if event_type == "book" {
                        // Extract fields from JSON
                        if let (Some(asset_id), Some(market), Some(bids_val), Some(asks_val)) = (
                            json_value.get("asset_id").and_then(|v| v.as_str()),
                            json_value.get("market").and_then(|v| v.as_str()),
                            json_value.get("bids"),
                            json_value.get("asks"),
                        ) {
                            // Parse bids and asks arrays
                            let bids: Vec<PriceLevel> = serde_json::from_value(bids_val.clone())
                                .unwrap_or_default();
                            let asks: Vec<PriceLevel> = serde_json::from_value(asks_val.clone())
                                .unwrap_or_default();

                            let timestamp = json_value.get("timestamp").and_then(|v| v.as_i64());
                            let hash = json_value.get("hash").and_then(|v| v.as_str()).map(String::from);

                            let book_update = BookUpdateMessage {
                                token_id: asset_id.to_string(),
                                market: market.to_string(),
                                asset: asset_id.to_string(),
                                timestamp,
                                hash,
                                bids,
                                asks,
                            };

                            let _ = self.message_tx.send(MarketMessage::BookUpdate(book_update));
                        }
                    }
                } else {
                    // If no event_type, log it for debugging
                    debug!("Unknown message format: {}", &text[..text.len().min(200)]);
                }
            }
            Message::Pong(_) => {
                debug!("Received WebSocket pong");
            }
            Message::Close(frame) => {
                warn!("WebSocket close frame received: {:?}", frame);
                return Err(BotError::WebSocket("Connection closed by server".to_string()));
            }
            _ => {
                debug!("Received other message type: {:?}", msg);
            }
        }

        Ok(())
    }
}
