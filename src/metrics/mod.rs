//! Metrics: HDR latency histograms and Prometheus integration.
//!
//! ## Latency histograms
//!
//! 1. `book_to_intent` — time from receiving a market book update to the
//!    strategy router returning order intents (strategy evaluation cost).
//!
//! 2. `submit_to_ack` — time from starting `build_sign_submit` to receiving
//!    the exchange response (sign + HTTP POST cost).
//!
//! ## Prometheus
//!
//! Call `install_prometheus()` once at startup to install the global recorder.
//! The returned `PrometheusHandle` is stored in `ApiState` and rendered by
//! the `GET /metrics` handler on each scrape.

use std::sync::{Arc, Mutex};

pub use metrics_exporter_prometheus::PrometheusHandle;

/// Install the Prometheus metrics recorder as the global `metrics` backend.
///
/// Must be called once before any `metrics::gauge!` / `metrics::counter!`
/// macros are used.  Returns a handle whose `.render()` method produces the
/// Prometheus text-format exposition.
pub fn install_prometheus() -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

use hdrhistogram::Histogram;

// ---------------------------------------------------------------------------
// LatencyTracker
// ---------------------------------------------------------------------------

/// Thread-safe HDR histogram for a single latency dimension (µs resolution).
#[derive(Clone)]
pub struct LatencyTracker {
    inner: Arc<Mutex<Histogram<u64>>>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(
                // 3 significant figures, auto-resize to accommodate any value
                Histogram::<u64>::new(3).expect("valid histogram config"),
            )),
        }
    }

    /// Record a single observation in **microseconds**.
    pub fn record_us(&self, us: u64) {
        if let Ok(mut h) = self.inner.lock() {
            // hdrhistogram requires value >= 1
            let _ = h.record(us.max(1));
        }
    }

    /// Snapshot p50 / p95 / p99 and the total sample count.
    pub fn stats(&self) -> LatencyStats {
        if let Ok(h) = self.inner.lock() {
            if h.len() == 0 {
                return LatencyStats::default();
            }
            LatencyStats {
                p50_us: h.value_at_percentile(50.0),
                p95_us: h.value_at_percentile(95.0),
                p99_us: h.value_at_percentile(99.0),
                count: h.len(),
            }
        } else {
            LatencyStats::default()
        }
    }

    /// Clear all recorded values (call after logging to start a fresh window).
    pub fn reset(&self) {
        if let Ok(mut h) = self.inner.lock() {
            h.reset();
        }
    }
}

impl Default for LatencyTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// LatencyStats
// ---------------------------------------------------------------------------

/// Point-in-time snapshot of a single histogram.
#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    /// Total number of observations recorded since last reset.
    pub count: u64,
}

// ---------------------------------------------------------------------------
// BotLatency — aggregate tracker for all instrumented points
// ---------------------------------------------------------------------------

/// Shared latency state threaded through Bot and OrderExecutor.
pub struct BotLatency {
    /// Strategy evaluation: book update received → intents returned (µs).
    pub book_to_intent: LatencyTracker,
    /// Order submission: build+sign+post_order start → response received (µs).
    pub submit_to_ack: LatencyTracker,
}

impl BotLatency {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            book_to_intent: LatencyTracker::new(),
            submit_to_ack: LatencyTracker::new(),
        })
    }
}
