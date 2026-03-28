//! Polymarket Trading Bot
//!
//! A high-frequency trading bot for Polymarket prediction markets.
//!
//! ## Architecture
//!
//! - `websocket` - Core types, real-time market data, user events, Gamma API, and market discovery
//! - `state` - Order book and market registry
//! - `ledger` - Authoritative state (orders, fills, positions, cash)
//! - `execution` - Order building, signing, submission (via SDK), and execution policies
//! - `strategy` - Pluggable trading strategies
//! - `risk` - Circuit breaker and risk limits

pub mod alerts;
pub mod api;
pub mod bot;
pub mod config;
pub mod constants;
pub mod error;
pub mod execution;
pub mod feeds;
pub mod kill_switch;
pub mod latency;
pub mod ledger;
pub mod metrics;
pub mod risk;
pub mod state;
pub mod strategy;
pub mod websocket;

pub use bot::Bot;
pub use config::Config;
pub use error::{BotError, Result};
pub use kill_switch::KillSwitch;

