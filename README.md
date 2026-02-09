# Polymarket Trading Bot 🤖

A high-frequency, event-driven trading bot for [Polymarket](https://polymarket.com) prediction markets, written in Rust for maximum performance and safety.

## 🎯 Overview

This bot automatically discovers and trades on Polymarket's 15-minute cryptocurrency prediction markets (BTC, ETH, SOL Up/Down). It features:

- **High-Performance Architecture**: Event-driven design with `tokio::select!` for sub-millisecond latency
- **Real-Time Market Data**: WebSocket streaming of order books and fill notifications
- **Mathematical Arbitrage**: Detects and exploits pricing inefficiencies (YES + NO < $1)
- **Risk Management**: Circuit breakers, position limits, and emergency kill switch
- **Paper & Live Trading**: Test strategies risk-free before going live
- **EIP-712 Signing**: Ethereum-compatible order signing for Polymarket's CTF Exchange

## ✨ Features

### Trading Engine
- **Automated Market Discovery**: Discovers active 15-min crypto markets via Gamma API
- **Dual WebSocket Streams**: Market data (order books) + User stream (fills/status)
- **Lock-Free Order Book**: Concurrent market state tracking with `DashMap`
- **Strategy Framework**: Pluggable strategies with intent-based execution
- **Multiple Execution Policies**:
  - **Taker Mode**: Immediate execution (FOK/FAK orders)
  - **Maker Mode**: Passive orders (GTC limit orders)
  - **Dual Mode**: Combines both strategies

### Safety & Risk Controls
- **Circuit Breaker**: Automatically halts trading on:
  - Daily loss limits exceeded
  - Per-market position limits violated
  - Maximum bet size violations
- **Kill Switch**: Emergency shutdown via:
  - Environment variable (`POLYBOT_KILL=1`)
  - File flag (`/tmp/polybot_kill`)
  - Ctrl+C signal handler
- **Authoritative Ledger**: Tracks orders, positions, fills, and PnL

### Performance
- **Target Latency**: 12-30ms total (vs 1000ms+ Python baseline)
- **SIMD-Optimized JSON**: Fast market data parsing with `simd-json`
- **Fixed-Point Math**: Precise price calculations with `rust_decimal` (no floating-point errors)
- **Optimized Builds**: LTO, single codegen unit, opt-level 3

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────────┐
│               BOT EVENT LOOP (main.rs)                  │
│            tokio::select! - <1ms latency                │
└─────────────────────────────────────────────────────────┘
      ↓                    ↓                    ↓
 Market WS           User WS (Fills)      Strategy Tick
(ORDER BOOKS)       (NOTIFICATIONS)       (Every 100ms)
      ↓                    ↓                    ↓
 OrderBookState      Process Fills       StrategyRouter
  (DashMap)         Update Ledger       (All strategies)
      ↓                    ↓                    ↓
      └────────────────────────────────────────┘
                          ↓
                   OrderIntent (WHAT)
                  + Context (read-only)
                          ↓
              ExecutionPolicy (HOW)
         Taker: FOK/FAK | Maker: GTC
                          ↓
                  OrderExecutor
        (EIP-712 signing + REST API)
                          ↓
            OrderTracker & Ledger
                          ↓
               CircuitBreaker
           (Risk limit validation)
```

### Key Components

| Component | Purpose |
|-----------|---------|
| **bot.rs** | Event-driven orchestrator using `tokio::select!` |
| **api/** | REST client, HMAC auth, market discovery |
| **websocket/** | Dual streams: Market (order books) + User (fills) |
| **strategy/** | Pluggable framework; `MathArbStrategy` detects arbitrage |
| **execution/** | Order state machine, policies (Taker/Maker), executor |
| **ledger/** | Authoritative state: orders, positions, cash, PnL |
| **risk/** | Circuit breaker, position limits, loss limits |
| **signing/** | EIP-712 order signing for Ethereum |
| **state/** | In-memory order book via `DashMap` |

## 🚀 Quick Start

### Prerequisites

- **Rust** 1.70+ ([install via rustup](https://rustup.rs))
- **Polymarket Account** with API credentials
- **Ethereum Wallet** with USDC on Polygon (for live trading)

### Installation

1. **Clone the repository**
   ```bash
   git clone https://github.com/pontiggia/poly-bot.git
   cd poly-bot
   ```

2. **Create a `.env` file** with your credentials:
   ```bash
   # Polymarket API Credentials
   POLYMARKET_API_KEY=your_api_key_here
   POLYMARKET_SECRET=your_secret_here
   POLYMARKET_PASSPHRASE=your_passphrase_here

   # Ethereum Wallet (for signing orders)
   PRIVATE_KEY=your_ethereum_private_key_here
   WALLET_ADDRESS=your_ethereum_wallet_address_here

   # Trading Configuration
   BOT_MODE=paper                    # "paper" or "live"
   MAX_BET_USD=100                   # Max per-order size
   MAX_DAILY_LOSS_USD=100            # Daily loss circuit breaker
   MAX_POSITION_PER_MARKET_USD=500   # Max exposure per market

   # Optional: Execution Mode
   USE_MAKER_MODE=false              # true = GTC orders, false = immediate
   ```

3. **Build the project**
   ```bash
   cargo build --release
   ```

4. **Run tests** (optional)
   ```bash
   cargo test
   ```

### Running the Bot

#### Paper Trading (Recommended First)
Test your strategies without risking real money:

```bash
# Make sure BOT_MODE=paper in .env
cargo run --release
```

You'll see:
```
===========================================
  Polymarket Trading Bot v0.1.0
===========================================
Configuration loaded successfully
  Wallet: 0x1234...5678
  Mode: Paper
  Max bet: $100
  Max daily loss: $100
>>> PAPER TRADING MODE - No real orders will be placed <<<
```

#### Live Trading (Use with Caution!)
When ready for real trading:

1. Change `BOT_MODE=live` in `.env`
2. Ensure your wallet has sufficient USDC on Polygon
3. Start small with conservative limits
4. Run the bot:
   ```bash
   cargo run --release
   ```

### Emergency Shutdown

Three ways to stop the bot immediately:

1. **Ctrl+C**: Press in the terminal
2. **Environment Variable**: `export POLYBOT_KILL=1`
3. **File Flag**: `touch /tmp/polybot_kill`

## 📁 Project Structure

```
poly-bot/
├── src/
│   ├── main.rs              # Entry point, event loop
│   ├── bot.rs               # Event-driven orchestrator
│   ├── config.rs            # Configuration from .env
│   ├── kill_switch.rs       # Emergency shutdown system
│   │
│   ├── api/                 # REST API & Authentication
│   │   ├── client.rs        # HTTP client with HMAC auth
│   │   ├── types.rs         # API request/response types
│   │   └── discovery.rs     # Market discovery (Gamma API)
│   │
│   ├── websocket/           # Real-Time Market Data
│   │   ├── market.rs        # Order book stream
│   │   └── user.rs          # Fill notifications
│   │
│   ├── strategy/            # Trading Strategies
│   │   ├── traits.rs        # Strategy interface
│   │   ├── router.rs        # Intent routing
│   │   ├── math_arb.rs      # Arbitrage detection
│   │   └── types.rs         # Market pairs, intents
│   │
│   ├── execution/           # Order Execution Pipeline
│   │   ├── policy.rs        # Taker/Maker/Dual modes
│   │   ├── executor.rs      # Order submission
│   │   ├── tracker.rs       # Outstanding orders
│   │   └── state_machine.rs # Order lifecycle
│   │
│   ├── ledger/              # Portfolio State
│   │   ├── orders.rs        # Order tracking
│   │   ├── positions.rs     # Position tracking
│   │   └── cash.rs          # Cash & PnL tracking
│   │
│   ├── risk/                # Risk Management
│   │   └── circuit_breaker.rs # Safety limits
│   │
│   ├── signing/             # Ethereum Signing
│   │   └── eip712.rs        # EIP-712 order signing
│   │
│   └── state/               # Market State
│       ├── orderbook.rs     # Lock-free order book
│       └── registry.rs      # Market pair registry
│
├── planning/                # Architecture Documentation
│   ├── 01-STRATEGY-ANALYSIS.md
│   ├── 02-RUST-ARCHITECTURE.md
│   ├── 03-API-REFERENCE.md
│   ├── 04-IMPLEMENTATION-PLAN.md
│   └── 05-API-DISCOVERY-FINDINGS.md
│
├── Cargo.toml               # Dependencies & build config
└── .gitignore               # Git ignore rules
```

## 🔧 Configuration

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `POLYMARKET_API_KEY` | Yes | - | Polymarket API key |
| `POLYMARKET_SECRET` | Yes | - | API secret for HMAC signing |
| `POLYMARKET_PASSPHRASE` | Yes | - | API passphrase |
| `PRIVATE_KEY` | Yes | - | Ethereum private key (hex, no 0x prefix) |
| `WALLET_ADDRESS` | Yes | - | Ethereum wallet address (0x...) |
| `BOT_MODE` | No | `paper` | Trading mode: `paper` or `live` |
| `MAX_BET_USD` | No | `100` | Maximum size per order (USD) |
| `MAX_DAILY_LOSS_USD` | No | `100` | Daily loss limit (circuit breaker) |
| `MAX_POSITION_PER_MARKET_USD` | No | `500` | Max exposure per market (USD) |
| `USE_MAKER_MODE` | No | `false` | Enable passive GTC orders |

### Trading Modes

#### Paper Mode (Default)
- **Purpose**: Test strategies without risk
- **Behavior**: Bot simulates order submission but doesn't send to exchange
- **Use Case**: Strategy development, debugging, backtesting logic
- **Set**: `BOT_MODE=paper`

#### Live Mode
- **Purpose**: Real trading with actual money
- **Behavior**: Orders are signed and submitted to Polymarket
- **Use Case**: Production trading (use conservative limits!)
- **Set**: `BOT_MODE=live`
- **⚠️ Warning**: Real money at risk! Start with small limits.

### Execution Policies

#### Taker Mode (Default)
- **Orders**: FOK (Fill-Or-Kill) or FAK (Fill-And-Kill)
- **Execution**: Immediate, takes liquidity from order book
- **Fee**: Higher (taker fee)
- **Use Case**: Fast execution, arbitrage opportunities
- **Set**: `USE_MAKER_MODE=false`

#### Maker Mode
- **Orders**: GTC (Good-Til-Canceled) limit orders
- **Execution**: Passive, adds liquidity to order book
- **Fee**: Lower (maker rebate)
- **Use Case**: Patience, spread capture, market making
- **Set**: `USE_MAKER_MODE=true`

## 🧪 Development

### Running Tests

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific module tests
cargo test ledger::
cargo test strategy::

# Run tests in release mode (faster)
cargo test --release
```

### Code Quality

```bash
# Format code
cargo fmt

# Lint with Clippy
cargo clippy -- -D warnings

# Check without building
cargo check
```

### Building

```bash
# Debug build (faster compile, slower runtime)
cargo build

# Release build (optimized for performance)
cargo build --release

# The release binary is at: ./target/release/polymarket_bot
```

## 📊 Strategy: Mathematical Arbitrage

The bot implements a **mathematical arbitrage** strategy that exploits pricing inefficiencies in prediction markets.

### How It Works

1. **Market Structure**: Each Polymarket event has two outcomes (e.g., BTC Up vs BTC Down)
2. **Pricing Rule**: YES + NO shares should total exactly $1.00 at settlement
3. **Arbitrage**: When YES + NO < $1.00, both can be bought for guaranteed profit

### Example Trade

```
Market: "Will BTC be higher in 15 minutes?"
  YES price: $0.48
  NO price:  $0.50
  Total:     $0.98  ← Arbitrage opportunity!

Action:
  1. Buy 100 YES shares @ $0.48 = $48.00
  2. Buy 100 NO shares @ $0.50  = $50.00
  3. Total cost:                = $98.00
  4. Settlement value:          = $100.00 (always)
  5. Gross profit:              = $2.00
  6. Net profit (after fees):   ≈ $1.80

Edge: $2.00 / $98.00 = 2.04% per trade
```

### Edge Calculation

The bot calculates **edge** as:
```
edge = 1.00 - (YES_price + NO_price)
```

Trades are only executed when:
- `edge > 0` (profit opportunity exists)
- `edge > fee_rate` (profitable after fees)
- Risk limits are satisfied

## 🛡️ Safety & Risk Management

### Circuit Breaker

The bot automatically halts trading when:

1. **Daily Loss Limit**: Total PnL drops below `-MAX_DAILY_LOSS_USD`
2. **Position Limit**: Exposure in any market exceeds `MAX_POSITION_PER_MARKET_USD`
3. **Bet Size Limit**: Individual order exceeds `MAX_BET_USD`

When triggered, the bot:
- Stops executing new orders
- Logs the violation with details
- Waits for manual intervention (restart required)

### Kill Switch

Emergency shutdown system with multiple triggers:

```bash
# Method 1: Keyboard interrupt
# Press Ctrl+C in terminal

# Method 2: Environment variable
export POLYBOT_KILL=1

# Method 3: File flag
touch /tmp/polybot_kill
```

The bot checks these conditions **every event loop iteration** (<1ms).

### Ledger Integrity

All state changes are tracked in an **authoritative ledger**:
- **Orders**: Submitted, pending, filled, canceled
- **Fills**: Partial/full, fees, timestamps
- **Positions**: Token holdings per market
- **Cash**: Available balance, locked in orders, PnL

The ledger is the **source of truth** for all risk calculations.

## 📈 Performance Characteristics

### Latency Breakdown

```
Market Data → Strategy Decision → Order Execution
   <1ms     +      <5ms         +     10-20ms      = 12-30ms total
```

- **Event Loop**: Sub-millisecond via `tokio::select!`
- **Strategy Evaluation**: <5ms for MathArbStrategy
- **Network RTT**: 10-20ms to Polymarket API
- **Total**: 12-30ms from market update to order submission

### Optimizations

1. **SIMD JSON Parsing**: 2-3x faster than serde_json
2. **Lock-Free State**: `DashMap` avoids mutex contention
3. **Fixed-Point Math**: `rust_decimal` eliminates floating-point errors
4. **Single-Threaded Event Loop**: No context switching overhead
5. **LTO & Codegen**: Release build fully optimized

## 🐛 Troubleshooting

### Common Issues

#### "Failed to load configuration"
- **Cause**: Missing or incorrect `.env` file
- **Fix**: Create `.env` with all required variables (see Quick Start)

#### "Failed to discover markets from API"
- **Cause**: Polymarket API credentials invalid or network issue
- **Fix**: Verify API credentials, check internet connection
- **Note**: Bot falls back to hardcoded test market

#### "WebSocket disconnected"
- **Cause**: Network interruption or server maintenance
- **Fix**: Bot auto-reconnects, no action needed
- **Monitor**: Check logs for reconnection success

#### "Circuit breaker triggered"
- **Cause**: Loss limit or position limit exceeded
- **Fix**: Review trades, adjust limits in `.env`, restart bot

#### Orders not executing in paper mode
- **Expected**: Paper mode simulates but doesn't submit real orders
- **Check**: Logs should show "PAPER MODE: Would submit order..."

## 📜 Testing Status

**Current Test Suite**: 150/150 passing ✅

### Test Coverage

- **Unit Tests**: 150 tests across all modules
- **Integration**: Event loop, WebSocket, API clients
- **Ledger**: Order tracking, position accounting, PnL
- **Risk**: Circuit breaker triggers, limit validation
- **Strategy**: Edge calculation, intent generation
- **Execution**: Policy selection, order building, signing

### Running Tests

```bash
# All tests
cargo test

# Specific module
cargo test strategy::

# Show output
cargo test -- --nocapture

# Release mode (faster)
cargo test --release
```

## 🗺️ Roadmap

### Phase 9: Live Testing (Current)
- [ ] Test with small live orders
- [ ] Monitor fills and WebSocket reconnection
- [ ] Validate ledger accuracy against API
- [ ] Fine-tune risk limits based on real data

### Future Enhancements
- [ ] Additional strategies (temporal arb, spread capture)
- [ ] Multi-market correlation analysis
- [ ] Advanced order types (iceberg, TWAP)
- [ ] Performance metrics dashboard
- [ ] Backtesting framework
- [ ] Machine learning integration

## 📝 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- **Polymarket**: For providing prediction market infrastructure
- **Alloy**: Ethereum library for EIP-712 signing
- **Tokio**: Async runtime powering the event loop
- **Rust Community**: For excellent ecosystem libraries

## ⚠️ Disclaimer

**Use at your own risk.** This software is provided "as is" without warranty of any kind. Trading involves substantial risk of loss. Past performance does not guarantee future results. The authors are not responsible for any financial losses incurred from using this software.

**Not Financial Advice**: This bot is for educational and research purposes. Consult with a financial advisor before trading.

---

**Built with ❤️ and Rust by [@pontiggia](https://github.com/pontiggia)**

For questions, issues, or contributions, please open an issue on GitHub.
