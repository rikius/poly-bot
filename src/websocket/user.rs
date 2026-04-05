//! User WebSocket — trade notifications and fill events
//!
//! Uses `polymarket-client-sdk` WS client which handles:
//! - TEXT "PING"/"PONG" frames (binary pings are silently ignored by Polymarket)
//! - 15-second PONG timeout with automatic reconnect
//! - L2 auth headers signed with URL_SAFE base64 HMAC (not STANDARD)

use crate::websocket::types::Side;
use crate::ledger::Fill;
use chrono::{TimeZone, Utc};
use futures_util::StreamExt as _;
use polymarket_client_sdk::auth::Credentials;
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::clob::ws::types::response::WsMessage;
use polymarket_client_sdk::types::{Address, B256};
use rust_decimal::Decimal;
use std::str::FromStr as _;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{info, warn};

use crate::error::{BotError, Result};

/// User WebSocket connection for fill notifications
pub struct UserWebSocket {
    /// SDK credentials (URL_SAFE HMAC — no STANDARD base64 bug)
    credentials: Credentials,
    /// Wallet address
    address: Address,
    /// Market condition IDs to subscribe to
    condition_ids: Vec<B256>,
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
///
/// Fields are kept as strings for compatibility with `to_fill()`.
#[derive(Debug, Clone)]
pub struct TradeNotification {
    pub id: String,
    pub taker_order_id: String,
    pub market: String,
    pub asset_id: String,
    pub side: String,
    pub size: String,
    pub price: String,
    pub fee_rate_bps: String,
    pub status: String,
    pub timestamp: String,
    pub trader_side: String,
}

impl TradeNotification {
    /// Convert to Fill struct for ledger
    pub fn to_fill(&self) -> Result<Fill> {
        // trade.side is the TAKER's side (the aggressor who crossed the spread).
        // When this wallet was the MAKER, the correct side is the opposite.
        let taker_side = match self.side.to_uppercase().as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => return Err(BotError::Json(format!("Unknown side: {}", self.side))),
        };
        let side = if self.trader_side.to_uppercase() == "MAKER" {
            match taker_side {
                Side::Buy => Side::Sell,
                Side::Sell => Side::Buy,
            }
        } else {
            taker_side
        };

        let price = Decimal::from_str(&self.price)
            .map_err(|e| BotError::Json(format!("Invalid price: {}", e)))?;

        let size = Decimal::from_str(&self.size)
            .map_err(|e| BotError::Json(format!("Invalid size: {}", e)))?;

        let fee_bps = self.fee_rate_bps.parse::<i64>().unwrap_or(0);
        let notional = (price * size).round_dp(4);
        let fee = notional * Decimal::new(fee_bps, 4);

        let timestamp = self
            .timestamp
            .parse::<i64>()
            .ok()
            .and_then(|ts| {
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
            // expected_price and slippage_cost are resolved by Ledger::process_fill,
            // which looks up the originating TrackedOrder by order_id.
            expected_price: None,
            slippage_cost: Decimal::ZERO,
            timestamp,
        })
    }
}

/// Order update notification
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    pub order_id: String,
    pub status: String,
    pub timestamp: String,
}

impl UserWebSocket {
    /// Create a new user WebSocket connection.
    ///
    /// `condition_ids` — market condition IDs (as hex strings, may be empty).
    /// Invalid hex strings are silently skipped.
    pub fn new(
        credentials: Credentials,
        address: Address,
        condition_ids: Vec<String>,
        fill_tx: mpsc::UnboundedSender<UserMessage>,
    ) -> Self {
        let condition_ids: Vec<B256> = condition_ids
            .iter()
            .filter_map(|id| B256::from_str(id).ok())
            .collect();

        Self {
            credentials,
            address,
            condition_ids,
            fill_tx,
        }
    }

    /// Run the subscription loop.
    ///
    /// Subscribes to user events (trades + order updates) for all registered
    /// markets.  Reconnects automatically when the stream ends.
    pub async fn run(self: Arc<Self>) {
        if self.condition_ids.is_empty() {
            warn!("No valid condition IDs — user WS will not subscribe to any markets");
        }

        loop {
            let ws = match WsClient::default()
                .authenticate(self.credentials.clone(), self.address)
            {
                Ok(w) => w,
                Err(e) => {
                    warn!(error = %e, "Failed to authenticate user WS client, retrying in 10s");
                    let _ = self.fill_tx.send(UserMessage::Reconnecting);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };

            let stream = match ws.subscribe_user_events(self.condition_ids.clone()) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "Failed to subscribe to user events, retrying in 5s");
                    let _ = self.fill_tx.send(UserMessage::Reconnecting);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let _ = self.fill_tx.send(UserMessage::Connected);
            info!(markets = self.condition_ids.len(), "User WebSocket subscribed via SDK");

            let mut stream = Box::pin(stream);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(WsMessage::Trade(trade)) => {
                        let notification = TradeNotification {
                            id: trade.id,
                            taker_order_id: trade.taker_order_id.unwrap_or_default(),
                            market: format!("{:#x}", trade.market),
                            asset_id: trade.asset_id.to_string(),
                            side: trade.side.to_string(),
                            size: trade.size.to_string(),
                            price: trade.price.to_string(),
                            fee_rate_bps: trade
                                .fee_rate_bps
                                .map(|f| f.to_string())
                                .unwrap_or_default(),
                            status: format!("{:?}", trade.status),
                            timestamp: trade
                                .timestamp
                                .map(|t| t.to_string())
                                .unwrap_or_default(),
                            trader_side: trade
                                .trader_side
                                .map(|ts| format!("{ts:?}"))
                                .unwrap_or_default(),
                        };

                        if self.fill_tx.send(UserMessage::Trade(notification)).is_err() {
                            return;
                        }
                    }
                    Ok(WsMessage::Order(order)) => {
                        let update = OrderUpdate {
                            order_id: order.id,
                            status: order
                                .status
                                .map(|s| s.to_string())
                                .unwrap_or_default(),
                            timestamp: order
                                .timestamp
                                .map(|t| t.to_string())
                                .unwrap_or_default(),
                        };

                        if self.fill_tx.send(UserMessage::OrderUpdate(update)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {
                        // Ignore other message types on the user channel
                    }
                    Err(e) => {
                        warn!(error = %e, "User WebSocket stream error");
                        break;
                    }
                }
            }

            warn!("User WebSocket stream ended, reconnecting in 5s...");
            let _ = self.fill_tx.send(UserMessage::Reconnecting);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}
