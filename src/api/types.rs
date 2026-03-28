//! Serializable API types for the frontend
//!
//! These are separate from the internal bot types to keep the API contract
//! stable and decouple serialization concerns from trading logic.

use serde::{Deserialize, Serialize};

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
    /// Cumulative slippage cost in USDC (positive = unfavourable)
    pub total_slippage_cost: String,
    /// Number of fills that built this position
    pub fill_count: u32,
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
    /// Actual fill price
    pub price: String,
    pub size: String,
    pub fee: String,
    /// Fee as basis points of notional (e.g. 1000 = 10%)
    pub fee_rate_bps: u32,
    pub notional: String,
    /// Limit price of the originating order (null when unknown)
    pub expected_price: Option<String>,
    /// Slippage cost in USDC: positive = unfavourable (paid more / received less)
    pub slippage_cost: String,
    /// Slippage in basis points relative to limit price (null when unknown)
    pub slippage_bps: Option<i64>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PnlInfo {
    pub realized: String,
    pub unrealized: String,
    pub total: String,
    pub total_fees: String,
    /// Cumulative slippage cost across all positions (positive = unfavourable)
    pub total_slippage_cost: String,
    /// Net P&L after fees and slippage
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

/// Latency percentiles for a single instrumented point.
#[derive(Debug, Clone, Serialize)]
pub struct LatencyPointInfo {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub count: u64,
}

/// Latency summary exposed over the API (rolling 60s window).
#[derive(Debug, Clone, Serialize)]
pub struct LatencyInfo {
    /// Strategy evaluation: book update → intents returned (µs)
    pub book_to_intent: LatencyPointInfo,
    /// Order submission: build+sign+post_order → response (µs)
    pub submit_to_ack: LatencyPointInfo,
}

/// Runtime-configurable controls broadcast in every snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlsInfo {
    /// Whether the bot is paused (no new intents generated)
    pub trading_paused: bool,
    pub max_bet_usd: String,
    pub max_position_per_market_usd: String,
    pub max_total_exposure_usd: String,
    pub max_daily_loss_usd: String,
    pub max_open_orders: u32,
    pub use_maker_mode: bool,
    pub temporal_arb_enabled: bool,
    pub temporal_arb_threshold_bps: i64,
    pub temporal_arb_sensitivity_bps: i64,
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
    pub latency: LatencyInfo,
    pub controls: ControlsInfo,
}
