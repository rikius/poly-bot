# SDK Migration Phase 3 — Dead code removal & latency integration

Phase 2 removed the REST client, HMAC auth, and endpoint wrappers.
This phase removes remaining dead types and threads the latency probe result into the bot.

> **Windows note**: Use `docker compose run --rm dev cargo check` to compile.
> Rebuild with `docker compose build dev` after Cargo.toml changes.

---

## What goes

| Step | File(s) | Action | Details |
|------|---------|--------|---------|
| 1 | `api/types.rs` | Remove `OrderBook`, `PriceChange`, `Outcome` | Dead types — no imports anywhere |
| 2 | `api/mod.rs` | Remove re-exports of deleted types | `OrderBook`, `PriceChange`, `Outcome` from `pub use types::` |
| 3 | `bot.rs` + `main.rs` | Thread `selected.url` into `Bot::new()` | Bot currently hardcodes `"https://clob.polymarket.com"` — use probed endpoint instead |
| 4 | Verify | `docker compose run --rm dev cargo test` | All tests pass, 0 warnings |

**Total: ~35 lines removed, latency probe actually used**

---

## Progress

| Step | Status |
|------|--------|
| 1 | ✅ Done — removed `OrderBook`, `PriceChange`, `Outcome` from types.rs |
| 2 | ✅ Done — trimmed re-exports in mod.rs |
| 3 | ✅ Done — `Bot::new()` accepts `clob_url`, main.rs passes `selected.url` |
| 4 | ✅ Done — 128 tests pass, 0 warnings |
