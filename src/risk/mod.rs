//! Risk module - circuit breaker, risk limits, and reconciliation
//!
//! Safety systems to prevent catastrophic losses:
//! - CircuitBreaker: Halts trading on repeated failures
//! - RiskLimits: Enforces position and loss limits
//! - Reconciliation: REST sync to detect state drift

pub mod circuit_breaker;
pub mod limits;
pub mod rate_limiter;
pub mod reconciliation;

pub use circuit_breaker::{CircuitBreaker, CircuitBreakerStats, CircuitState, OpenReason};
pub use limits::{LimitViolation, RiskLimits, RiskLimitsConfig, RiskLimitsStats};
pub use rate_limiter::RateLimiter;
pub use reconciliation::{ReconciliationLoop, ReconciliationMessage, ReconciliationResult};