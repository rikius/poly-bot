//! Market WebSocket — real-time order book updates
//!
//! Uses `polymarket-client-sdk` WS client which handles:
//! - TEXT "PING"/"PONG" frames (not binary, which Polymarket ignores)
//! - 15-second PONG timeout detection
//! - Exponential backoff reconnect with jitter

use crate::websocket::types::PriceLevel;
use futures_util::StreamExt as _;
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::U256;
use std::str::FromStr as _;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{error, info, warn};

/// Market WebSocket connection
pub struct MarketWebSocket {
    /// Token IDs to subscribe to (decimal string representation)
    token_ids: Vec<String>,
    /// Channel to send parsed messages
    message_tx: mpsc::UnboundedSender<MarketMessage>,
}

/// Message types from market WebSocket
#[derive(Debug, Clone)]
pub enum MarketMessage {
    /// Full book snapshot for a specific token
    BookSnapshot(BookUpdateMessage),
    /// Single level update — kept for bot.rs API compatibility;
    /// the SDK stream sends full snapshots so this variant is never produced.
    LevelUpdate(LevelUpdateMessage),
    /// Connection established
    Connected,
    /// Connection lost, reconnecting
    Reconnecting,
}

/// Full order book snapshot received from the WS stream
#[derive(Debug, Clone)]
pub struct BookUpdateMessage {
    /// Token ID (decimal string)
    pub token_id: String,
    /// Market condition ID (0x-prefixed hex)
    pub market: String,
    /// Asset ID — same as token_id for binary markets
    pub asset: String,
    /// Timestamp in milliseconds
    pub timestamp: Option<i64>,
    /// Book hash for deduplication
    pub hash: Option<String>,
    /// Bid levels (price descending)
    pub bids: Vec<PriceLevel>,
    /// Ask levels (price ascending)
    pub asks: Vec<PriceLevel>,
}

/// Incremental level update — kept for API compatibility with bot.rs
#[derive(Debug, Clone)]
pub struct LevelUpdateMessage {
    pub token_id: String,
    pub market: String,
    pub side: String,
    pub price: String,
    pub size: String,
    pub timestamp: Option<i64>,
    pub hash: Option<String>,
}

impl MarketWebSocket {
    /// Create a new market WebSocket wrapper
    pub fn new(
        token_ids: Vec<String>,
        message_tx: mpsc::UnboundedSender<MarketMessage>,
    ) -> Self {
        Self {
            token_ids,
            message_tx,
        }
    }

    /// Run the subscription loop, reconnecting automatically on disconnection.
    ///
    /// The SDK's `ConnectionManager` handles TEXT PING/PONG and PONG timeouts
    /// internally.  This outer loop restarts the subscription if the stream ever
    /// terminates cleanly.
    pub async fn run(self: Arc<Self>) {
        let asset_ids: Vec<U256> = self
            .token_ids
            .iter()
            .filter_map(|id| U256::from_str(id).ok())
            .collect();

        if asset_ids.is_empty() {
            error!("No valid token IDs to subscribe to — market WS will not start");
            return;
        }

        loop {
            let ws = WsClient::default();

            let stream = match ws.subscribe_orderbook(asset_ids.clone()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "Failed to create orderbook subscription, retrying in 5s");
                    let _ = self.message_tx.send(MarketMessage::Reconnecting);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let _ = self.message_tx.send(MarketMessage::Connected);
            info!(tokens = self.token_ids.len(), "Market WebSocket subscribed via SDK");

            let mut stream = Box::pin(stream);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(book) => {
                        let token_id = book.asset_id.to_string();
                        let market = format!("{:#x}", book.market);

                        let bids = book
                            .bids
                            .iter()
                            .map(|l| PriceLevel {
                                price: l.price.to_string(),
                                size: l.size.to_string(),
                            })
                            .collect();

                        let asks = book
                            .asks
                            .iter()
                            .map(|l| PriceLevel {
                                price: l.price.to_string(),
                                size: l.size.to_string(),
                            })
                            .collect();

                        let msg = BookUpdateMessage {
                            asset: token_id.clone(),
                            token_id,
                            market,
                            timestamp: Some(book.timestamp),
                            hash: book.hash,
                            bids,
                            asks,
                        };

                        if self.message_tx.send(MarketMessage::BookSnapshot(msg)).is_err() {
                            // Receiver dropped — bot is shutting down
                            return;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Market WebSocket stream error");
                        break;
                    }
                }
            }

            warn!("Market WebSocket stream ended, reconnecting in 1s...");
            let _ = self.message_tx.send(MarketMessage::Reconnecting);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
