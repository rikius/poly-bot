//! Background sync — periodic cash balance and open-order reconciliation.
//!
//! Runs as an independent tokio task alongside the main trading loop.
//! Three responsibilities:
//!
//! 1. **Cash sync** (every 30s) — reads the real USDC balance from the exchange
//!    and corrects any drift in `ledger.cash` caused by missed fills/cancels.
//!
//! 2. **Order reconciliation** (every 60s) — fetches all open orders from the
//!    exchange and cancels any that our ledger still tracks as active but the
//!    server no longer has (ghost orders caused by missed WS events).
//!
//! 3. **Position sync + redemption** (every 60s) — checks on-chain conditional
//!    token balances.  When a market has resolved and we still hold winning tokens
//!    on-chain, submits a `redeemPositions()` transaction to claim the USDC payout.

use crate::claim::{try_redeem, RedeemRequest};
use crate::ledger::Ledger;
use crate::strategy::MarketPairRegistry;
use crate::websocket::GammaClient;
use alloy_signer_local::PrivateKeySigner;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::Client as ClobClient;
use polymarket_client_sdk::clob::types::request::{BalanceAllowanceRequest, OrdersRequest};
use polymarket_client_sdk::clob::types::AssetType;
use polymarket_client_sdk::types::U256;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

const CASH_SYNC_SECS: u64 = 30;
const ORDER_SYNC_SECS: u64 = 60;
const POSITION_SYNC_SECS: u64 = 60;

/// Background sync task.
pub struct SyncTask {
    client: ClobClient<Authenticated<Normal>>,
    ledger: Arc<Ledger>,
    /// Market pair registry — used to look up condition_id and first token for redemption.
    market_registry: Arc<MarketPairRegistry>,
    /// EIP-712 signer — used to sign on-chain redemption transactions.
    signer: Option<Arc<PrivateKeySigner>>,
    /// Polygon RPC URL for on-chain redemptions.
    polygon_rpc_url: String,
    /// Whether on-chain redemption is enabled.
    redeem_enabled: bool,
    /// Snapshot of all known token → market metadata, captured at startup.
    ///
    /// The live registry drops markets once they expire/rotate out of discovery.
    /// We keep a permanent snapshot so we can still redeem positions from markets
    /// that were active at startup but have since closed.
    /// token_id → condition_id snapshot for post-expiry redemption.
    token_market_cache: std::collections::HashMap<String, String>,
}

impl SyncTask {
    pub fn new(
        client: ClobClient<Authenticated<Normal>>,
        ledger: Arc<Ledger>,
        market_registry: Arc<MarketPairRegistry>,
        signer: Option<Arc<PrivateKeySigner>>,
        polygon_rpc_url: String,
        redeem_enabled: bool,
    ) -> Self {
        // Build the token → condition_id snapshot.
        let mut token_market_cache = std::collections::HashMap::new();
        for pair in market_registry.all_pairs() {
            token_market_cache.insert(pair.yes_token_id.clone(), pair.condition_id.clone());
            token_market_cache.insert(pair.no_token_id.clone(), pair.condition_id.clone());
        }
        Self { client, ledger, market_registry, signer, polygon_rpc_url, redeem_enabled, token_market_cache }
    }

    /// Run the sync loop indefinitely.  Spawn this in a `tokio::spawn`.
    pub async fn run(self) {
        let mut cash_tick = interval(Duration::from_secs(CASH_SYNC_SECS));
        let mut order_tick = interval(Duration::from_secs(ORDER_SYNC_SECS));
        let mut position_tick = interval(Duration::from_secs(POSITION_SYNC_SECS));

        // Skip the immediate first ticks — startup reconciliation is handled
        // separately in auth.rs / setup.rs.
        cash_tick.tick().await;
        order_tick.tick().await;
        // Run position sync immediately on first tick to clear stale positions
        // from previous sessions (e.g. expired 15-min markets).

        info!(
            "Sync task running (cash={}s, orders={}s, positions={}s)",
            CASH_SYNC_SECS, ORDER_SYNC_SECS, POSITION_SYNC_SECS
        );

        loop {
            tokio::select! {
                _ = cash_tick.tick() => {
                    self.sync_cash().await;
                }
                _ = order_tick.tick() => {
                    self.sync_orders().await;
                }
                _ = position_tick.tick() => {
                    self.sync_positions().await;
                }
            }
        }
    }

    // ── Cash sync ────────────────────────────────────────────────────────────

    async fn sync_cash(&self) {
        match self.client
            .balance_allowance(BalanceAllowanceRequest::default())
            .await
        {
            Ok(b) => {
                // balance is in raw USDC units (6 decimals)
                let exchange_total = b.balance / Decimal::from(1_000_000u64);
                let threshold = dec!(0.01);

                match self.ledger.cash.sync_from_exchange(exchange_total, threshold) {
                    Some(drift) => {
                        warn!(
                            drift_usdc = %drift,
                            exchange_total = %exchange_total,
                            ledger_total = %self.ledger.cash.total(),
                            "Cash drift corrected"
                        );
                    }
                    None => {
                        debug!(exchange_total = %exchange_total, "Cash sync: within tolerance");
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "Cash sync: failed to read balance from exchange");
            }
        }
    }

    // ── Order reconciliation ─────────────────────────────────────────────────

    async fn sync_orders(&self) {
        // Collect all exchange-side open order IDs (paginated).
        let mut server_ids: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;

        loop {
            let request = OrdersRequest::builder().build();
            match self.client.orders(&request, cursor.clone()).await {
                Ok(page) => {
                    for order in &page.data {
                        server_ids.insert(order.id.clone());
                    }
                    // "LTE=" is the SDK's sentinel for "no more pages"
                    if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
                        break;
                    }
                    cursor = Some(page.next_cursor);
                }
                Err(e) => {
                    // Bail on partial data — acting on an incomplete server list
                    // would incorrectly cancel valid orders.
                    error!(error = %e, "Order sync: fetch failed, skipping reconciliation");
                    return;
                }
            }
        }

        let local_open = self.ledger.orders.all_open();
        let mut ghost_count = 0usize;

        for order in &local_open {
            let Some(ref exchange_id) = order.order_id else {
                // Pre-ack order — ack may still be in flight, skip.
                continue;
            };

            if !server_ids.contains(exchange_id) {
                if self.ledger.apply_server_cancel(exchange_id) {
                    ghost_count += 1;
                    warn!(
                        order_id = %exchange_id,
                        token = %&order.token_id[..order.token_id.len().min(12)],
                        remaining = %order.remaining_notional(),
                        "Ghost order cancelled: not found on exchange"
                    );
                }
            }
        }

        info!(
            server_open = server_ids.len(),
            local_open = local_open.len(),
            ghosts_cancelled = ghost_count,
            "Order sync complete"
        );
    }

    // ── Position validation + redemption ────────────────────────────────────

    /// Validate each open position against the actual on-chain conditional token
    /// balance.
    ///
    /// Two outcomes:
    /// - **On-chain balance = 0, ledger > 0**: market resolved and tokens were
    ///   already auto-redeemed (or sold externally without a WS event) — zero out
    ///   the ledger position.
    /// - **On-chain balance > 0, market closed**: we hold winning tokens that have
    ///   not yet been redeemed.  Submit `redeemPositions()` to claim the USDC payout,
    ///   then zero out the ledger position.
    async fn sync_positions(&self) {
        let positions = self.ledger.positions.all_positions();
        if positions.is_empty() {
            return;
        }

        let mut zeroed = 0usize;
        let mut redeemed = 0usize;

        for pos in &positions {
            let token_u256 = match pos.token_id.parse::<U256>() {
                Ok(v) => v,
                Err(_) => {
                    debug!(token = %pos.token_id, "Position sync: skipping non-numeric token_id");
                    continue;
                }
            };

            let request = BalanceAllowanceRequest::builder()
                .asset_type(AssetType::Conditional)
                .token_id(token_u256)
                .build();

            match self.client.balance_allowance(request).await {
                Ok(b) => {
                    // Conditional tokens use 6-decimal scaling (same as USDC collateral)
                    let onchain_shares = b.balance / Decimal::from(1_000_000u64);
                    let ledger_shares = pos.shares.abs();

                    if onchain_shares == Decimal::ZERO && ledger_shares > Decimal::ZERO {
                        // Tokens are gone — market auto-redeemed or sold externally.
                        self.ledger.positions.force_close(&pos.token_id);
                        zeroed += 1;
                        warn!(
                            token = %&pos.token_id[..pos.token_id.len().min(12)],
                            ledger_shares = %ledger_shares,
                            "Position zeroed: on-chain balance is 0 (market resolved or sold externally)"
                        );
                    } else if onchain_shares > Decimal::ZERO {
                        // We still hold on-chain tokens.
                        // Only attempt redemption if the market is no longer in the
                        // live registry — i.e., it has expired/rotated out of discovery.
                        // If it's still active, the contract would revert anyway.
                        let market_still_active = self.market_registry
                            .get_by_token(&pos.token_id)
                            .is_some();

                        if market_still_active {
                            debug!(
                                token = %&pos.token_id[..pos.token_id.len().min(12)],
                                onchain = %onchain_shares,
                                ledger = %ledger_shares,
                                "Position sync: ok (market still active)"
                            );
                        } else {
                            // Market has expired — try to claim winning tokens.
                            info!(
                                token = %&pos.token_id[..pos.token_id.len().min(12)],
                                onchain = %onchain_shares,
                                "Position sync: market expired, attempting redemption"
                            );
                            if self.try_claim_position(&pos.token_id, onchain_shares).await {
                                redeemed += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        token = %&pos.token_id[..pos.token_id.len().min(12)],
                        "Position sync: failed to read on-chain balance"
                    );
                }
            }
        }

        if zeroed > 0 || redeemed > 0 || !positions.is_empty() {
            info!(
                checked = positions.len(),
                zeroed,
                redeemed,
                "Position sync complete"
            );
        }
    }

    /// Attempt to redeem a resolved position on-chain.
    ///
    /// Uses the startup-time token→market cache so we can still redeem positions
    /// from markets that have since expired out of the live registry.
    ///
    /// Returns `true` if redemption was attempted (not necessarily successful).
    /// Returns `false` if prerequisites are missing (no signer, market not cached).
    async fn try_claim_position(&self, token_id: &str, onchain_shares: Decimal) -> bool {
        let Some(ref signer) = self.signer else {
            debug!(
                token = %&token_id[..token_id.len().min(12)],
                "Claim skipped: no signer available (simulation mode)"
            );
            return false;
        };

        // Look up condition_id — first try the startup-time snapshot, then fall back to Gamma API.
        let condition_id = match self.token_market_cache.get(token_id).cloned() {
            Some(cid) => cid,
            None => {
                info!(
                    token = %&token_id[..token_id.len().min(12)],
                    "Token not in startup cache — querying Gamma API for condition_id"
                );
                match GammaClient::new().condition_id_for_token(token_id).await {
                    Ok(Some(cid)) => {
                        info!(
                            token = %&token_id[..token_id.len().min(12)],
                            condition_id = %cid,
                            "Gamma API resolved condition_id for expired market"
                        );
                        cid
                    }
                    Ok(None) => {
                        warn!(
                            token = %&token_id[..token_id.len().min(12)],
                            onchain_shares = %onchain_shares,
                            "Claim skipped: condition_id not found via Gamma API"
                        );
                        return false;
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            token = %&token_id[..token_id.len().min(12)],
                            "Claim skipped: Gamma API lookup failed"
                        );
                        return false;
                    }
                }
            }
        };

        let req = RedeemRequest {
            condition_id,
            shares: onchain_shares,
        };

        if !self.redeem_enabled {
            info!(
                condition_id = %req.condition_id,
                token = %&token_id[..token_id.len().min(12)],
                "Redemption disabled (REDEEM_ENABLED=false) — skipping on-chain claim"
            );
            return false;
        }

        if let Some(usdc) = try_redeem(Arc::clone(signer), &req, &self.polygon_rpc_url).await {
            // Credit the redeemed USDC to the ledger and zero out the position.
            self.ledger.cash.deposit(usdc);
            let token_id_owned = token_id.to_string();
            self.ledger.positions.force_close(&token_id_owned);
            info!(
                token = %&token_id[..token_id.len().min(12)],
                usdc = %usdc,
                "Position redeemed and ledger updated"
            );
        }

        true
    }
}
