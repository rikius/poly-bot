# Polymarket Bot — Build Plan

## Base

This project is built on top of [pontiggia/poly-bot](https://github.com/pontiggia/poly-bot) — a high-frequency, event-driven Polymarket trading bot in Rust. We extend it with:

1. **Docker** — `Dockerfile` + `docker-compose.yml` for isolated, reproducible deployment
2. **Latency probe** — `src/latency/mod.rs` startup RTT measurement to pick the fastest server endpoint
3. **`.env.example`** — documented environment variable template

---

## Current Structure (from poly-bot)

```
src/
├── main.rs                # Entry point — config, logging, discovery, bot run
├── bot.rs                 # tokio::select! event loop orchestrator
├── config.rs              # Config::from_env()
├── constants.rs           # URLs, chain IDs, thresholds, timing constants
├── error.rs               # BotError enum
├── kill_switch.rs         # Ctrl+C / POLYBOT_KILL env / /tmp/polybot_kill file
├── lib.rs                 # Library re-exports
├── api/
│   ├── auth.rs            # HMAC-SHA256 API authentication
│   ├── client.rs          # Optimized reqwest HTTP client (TCP_NODELAY, pooling)
│   ├── discovery.rs       # Market discovery (all markets, crypto 15m, by slug)
│   ├── endpoints.rs       # Endpoint construction helpers
│   ├── gamma.rs           # Gamma API client (market metadata)
│   └── types.rs           # API request/response types
├── execution/
│   ├── executor.rs        # Order build/sign/submit
│   ├── cancellation.rs    # Order cancellation logic
│   ├── order_tracker.rs   # Live GTC/GTD order registry
│   ├── policy.rs          # TakerPolicy / MakerPolicy / DualPolicy
│   └── state_machine.rs   # Order lifecycle state machine
├── ledger/
│   ├── mod.rs             # Ledger orchestrator
│   ├── cash.rs            # Cash balance tracking
│   ├── orders.rs          # Open order tracking
│   └── positions.rs       # Token position + P&L tracking
├── risk/
│   ├── mod.rs             # Risk orchestrator
│   ├── circuit_breaker.rs # Halt after N fatal errors
│   ├── limits.rs          # Position/loss limit checks
│   └── reconciliation.rs  # REST reconciliation with server state
├── signing/
│   ├── mod.rs             # EIP-712 order signing
│   └── order.rs           # Order type + builder
├── state/
│   ├── mod.rs             # BookState (DashMap<token_id, OrderBook>)
│   └── order_book.rs      # OrderBook (BTreeMap bids/asks)
├── strategy/
│   ├── arbitrage.rs       # YES+NO < $1 − edge → paired OrderIntents
│   ├── edge_calculator.rs # Dynamic edge: fee_bps + slippage + spread penalty
│   ├── market_pair.rs     # MarketPairRegistry (dual-indexed by condition/token)
│   ├── router.rs          # Routes book updates to correct strategy
│   └── traits.rs          # Strategy trait + OrderIntent type
└── websocket/
    ├── market.rs          # Order book WS stream (subscribe + reconnect)
    └── user.rs            # Fill/order WS stream (authenticated)
```

---

## Added by This Project

### `Dockerfile`
Multi-stage build:
- **Builder**: `rust:1.91-slim` — compiles with full toolchain, deps cached as separate layer (1.91 required by alloy v1.8.3)
- **Runtime**: `debian:bookworm-slim` + `ca-certificates` only — minimal image (~80MB)

### `docker-compose.yml`
- `restart: unless-stopped`
- `env_file: .env`
- Named volume `ledger-data:/app/data` — ledger snapshots survive container restarts
- `tmpfs: /tmp` — kill switch file lives in tmpfs

### `src/latency/mod.rs`
Startup RTT probe:
- Fires 3 concurrent `GET /time` to each candidate endpoint
- Takes median RTT per endpoint, picks the lowest
- Returns `SelectedEndpoint { url, rtt_ms }` used to configure all HTTP/WS clients
- Logs selected endpoint at startup: `INFO selected endpoint: https://clob.polymarket.com (12ms)`

---

## Running with Docker

```bash
# Copy and fill in credentials
cp .env.example .env

# Build image (no Rust toolchain needed on host)
docker compose build

# Run in paper mode (BOT_MODE=paper in .env)
docker compose up

# View logs
docker compose logs -f

# Graceful shutdown via kill switch file
docker exec $(docker compose ps -q bot) touch /tmp/polybot_kill

# Or stop directly
docker compose stop
```

## Running Locally

```bash
cp .env.example .env
# edit .env with your credentials

cargo build --release
BOT_MODE=paper cargo run --release
```

---

## Key Configuration

| Variable | Default | Description |
|---|---|---|
| `BOT_MODE` | `paper` | `paper` (simulated) or `live` (real money) |
| `MAX_BET_USD` | `100` | Max size per trade |
| `MAX_DAILY_LOSS_USD` | `100` | Daily loss circuit breaker |
| `USE_MAKER_MODE` | `false` | Passive limit orders instead of taker |
| `RUST_LOG` | `info` | Log verbosity |

---

## Strategy: Binary Arbitrage

Monitors 15-minute BTC/ETH/SOL YES/NO prediction markets.

**Entry condition**: `YES_ask + NO_ask < $1.00 - required_edge`

**Edge model** accounts for:
- Taker fees (1000 bps for crypto 15m markets)
- Slippage proportional to order size vs. book depth
- Spread penalty for spreads > 5¢
- Partial fill margin for thin books (< 100 shares)

**Execution**: Two linked orders (YES + NO) with matching group IDs. Both must fill for arbitrage to realize.

---

## Performance Targets

| Metric | Target |
|---|---|
| Market update → order submission | < 30ms |
| Strategy tick interval | 100ms |
| WS ping interval | 5s |
| Market discovery refresh | 60s |

---

## SDK Migration Plan

Incrementally replacing poly-bot's custom code with `polymarket-client-sdk` v0.4 to fix bugs, reduce maintenance, and improve reliability.

### Why migrate?

| Area | poly-bot bug | SDK fix |
|---|---|---|
| Auth | `base64::STANDARD` instead of `URL_SAFE` → wrong HMAC signature | SDK uses `URL_SAFE` + `SecretString` (secrets not in logs) |
| WebSocket PING | Sends binary `Message::Ping(vec![])` — Polymarket expects TEXT `"PING"` | SDK sends correct TEXT frame, 15s PONG timeout detection |
| Order signing | Low-entropy salt (timestamp XOR) | SDK uses `rand::rng().random()` masked to IEEE 754 range |
| EIP-712 | 482 lines hand-rolled | SDK uses alloy `sol!` macro |

### Steps

| Step | Status | What changes |
|---|---|---|
| **1** — Add SDK + upgrade Rust | ✅ Done | `polymarket-client-sdk = "0.4"` added to `Cargo.toml`; Dockerfile upgraded `rust:1.88` → `rust:1.91` |
| **2** — Replace auth | ⏳ Pending | Remove `src/api/auth.rs`; inject SDK `Credentials` into HTTP client |
| **3** — Replace market WS | ⏳ Pending | Remove `src/websocket/market.rs`; use `clob::ws::Client::subscribe_orderbook()` |
| **4** — Replace user WS | ⏳ Pending | Remove `src/websocket/user.rs`; use `clob::ws::Client::subscribe_user_events()` |
| **5** — Replace Gamma discovery | ⏳ Pending | Replace HTTP polling in `src/api/gamma.rs`; use SDK `gamma::Client::stream_data()` |
| **6** — Replace order signing | ⏳ Pending | Remove `src/signing/`; use SDK `OrderBuilder` |
| **7** — Clean up dead code | ⏳ Pending | Remove `hmac`, `sha2`, `base64`, `hex`, `alloy` direct, `tokio-tungstenite`, `futures-util` from `Cargo.toml` |
