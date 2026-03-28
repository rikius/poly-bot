//! Axum HTTP + WebSocket server
//!
//! Serves:
//! - GET  /api/status  — health check (JSON)
//! - GET  /ws          — WebSocket endpoint; pushes WsSnapshot every 500ms
//! - GET  /*           — static files from frontend/dist (production)

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, info, warn};

use crate::config::{Config, OperatingMode};
use crate::ledger::{orders::OrderState, positions::Fill, Ledger};
use crate::state::OrderBookState;
use crate::websocket::types::Side;

use super::types::*;

/// Shared state passed to all route handlers
pub struct ApiState {
    pub ledger: Arc<Ledger>,
    pub order_book_state: Arc<OrderBookState>,
    pub config: Arc<Config>,
    pub start_time: Instant,
}

impl ApiState {
    pub fn new(
        ledger: Arc<Ledger>,
        order_book_state: Arc<OrderBookState>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            ledger,
            order_book_state,
            config,
            start_time: Instant::now(),
        }
    }

    fn build_snapshot(&self) -> WsSnapshot {
        let uptime = self.start_time.elapsed().as_secs();
        let mode = match self.config.mode {
            OperatingMode::Paper => "paper",
            OperatingMode::Live => "live",
        };

        let bot_status = BotStatus {
            mode: mode.to_string(),
            is_live: self.config.mode == OperatingMode::Live,
            uptime_secs: uptime,
        };

        // Cash
        let cash_snap = self.ledger.cash.snapshot();
        let cash = CashInfo {
            available: cash_snap.available.to_string(),
            reserved: cash_snap.reserved.to_string(),
            total: cash_snap.total.to_string(),
        };

        // Positions
        let positions: Vec<PositionInfo> = self
            .ledger
            .positions
            .all_positions()
            .into_iter()
            .map(|p| PositionInfo {
                token_id: p.token_id.clone(),
                shares: p.shares.to_string(),
                avg_cost: p.avg_cost.to_string(),
                cost_basis: p.cost_basis.to_string(),
                realized_pnl: p.realized_pnl.to_string(),
                unrealized_pnl: p.unrealized_pnl.to_string(),
                total_pnl: p.total_pnl().to_string(),
                total_fees: p.total_fees.to_string(),
                direction: if p.is_long() {
                    "long".to_string()
                } else {
                    "short".to_string()
                },
                notional: p.notional().to_string(),
            })
            .collect();

        // Active orders
        let active_orders: Vec<OrderInfo> = self
            .ledger
            .orders
            .active_orders()
            .into_iter()
            .map(|o| OrderInfo {
                local_id: o.local_id.to_string(),
                order_id: o.order_id.clone(),
                token_id: o.token_id.clone(),
                side: match o.side {
                    Side::Buy => "buy".to_string(),
                    Side::Sell => "sell".to_string(),
                },
                price: o.price.to_string(),
                original_size: o.original_size.to_string(),
                filled_size: o.filled_size.to_string(),
                remaining_size: o.remaining_size.to_string(),
                state: order_state_str(&o.state),
                strategy_id: o.strategy_id.clone(),
                created_at: o.created_at.to_rfc3339(),
            })
            .collect();

        // Order stats
        let stats = self.ledger.orders.stats();
        let order_stats = OrderStatsInfo {
            total_created: stats.total_created,
            total_filled: stats.total_filled,
            total_cancelled: stats.total_cancelled,
            total_rejected: stats.total_rejected,
            active_count: stats.active_count,
        };

        // Recent fills (last 50)
        let all_fills = self.ledger.fills();
        let recent_fills: Vec<FillInfo> = all_fills
            .iter()
            .rev()
            .take(50)
            .map(fill_to_info)
            .collect();

        // PnL
        let realized = self.ledger.positions.total_realized_pnl();
        let unrealized = self.ledger.positions.total_unrealized_pnl();
        let fees = self.ledger.positions.total_fees();
        let total = realized + unrealized;
        let net = total - fees;
        let pnl = PnlInfo {
            realized: realized.to_string(),
            unrealized: unrealized.to_string(),
            total: total.to_string(),
            total_fees: fees.to_string(),
            net: net.to_string(),
        };

        WsSnapshot {
            msg_type: "snapshot".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            bot_status,
            cash,
            positions,
            active_orders,
            order_stats,
            recent_fills,
            pnl,
        }
    }
}

fn order_state_str(s: &OrderState) -> String {
    match s {
        OrderState::CreatedLocal => "created",
        OrderState::Signed => "signed",
        OrderState::Submitted => "submitted",
        OrderState::Acked => "acked",
        OrderState::PartiallyFilled => "partial",
        OrderState::Filled => "filled",
        OrderState::CancelPending => "cancel_pending",
        OrderState::Cancelled => "cancelled",
        OrderState::Expired => "expired",
        OrderState::Rejected => "rejected",
        OrderState::Unknown => "unknown",
    }
    .to_string()
}

fn fill_to_info(f: &Fill) -> FillInfo {
    FillInfo {
        fill_id: f.fill_id.clone(),
        order_id: f.order_id.clone(),
        token_id: f.token_id.clone(),
        side: match f.side {
            Side::Buy => "buy".to_string(),
            Side::Sell => "sell".to_string(),
        },
        price: f.price.to_string(),
        size: f.size.to_string(),
        fee: f.fee.to_string(),
        notional: f.notional().to_string(),
        timestamp: f.timestamp.to_rfc3339(),
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

async fn status_handler(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    let uptime = state.start_time.elapsed().as_secs();
    let mode = match state.config.mode {
        OperatingMode::Paper => "paper",
        OperatingMode::Live => "live",
    };
    Json(json!({
        "status": "ok",
        "mode": mode,
        "uptime_secs": uptime,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<ApiState>) {
    debug!("WebSocket client connected");

    // Send initial snapshot immediately
    let snapshot = state.build_snapshot();
    match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if socket.send(Message::Text(json)).await.is_err() {
                return;
            }
        }
        Err(e) => warn!("Failed to serialize snapshot: {}", e),
    }

    let mut tick = interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let snapshot = state.build_snapshot();
                match serde_json::to_string(&snapshot) {
                    Ok(json) => {
                        if socket.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to serialize snapshot: {}", e);
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket client disconnected");
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

pub async fn run_api_server(state: Arc<ApiState>, port: u16) {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Try to serve the built frontend from frontend/dist if it exists
    let static_dir = std::path::Path::new("frontend/dist");

    let app = Router::new()
        .route("/api/status", get(status_handler))
        .route("/ws", get(ws_handler))
        .with_state(state)
        .layer(cors);

    // Serve static frontend assets if the build exists
    let app = if static_dir.exists() {
        app.nest_service(
            "/",
            tower_http::services::ServeDir::new(static_dir)
                .not_found_service(tower_http::services::ServeFile::new("frontend/dist/index.html")),
        )
    } else {
        app
    };

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("API server listening on http://0.0.0.0:{}", port);
    info!("  WebSocket: ws://0.0.0.0:{}/ws", port);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Failed to bind API server on port {}: {}", port, e);
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        warn!("API server error: {}", e);
    }
}
