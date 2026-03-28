# SDK Migration — polymarket-client-sdk v0.4

This document tracks the step-by-step migration from poly-bot's custom implementations
to the official `polymarket-client-sdk` crate.

**SDK source reference**: `C:\Users\Ramunas\projects\rs-clob-client\src\`

> **Note — Windows development machine**: This project is developed on Windows 11.
> Native `cargo build` / `cargo check` may fail due to C/C++ toolchain differences
> (e.g. `aws-lc-sys`, `ring`, OpenSSL). Always use `docker compose run --rm dev cargo check`
> (or `cargo build`, `cargo test`, `cargo clippy`) to compile inside the Linux container.
> Rebuild the image with `docker compose build dev` after changing `Cargo.toml` or `Dockerfile`.

---

## Why migrate?

| Area | poly-bot bug / problem | SDK fix |
|---|---|---|
| **Auth base64** | Uses `base64::STANDARD` to decode/encode HMAC — Polymarket expects `URL_SAFE` | SDK uses `URL_SAFE` throughout (`auth.rs:415`) |
| **Secret exposure** | `ApiCredentials` derives `Debug` — secrets print in logs/panics | SDK wraps in `SecretString` (redacted in `Debug`) |
| **WS PING frame** | Sends `Message::Ping(vec![])` (binary) — Polymarket protocol expects TEXT `"PING"` | SDK's `ConnectionManager` sends correct TEXT frame |
| **WS PONG timeout** | No detection of dead connections (sends pings, never checks if PONG arrives) | SDK has 15s PONG timeout, reconnects automatically |
| **Order salt entropy** | `timestamp ^ (timestamp >> 17) ^ (timestamp << 7)` — predictable, low-entropy | SDK uses `rand::rng().random::<u64>()` (`generate_seed`) |
| **EIP-712 signing** | 482-line hand-rolled implementation in `src/signing/order.rs` | SDK uses alloy `sol!` macro + `client.sign(signer, order)` |
| **Maintenance burden** | Custom HMAC, signing, WS reconnect code to maintain forever | All handled by SDK |

---

## Progress

| Step | Status | Area |
|---|---|---|
| 1 | ✅ Done | Add SDK to Cargo.toml; upgrade Dockerfile rust:1.88 → rust:1.91 |
| 2 | ✅ Done | Fix `src/api/auth.rs` base64 STANDARD → URL_SAFE |
| 3 | ✅ Done | Replace `src/websocket/market.rs` with SDK `clob::ws::Client` |
| 4 | ✅ Done | Replace `src/websocket/user.rs` with SDK authenticated WS |
| 5 | ✅ Done | Replace `src/execution/executor.rs` with SDK `OrderBuilder` + `sign()` + `post_order()` |
| 6 | ✅ Done | Update `src/bot.rs` — SDK `ClobClient`, `PrivateKeySigner`, `Credentials`; remove `OrderSigner` |
| 7 | ✅ Done | Clean up: removed `src/signing/`, `tokio-tungstenite`, `futures-util`, `simd-json`, `sol-types` |

---

## Step 1 — Add SDK + upgrade Rust ✅

**What changed:**
- `Cargo.toml`: added `polymarket-client-sdk = { version = "0.4", features = ["ws", "gamma", "data", "tracing"] }`
- `Dockerfile`: `FROM rust:1.88-slim` → `FROM rust:1.91-slim`
  - Reason: SDK pulls in `alloy v1.8.3` which requires `rustc >= 1.91`

**Verify:**
```bash
docker compose run --rm dev cargo build
```

---

## Step 2 — Replace auth ⬜

### Current code (poly-bot): `src/api/auth.rs`

```rust
// BUG: STANDARD instead of URL_SAFE
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

#[derive(Debug, Clone)]   // BUG: Debug exposes secret/passphrase in logs
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,    // plaintext secret in memory
    pub passphrase: String,
    pub wallet_address: String,
}

// Decode secret with STANDARD, then encode result with STANDARD
let key = BASE64.decode(&self.secret)...;
let signature = BASE64.encode(result.into_bytes());
```

### SDK equivalent: `src/auth.rs`

```rust
use base64::engine::general_purpose::URL_SAFE;  // correct
use secrecy::SecretString;                        // redacted in Debug

pub struct Credentials {
    pub(crate) key: ApiKey,          // Uuid
    pub(crate) secret: SecretString, // never printed
    pub(crate) passphrase: SecretString,
}

impl Credentials {
    pub fn new(key: Uuid, secret: String, passphrase: String) -> Self { ... }
}

// HMAC uses URL_SAFE decode + URL_SAFE encode
fn hmac(secret: &SecretString, message: &str) -> Result<String> {
    let decoded_secret = URL_SAFE.decode(secret.expose_secret())?;
    ...
    Ok(URL_SAFE.encode(result))
}
```

### Migration plan

1. Remove `src/api/auth.rs`
2. In `src/config.rs`: parse `POLYMARKET_API_KEY` as `Uuid`, keep `secret` and `passphrase` as `String`
3. In `src/api/client.rs`:
   - Remove `use crate::api::auth::{ApiCredentials, AuthHeaders}`
   - Store `polymarket_client_sdk::auth::Credentials` instead
   - The SDK `clob::Client` handles header injection automatically — no manual `POLY_SIGNATURE` header building needed
4. Construct SDK client in `src/bot.rs`:

```rust
use polymarket_client_sdk::auth::{Credentials, LocalSigner};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::POLYGON;
use std::str::FromStr as _;

let signer = LocalSigner::from_str(&config.private_key)?
    .with_chain_id(Some(POLYGON));

let credentials = Credentials::new(
    config.api_key,       // Uuid
    config.secret,        // String → wrapped in SecretString internally
    config.passphrase,    // String → wrapped in SecretString internally
);

let clob_client = ClobClient::new(&config.clob_url, ClobConfig::default())?
    .authentication_builder(&signer)
    .credentials(credentials)   // skip the network round-trip to create new key
    .authenticate()
    .await?;
```

5. Remove `hmac`, `sha2`, `base64`, `hex` from `Cargo.toml` (done in Step 7)

**Files changed:** `src/api/auth.rs` (delete), `src/api/client.rs`, `src/config.rs`, `src/bot.rs`

---

## Step 3 — Replace market WebSocket ⬜

### Current code: `src/websocket/market.rs`

Problem areas:
- `write.send(Message::Ping(vec![]))` — binary frame (line 180); Polymarket expects TEXT `"PING"`
- No PONG timeout: if server stops responding, connection stays "alive" indefinitely
- Manual exponential backoff reconnect (~100 lines)
- Manual subscription message construction + JSON serialization

### SDK equivalent: `src/clob/ws/client.rs`

```rust
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::U256;
use futures::StreamExt;
use std::str::FromStr as _;

let ws = WsClient::default(); // connects to wss://ws-subscriptions-clob.polymarket.com

// asset_ids are U256 token IDs
let asset_ids: Vec<U256> = token_ids
    .iter()
    .map(|id| U256::from_str(id).unwrap())
    .collect();

let mut stream = Box::pin(ws.subscribe_orderbook(asset_ids)?);

while let Some(result) = stream.next().await {
    let book_update = result?;
    // book_update: BookUpdate { asset_id, market, bids: Vec<PriceLevel>, asks: Vec<PriceLevel>, timestamp }
}
```

**SDK `BookUpdate` type** (`src/clob/ws/types/response.rs`):
- `asset_id: U256`
- `market: B256` (condition ID)
- `bids: Vec<PriceLevel>` where `PriceLevel { price: Decimal, size: Decimal }`
- `asks: Vec<PriceLevel>`
- `timestamp: u64`

### Migration plan

1. Replace the body of `src/websocket/market.rs` to use `WsClient::subscribe_orderbook()`
2. Map `BookUpdate` → existing `MarketUpdate` type (or adapt state module to accept `BookUpdate` directly)
3. SDK handles: reconnection, TEXT PING/PONG, PONG timeout (15s), backoff with jitter

**Files changed:** `src/websocket/market.rs`

---

## Step 4 — Replace user WebSocket ⬜

### Current code: `src/websocket/user.rs`

Same PING bug as market WS. Also builds L2 auth headers manually using the buggy `STANDARD` base64.

### SDK equivalent

```rust
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::B256;
use std::str::FromStr as _;

// ws_client must be authenticated (from Step 2)
let ws_client = WsClient::default().authenticate(credentials, address)?;

// market condition IDs as B256
let markets: Vec<B256> = market_ids
    .iter()
    .map(|id| B256::from_str(id).unwrap())
    .collect();

// Subscribe to orders only
let mut order_stream = Box::pin(ws_client.subscribe_orders(markets.clone())?);
// Or subscribe to trades only
let mut trade_stream = Box::pin(ws_client.subscribe_trades(markets)?);
// Or both at once
let mut all_events = Box::pin(ws_client.subscribe_user_events(markets)?);
```

**SDK `OrderMessage` type** (`src/clob/ws/types/response.rs`):
- Contains order ID, status, fill info, token ID, side, price, size

**SDK `TradeMessage` type**:
- Contains trade ID, order IDs (maker+taker), price, size, fee

### Migration plan

1. Replace `src/websocket/user.rs` to use `ws_client.subscribe_orders()` and `ws_client.subscribe_trades()`
2. Map `OrderMessage`/`TradeMessage` → existing `FillEvent` type used by the strategy/ledger
3. Share the same authenticated `WsClient` instance between Steps 3 and 4 (SDK allows multiple subscriptions on one client)

**Files changed:** `src/websocket/user.rs`, `src/bot.rs`

---

## Step 5 — Replace order signing ⬜

### Current code: `src/signing/order.rs`

```rust
// BUG: low-entropy salt
fn rand_salt() -> u64 {
    let timestamp = SystemTime::now()...as_nanos() as u64;
    timestamp ^ (timestamp >> 17) ^ (timestamp << 7)  // predictable!
}

// 482 lines of hand-rolled EIP-712 encoding
fn eip712_signing_hash(...) -> [u8; 32] { ... }
```

### SDK equivalent: `src/clob/order_builder.rs` + `client.sign()`

```rust
use polymarket_client_sdk::clob::types::{Side, OrderType};
use polymarket_client_sdk::types::{Decimal, U256};
use std::str::FromStr as _;

// SDK generate_seed uses rand::rng().random::<u64>() — cryptographically strong
let signable = clob_client
    .limit_order()
    .token_id(U256::from_str(&intent.token_id)?)
    .side(if intent.side == Side::Buy { Side::Buy } else { Side::Sell })
    .price(intent.price)
    .size(intent.size)
    .order_type(OrderType::GTC)
    .build()
    .await?;

// Signs with alloy sol! macro EIP-712 — no hand-rolled encoding
let signed = clob_client.sign(&signer, signable).await?;

// Submit
let response = clob_client.post_order(signed).await?;
// Or batch
let responses = clob_client.post_orders(vec![signed_yes, signed_no]).await?;
```

**SDK `SignedOrder`** is what `post_order` accepts — produced by `client.sign()`.

**SDK salt generation** (`order_builder.rs:6-7`):
```rust
use rand::RngExt as _;
// generate_seed() = rand::rng().random::<f64>() reinterpreted as u64 per IEEE 754
```

### Migration plan

1. Delete `src/signing/` directory entirely
2. In `src/execution/executor.rs`: replace `OrderBuilder` (custom) with `clob_client.limit_order()` chain
3. Pass the authenticated `clob_client` (from Step 2) into the executor
4. Remove `alloy` direct dependency from `Cargo.toml` (SDK re-exports what's needed)

**Files changed:** `src/signing/` (delete), `src/execution/executor.rs`

---

## Step 6 — Replace executor HTTP calls ⬜

### Current code: `src/execution/executor.rs`

Calls `ApiClient::post_authenticated("/order", &json_body)` manually.

### SDK equivalent

Already covered in Step 5 — `clob_client.post_order(signed)` and `clob_client.post_orders(vec![...])`.

The SDK client:
- Builds the request internally
- Injects all L2 auth headers automatically (with correct URL_SAFE base64)
- Returns typed `PostOrderResponse` (not raw `reqwest::Response`)

### Migration plan

1. Remove `ApiClient::post_authenticated` calls from executor
2. Pass `Arc<ClobClient<Authenticated<Normal>>>` to executor
3. Use `clob_client.post_orders(batch)` for the paired YES+NO arbitrage orders
4. Map `PostOrderResponse` → existing `ExecutionResult` enum

**Files changed:** `src/execution/executor.rs`

---

## Step 7 — Clean up dead code ⬜

After all steps above, remove:

### Files to delete
- `src/api/auth.rs`
- `src/signing/mod.rs`
- `src/signing/order.rs`

### Cargo.toml deps to remove
```toml
# Remove these (all replaced by SDK internals):
hmac = "0.12"
sha2 = "0.10"
base64 = "0.21"
hex = "0.4"
alloy = { version = "1", features = [...] }   # SDK re-exports what we need
tokio-tungstenite = "0.21"                     # SDK uses its own WS stack
futures-util = "0.3"                           # SDK re-exports futures
simd-json = "0.13"                             # only needed if we keep custom parsing
```

### Verify nothing is broken
```bash
docker compose run --rm dev cargo build
docker compose run --rm dev cargo test
docker compose run --rm dev cargo clippy -- -D warnings
```

---

## SDK quick-reference

### Client construction

```rust
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::auth::{Credentials, LocalSigner};
use polymarket_client_sdk::POLYGON;
use std::str::FromStr as _;

// Step 1: signer (holds private key)
let signer = LocalSigner::from_str(&private_key_hex)?.with_chain_id(Some(POLYGON));

// Step 2: credentials (API key + secret + passphrase)
let credentials = Credentials::new(api_key_uuid, secret_string, passphrase_string);

// Step 3: authenticated CLOB client
let client = Client::new("https://clob.polymarket.com", Config::default())?
    .authentication_builder(&signer)
    .credentials(credentials)  // use existing key; omit to auto-create
    .authenticate()
    .await?;
```

### WebSocket client construction

```rust
use polymarket_client_sdk::clob::ws::Client as WsClient;

// Market WS (unauthenticated)
let market_ws = WsClient::default();

// User WS (authenticated) — share credentials from clob client
let user_ws = WsClient::default().authenticate(credentials, signer.address())?;
```

### Key SDK types

| Type | Module | Description |
|---|---|---|
| `Credentials` | `auth` | `key: Uuid`, `secret: SecretString`, `passphrase: SecretString` |
| `LocalSigner` | `auth` | re-export of `alloy::signers::local::LocalSigner` |
| `clob::Client<Authenticated<Normal>>` | `clob` | Authenticated HTTP client |
| `clob::ws::Client<Authenticated<Normal>>` | `clob::ws` | Authenticated WS client |
| `BookUpdate` | `clob::ws::types::response` | Orderbook snapshot from WS |
| `OrderMessage` | `clob::ws::types::response` | User order event from WS |
| `TradeMessage` | `clob::ws::types::response` | User trade event from WS |
| `SignableOrder` | `clob::types` | Unsigned order ready for signing |
| `SignedOrder` | `clob::types` | Signed order ready for submission |
| `PostOrderResponse` | `clob::types::response` | Response from `post_order` |

### Constants

```rust
use polymarket_client_sdk::{POLYGON, AMOY, PRIVATE_KEY_VAR};
// POLYGON = 137u64
// AMOY    = 80002u64 (testnet)
// PRIVATE_KEY_VAR = "PRIVATE_KEY"
```
