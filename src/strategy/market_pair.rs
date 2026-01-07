//! Market pair registry - tracks YES/NO token pairs for binary markets
//!
//! Binary markets on Polymarket have two outcome tokens (YES and NO).
//! For arbitrage detection, we need to know which tokens are complements.
//!
//! ## Key Insight
//!
//! The WebSocket sends updates per token_id, not per market. We need to:
//! 1. Discover which tokens belong to the same market
//! 2. Know which is YES and which is NO
//! 3. Look up the complement when one side updates

use crate::api::types::{ConditionId, TokenId};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::Arc;

/// A binary market pair (YES and NO tokens)
#[derive(Debug, Clone)]
pub struct MarketPair {
    /// Market/condition ID (0x-prefixed hash)
    pub condition_id: ConditionId,

    /// YES outcome token ID (256-bit integer as string)
    pub yes_token_id: TokenId,

    /// NO outcome token ID (256-bit integer as string)
    pub no_token_id: TokenId,

    /// Fee rate in basis points (1000 = 10% for 15-min crypto)
    pub fee_rate_bps: u32,

    /// Whether this is a neg-risk market (affects signing)
    pub is_neg_risk: bool,

    /// Market description/question (for logging)
    pub description: String,

    /// Minimum order size
    pub min_order_size: Decimal,

    /// Tick size for prices
    pub tick_size: Decimal,
}

impl MarketPair {
    /// Create a new market pair
    pub fn new(
        condition_id: ConditionId,
        yes_token_id: TokenId,
        no_token_id: TokenId,
    ) -> Self {
        Self {
            condition_id,
            yes_token_id,
            no_token_id,
            fee_rate_bps: 0,
            is_neg_risk: false,
            description: String::new(),
            min_order_size: Decimal::ONE,
            tick_size: Decimal::new(1, 2), // 0.01 default
        }
    }

    /// Create with fee rate
    pub fn with_fee_rate(mut self, fee_rate_bps: u32) -> Self {
        self.fee_rate_bps = fee_rate_bps;
        self
    }

    /// Create with neg-risk flag
    pub fn with_neg_risk(mut self, is_neg_risk: bool) -> Self {
        self.is_neg_risk = is_neg_risk;
        self
    }

    /// Create with description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Create with min order size
    pub fn with_min_order_size(mut self, min_order_size: Decimal) -> Self {
        self.min_order_size = min_order_size;
        self
    }

    /// Is this a 15-minute crypto market (has taker fees)?
    pub fn is_crypto_15min(&self) -> bool {
        // 15-min crypto markets have fee_rate_bps = 1000 (10%)
        self.fee_rate_bps >= 100
    }

    /// Get the complement token ID
    pub fn complement(&self, token_id: &TokenId) -> Option<&TokenId> {
        if token_id == &self.yes_token_id {
            Some(&self.no_token_id)
        } else if token_id == &self.no_token_id {
            Some(&self.yes_token_id)
        } else {
            None
        }
    }

    /// Check if a token belongs to this pair
    pub fn contains(&self, token_id: &TokenId) -> bool {
        token_id == &self.yes_token_id || token_id == &self.no_token_id
    }

    /// Is this the YES token?
    pub fn is_yes(&self, token_id: &TokenId) -> bool {
        token_id == &self.yes_token_id
    }

    /// Is this the NO token?
    pub fn is_no(&self, token_id: &TokenId) -> bool {
        token_id == &self.no_token_id
    }
}

/// Registry of market pairs for arbitrage detection
///
/// Provides O(1) lookup by:
/// - condition_id (market ID)
/// - token_id (either YES or NO)
pub struct MarketPairRegistry {
    /// Map of condition_id -> MarketPair
    by_condition: DashMap<ConditionId, Arc<MarketPair>>,

    /// Map of token_id -> MarketPair (for quick lookup from WebSocket updates)
    by_token: DashMap<TokenId, Arc<MarketPair>>,
}

impl MarketPairRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            by_condition: DashMap::new(),
            by_token: DashMap::new(),
        }
    }

    /// Register a market pair
    pub fn register(&self, pair: MarketPair) {
        let pair = Arc::new(pair);

        // Index by condition ID
        self.by_condition
            .insert(pair.condition_id.clone(), pair.clone());

        // Index by both token IDs for O(1) lookup from WebSocket
        self.by_token
            .insert(pair.yes_token_id.clone(), pair.clone());
        self.by_token.insert(pair.no_token_id.clone(), pair.clone());
    }

    /// Unregister a market pair by condition ID
    pub fn unregister(&self, condition_id: &ConditionId) {
        if let Some((_, pair)) = self.by_condition.remove(condition_id) {
            self.by_token.remove(&pair.yes_token_id);
            self.by_token.remove(&pair.no_token_id);
        }
    }

    /// Get market pair by condition ID
    pub fn get_by_condition(&self, condition_id: &ConditionId) -> Option<Arc<MarketPair>> {
        self.by_condition.get(condition_id).map(|r| r.clone())
    }

    /// Get market pair by token ID (either YES or NO)
    pub fn get_by_token(&self, token_id: &TokenId) -> Option<Arc<MarketPair>> {
        self.by_token.get(token_id).map(|r| r.clone())
    }

    /// Get the complement token ID for a given token
    pub fn get_complement(&self, token_id: &TokenId) -> Option<TokenId> {
        self.by_token
            .get(token_id)
            .and_then(|pair| pair.complement(token_id).cloned())
    }

    /// Get all registered market pairs
    pub fn all_pairs(&self) -> Vec<Arc<MarketPair>> {
        self.by_condition.iter().map(|r| r.value().clone()).collect()
    }

    /// Get all condition IDs
    pub fn all_condition_ids(&self) -> Vec<ConditionId> {
        self.by_condition.iter().map(|r| r.key().clone()).collect()
    }

    /// Number of registered markets
    pub fn len(&self) -> usize {
        self.by_condition.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.by_condition.is_empty()
    }

    /// Clear all registered pairs
    pub fn clear(&self) {
        self.by_condition.clear();
        self.by_token.clear();
    }

    /// Filter pairs by predicate
    pub fn filter<F>(&self, predicate: F) -> Vec<Arc<MarketPair>>
    where
        F: Fn(&MarketPair) -> bool,
    {
        self.by_condition
            .iter()
            .filter(|r| predicate(r.value()))
            .map(|r| r.value().clone())
            .collect()
    }

    /// Get all 15-min crypto markets (for arb strategies)
    pub fn crypto_15min_markets(&self) -> Vec<Arc<MarketPair>> {
        self.filter(|pair| pair.is_crypto_15min())
    }
}

impl Default for MarketPairRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pair() -> MarketPair {
        MarketPair::new(
            "0xabc123".to_string(),
            "123456789".to_string(), // YES
            "987654321".to_string(), // NO
        )
        .with_fee_rate(1000)
        .with_description("Will BTC go up?")
    }

    #[test]
    fn test_market_pair_complement() {
        let pair = sample_pair();

        assert_eq!(
            pair.complement(&"123456789".to_string()),
            Some(&"987654321".to_string())
        );
        assert_eq!(
            pair.complement(&"987654321".to_string()),
            Some(&"123456789".to_string())
        );
        assert_eq!(pair.complement(&"unknown".to_string()), None);
    }

    #[test]
    fn test_market_pair_yes_no() {
        let pair = sample_pair();

        assert!(pair.is_yes(&"123456789".to_string()));
        assert!(!pair.is_no(&"123456789".to_string()));
        assert!(pair.is_no(&"987654321".to_string()));
        assert!(!pair.is_yes(&"987654321".to_string()));
    }

    #[test]
    fn test_is_crypto_15min() {
        let crypto = sample_pair().with_fee_rate(1000);
        let standard = sample_pair().with_fee_rate(0);

        assert!(crypto.is_crypto_15min());
        assert!(!standard.is_crypto_15min());
    }

    #[test]
    fn test_registry_register_lookup() {
        let registry = MarketPairRegistry::new();
        let pair = sample_pair();

        registry.register(pair.clone());

        // Lookup by condition
        let found = registry.get_by_condition(&"0xabc123".to_string());
        assert!(found.is_some());
        assert_eq!(found.unwrap().yes_token_id, "123456789");

        // Lookup by YES token
        let found = registry.get_by_token(&"123456789".to_string());
        assert!(found.is_some());

        // Lookup by NO token
        let found = registry.get_by_token(&"987654321".to_string());
        assert!(found.is_some());

        // Lookup unknown
        let found = registry.get_by_token(&"unknown".to_string());
        assert!(found.is_none());
    }

    #[test]
    fn test_registry_get_complement() {
        let registry = MarketPairRegistry::new();
        registry.register(sample_pair());

        let complement = registry.get_complement(&"123456789".to_string());
        assert_eq!(complement, Some("987654321".to_string()));

        let complement = registry.get_complement(&"987654321".to_string());
        assert_eq!(complement, Some("123456789".to_string()));
    }

    #[test]
    fn test_registry_unregister() {
        let registry = MarketPairRegistry::new();
        registry.register(sample_pair());

        assert_eq!(registry.len(), 1);

        registry.unregister(&"0xabc123".to_string());

        assert_eq!(registry.len(), 0);
        assert!(registry.get_by_token(&"123456789".to_string()).is_none());
    }

    #[test]
    fn test_registry_filter() {
        let registry = MarketPairRegistry::new();

        registry.register(sample_pair().with_fee_rate(1000));
        registry.register(
            MarketPair::new(
                "0xdef456".to_string(),
                "111".to_string(),
                "222".to_string(),
            )
            .with_fee_rate(0),
        );

        let crypto = registry.crypto_15min_markets();
        assert_eq!(crypto.len(), 1);
        assert_eq!(crypto[0].condition_id, "0xabc123");
    }
}
