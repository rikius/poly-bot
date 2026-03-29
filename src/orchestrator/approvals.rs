//! One-time on-chain approval setup for Polymarket CLOB trading.
//!
//! Before any order can be placed the wallet must approve three contracts to
//! spend its USDC (ERC-20 approval) and transfer its Conditional Tokens
//! (ERC-1155 setApprovalForAll):
//!
//! 1. CTF Exchange          — standard market orders
//! 2. Neg-Risk CTF Exchange — neg-risk market orders
//! 3. Neg-Risk Adapter      — token minting/splitting
//!
//! These approvals only need to be set once per wallet.  `ensure_approvals`
//! checks the current on-chain state and only sends transactions for missing
//! approvals, so it is safe to call on every startup.

use alloy::primitives::U256;
use alloy::providers::ProviderBuilder;
use alloy::sol;
use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::{POLYGON, contract_config};
use std::sync::Arc;
use tracing::{info, warn};

const RPC_URL: &str = "https://polygon-rpc.com";

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 value) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC1155 {
        function setApprovalForAll(address operator, bool approved) external;
        function isApprovedForAll(address account, address operator) external view returns (bool);
    }
}

// USDC.e on Polygon (used by Polymarket as collateral)
const USDC_ADDRESS: alloy::primitives::Address =
    alloy::primitives::address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

#[allow(dead_code)]
/// Check whether all required approvals are in place for `wallet`.
///
/// Returns `true` if every approval is set, `false` if any is missing.
pub async fn check_approvals(wallet: alloy::primitives::Address) -> bool {
    let provider = match ProviderBuilder::new().connect(RPC_URL).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Could not connect to Polygon RPC for approval check");
            return false;
        }
    };

    let config = match contract_config(POLYGON, false) {
        Some(c) => c,
        None => { warn!("No contract config for POLYGON"); return false; }
    };
    let neg_config = match contract_config(POLYGON, true) {
        Some(c) => c,
        None => { warn!("No neg-risk contract config for POLYGON"); return false; }
    };

    let mut targets: Vec<(&str, alloy::primitives::Address)> = vec![
        ("CTF Exchange",          config.exchange),
        ("Neg Risk CTF Exchange", neg_config.exchange),
    ];
    if let Some(adapter) = neg_config.neg_risk_adapter {
        targets.push(("Neg Risk Adapter", adapter));
    }

    let usdc = IERC20::new(USDC_ADDRESS, provider.clone());
    let ctf  = IERC1155::new(config.conditional_tokens, provider.clone());

    let mut all_ok = true;
    for (name, target) in &targets {
        let usdc_ok = usdc.allowance(wallet, *target).call().await
            .map(|a| a > U256::ZERO)
            .unwrap_or(false);
        let ctf_ok = ctf.isApprovedForAll(wallet, *target).call().await
            .unwrap_or(false);

        info!(
            contract = name,
            usdc_approved = usdc_ok,
            ctf_approved  = ctf_ok,
            "Approval status"
        );

        if !usdc_ok || !ctf_ok {
            all_ok = false;
        }
    }
    all_ok
}

/// Ensure all required approvals are set, sending transactions for any that are missing.
///
/// Uses `signer` both for signing transactions AND as the wallet whose approvals
/// are being set — i.e. `signer.address()` must be the trading wallet.
///
/// This is safe to call on every startup: it checks the current on-chain state
/// and only sends transactions when an approval is missing.
pub async fn ensure_approvals(signer: Arc<PrivateKeySigner>) {
    let owner = signer.address();
    info!(wallet = %owner, "Checking on-chain approvals...");

    let provider = match ProviderBuilder::new()
        .wallet((*signer).clone())
        .connect(RPC_URL)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Could not connect to Polygon RPC — skipping approval setup");
            return;
        }
    };

    let config = match contract_config(POLYGON, false) {
        Some(c) => c,
        None => { warn!("No contract config for POLYGON — skipping approvals"); return; }
    };
    let neg_config = match contract_config(POLYGON, true) {
        Some(c) => c,
        None => { warn!("No neg-risk contract config — skipping approvals"); return; }
    };

    let mut targets: Vec<(&str, alloy::primitives::Address)> = vec![
        ("CTF Exchange",          config.exchange),
        ("Neg Risk CTF Exchange", neg_config.exchange),
    ];
    if let Some(adapter) = neg_config.neg_risk_adapter {
        targets.push(("Neg Risk Adapter", adapter));
    }

    let usdc = IERC20::new(USDC_ADDRESS, provider.clone());
    let ctf  = IERC1155::new(config.conditional_tokens, provider.clone());

    for (name, target) in &targets {
        // --- ERC-20 USDC approval ---
        let usdc_ok = usdc.allowance(owner, *target).call().await
            .map(|a| a > U256::ZERO)
            .unwrap_or(false);

        if !usdc_ok {
            info!(contract = name, "Setting USDC approval...");
            match usdc.approve(*target, U256::MAX).send().await {
                Ok(pending) => {
                    match pending.watch().await {
                        Ok(tx) => info!(contract = name, tx = %tx, "USDC approved"),
                        Err(e) => warn!(contract = name, error = %e, "USDC approve tx failed"),
                    }
                }
                Err(e) => warn!(contract = name, error = %e, "Failed to send USDC approve tx"),
            }
        } else {
            info!(contract = name, "USDC approval already set");
        }

        // --- ERC-1155 CTF approval ---
        let ctf_ok = ctf.isApprovedForAll(owner, *target).call().await
            .unwrap_or(false);

        if !ctf_ok {
            info!(contract = name, "Setting CTF setApprovalForAll...");
            match ctf.setApprovalForAll(*target, true).send().await {
                Ok(pending) => {
                    match pending.watch().await {
                        Ok(tx) => info!(contract = name, tx = %tx, "CTF approved"),
                        Err(e) => warn!(contract = name, error = %e, "CTF approve tx failed"),
                    }
                }
                Err(e) => warn!(contract = name, error = %e, "Failed to send CTF approve tx"),
            }
        } else {
            info!(contract = name, "CTF approval already set");
        }
    }

    info!(wallet = %owner, "Approval setup complete");
}
