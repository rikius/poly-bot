//! On-chain position redemption for resolved Polymarket markets.
//!
//! When a prediction market resolves, winning conditional tokens can be redeemed
//! 1-for-1 into USDC by calling `redeemPositions()` on the Polymarket CTF Exchange
//! contract on Polygon.
//!
//! This module delegates all ABI encoding, provider setup, and transaction handling
//! to the official `polymarket_client_sdk` CTF client.

use alloy::network::EthereumWallet;
use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::ctf::Client as CtfClient;
use polymarket_client_sdk::ctf::types::RedeemPositionsRequest;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{error, info};

/// USDC contract address on Polygon.
const USDC_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";

/// Polygon mainnet chain ID.
const POLYGON_CHAIN_ID: u64 = 137;

/// Polygon RPC endpoint fallback (used when POLYGON_RPC_URL is not set).
/// Exposed so config.rs can reference it as the default value.
pub const POLYGON_RPC_DEFAULT: &str = "https://polygon-rpc.com";

/// Information needed to redeem a single position.
pub struct RedeemRequest {
    /// Market condition ID (0x-prefixed, 66-char hex)
    pub condition_id: String,
    /// Human-readable shares (for logging)
    pub shares: Decimal,
}

/// Submit a `redeemPositions()` transaction on Polygon using the SDK CTF client.
///
/// Uses `RedeemPositionsRequest::for_binary_market` which redeems both YES and NO
/// index sets — only the winning side actually pays out, the other is a no-op.
///
/// Returns `Ok(tx_hash_hex)` on success, `Err` on failure.
pub async fn redeem_positions(
    signer: Arc<PrivateKeySigner>,
    req: &RedeemRequest,
    rpc_url_str: &str,
) -> anyhow::Result<String> {
    let wallet = EthereumWallet::from(signer.as_ref().clone());
    let rpc_url = rpc_url_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid RPC URL '{}': {:?}", rpc_url_str, e))?;

    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url);

    let ctf_client = CtfClient::new(provider, POLYGON_CHAIN_ID)
        .map_err(|e| anyhow::anyhow!("Failed to create CTF client: {}", e))?;

    let usdc: Address = Address::from_str(USDC_POLYGON)
        .map_err(|e| anyhow::anyhow!("Invalid USDC address: {}", e))?;

    let condition_id: alloy::primitives::B256 = req.condition_id
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid conditionId '{}': {:?}", req.condition_id, e))?;

    let redeem_req = RedeemPositionsRequest::for_binary_market(usdc, condition_id);

    info!(
        condition_id = %req.condition_id,
        shares = %req.shares,
        "Submitting redeemPositions() via SDK"
    );

    let resp = ctf_client
        .redeem_positions(&redeem_req)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send redeemPositions tx: {}", e))?;

    let tx_hash = format!("{}", resp.transaction_hash);

    info!(
        tx_hash = %tx_hash,
        block = %resp.block_number,
        condition_id = %req.condition_id,
        shares = %req.shares,
        "redeemPositions() confirmed — USDC credited to wallet"
    );

    Ok(tx_hash)
}

/// Try to redeem a position, logging errors without panicking.
///
/// Returns the redeemed USDC amount (estimated as shares × $1.00 for winning tokens)
/// or `None` if the redemption failed.
pub async fn try_redeem(
    signer: Arc<PrivateKeySigner>,
    req: &RedeemRequest,
    rpc_url: &str,
) -> Option<Decimal> {
    match redeem_positions(signer, req, rpc_url).await {
        Ok(tx_hash) => {
            let usdc_received = req.shares;
            info!(
                tx_hash = %tx_hash,
                usdc = %usdc_received,
                "Position redeemed successfully"
            );
            Some(usdc_received)
        }
        Err(e) => {
            error!(
                error = %e,
                condition_id = %req.condition_id,
                "redeemPositions() failed"
            );
            None
        }
    }
}

// ============================================================================
// MARKET RESOLUTION DETECTION
// ============================================================================

/// Determine whether a market is likely resolved based on price signals.
///
/// A market is considered resolved when the CLOB reports a price ≥ 0.99 for
/// the winning side (essentially $1.00).
pub fn likely_resolved(best_ask: Option<f64>, best_bid: Option<f64>) -> bool {
    let ask_resolved = best_ask.map(|a| a >= 0.99).unwrap_or(false);
    let bid_resolved = best_bid.map(|b| b >= 0.99).unwrap_or(false);
    ask_resolved || bid_resolved
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_likely_resolved() {
        assert!(likely_resolved(Some(0.99), None));
        assert!(likely_resolved(None, Some(0.995)));
        assert!(!likely_resolved(Some(0.95), Some(0.94)));
        assert!(!likely_resolved(None, None));
    }
}
