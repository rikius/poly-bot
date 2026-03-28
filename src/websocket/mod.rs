//! WebSocket connectivity, market discovery, and core types
//!
//! - `types` - Core type definitions (Side, TokenId, PriceLevel, etc.)
//! - `market` - Order book updates for subscribed markets
//! - `user` - Trade notifications (fills, order status)
//! - `gamma` - REST client for Gamma API (market metadata)
//! - `discovery` - Market discovery logic (slug-based 15-min crypto)

pub mod discovery;
pub mod gamma;
pub mod market;
pub mod types;
pub mod user;

pub use discovery::{DiscoveredMarket, MarketDiscovery, MarketFilter, OutcomeType};
pub use gamma::{GammaClient, GammaEvent, GammaMarket};
pub use market::{MarketWebSocket, MarketMessage, BookUpdateMessage, LevelUpdateMessage};
pub use types::{Address, ConditionId, OrderId, OrderType, PriceLevel, Side, TokenId};
pub use user::{UserWebSocket, UserMessage, TradeNotification, OrderUpdate};

