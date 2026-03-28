//! Core bot types for Polymarket CLOB
//!
//! Lightweight string-based types used throughout the bot.
//! These are intentionally simple (String aliases) rather than SDK's
//! SDK types (U256, B256, Address) to avoid conversion overhead
//! in hot paths like order book updates.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ============================================================================
// TYPE ALIASES
// ============================================================================

/// Market condition ID (0x-prefixed, 66 chars)
pub type ConditionId = String;

/// Token ID (256-bit integer as string)
pub type TokenId = String;

/// Order ID (0x-prefixed, 66 chars)
pub type OrderId = String;

/// Ethereum address (0x-prefixed, 42 chars)
pub type Address = String;

// ============================================================================
// ENUMS
// ============================================================================

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Buy,
    Sell,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Buy => write!(f, "BUY"),
            Side::Sell => write!(f, "SELL"),
        }
    }
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Fill And Kill - execute immediately, cancel unfilled
    FAK,
    /// Good Till Cancel - remain on book until filled
    GTC,
    /// Fill Or Kill - execute entire order or cancel
    FOK,
}

// ============================================================================
// ORDER BOOK TYPES
// ============================================================================

/// A single price level in the order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

impl PriceLevel {
    /// Parse price as Decimal
    pub fn price_decimal(&self) -> Option<Decimal> {
        self.price.parse().ok()
    }

    /// Parse size as Decimal
    pub fn size_decimal(&self) -> Option<Decimal> {
        self.size.parse().ok()
    }
}
