//! API module - HTTP client and types for Polymarket CLOB and Gamma APIs
//!
//! This module provides:
//! - `auth` - HMAC signature generation for authenticated requests
//! - `client` - Optimized HTTP client with connection pooling
//! - `discovery` - Market discovery from Gamma API
//! - `endpoints` - REST endpoint wrappers for CLOB API
//! - `gamma` - Gamma API client for market discovery
//! - `types` - Core type definitions (PriceLevel, OrderBook, etc.)

pub mod auth;
pub mod client;
pub mod discovery;
pub mod endpoints;
pub mod gamma;
pub mod types;

pub use auth::{ApiCredentials, AuthHeaders};
pub use client::{ApiClient, ConnectionWarmer};
pub use discovery::{DiscoveredMarket, MarketDiscovery, MarketFilter, OutcomeType};
pub use gamma::{GammaClient, GammaEvent, GammaMarket};

// Re-export endpoint response types (avoid ambiguity with types module)
pub use endpoints::{
    BookResponse, MarketsResponse, MidpointResponse, OpenOrder, PriceResponse, TimeResponse,
};
// Note: MarketInfo, FeeRateResponse, TradeNotification are defined in both
// endpoints and types - use endpoints:: or types:: prefix explicitly

// Re-export core types
pub use types::{
    Address, ConditionId, OrderBook, OrderId, OrderRequest, OrderResponse, OrderType, Outcome,
    PriceChange, PriceLevel, Side, SignedOrder, Signature, TokenId, TxHash,
};

