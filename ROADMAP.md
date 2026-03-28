# Polymarket Bot — Roadmap

**Status as of 2026-03-28** | 133 tests passing | All core phases complete

---

## Completed

| Phase | Description |
|-------|-------------|
| 1 | Project skeleton: Docker, latency probe, `.env.example`, kill switch |
| 2 | Ledger system: cash, orders, positions, P&L, reconciliation |
| 3 | Execution engine: order state machine, circuit breaker, rate limiter |
| 4 | MathArbStrategy: paired YES/NO arb, dynamic edge calculator, partial fill handler |
| 5 | SDK migration: auth, WebSocket (market + user), order signing |
| 6 | Monitoring groundwork: health metrics, reconciliation loop |
| 7 | Paper trading: `BOT_MODE=paper` simulation with real book data |
| 8 | Fees & slippage: per-fill fee rate (bps), slippage cost, position aggregates |

---

## Phase 9 — Live Testing

**Goal**: Validate end-to-end with real money in a controlled way.

### 9.1 Pre-flight checklist

- [ ] Confirm `.env` has correct `PRIVATE_KEY`, `CLOB_API_KEY`, `CLOB_SECRET`, `CLOB_PASSPHRASE`
- [ ] Verify `MAX_BET_USD` is set to a small test amount (e.g. `5`)
- [ ] Verify `MAX_DAILY_LOSS_USD` is set conservatively (e.g. `20`)
- [ ] Run paper mode for ≥1h and confirm fills/P&L log correctly
- [ ] Confirm GeoIP block check passes (`check_geoblock()` in `main.rs`)
- [ ] Confirm USDC balance in wallet matches `LedgerSnapshot.available_cash`
- [ ] Confirm rate limiter is active (POST /order: 3500/10s; DELETE: 3000/10s)

### 9.2 Live smoke test

- [ ] Set `BOT_MODE=live`, `MAX_BET_USD=5`
- [ ] Watch logs: confirm WS connects, book updates arrive, strategy evaluates
- [ ] Confirm first arb order submits, fills appear in user WS
- [ ] Confirm ledger reconciles correctly after fills (compare to Polymarket UI)
- [ ] Confirm circuit breaker does NOT trip on normal operation
- [ ] Run for 4–8h, review P&L and slippage summary

### 9.3 Post-smoke tuning

- [ ] Adjust `MIN_EDGE_BPS` if no opportunities found (start at 50 bps)
- [ ] Adjust `MAX_BET_USD` upward once smoke test confirms correct behaviour
- [ ] Check `total_slippage_cost` vs `total_fees` ratio; if slippage >> fees, tighten `SLIPPAGE_TOLERANCE`

---

## Phase 10 — MakerRebateArbStrategy

**Goal**: Capture maker rebates by posting passive limit orders on both legs instead of hitting the book.

**Background**: Polymarket pays maker rebates on CLOB. For crypto 15m markets the fee model is:
- Taker pays 100 bps
- Maker receives ~50 bps rebate

By posting GTC limits slightly inside the spread we can:
1. Pay ~0 in fees (taker cost offset by rebate)
2. Still capture arb when both legs fill

### Implementation tasks

- [ ] Add `MakerRebateArbStrategy` in `src/strategy/maker_rebate_arb.rs`
  - Same `on_book_update()` signature as `MathArbStrategy`
  - Returns `OrderIntent` with `urgency: Urgency::Passive`
  - Price: post at `best_ask - 1 tick` for YES, same for NO
  - Only open if `YES_ask + NO_ask < $1.00 - edge` (same arb condition)
- [ ] Extend `ExecutionPolicy` / `MakerPolicy` to handle dual-leg passive orders
- [ ] Handle partial fills: if one leg fills and other doesn't after TTL, cancel unfilled leg and unwind filled leg via taker
- [ ] Add config vars: `MAKER_REBATE_ENABLED=false`, `MAKER_TTL_SECS=30`
- [ ] Tests: mock fills on one leg, assert unwind order generated

---

## Phase 11 — TemporalArbStrategy

**Goal**: Detect when Polymarket's binary price diverges from external probability and trade the gap.

**Background**: JaneStreetIndia earned $324K in 25 days (99.5% win rate) by temporal arb — using faster external data feeds to front-run stale Polymarket prices.

**Data sources to integrate**:
- Binance price feed (for BTC/ETH/SOL result markets): `wss://stream.binance.com/ws/btcusdt@ticker`
- Kalshi: REST API for equivalent contracts (cross-exchange arb)
- Polymarket price itself: already have real-time via user WS

### Implementation tasks

- [ ] Add `src/feeds/binance.rs` — WebSocket ticker feed, publishes `ExternalPrice { token_id, price, ts }`
- [ ] Add `src/feeds/mod.rs` — trait `ExternalFeed`, registry keyed by market condition
- [ ] Add `TemporalArbStrategy` in `src/strategy/temporal_arb.rs`
  - `on_external_price(ext: ExternalPrice, book: &BookState)` — compare implied binary prob vs external
  - Entry: `|implied_prob - external_prob| > threshold`
  - Direction: buy underpriced token
- [ ] Wire `ExternalFeed` updates into `bot.rs` event loop (`tokio::select!`)
- [ ] Add config: `TEMPORAL_ARB_ENABLED=false`, `TEMPORAL_ARB_THRESHOLD_BPS=200`
- [ ] Tests: inject mock external price, assert correct direction order intent

---

## Phase 12 — Observability

**Goal**: Instrument the critical path and expose metrics for dashboards and alerting.

### 12.1 Latency instrumentation

- [ ] Add `hdrhistogram` crate (`features = ["serialization"]`)
- [ ] In `bot.rs` event loop, record `book_update_to_intent_us` histogram
- [ ] In `executor.rs`, record `intent_to_submit_us` and `submit_to_ack_us`
- [ ] Expose p50/p95/p99 in `LedgerSnapshot` (or a separate `PerfSnapshot`)
- [ ] Log summary every 60s: `INFO latency: p50=12ms p95=28ms p99=45ms`

### 12.2 Prometheus metrics

- [ ] Add `metrics` + `metrics-exporter-prometheus` crates
- [ ] Expose `/metrics` endpoint on port 9090 (separate Axum router or `axum::Router::merge`)
- [ ] Key counters and gauges:
  - `polybot_orders_submitted_total` (counter, labels: side, strategy)
  - `polybot_orders_filled_total`
  - `polybot_orders_rejected_total`
  - `polybot_ws_reconnects_total` (market, user)
  - `polybot_pnl_usd` (gauge)
  - `polybot_available_cash_usd` (gauge)
  - `polybot_book_update_latency_us` (histogram)

### 12.3 Alerting

- [ ] Add `src/alerts/mod.rs` with trait `Alerter`
- [ ] Implement `DiscordAlerter` (webhook URL from env `DISCORD_WEBHOOK_URL`)
- [ ] Implement `TelegramAlerter` (token + chat_id from env)
- [ ] Alert conditions:
  - Circuit breaker trips
  - Daily loss limit approached (> 80% of `MAX_DAILY_LOSS_USD`)
  - WS reconnect > 3 times in 1 min
  - Position reconciliation mismatch
  - Bot process restart (startup alert: `"Bot started, paper=false"`)
- [ ] Add config: `ALERT_BACKEND=none|discord|telegram`

---

## Phase 13 — VPS Deployment

**Goal**: Run continuously in production on low-latency infrastructure close to Polymarket's servers.

### 13.1 Server setup

- [ ] Provision `t3.small` or `c6i.large` in AWS `us-east-1` (Polymarket servers are in us-east-1)
- [ ] OS: Ubuntu 24.04 LTS
- [ ] Kernel tuning (`/etc/sysctl.d/99-polybot.conf`):
  ```
  net.core.rmem_max = 16777216
  net.core.wmem_max = 16777216
  net.ipv4.tcp_rmem = 4096 87380 16777216
  net.ipv4.tcp_wmem = 4096 65536 16777216
  net.ipv4.tcp_fastopen = 3
  net.ipv4.tcp_nodelay = 1
  ```
- [ ] CPU governor: `cpupower frequency-set --governor performance`

### 13.2 Deployment

- [ ] Build Docker image in CI (GitHub Actions), push to ECR or GHCR
- [ ] `docker-compose.yml` on VPS pulls latest image, mounts `.env`
- [ ] systemd unit file (`/etc/systemd/system/polybot.service`) wraps `docker compose up` with `Restart=always`
- [ ] Log rotation: `journald` with 7-day retention

### 13.3 Secrets management

- [ ] Store `PRIVATE_KEY`, API credentials in AWS Secrets Manager or Vault
- [ ] Docker startup script pulls secrets at container init, injects into env
- [ ] Never commit `.env` with live credentials

### 13.4 Backup and recovery

- [ ] `ledger-data` Docker volume backed up to S3 every 15 min
- [ ] Recovery playbook: restore volume, restart container — state resumes from snapshot
- [ ] Optional: standby VPS in `us-west-2` for manual failover

---

## Quick Reference — Config Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `BOT_MODE` | `paper` | `paper` or `live` |
| `MAX_BET_USD` | `100` | Max size per trade |
| `MAX_DAILY_LOSS_USD` | `100` | Daily loss circuit breaker |
| `USE_MAKER_MODE` | `false` | Use MakerPolicy instead of TakerPolicy |
| `MAKER_REBATE_ENABLED` | `false` | Enable MakerRebateArbStrategy |
| `MAKER_TTL_SECS` | `30` | Passive order timeout before cancel |
| `TEMPORAL_ARB_ENABLED` | `false` | Enable TemporalArbStrategy |
| `TEMPORAL_ARB_THRESHOLD_BPS` | `200` | Min divergence to trade |
| `ALERT_BACKEND` | `none` | `none`, `discord`, or `telegram` |
| `DISCORD_WEBHOOK_URL` | — | Discord alert webhook |
| `TELEGRAM_TOKEN` | — | Telegram bot token |
| `TELEGRAM_CHAT_ID` | — | Telegram chat to send alerts to |
| `RUST_LOG` | `info` | Log verbosity |

---

## Competitive Benchmarks

| Trader | Profit | Period | Method |
|--------|--------|--------|--------|
| JaneStreetIndia | $324K | 25 days | Temporal arb (99.5% win rate) |
| distinct-baguette | $325K | — | Math arb |
| ilovecircle | $2.2M | — | AI probability edge |

Our current `MathArbStrategy` covers the `distinct-baguette` playbook.
Phase 11 (`TemporalArbStrategy`) targets the `JaneStreetIndia` playbook.

---

## Priority Order

1. **Phase 9** — live smoke test (small $, controlled)
2. **Phase 12.1** — latency tracking (needed to tune temporal arb)
3. **Phase 10** — maker rebate arb (free alpha on existing arb logic)
4. **Phase 11** — temporal arb (higher risk, higher reward)
5. **Phase 12.2–12.3** — Prometheus + alerting
6. **Phase 13** — VPS deployment
