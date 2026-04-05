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
//! - `MarketPairRegistry`: Tracks YES/NO token pairs for binary markets
//! - `EdgeCalculator`: Dynamic edge calculation for arb strategies
//! - `MathArbStrategy`: Mathematical arbitrage (YES + NO < $1)

pub mod arbitrage;
pub mod contrarian;
pub mod edge_calculator;
pub mod late_entry;
pub mod maker_rebate_arb;
pub mod market_pair;
pub mod router;
pub mod tbo_trend;
pub mod tbt_divergence;
pub mod temporal_arb;
pub mod traits;

// Re-export main types
pub use router::StrategyRouter;
pub use traits::{OrderIntent, Strategy, StrategyContext, StrategyError, StrategyResult, Urgency};

// Re-export strategy implementations
pub use arbitrage::{MathArbConfig, MathArbStrategy};
pub use contrarian::{ContrarianConfig, ContrarianStrategy};
pub use late_entry::{LateEntryConfig, LateEntryStrategy};
pub use tbo_trend::{TboConfig, TboTrendStrategy};
pub use tbt_divergence::{TbtConfig, TbtDivergenceStrategy};
pub use edge_calculator::{EdgeCalculation, EdgeCalculator, EdgeConfig};
pub use maker_rebate_arb::{MakerRebateArbStrategy, MakerRebateConfig};
pub use market_pair::{MarketPair, MarketPairRegistry};
pub use temporal_arb::{TemporalArbConfig, TemporalArbStrategy};
