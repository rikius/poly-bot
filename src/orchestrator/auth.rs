//! Authentication and portfolio initialisation.
//!
//! Single entry-point for everything that must happen before the bot can trade:
//!
//! 1. Derive L2 HMAC credentials from the private key (EIP-712 signature).
//! 2. Build an authenticated CLOB client.
//! 3. Refresh the exchange's on-chain allowance cache (prevents balance=0 errors).
//! 4. Read the current USDC portfolio balance from the CLOB.
//! 5. Load recent confirmed trades for position reconciliation after restart.
//!
//! The resulting [`AuthComponents`] is handed to [`Bot::new`] which uses it to
//! build the order executor and seed the internal ledger.

use crate::config::Config;
use crate::ledger::Fill;
use crate::websocket::types::Side;
use alloy_signer_local::PrivateKeySigner;
use chrono::Utc;
use polymarket_client_sdk::auth::{Credentials, Normal, Signer as _};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::clob::types::AssetType;
use polymarket_client_sdk::clob::types::TradeStatusType;
use polymarket_client_sdk::clob::types::request::{
    BalanceAllowanceRequest, TradesRequest, UpdateBalanceAllowanceRequest,
};
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::POLYGON;
use rust_decimal::Decimal;
use std::str::FromStr as _;
use std::sync::Arc;
use tracing::{error, info, warn};

// ============================================================================
// AUTH COMPONENTS
// ============================================================================

/// Everything produced by a successful authentication sequence.
///
/// Passed into [`Bot::new`] so the bot can build its order executor and seed
/// the ledger without repeating any network round-trips.
pub struct AuthComponents {
    /// Fully authenticated CLOB client ready for order submission.
    pub clob_client: ClobClient<Authenticated<Normal>>,

    /// Separate authenticated client for the background sync task.
    /// Kept distinct from `clob_client` so sync REST calls never block order submission.
    pub sync_client: ClobClient<Authenticated<Normal>>,

    /// EIP-712 signer derived from the private key.
    pub signer: Arc<PrivateKeySigner>,

    /// L2 HMAC credentials (API key / secret / passphrase).
    pub creds: Credentials,

    /// USDC portfolio balance read from the CLOB at startup.
    /// `None` if the balance endpoint was unavailable.
    /// Used to seed the internal ledger with the real account value.
    pub portfolio_usdc: Option<Decimal>,

    /// Recent confirmed fills loaded from the CLOB REST API at startup.
    /// Processed into the ledger so positions are restored after a bot restart.
    pub recent_fills: Vec<Fill>,
}

// ============================================================================
// AUTHENTICATE
// ============================================================================

/// Authenticate against the Polymarket CLOB and return ready-to-use components.
///
/// Steps performed (all against `endpoint_url`):
/// - Derive L2 API credentials from the private key
/// - Build an authenticated CLOB client
/// - Refresh the exchange's allowance cache so balance reads are accurate
/// - Read the current USDC balance and return it alongside the client
///
/// Errors are returned as `anyhow::Error` so the caller can decide whether to
/// abort or continue in paper/simulation mode.
pub async fn authenticate(
    config: &Config,
    endpoint_url: &str,
) -> anyhow::Result<AuthComponents> {
    let private_key = config
        .private_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("PRIVATE_KEY is required for authentication"))?;

    // ── 1. Build signer ───────────────────────────────────────────────────────
    let signer = Arc::new(
        PrivateKeySigner::from_str(private_key)
            .map_err(|e| anyhow::anyhow!("Invalid PRIVATE_KEY: {}", e))?
            .with_chain_id(Some(POLYGON)),
    );

    // ── 2. Unauthenticated client (needed for key derivation) ─────────────────
    let unauth_client = ClobClient::new(endpoint_url, ClobConfig::default())
        .map_err(|e| anyhow::anyhow!("Failed to create CLOB client: {}", e))?;

    // ── 3. Derive L2 credentials ──────────────────────────────────────────────
    let creds = unauth_client
        .derive_api_key(signer.as_ref(), None)
        .await
        .map_err(|e| anyhow::anyhow!(
            "Failed to derive API key — ensure the wallet is registered on Polymarket: {}",
            e
        ))?;

    info!(api_key = %creds.key(), "L2 credentials derived");

    // ── 4. Authenticate ───────────────────────────────────────────────────────
    let clob_client = unauth_client
        .authentication_builder(signer.as_ref())
        .credentials(creds.clone())
        .signature_type(SignatureType::Proxy)
        .authenticate()
        .await
        .map_err(|e| anyhow::anyhow!("Authentication failed: {}", e))?;

    info!("CLOB client authenticated");

    // ── 4b. Second client for the background sync task ───────────────────────
    // Keeps sync REST calls on a separate connection so they never block
    // the hot path of the order executor.
    let unauth_sync = ClobClient::new(endpoint_url, ClobConfig::default())
        .map_err(|e| anyhow::anyhow!("Failed to create sync CLOB client: {}", e))?;
    let sync_client = unauth_sync
        .authentication_builder(signer.as_ref())
        .credentials(creds.clone())
        .signature_type(SignatureType::Proxy)
        .authenticate()
        .await
        .map_err(|e| anyhow::anyhow!("Sync client authentication failed: {}", e))?;
    info!("Sync CLOB client authenticated");

    // ── 5. Refresh allowance cache ────────────────────────────────────────────
    // The CLOB caches on-chain allowances.  Calling update_balance_allowance
    // forces a re-read so the exchange won't report balance=0 on the first order.
    if let Err(e) = clob_client
        .update_balance_allowance(
            UpdateBalanceAllowanceRequest::builder()
                .asset_type(AssetType::Collateral)
                .build(),
        )
        .await
    {
        warn!(error = %e, "Could not refresh CLOB allowance cache — first orders may be rejected");
    } else {
        info!("CLOB allowance cache refreshed");
    }

    // ── 6. Read portfolio balance ─────────────────────────────────────────────
    let portfolio_usdc = read_portfolio_balance(&clob_client).await;

    // ── 7. Load recent trades for position reconciliation ─────────────────────
    let recent_fills = load_recent_trades(&clob_client).await;

    Ok(AuthComponents {
        clob_client,
        sync_client,
        signer,
        creds,
        portfolio_usdc,
        recent_fills,
    })
}

// ============================================================================
// PORTFOLIO BALANCE
// ============================================================================

/// Read the current USDC balance from the CLOB balance_allowance endpoint.
///
/// Returns `None` if the endpoint fails or returns zero (e.g. allowance not
/// yet set — in that case the caller falls back to `INITIAL_CASH_USD`).
async fn read_portfolio_balance(
    clob_client: &ClobClient<Authenticated<Normal>>,
) -> Option<Decimal> {
    match clob_client
        .balance_allowance(BalanceAllowanceRequest::default())
        .await
    {
        Ok(b) => {
            // b.balance is in raw USDC units (6 decimals), e.g. 7119324 = $7.119324
            let balance = b.balance / Decimal::from(1_000_000u64);
            if balance > Decimal::ZERO {
                info!(balance_usdc = %balance, "Portfolio balance read from CLOB");
                Some(balance)
            } else {
                warn!(
                    raw_balance = %b.balance,
                    "CLOB reports USDC balance = 0 — allowance may not be set. \
                     Complete 'Enable Trading' on polymarket.com or set INITIAL_CASH_USD."
                );
                None
            }
        }
        Err(e) => {
            error!(error = %e, "Could not read portfolio balance from CLOB");
            None
        }
    }
}

// ============================================================================
// RECENT TRADES
// ============================================================================

/// Load confirmed fills from the last 2 hours via the CLOB REST API.
///
/// This restores in-memory positions after a bot restart — without it, any
/// existing position is invisible until the next WS fill notification arrives.
/// The fills are fed through `Ledger::process_fill` which deduplicates by
/// fill_id, so any overlap with subsequent WS-delivered fills is harmless.
async fn load_recent_trades(clob_client: &ClobClient<Authenticated<Normal>>) -> Vec<Fill> {
    let after = Utc::now().timestamp() - 7_200; // look back 2 hours

    let request = TradesRequest::builder()
        .after(after)
        .build();

    match clob_client.trades(&request, None).await {
        Ok(page) => {
            let count = page.data.len();
            let fills: Vec<Fill> = page
                .data
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        TradeStatusType::Confirmed
                            | TradeStatusType::Mined
                            | TradeStatusType::Matched
                    )
                })
                .filter_map(|t| {
                    let side = match t.side {
                        polymarket_client_sdk::clob::types::Side::Buy => Side::Buy,
                        polymarket_client_sdk::clob::types::Side::Sell => Side::Sell,
                        _ => return None,
                    };
                    let notional = (t.price * t.size).round_dp(4);
                    let fee = notional * t.fee_rate_bps / Decimal::from(10_000u32);
                    Some(Fill {
                        fill_id: t.id.clone(),
                        order_id: t.taker_order_id.clone(),
                        token_id: t.asset_id.to_string(),
                        side,
                        price: t.price,
                        size: t.size,
                        fee,
                        expected_price: None,
                        slippage_cost: Decimal::ZERO,
                        timestamp: t.match_time,
                    })
                })
                .collect();

            info!(
                total = count,
                loaded = fills.len(),
                "Recent trades loaded for position reconciliation"
            );
            fills
        }
        Err(e) => {
            warn!(error = %e, "Could not load recent trades — positions will be empty until next WS fill");
            vec![]
        }
    }
}
