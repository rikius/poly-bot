//! Strategy module - pluggable trading strategies
//!
//! ## Architecture
//!
//! Strategies output WHAT they want (OrderIntent), not HOW to execute.
//! The execution layer (policies + executor) handles the HOW.
//!
//! ## Components
//!
//! - `Strategy` trait: Core abstraction for trading strategies
//! - `OrderIntent`: What a strategy wants to do
//! - `Urgency`: How quickly execution should happen
//! - `StrategyContext`: Read-only view of system state
//! - `StrategyRouter`: Dispatches events to subscribed strategies

pub mod router;
pub mod traits;

// Re-export main types
pub use router::StrategyRouter;
pub use traits::{OrderIntent, Strategy, StrategyContext, StrategyError, StrategyResult, Urgency};
