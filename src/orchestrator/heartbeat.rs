//! Heartbeat logging and per-market edge diagnostics.

use super::Bot;
use crate::config::OperatingMode;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info};

impl Bot {
    /// Log heartbeat with current stats (every 10s).
    ///
    /// Latency histograms are logged and reset every 60s (every 6th heartbeat).
    /// Fires an alert if the circuit breaker just transitioned to Open.
    pub(crate) async fn log_heartbeat(&mut self) {
        let mode_str = match self.config.mode {
            OperatingMode::Paper => "PAPER",
            OperatingMode::Live => "LIVE",
        };
        let circuit_status = if self.circuit_breaker.is_trading_allowed() {
            "✅"
        } else {
            "🔴 OPEN"
        };

        let active_orders = self.order_tracker.active_count();

        info!(
            "Heartbeat [{}]: {} markets | {} msgs | {:.1} msg/s | {} intents | {} execs | \
             {} fills | {} active | CB: {}",
            mode_str,
            self.order_book_state.num_markets(),
            self.total_messages,
            self.total_messages as f64 / 10.0,
            self.total_intents,
            self.total_executions,
            self.total_fills,
            active_orders,
            circuit_status
        );
        self.total_messages = 0;

        // Log per-strategy evaluation counts then reset for the next window.
        let eval_counts = self.strategy_router.evaluation_counts();
        if !eval_counts.is_empty() {
            let mut sorted: Vec<_> = eval_counts.iter().collect();
            sorted.sort_by_key(|(name, _)| name.as_str());
            for (name, (evals, intents)) in &sorted {
                info!(
                    "  Strategy [{}]: {} evals, {} intents generated (last 10s)",
                    name, evals, intents
                );
            }
            self.strategy_router.reset_evaluation_counts();
        }

        // Mark all open positions to market so unrealized_pnl stays current.
        {
            let prices: Vec<(String, Decimal)> = self
                .ledger
                .positions
                .all_positions()
                .iter()
                .filter_map(|p| {
                    self.order_book_state
                        .mid_price(&p.token_id)
                        .map(|mid| (p.token_id.clone(), mid))
                })
                .collect();
            if !prices.is_empty() {
                self.ledger.positions.mark_all_to_market(&prices);
            }
        }

        self.heartbeat_count += 1;
        if self.heartbeat_count % 6 == 0 {
            // Log latency summary every 60s then reset for the next window.
            let book = self.latency.book_to_intent.stats();
            let submit = self.latency.submit_to_ack.stats();

            if book.count > 0 {
                info!(
                    "Latency book→intent: p50={}µs p95={}µs p99={}µs (n={})",
                    book.p50_us, book.p95_us, book.p99_us, book.count
                );
                self.latency.book_to_intent.reset();
            }
            if submit.count > 0 {
                info!(
                    "Latency submit→ack:  p50={}µs p95={}µs p99={}µs (n={})",
                    submit.p50_us, submit.p95_us, submit.p99_us, submit.count
                );
                self.latency.submit_to_ack.reset();
            }

            self.log_edge_diagnostics();
        }

        // Detect circuit breaker transition → Open and fire alert exactly once.
        let is_open = !self.circuit_breaker.is_trading_allowed();
        if is_open && !self.circuit_was_open {
            if let Some(ref alerts) = self.alerts {
                let reason = self
                    .circuit_breaker
                    .open_reason()
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                alerts.send_circuit_breaker_trip(&reason).await;
            }
        }
        self.circuit_was_open = is_open;
    }

    /// Log per-market edge diagnostics (why no intents are being generated).
    ///
    /// Called every 60s. For each registered market pair we compute:
    ///   raw_edge  = 1 - YES_ask - NO_ask
    ///   fee_cost  = fee_rate_bps / 10_000 * (YES_ask + NO_ask)
    ///   tradeable = raw_edge > fee_cost + min_edge (3¢ taker / 1¢ maker)
    fn log_edge_diagnostics(&self) {
        let pairs = self.market_registry.all_pairs();
        if pairs.is_empty() {
            return;
        }

        let taker_min_edge = dec!(0.03);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut any_logged = false;
        for pair in &pairs {
            let yes_ask = match self.order_book_state.best_ask(&pair.yes_token_id) {
                Some(v) => v,
                None => continue,
            };
            let no_ask = match self.order_book_state.best_ask(&pair.no_token_id) {
                Some(v) => v,
                None => continue,
            };
            let yes_bid = self.order_book_state.best_bid(&pair.yes_token_id);
            let no_bid = self.order_book_state.best_bid(&pair.no_token_id);

            let yes_age_ms = self
                .order_book_state
                .get_book(&pair.yes_token_id)
                .and_then(|b| b.last_update)
                .map(|ts| now_ms - ts);
            let no_age_ms = self
                .order_book_state
                .get_book(&pair.no_token_id)
                .and_then(|b| b.last_update)
                .map(|ts| now_ms - ts);
            let book_age_s = yes_age_ms
                .unwrap_or(i64::MAX)
                .min(no_age_ms.unwrap_or(i64::MAX));
            let age_label = if book_age_s == i64::MAX {
                "age=unknown".to_string()
            } else {
                format!("age={:.1}s", book_age_s as f64 / 1000.0)
            };

            let combined = yes_ask + no_ask;
            let raw_edge = Decimal::ONE - combined;
            let fee_rate = Decimal::from(pair.fee_rate_bps) / dec!(10000);
            let fee_cost = fee_rate * combined;
            let required = fee_cost + taker_min_edge;
            let tradeable = raw_edge >= required;

            let yes_mid = yes_bid.map(|b| (yes_ask + b) / dec!(2));
            let no_mid = no_bid.map(|b| (no_ask + b) / dec!(2));
            let mid_sum = yes_mid.zip(no_mid).map(|(y, n)| y + n);

            let label = if combined > dec!(1.5) {
                "thin book — stale protective orders (combined >> 1)"
            } else if combined >= Decimal::ONE {
                "combined≥1.00 — no arb (efficient market)"
            } else if !tradeable {
                "edge < fees+min_edge"
            } else {
                "TRADEABLE"
            };

            debug!(
                "EdgeDiag: {} | ask={:.3}+{:.3}={:.3} mid_sum={} \
                 raw_edge={:+.3} fee_cost={:.3} required={:.3} {} → {}",
                &pair.condition_id[..pair.condition_id.len().min(12)],
                yes_ask,
                no_ask,
                combined,
                mid_sum
                    .map(|m| format!("{:.3}", m))
                    .unwrap_or_else(|| "n/a".into()),
                raw_edge,
                fee_cost,
                required,
                age_label,
                label,
            );

            let (yes_bids, yes_asks) = self.order_book_state.top_levels(&pair.yes_token_id, 3);
            let (no_bids, no_asks) = self.order_book_state.top_levels(&pair.no_token_id, 3);
            let fmt_levels = |levels: &[(Decimal, Decimal)]| -> String {
                levels
                    .iter()
                    .map(|(p, s)| format!("{:.3}x{:.0}", p, s))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            debug!(
                "  YES bids=[{}] asks=[{}]",
                fmt_levels(&yes_bids),
                fmt_levels(&yes_asks),
            );
            debug!(
                "  NO  bids=[{}] asks=[{}]",
                fmt_levels(&no_bids),
                fmt_levels(&no_asks),
            );

            any_logged = true;
        }

        if !any_logged {
            debug!("EdgeDiag: no books received yet for any registered market pair");
        }

        let all_fee_blocked = pairs.iter().all(|p| {
            let ya = self.order_book_state.best_ask(&p.yes_token_id);
            let na = self.order_book_state.best_ask(&p.no_token_id);
            match (ya, na) {
                (Some(y), Some(n)) => {
                    let combined = y + n;
                    combined >= Decimal::ONE || {
                        let fee_cost =
                            Decimal::from(p.fee_rate_bps) / dec!(10000) * combined;
                        (Decimal::ONE - combined) < fee_cost + taker_min_edge
                    }
                }
                _ => true,
            }
        });

        if all_fee_blocked && !pairs.is_empty() {
            debug!(
                "EdgeDiag: all markets below required edge. \
                 For 15-min crypto markets (fee_rate=1000bps/10%), required edge ≈ 10%+3¢. \
                 Consider enabling TemporalArbStrategy (TEMPORAL_ARB_ENABLED=true) which \
                 trades momentum rather than pure arb."
            );
        }
    }
}
