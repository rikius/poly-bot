# SDK Migration Phase 2 — Remove duplicate REST/auth infrastructure

Phase 1 replaced signing, WebSocket, and order execution with the SDK.
This phase removes the now-redundant REST client, HMAC auth, and endpoint wrappers.

> **Windows note**: Use `docker compose run --rm dev cargo check` to compile.
> Rebuild with `docker compose build dev` after Cargo.toml changes.

---

## What stays

| File | Reason |
|------|--------|
| `api/types.rs` | String type aliases (`TokenId`, `Side`, `PriceLevel`, etc.) used by 20+ files — not worth replacing |
| `api/gamma.rs` | Gamma API for market discovery — not in SDK |
| `api/discovery.rs` | Bot-specific market discovery logic using Gamma |

## What goes

| Step | File(s) | Action | Lines removed |
|------|---------|--------|---------------|
| 1 | `execution/cancellation.rs` | Rewrite: `Arc<ApiClient>` → `ClobClient<Authenticated<Normal>>` | ~0 (rewrite) |
| 2 | `risk/reconciliation.rs` | Rewrite: `Arc<ApiClient>` → `ClobClient<Authenticated<Normal>>` | ~0 (rewrite) |
| 3 | `api/auth.rs` | Delete — no consumers after steps 1-2 | 177 |
| 4 | `api/client.rs` | Delete — no consumers after steps 1-2 | 252 |
| 5 | `api/endpoints.rs` | Delete — no consumers after steps 1-2 | 478 |
| 6 | `api/mod.rs` | Trim re-exports to only `types`, `gamma`, `discovery` | ~15 |
| 7 | Cargo.toml | Remove `hmac`, `sha2`, `base64`, `hex` (only used by auth.rs) | 4 deps |
| 8 | Verify | `docker compose run --rm dev cargo test` — all 140 tests pass | — |

**Total: ~900 lines removed, 4 unused dependencies dropped**

---

## Step 1 — Rewrite `execution/cancellation.rs`

**Current**: `CancellationManager` holds `Arc<ApiClient>`, calls `api_client.cancel_order(order_id)`.

**New**: Hold `ClobClient<Authenticated<Normal>>`, call `clob_client.cancel_order(order_id)`.

SDK method: `Client::cancel_order(&self, order_id: &str) -> Result<CancelOrdersResponse>`

## Step 2 — Rewrite `risk/reconciliation.rs`

**Current**: `ReconciliationLoop` holds `Arc<ApiClient>`, calls `client.get_orders()` which returns `Vec<OpenOrder>`.

**New**: Hold `ClobClient<Authenticated<Normal>>`, call `clob_client.orders(&request)` which returns `Vec<OpenOrderResponse>`.

SDK method: `Client::orders(&self, request: &OrdersRequest) -> Result<Vec<OpenOrderResponse>>`

Key field mapping (`OpenOrder` → `OpenOrderResponse`):
- `id` → `id` (String)
- `status` → `status` (OrderStatusType — use `.to_string()`)
- `size_matched` → `size_matched` (Decimal)

## Steps 3-5 — Delete dead modules

After steps 1-2, `ApiClient`, `ApiCredentials`, `AuthHeaders`, and all endpoint wrappers have zero consumers.
Delete `api/auth.rs`, `api/client.rs`, `api/endpoints.rs`.

## Step 6 — Trim `api/mod.rs`

Remove `pub mod auth`, `pub mod client`, `pub mod endpoints` and their re-exports.
Keep only `pub mod types`, `pub mod gamma`, `pub mod discovery`.

## Step 7 — Remove unused Cargo.toml deps

`hmac`, `sha2`, `base64`, `hex` were only used by `api/auth.rs` for HMAC signing.
The SDK handles all auth internally.

---

## Progress

| Step | Status |
|------|--------|
| 1 | ✅ Done |
| 2 | ✅ Done |
| 3 | ✅ Done |
| 4 | ✅ Done |
| 5 | ✅ Done |
| 6 | ✅ Done |
| 7 | ✅ Done |
| 8 | ✅ Done — 128 tests pass, 0 warnings |
