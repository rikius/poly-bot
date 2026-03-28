//! Serializable API types for the frontend
//!
//! These are separate from the internal bot types to keep the API contract
//! stable and decouple serialization concerns from trading logic.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BotStatus {
    pub mode: String,
    pub is_live: bool,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashInfo {
    pub available: String,
    pub reserved: String,
    pub total: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionInfo {
    pub token_id: String,
    pub shares: String,
    pub avg_cost: String,
    pub cost_basis: String,
    pub realized_pnl: String,
    pub unrealized_pnl: String,
    pub total_pnl: String,
    pub total_fees: String,
    pub direction: String,
    pub notional: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderInfo {
    pub local_id: String,
    pub order_id: Option<String>,
    pub token_id: String,
    pub side: String,
    pub price: String,
    pub original_size: String,
    pub filled_size: String,
    pub remaining_size: String,
    pub state: String,
    pub strategy_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FillInfo {
    pub fill_id: String,
    pub order_id: String,
    pub token_id: String,
    pub side: String,
    pub price: String,
    pub size: String,
    pub fee: String,
    pub notional: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PnlInfo {
    pub realized: String,
    pub unrealized: String,
    pub total: String,
    pub total_fees: String,
    pub net: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderStatsInfo {
    pub total_created: u64,
    pub total_filled: u64,
    pub total_cancelled: u64,
    pub total_rejected: u64,
    pub active_count: usize,
}

/// Full snapshot broadcast over WebSocket every 500ms
#[derive(Debug, Clone, Serialize)]
pub struct WsSnapshot {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub timestamp: String,
    pub bot_status: BotStatus,
    pub cash: CashInfo,
    pub positions: Vec<PositionInfo>,
    pub active_orders: Vec<OrderInfo>,
    pub order_stats: OrderStatsInfo,
    pub recent_fills: Vec<FillInfo>,
    pub pnl: PnlInfo,
}
