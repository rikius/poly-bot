//! API module - HTTP client and types for Polymarket CLOB
//!
//! This module provides:
//! - `auth` - HMAC signature generation for authenticated requests
//! - `client` - Optimized HTTP client with connection pooling
//! - `endpoints` - REST endpoint wrappers for CLOB API
//! - `types` - Core type definitions (PriceLevel, OrderBook, etc.)

pub mod auth;
pub mod client;
pub mod endpoints;
pub mod types;

pub use auth::{ApiCredentials, AuthHeaders};
pub use client::{ApiClient, ConnectionWarmer};

// Re-export endpoint response types (avoid ambiguity with types module)
pub use endpoints::{
    BookResponse, MidpointResponse, OpenOrder, PriceResponse, TimeResponse,
};
// Note: MarketInfo, FeeRateResponse, TradeNotification are defined in both
// endpoints and types - use endpoints:: or types:: prefix explicitly

// Re-export core types
pub use types::{
    Address, ConditionId, OrderBook, OrderId, OrderRequest, OrderResponse, OrderType, Outcome,
    PriceChange, PriceLevel, Side, SignedOrder, Signature, TokenId, TxHash,
};

