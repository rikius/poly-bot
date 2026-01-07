//! WebSocket connectivity for real-time market data
//!
//! Two WebSocket streams:
//! - Market WS: Order book updates for subscribed markets
//! - User WS: Trade notifications (fills, order status)

pub mod market;
pub mod user;

pub use market::{MarketWebSocket, MarketMessage, BookUpdateMessage, LevelUpdateMessage};
pub use user::{UserWebSocket, UserMessage, TradeNotification, OrderUpdate};

