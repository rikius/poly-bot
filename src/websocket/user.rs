//! User WebSocket - trade notifications and fills
//!
//! Connects to wss://ws-subscriptions-clob.polymarket.com/ws/user
//! Receives fill notifications for authenticated user's orders.

use crate::api::types::Side;
use crate::constants::*;
use crate::error::{BotError, Result};
use crate::ledger::Fill;
use chrono::{TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// User WebSocket connection for fill notifications
pub struct UserWebSocket {
    /// API key for authentication
    api_key: String,
    /// API secret
    secret: String,
    /// API passphrase
    passphrase: String,
    /// Channel to send fill notifications
    fill_tx: mpsc::UnboundedSender<UserMessage>,
}

/// Message types from user WebSocket
#[derive(Debug, Clone)]
pub enum UserMessage {
    /// Connection established
    Connected,
    /// Connection lost, reconnecting
    Reconnecting,
    /// Trade/fill notification
    Trade(TradeNotification),
    /// Order update (ack, cancel, etc.)
    OrderUpdate(OrderUpdate),
}

/// Trade notification from WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeNotification {
    /// Trade ID
    #[serde(default)]
    pub id: String,
    /// Order ID that was filled
    #[serde(default)]
    pub taker_order_id: String,
    /// Market/condition ID
    #[serde(default)]
    pub market: String,
    /// Asset/token ID
    #[serde(default)]
    pub asset_id: String,
    /// Side (BUY/SELL)
    #[serde(default)]
    pub side: String,
    /// Size filled
    #[serde(default)]
    pub size: String,
    /// Price of fill
    #[serde(default)]
    pub price: String,
    /// Fee rate in bps
    #[serde(default)]
    pub fee_rate_bps: String,
    /// Status (MATCHED, etc.)
    #[serde(default)]
    pub status: String,
    /// Timestamp
    #[serde(default)]
    pub timestamp: String,
    /// Whether we are taker or maker
    #[serde(default)]
    pub trader_side: String,
}

impl TradeNotification {
    /// Convert to Fill struct for ledger
    pub fn to_fill(&self) -> Result<Fill> {
        let side = match self.side.to_uppercase().as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => return Err(BotError::Json(format!("Unknown side: {}", self.side))),
        };

        let price = Decimal::from_str(&self.price)
            .map_err(|e| BotError::Json(format!("Invalid price: {}", e)))?;

        let size = Decimal::from_str(&self.size)
            .map_err(|e| BotError::Json(format!("Invalid size: {}", e)))?;

        // Calculate fee from fee_rate_bps
        let fee_bps = self
            .fee_rate_bps
            .parse::<i64>()
            .unwrap_or(0);
        let fee = price * size * Decimal::new(fee_bps, 4);

        // Parse timestamp
        let timestamp = self
            .timestamp
            .parse::<i64>()
            .ok()
            .and_then(|ts| {
                // Could be seconds or milliseconds
                if ts > 1_000_000_000_000 {
                    Utc.timestamp_millis_opt(ts).single()
                } else {
                    Utc.timestamp_opt(ts, 0).single()
                }
            })
            .unwrap_or_else(Utc::now);

        Ok(Fill {
            fill_id: self.id.clone(),
            order_id: self.taker_order_id.clone(),
            token_id: self.asset_id.clone(),
            side,
            price,
            size,
            fee,
            timestamp,
        })
    }
}

/// Order update notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdate {
    /// Order ID
    #[serde(default)]
    pub order_id: String,
    /// New status
    #[serde(default)]
    pub status: String,
    /// Timestamp
    #[serde(default)]
    pub timestamp: String,
}

/// Authentication message for user WebSocket
#[derive(Debug, Serialize)]
struct AuthMessage {
    auth: AuthPayload,
    markets: Vec<String>,
    #[serde(rename = "type")]
    msg_type: String,
}

#[derive(Debug, Serialize)]
struct AuthPayload {
    #[serde(rename = "apiKey")]
    api_key: String,
    secret: String,
    passphrase: String,
}

impl UserWebSocket {
    /// Create a new user WebSocket connection
    pub fn new(
        api_key: String,
        secret: String,
        passphrase: String,
        fill_tx: mpsc::UnboundedSender<UserMessage>,
    ) -> Self {
        Self {
            api_key,
            secret,
            passphrase,
            fill_tx,
        }
    }

    /// Start the WebSocket connection with automatic reconnection
    pub async fn run(self: Arc<Self>) {
        let mut reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
        const MAX_BACKOFF: Duration = Duration::from_secs(30);

        loop {
            info!("Connecting to user WebSocket: {}", USER_WS_URL);

            match self.connect_and_run().await {
                Ok(_) => {
                    warn!("User WebSocket connection closed normally");
                    reconnect_delay = Duration::from_millis(WEBSOCKET_RECONNECT_DELAY_MS);
                }
                Err(e) => {
                    error!("User WebSocket error: {}", e);
                    let _ = self.fill_tx.send(UserMessage::Reconnecting);

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
        let (ws_stream, _) = connect_async(USER_WS_URL)
            .await
            .map_err(|e| BotError::WebSocket(format!("Connection failed: {}", e)))?;

        info!("User WebSocket connected");
        let _ = self.fill_tx.send(UserMessage::Connected);

        let (mut write, mut read) = ws_stream.split();

        // Send authentication message
        let auth_msg = AuthMessage {
            auth: AuthPayload {
                api_key: self.api_key.clone(),
                secret: self.secret.clone(),
                passphrase: self.passphrase.clone(),
            },
            markets: vec![], // Subscribe to all markets
            msg_type: "user".to_string(),
        };

        let auth_json = serde_json::to_string(&auth_msg)
            .map_err(|e| BotError::Json(e.to_string()))?;

        write
            .send(Message::Text(auth_json))
            .await
            .map_err(|e| BotError::WebSocket(format!("Failed to authenticate: {}", e)))?;

        info!("User WebSocket authenticated");

        // Keepalive ping interval
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
                    debug!("Sent user WebSocket ping");
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

                // Check event_type or type field
                let event_type = json_value
                    .get("event_type")
                    .or_else(|| json_value.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match event_type.to_lowercase().as_str() {
                    "trade" => {
                        // Parse trade notification
                        let trade: TradeNotification = serde_json::from_value(json_value)
                            .map_err(|e| BotError::Json(format!("Failed to parse trade: {}", e)))?;

                        info!(
                            "Trade notification: {} {} {} @ {}",
                            trade.side, trade.size, trade.asset_id, trade.price
                        );

                        let _ = self.fill_tx.send(UserMessage::Trade(trade));
                    }
                    "order" | "order_update" => {
                        // Parse order update
                        let update: OrderUpdate = serde_json::from_value(json_value)
                            .map_err(|e| BotError::Json(format!("Failed to parse order update: {}", e)))?;

                        debug!("Order update: {} -> {}", update.order_id, update.status);

                        let _ = self.fill_tx.send(UserMessage::OrderUpdate(update));
                    }
                    "" => {
                        // Could be auth response or other message
                        debug!("User WS message: {}", &text[..text.len().min(200)]);
                    }
                    _ => {
                        debug!("Unknown user message type: {}", event_type);
                    }
                }
            }
            Message::Pong(_) => {
                debug!("Received user WebSocket pong");
            }
            Message::Close(frame) => {
                warn!("User WebSocket close frame received: {:?}", frame);
                return Err(BotError::WebSocket("Connection closed by server".to_string()));
            }
            _ => {
                debug!("Received other message type: {:?}", msg);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_trade_notification_to_fill() {
        let trade = TradeNotification {
            id: "trade123".to_string(),
            taker_order_id: "order456".to_string(),
            market: "market789".to_string(),
            asset_id: "token_abc".to_string(),
            side: "BUY".to_string(),
            size: "100".to_string(),
            price: "0.55".to_string(),
            fee_rate_bps: "50".to_string(), // 0.5%
            status: "MATCHED".to_string(),
            timestamp: "1704067200000".to_string(), // 2024-01-01
            trader_side: "TAKER".to_string(),
        };

        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.fill_id, "trade123");
        assert_eq!(fill.order_id, "order456");
        assert_eq!(fill.token_id, "token_abc");
        assert_eq!(fill.side, Side::Buy);
        assert_eq!(fill.price, dec!(0.55));
        assert_eq!(fill.size, dec!(100));
        // Fee = 0.55 * 100 * 0.005 = 0.275
        assert_eq!(fill.fee, dec!(0.275));
    }

    #[test]
    fn test_trade_notification_sell() {
        let trade = TradeNotification {
            id: "trade_sell".to_string(),
            taker_order_id: "order_sell".to_string(),
            market: "market".to_string(),
            asset_id: "token".to_string(),
            side: "SELL".to_string(),
            size: "50".to_string(),
            price: "0.80".to_string(),
            fee_rate_bps: "0".to_string(),
            status: "MATCHED".to_string(),
            timestamp: "1704067200".to_string(),
            trader_side: "MAKER".to_string(),
        };

        let fill = trade.to_fill().unwrap();
        assert_eq!(fill.side, Side::Sell);
        assert_eq!(fill.size, dec!(50));
        assert_eq!(fill.price, dec!(0.80));
        assert_eq!(fill.fee, dec!(0));
    }
}
