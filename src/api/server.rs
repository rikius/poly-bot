//! Axum HTTP + WebSocket server
//!
//! Serves:
//! - GET  /api/status          — health check (JSON)
//! - POST /api/bot/pause       — pause strategy execution
//! - POST /api/bot/resume      — resume strategy execution
//! - POST /api/orders/cancel-all — cancel all live orders
//! - PATCH /api/config         — update runtime strategy config
//! - GET  /metrics             — Prometheus text exposition
//! - GET  /ws                  — WebSocket; pushes WsSnapshot every 500ms
//! - GET  /*                   — static files from frontend/dist (production)

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};

use crate::api::controls::{ConfigPatch, ControlState};
use crate::config::{Config, OperatingMode};
use crate::execution::OrderExecutor;
use crate::ledger::{orders::OrderState, positions::Fill, Ledger};
use crate::metrics::{BotLatency, PrometheusHandle};
use crate::state::OrderBookState;
use crate::websocket::types::Side;

use super::types::*;

/// Shared state passed to all route handlers
pub struct ApiState {
    pub ledger: Arc<Ledger>,
    pub order_book_state: Arc<OrderBookState>,
    pub config: Arc<Config>,
    pub latency: Arc<BotLatency>,
    pub start_time: Instant,
    /// Prometheus recorder handle — rendered on each GET /metrics scrape.
    pub prometheus: PrometheusHandle,
    /// Mutable runtime controls (pause/resume, config patch).
    pub controls: Arc<ControlState>,
    /// Live order executor — None in paper-mode without credentials.
    pub executor: Option<Arc<OrderExecutor>>,
}

impl ApiState {
    pub fn new(
        ledger: Arc<Ledger>,
        order_book_state: Arc<OrderBookState>,
        config: Arc<Config>,
        latency: Arc<BotLatency>,
        prometheus: PrometheusHandle,
        controls: Arc<ControlState>,
        executor: Option<Arc<OrderExecutor>>,
    ) -> Self {
        Self {
            ledger,
            order_book_state,
            config,
            latency,
            start_time: Instant::now(),
            prometheus,
            controls,
            executor,
        }
    }

    /// Push current bot state into the `metrics` global registry so the next
    /// Prometheus scrape reflects up-to-date values.
    fn update_prometheus_metrics(&self) {
        use metrics::gauge;

        let uptime = self.start_time.elapsed().as_secs() as f64;
        gauge!("polybot_uptime_seconds").set(uptime);

        // Cash
        let cash = self.ledger.cash.snapshot();
        gauge!("polybot_cash_available_usd").set(decimal_to_f64(cash.available));
        gauge!("polybot_cash_reserved_usd").set(decimal_to_f64(cash.reserved));

        // Order stats
        let stats = self.ledger.orders.stats();
        gauge!("polybot_orders_active").set(stats.active_count as f64);
        gauge!("polybot_orders_filled_total").set(stats.total_filled as f64);
        gauge!("polybot_orders_cancelled_total").set(stats.total_cancelled as f64);
        gauge!("polybot_orders_rejected_total").set(stats.total_rejected as f64);

        // Positions
        let pos_count = self.ledger.positions.all_positions().len() as f64;
        gauge!("polybot_positions_count").set(pos_count);

        // PnL
        let realized = decimal_to_f64(self.ledger.positions.total_realized_pnl());
        let unrealized = decimal_to_f64(self.ledger.positions.total_unrealized_pnl());
        let fees = decimal_to_f64(self.ledger.positions.total_fees());
        let slippage = decimal_to_f64(self.ledger.positions.total_slippage_cost());
        gauge!("polybot_pnl_realized_usd").set(realized);
        gauge!("polybot_pnl_unrealized_usd").set(unrealized);
        gauge!("polybot_pnl_fees_usd").set(fees);
        gauge!("polybot_pnl_slippage_usd").set(slippage);
        gauge!("polybot_pnl_net_usd").set(realized + unrealized - fees - slippage);

        // Latency histograms
        let b = self.latency.book_to_intent.stats();
        gauge!("polybot_latency_book_to_intent_p50_us").set(b.p50_us as f64);
        gauge!("polybot_latency_book_to_intent_p95_us").set(b.p95_us as f64);
        gauge!("polybot_latency_book_to_intent_p99_us").set(b.p99_us as f64);
        gauge!("polybot_latency_book_to_intent_count").set(b.count as f64);

        let s = self.latency.submit_to_ack.stats();
        gauge!("polybot_latency_submit_to_ack_p50_us").set(s.p50_us as f64);
        gauge!("polybot_latency_submit_to_ack_p95_us").set(s.p95_us as f64);
        gauge!("polybot_latency_submit_to_ack_p99_us").set(s.p99_us as f64);
        gauge!("polybot_latency_submit_to_ack_count").set(s.count as f64);
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
                total_slippage_cost: p.total_slippage_cost.to_string(),
                fill_count: p.fill_count,
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
        let slippage = self.ledger.positions.total_slippage_cost();
        let total = realized + unrealized;
        let net = total - fees - slippage;
        let pnl = PnlInfo {
            realized: realized.to_string(),
            unrealized: unrealized.to_string(),
            total: total.to_string(),
            total_fees: fees.to_string(),
            total_slippage_cost: slippage.to_string(),
            net: net.to_string(),
        };

        // Latency (rolling 60s histogram snapshot — values reset by heartbeat)
        let book_stats = self.latency.book_to_intent.stats();
        let submit_stats = self.latency.submit_to_ack.stats();
        let latency = LatencyInfo {
            book_to_intent: LatencyPointInfo {
                p50_us: book_stats.p50_us,
                p95_us: book_stats.p95_us,
                p99_us: book_stats.p99_us,
                count: book_stats.count,
            },
            submit_to_ack: LatencyPointInfo {
                p50_us: submit_stats.p50_us,
                p95_us: submit_stats.p95_us,
                p99_us: submit_stats.p99_us,
                count: submit_stats.count,
            },
        };

        // Controls snapshot
        let rc = self.controls.runtime_config.read().unwrap();
        let controls = ControlsInfo {
            trading_paused: self.controls.is_paused(),
            max_bet_usd: rc.max_bet_usd.to_string(),
            max_position_per_market_usd: rc.max_position_per_market_usd.to_string(),
            max_total_exposure_usd: rc.max_total_exposure_usd.to_string(),
            max_daily_loss_usd: rc.max_daily_loss_usd.to_string(),
            max_open_orders: rc.max_open_orders,
            use_maker_mode: rc.use_maker_mode,
            temporal_arb_enabled: rc.temporal_arb_enabled,
            temporal_arb_threshold_bps: rc.temporal_arb_threshold_bps,
            temporal_arb_sensitivity_bps: rc.temporal_arb_sensitivity_bps,
        };
        drop(rc);

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
            latency,
            controls,
        }
    }
}

fn decimal_to_f64(d: rust_decimal::Decimal) -> f64 {
    d.to_string().parse::<f64>().unwrap_or(0.0)
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
        fee_rate_bps: f.fee_rate_bps(),
        notional: f.notional().to_string(),
        expected_price: f.expected_price.map(|p| p.to_string()),
        slippage_cost: f.slippage_cost.to_string(),
        slippage_bps: f.slippage_bps(),
        timestamp: f.timestamp.to_rfc3339(),
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// GET /metrics — Prometheus text-format exposition
async fn metrics_handler(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    state.update_prometheus_metrics();
    let body = state.prometheus.render();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

/// GET /api/status — health check
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
        "trading_paused": state.controls.is_paused(),
    }))
}

/// POST /api/bot/pause — stop generating new intents
async fn pause_handler(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    state.controls.pause();
    warn!("Bot paused via API");
    Json(json!({ "ok": true, "trading_paused": true }))
}

/// POST /api/bot/resume — resume generating intents
async fn resume_handler(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    state.controls.resume();
    info!("Bot resumed via API");
    Json(json!({ "ok": true, "trading_paused": false }))
}

/// POST /api/orders/cancel-all — cancel all live orders via exchange API
async fn cancel_all_handler(State(state): State<Arc<ApiState>>) -> impl IntoResponse {
    match &state.executor {
        Some(executor) => {
            let count = executor.cancel_all_orders().await;
            warn!(cancelled = count, "Cancel-all triggered via API");
            Json(json!({ "ok": true, "cancelled": count })).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "error": "No executor (paper mode without credentials)" })),
        )
            .into_response(),
    }
}

/// PATCH /api/config — update runtime strategy parameters
async fn config_patch_handler(
    State(state): State<Arc<ApiState>>,
    Json(patch): Json<ConfigPatch>,
) -> impl IntoResponse {
    state.controls.apply_patch(patch);
    let rc = state.controls.runtime_config.read().unwrap();
    info!(
        max_bet_usd = %rc.max_bet_usd,
        temporal_arb_enabled = rc.temporal_arb_enabled,
        "Runtime config updated via API"
    );
    Json(json!({
        "ok": true,
        "config": {
            "max_bet_usd": rc.max_bet_usd.to_string(),
            "max_position_per_market_usd": rc.max_position_per_market_usd.to_string(),
            "max_total_exposure_usd": rc.max_total_exposure_usd.to_string(),
            "max_daily_loss_usd": rc.max_daily_loss_usd.to_string(),
            "max_open_orders": rc.max_open_orders,
            "use_maker_mode": rc.use_maker_mode,
            "temporal_arb_enabled": rc.temporal_arb_enabled,
            "temporal_arb_threshold_bps": rc.temporal_arb_threshold_bps,
            "temporal_arb_sensitivity_bps": rc.temporal_arb_sensitivity_bps,
        }
    }))
}

/// GET /ws — WebSocket upgrade
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

pub async fn run_api_server(state: Arc<ApiState>, port: u16) -> anyhow::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let static_dir = std::path::Path::new("frontend/dist");

    let app = Router::new()
        .route("/api/status", get(status_handler))
        .route("/api/bot/pause", post(pause_handler))
        .route("/api/bot/resume", post(resume_handler))
        .route("/api/orders/cancel-all", post(cancel_all_handler))
        .route("/api/config", patch(config_patch_handler))
        .route("/metrics", get(metrics_handler))
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
    info!("  WebSocket:  ws://0.0.0.0:{}/ws", port);
    info!("  Metrics:    http://0.0.0.0:{}/metrics", port);

    let listener = tokio::net::TcpListener::bind(addr).await
        .map_err(|e| {
            error!("Failed to bind API server on port {}: {}", port, e);
            e
        })?;

    axum::serve(listener, app).await
        .map_err(|e| {
            error!("API server error: {}", e);
            e
        })?;

    Ok(())
}
