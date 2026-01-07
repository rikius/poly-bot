//! REST API endpoint wrappers for Polymarket CLOB
//!
//! Provides typed wrappers for all CLOB API endpoints:
//! - Public: /markets, /book, /price, /midpoint, /trades, /time
//! - Authenticated: /orders, /order (POST/DELETE), /notifications

use serde::Deserialize;
use tracing::{debug, warn};

use crate::api::client::ApiClient;
use crate::api::types::{
    ConditionId, OrderId, OrderRequest, OrderResponse, PriceLevel, Side, TokenId,
};
use crate::error::{BotError, Result};

// ============================================================================
// PUBLIC ENDPOINT RESPONSE TYPES
// ============================================================================

/// Market information from /markets endpoint
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketInfo {
    pub condition_id: ConditionId,
    #[serde(default)]
    pub question_id: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub market_slug: String,
    #[serde(default)]
    pub end_date_iso: String,
    #[serde(default)]
    pub game_start_time: Option<String>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub accepting_orders: bool,
    #[serde(default)]
    pub accepting_order_timestamp: Option<String>,
    #[serde(default)]
    pub minimum_order_size: String,
    #[serde(default)]
    pub minimum_tick_size: String,
    #[serde(default)]
    pub tokens: Vec<TokenInfo>,
}

/// Token information within a market
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub token_id: TokenId,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub winner: bool,
}

/// Order book from /book endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct BookResponse {
    pub market: ConditionId,
    pub asset_id: TokenId,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub bids: Vec<PriceLevel>,
    #[serde(default)]
    pub asks: Vec<PriceLevel>,
}

/// Price from /price endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct PriceResponse {
    pub price: String,
}

/// Midpoint from /midpoint endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct MidpointResponse {
    pub mid: String,
}

/// Server time from /time endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct TimeResponse {
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub timestamp: i64,
}

/// Fee rate from /fee-rate endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct FeeRateResponse {
    pub fee_rate_bps: u32,
}

// ============================================================================
// AUTHENTICATED ENDPOINT RESPONSE TYPES
// ============================================================================

/// Open order from /orders endpoint
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenOrder {
    pub id: OrderId,
    #[serde(default)]
    pub status: String,
    pub market: ConditionId,
    pub asset_id: TokenId,
    pub side: Side,
    #[serde(default)]
    pub original_size: String,
    #[serde(default)]
    pub size_matched: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub expiration: String,
    #[serde(rename = "type", default)]
    pub order_type: String,
    #[serde(default)]
    pub created_at: String,
}

/// Trade notification from /notifications or user WebSocket
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeNotification {
    pub id: String,
    pub taker_order_id: OrderId,
    pub market: ConditionId,
    pub asset_id: TokenId,
    pub side: Side,
    pub size: String,
    pub fee_rate_bps: String,
    pub price: String,
    pub status: String,
    #[serde(default)]
    pub match_time: String,
    #[serde(default)]
    pub transaction_hash: String,
    #[serde(default)]
    pub outcome: String,
}

// ============================================================================
// API ENDPOINT IMPLEMENTATIONS
// ============================================================================

impl ApiClient {
    // ========================================================================
    // PUBLIC ENDPOINTS (No Authentication Required)
    // ========================================================================

    /// Get server time - useful for connection warming and clock sync
    pub async fn get_time(&self) -> Result<TimeResponse> {
        let response = self.get("/time").await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get all markets
    pub async fn get_markets(&self) -> Result<Vec<MarketInfo>> {
        let response = self.get("/markets").await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get a specific market by condition ID
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketInfo> {
        let path = format!("/market/{}", condition_id);
        let response = self.get(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get order book for a token
    pub async fn get_book(&self, token_id: &str) -> Result<BookResponse> {
        let path = format!("/book?token_id={}", token_id);
        let response = self.get(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get current price for a token
    pub async fn get_price(&self, token_id: &str, side: Side) -> Result<PriceResponse> {
        let side_str = match side {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        };
        let path = format!("/price?token_id={}&side={}", token_id, side_str);
        let response = self.get(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get midpoint price for a token
    pub async fn get_midpoint(&self, token_id: &str) -> Result<MidpointResponse> {
        let path = format!("/midpoint?token_id={}", token_id);
        let response = self.get(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get fee rate for a token (identifies market type)
    /// - fee_rate_bps: 1000 = 15-min crypto market
    /// - fee_rate_bps: 0 = standard market
    pub async fn get_fee_rate(&self, token_id: &str) -> Result<FeeRateResponse> {
        let path = format!("/fee-rate?token_id={}", token_id);
        let response = self.get(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    // ========================================================================
    // AUTHENTICATED ENDPOINTS
    // ========================================================================

    /// Get open orders (authenticated)
    pub async fn get_orders(&self) -> Result<Vec<OpenOrder>> {
        let response = self.get_authenticated("/orders").await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        debug!(body = %body, "Orders response");
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get open orders filtered by market (authenticated)
    pub async fn get_orders_by_market(&self, market: &str) -> Result<Vec<OpenOrder>> {
        let path = format!("/orders?market={}", market);
        let response = self.get_authenticated(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Get open orders filtered by asset (authenticated)
    pub async fn get_orders_by_asset(&self, asset_id: &str) -> Result<Vec<OpenOrder>> {
        let path = format!("/orders?asset_id={}", asset_id);
        let response = self.get_authenticated(&path).await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }

    /// Place an order (authenticated)
    ///
    /// # Returns
    /// OrderResponse with order_id on success, or error message on failure
    pub async fn place_order(&self, request: &OrderRequest) -> Result<OrderResponse> {
        let body = serde_json::to_string(request)
            .map_err(|e| BotError::Json(format!("Failed to serialize order: {}", e)))?;

        debug!(body = %body, "Placing order");

        let response = self.post_authenticated("/order", &body).await?;
        let status = response.status();
        let response_body = response.text().await.map_err(|e| BotError::Http(e))?;

        debug!(status = %status, body = %response_body, "Order response");

        let order_response: OrderResponse =
            serde_json::from_str(&response_body).map_err(|e| BotError::Json(e.to_string()))?;

        // Check for API errors
        if !order_response.success && !order_response.error_msg.is_empty() {
            warn!(
                error = %order_response.error_msg,
                "Order placement failed"
            );
            return Err(BotError::Api {
                code: order_response.error_msg.clone(),
                message: order_response.error_msg,
            });
        }

        Ok(order_response)
    }

    /// Cancel an order by ID (authenticated)
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let path = format!("/order/{}", order_id);
        let response = self.delete_authenticated(&path).await?;
        let status = response.status();

        if status.is_success() {
            debug!(order_id = %order_id, "Order cancelled");
            Ok(())
        } else {
            let body = response.text().await.map_err(|e| BotError::Http(e))?;
            warn!(
                order_id = %order_id,
                status = %status,
                body = %body,
                "Failed to cancel order"
            );
            Err(BotError::Order(format!(
                "Failed to cancel order: {} - {}",
                status, body
            )))
        }
    }

    /// Cancel all open orders (authenticated)
    pub async fn cancel_all_orders(&self) -> Result<Vec<String>> {
        let orders = self.get_orders().await?;
        let mut cancelled = Vec::new();

        for order in orders {
            match self.cancel_order(&order.id).await {
                Ok(_) => cancelled.push(order.id),
                Err(e) => warn!(error = %e, order_id = %order.id, "Failed to cancel order"),
            }
        }

        Ok(cancelled)
    }

    /// Get trade notifications (authenticated)
    pub async fn get_notifications(&self) -> Result<Vec<TradeNotification>> {
        let response = self.get_authenticated("/notifications").await?;
        let body = response.text().await.map_err(|e| BotError::Http(e))?;
        serde_json::from_str(&body).map_err(|e| BotError::Json(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_info_deserialize() {
        let json = r#"{
            "conditionId": "0x1234",
            "question": "Test market?",
            "active": true,
            "closed": false,
            "acceptingOrders": true,
            "minimumOrderSize": "5",
            "minimumTickSize": "0.01",
            "tokens": [
                {"tokenId": "123", "outcome": "Yes", "price": "0.50"}
            ]
        }"#;

        let market: MarketInfo = serde_json::from_str(json).unwrap();
        assert_eq!(market.condition_id, "0x1234");
        assert_eq!(market.question, "Test market?");
        assert!(market.active);
        assert!(market.accepting_orders);
        assert_eq!(market.tokens.len(), 1);
    }

    #[test]
    fn test_order_response_deserialize() {
        let json = r#"{
            "errorMsg": "",
            "orderID": "0xabc123",
            "takingAmount": "1.5",
            "makingAmount": "0.5",
            "status": "matched",
            "transactionsHashes": ["0xdef456"],
            "success": true
        }"#;

        let response: OrderResponse = serde_json::from_str(json).unwrap();
        assert!(response.success);
        assert_eq!(response.order_id, "0xabc123");
        assert_eq!(response.transactions_hashes.len(), 1);
    }

    #[test]
    fn test_fee_rate_deserialize() {
        let json = r#"{"fee_rate_bps": 1000}"#;
        let response: FeeRateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.fee_rate_bps, 1000);
    }
}
