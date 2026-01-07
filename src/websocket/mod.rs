//! WebSocket connectivity for real-time market data
//!
//! Two WebSocket streams:
//! - Market WS: Order book updates for subscribed markets
//! - User WS: Trade notifications (fills, order status)

pub mod market;

pub use market::{MarketWebSocket, MarketMessage, BookUpdateMessage};
