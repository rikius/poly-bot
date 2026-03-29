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
//! ## Direct wallet (EOA)
//! When `WALLET_ADDRESS` matches the address derived from `PRIVATE_KEY`, approvals
//! are set automatically on startup.
//!
//! ## Proxy wallet
//! When `WALLET_ADDRESS` is a smart-contract proxy (common with Polymarket), a
//! warning is logged with instructions to complete the one-time setup on polymarket.com.

use alloy::primitives::U256;
use alloy::providers::ProviderBuilder;
use alloy::sol;
use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::{POLYGON, contract_config};
use std::sync::Arc;
use tracing::{info, warn};

/// Free public Polygon RPC endpoints, tried in order until one works.
const RPC_ENDPOINTS: &[&str] = &[
    "https://rpc.ankr.com/polygon",
    "https://polygon.llamarpc.com",
    "https://polygon-bor-rpc.publicnode.com",
];

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

/// Ensure all required approvals are set for `trading_wallet`.
///
/// If `signer.address() == trading_wallet`, sends missing approval transactions.
/// If they differ (proxy wallet pattern), logs a clear warning with manual steps.
pub async fn ensure_approvals(
    signer: Arc<PrivateKeySigner>,
    trading_wallet: alloy::primitives::Address,
) {
    let signer_addr = signer.address();

    // ── Proxy wallet detection ─────────────────────────────────────────────────
    if signer_addr != trading_wallet {
        warn!(
            signer = %signer_addr,
            proxy  = %trading_wallet,
            "WALLET_ADDRESS is a proxy contract — cannot auto-set approvals"
        );
        warn!("To enable trading, complete the one-time setup on polymarket.com:");
        warn!("  1. Go to https://polymarket.com and connect your wallet");
        warn!("  2. Complete any deposit or first trade (this sets on-chain approvals)");
        warn!("  3. Restart the bot after setup is done");
        warn!(
            "Alternative: set WALLET_ADDRESS={} to use the EOA directly \
             (move USDC to that address first)",
            signer_addr
        );
        return;
    }

    // ── Direct EOA wallet ──────────────────────────────────────────────────────
    info!(wallet = %signer_addr, "Checking on-chain approvals...");

    // Try each RPC endpoint until one connects.
    let mut provider = None;
    for &url in RPC_ENDPOINTS {
        match ProviderBuilder::new()
            .wallet((*signer).clone())
            .connect(url)
            .await
        {
            Ok(p) => {
                info!(rpc = %url, "Connected to Polygon RPC");
                provider = Some(p);
                break;
            }
            Err(e) => warn!(rpc = %url, error = %e, "RPC unavailable, trying next..."),
        }
    }
    let Some(provider) = provider else {
        warn!("All Polygon RPC endpoints failed — skipping approval setup");
        return;
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
        // ERC-20 USDC approval
        let usdc_ok = usdc.allowance(signer_addr, *target).call().await
            .map(|a| a > U256::ZERO)
            .unwrap_or(false);
        if !usdc_ok {
            info!(contract = name, "Setting USDC approval...");
            match usdc.approve(*target, U256::MAX).send().await {
                Ok(pending) => match pending.watch().await {
                    Ok(tx)  => info!(contract = name, tx = %tx, "USDC approved"),
                    Err(e)  => warn!(contract = name, error = %e, "USDC approve tx failed"),
                },
                Err(e) => warn!(contract = name, error = %e, "Failed to send USDC approve tx"),
            }
        } else {
            info!(contract = name, "USDC approval already set");
        }

        // ERC-1155 CTF approval
        let ctf_ok = ctf.isApprovedForAll(signer_addr, *target).call().await
            .unwrap_or(false);
        if !ctf_ok {
            info!(contract = name, "Setting CTF setApprovalForAll...");
            match ctf.setApprovalForAll(*target, true).send().await {
                Ok(pending) => match pending.watch().await {
                    Ok(tx)  => info!(contract = name, tx = %tx, "CTF approved"),
                    Err(e)  => warn!(contract = name, error = %e, "CTF approve tx failed"),
                },
                Err(e) => warn!(contract = name, error = %e, "Failed to send CTF approve tx"),
            }
        } else {
            info!(contract = name, "CTF approval already set");
        }
    }

    info!(wallet = %signer_addr, "Approval setup complete");
}
