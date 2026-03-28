//! API rate limiter — prevents exceeding Polymarket rate limits
//!
//! Polymarket uses Cloudflare throttling (delay, not reject), so exceeding
//! limits degrades latency rather than failing. This limiter tracks submission
//! rates and blocks when approaching limits to maintain low latency.
//!
//! Key limits (from https://docs.polymarket.com/api-reference/rate-limits):
//! - POST /order:  3,500 req/10s (burst), 36,000 req/10min (sustained)
//! - DELETE /order: 3,000 req/10s (burst)
//! - General CLOB:  9,000 req/10s

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::warn;

/// Configuration for a rate limit window
#[derive(Debug, Clone, Copy)]
struct Window {
    /// Maximum requests allowed in this window
    max_requests: u32,
    /// Window duration
    duration: Duration,
}

/// Sliding window rate limiter
///
/// Tracks timestamps of recent requests and checks against configured limits.
/// Uses a conservative threshold (80% of limit) to leave headroom.
pub struct RateLimiter {
    /// Recent request timestamps
    timestamps: Mutex<VecDeque<Instant>>,
    /// Burst limit (short window)
    burst: Window,
    /// Sustained limit (long window)
    sustained: Window,
    /// Safety margin — block at this fraction of the limit (0.0–1.0)
    threshold: f64,
}

impl RateLimiter {
    /// Create a rate limiter for order submission
    ///
    /// Uses Polymarket's POST /order limits:
    /// - Burst: 3,500 req/10s
    /// - Sustained: 36,000 req/10min
    pub fn for_order_submission() -> Self {
        Self {
            timestamps: Mutex::new(VecDeque::new()),
            burst: Window {
                max_requests: 3_500,
                duration: Duration::from_secs(10),
            },
            sustained: Window {
                max_requests: 36_000,
                duration: Duration::from_secs(600),
            },
            threshold: 0.80,
        }
    }

    /// Create a rate limiter for order cancellation
    ///
    /// Uses Polymarket's DELETE /order limits:
    /// - Burst: 3,000 req/10s
    /// - Sustained: 30,000 req/10min
    pub fn for_order_cancellation() -> Self {
        Self {
            timestamps: Mutex::new(VecDeque::new()),
            burst: Window {
                max_requests: 3_000,
                duration: Duration::from_secs(10),
            },
            sustained: Window {
                max_requests: 30_000,
                duration: Duration::from_secs(600),
            },
            threshold: 0.80,
        }
    }

    /// Check if a request is allowed (does NOT record it)
    pub fn check(&self) -> bool {
        let now = Instant::now();
        let timestamps = self.timestamps.lock().unwrap();
        !self.would_exceed(&timestamps, now)
    }

    /// Record a request and return whether it was allowed
    ///
    /// Returns `true` if the request is within limits and was recorded.
    /// Returns `false` if the request would exceed limits (not recorded).
    pub fn try_acquire(&self) -> bool {
        let now = Instant::now();
        let mut timestamps = self.timestamps.lock().unwrap();

        if self.would_exceed(&timestamps, now) {
            return false;
        }

        timestamps.push_back(now);

        // Prune old timestamps beyond the sustained window
        let cutoff = now - self.sustained.duration;
        while timestamps.front().is_some_and(|&t| t < cutoff) {
            timestamps.pop_front();
        }

        true
    }

    /// Get current usage stats
    pub fn stats(&self) -> RateLimiterStats {
        let now = Instant::now();
        let timestamps = self.timestamps.lock().unwrap();

        let burst_count = timestamps
            .iter()
            .rev()
            .take_while(|&&t| now.duration_since(t) < self.burst.duration)
            .count() as u32;

        let sustained_count = timestamps.len() as u32;

        RateLimiterStats {
            burst_count,
            burst_limit: self.burst.max_requests,
            burst_window: self.burst.duration,
            sustained_count,
            sustained_limit: self.sustained.max_requests,
            sustained_window: self.sustained.duration,
        }
    }

    fn would_exceed(&self, timestamps: &VecDeque<Instant>, now: Instant) -> bool {
        let burst_threshold = (self.burst.max_requests as f64 * self.threshold) as u32;
        let sustained_threshold = (self.sustained.max_requests as f64 * self.threshold) as u32;

        // Check burst window
        let burst_cutoff = now - self.burst.duration;
        let burst_count = timestamps
            .iter()
            .rev()
            .take_while(|&&t| t >= burst_cutoff)
            .count() as u32;

        if burst_count >= burst_threshold {
            warn!(
                count = burst_count,
                limit = self.burst.max_requests,
                threshold = burst_threshold,
                "Rate limit: burst window approaching limit"
            );
            return true;
        }

        // Check sustained window
        let sustained_count = timestamps.len() as u32;
        if sustained_count >= sustained_threshold {
            warn!(
                count = sustained_count,
                limit = self.sustained.max_requests,
                threshold = sustained_threshold,
                "Rate limit: sustained window approaching limit"
            );
            return true;
        }

        false
    }
}

/// Rate limiter statistics
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    pub burst_count: u32,
    pub burst_limit: u32,
    pub burst_window: Duration,
    pub sustained_count: u32,
    pub sustained_limit: u32,
    pub sustained_window: Duration,
}

impl std::fmt::Display for RateLimiterStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "burst={}/{} ({:.0}s), sustained={}/{} ({:.0}s)",
            self.burst_count,
            self.burst_limit,
            self.burst_window.as_secs_f64(),
            self.sustained_count,
            self.sustained_limit,
            self.sustained_window.as_secs_f64(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::for_order_submission();
        assert!(limiter.try_acquire());
        assert!(limiter.check());
    }

    #[test]
    fn test_rate_limiter_stats() {
        let limiter = RateLimiter::for_order_submission();
        limiter.try_acquire();
        limiter.try_acquire();
        let stats = limiter.stats();
        assert_eq!(stats.burst_count, 2);
        assert_eq!(stats.sustained_count, 2);
    }
}
