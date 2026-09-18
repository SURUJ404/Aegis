# Aegis

A multi-venue crypto liquidity engine written in Rust. Aegis is a local order book, market data pipeline, strategy engine, risk gate, and execution layer designed for deterministic backtesting and paper trading. Live trading is not yet implemented.

## Problem

Market making across centralized and decentralized venues requires:
- Normalizing heterogeneous market data streams (CEX WebSocket, on-chain gRPC) into a single event model
- Maintaining a consistent local order book with gap detection
- Inserting a risk gate between strategy decisions and order placement
- Running the full pipeline deterministically so backtests are reproducible

Aegis addresses this with a pipeline architecture where each stage is a separate crate, all inter-component communication flows through typed bounded event buses, and the backtest runner replays identical inputs to byte-identical outputs.

## Status

Features marked **[IMPLEMENTED]** are working and tested. Features marked **[PLANNED]** are designed but not yet built.

| Area | Status |
|---|---|
| Order book (BTreeMap tick-based, sequence management, gap detection) | **[IMPLEMENTED]** |
| Market data adapters (OKX, Binance, Bybit WebSocket) | **[IMPLEMENTED]** |
| Solana market data adapter (Raydium AMM/CLMM/Cpmm, OpenBook, Jupiter, Pump.fun) | **[IMPLEMENTED]** |
| Solana execution layer (intent, transaction building, submission, reconciliation) | **[IMPLEMENTED]** |
| Strategy trait and baseline market-making strategy | **[IMPLEMENTED]** |
| Risk engine (limits, kill switch, trip-wires) | **[IMPLEMENTED]** |
| Paper execution venue with fill model | **[IMPLEMENTED]** |
| Deterministic backtest runner | **[IMPLEMENTED]** |
| Synthetic market data generator | **[IMPLEMENTED]** |
| Event bus (bounded, typed, DropNewest/Block policies) | **[IMPLEMENTED]** |
| Control-plane API (Axum, auth, start/stop/kill) | **[IMPLEMENTED]** |
| Persistence (Postgres, Redis hot state) | **[IMPLEMENTED]** |
| Telemetry (tracing, Prometheus metrics, latency histograms) | **[IMPLEMENTED]** |
| Web dashboard (React/Vite) | **[IMPLEMENTED]** |
| Docker / docker-compose / Kubernetes manifests | **[IMPLEMENTED]** |
| Live trading mode | **[PLANNED]** |
| On-chain order execution through Raydium/Jupiter | **[PLANNED]** |

## Architecture

```
                    WebSocket / gRPC Feeds
                            |
                            v
                    Market Data Adapter
                    (OKX / Binance / Bybit / Solana)
                            |
                            v
                   +--------+--------+
                   |                 |
                   v                 v
             Order Book        Cross-Venue
              Engine            Analyzer
                   |                 |
                   +--------+--------+
                            |
                            v
                     Strategy Engine
                   (pure, no I/O)
                            |
                            v
                       Risk Engine
                   Allow | Reduce
                  Reject | Halt
                            |
                            v
                     Execution Engine
                   (paper / Solana)
                            |
                            v
                   Position / PnL
                            |
                   +--------+--------+
                   v                 v
               Postgres           Redis
              (durable)        (hot state)
```

### Crate map

| Crate | Responsibility |
|---|---|
| `lq-types` | Domain primitives: `Exchange`, `Side`, `Price`, `Qty`, `Symbol`, `TimestampMs` |
| `lq-core` | `EventBus`, `Event` types, `EngineConfig`, `EngineState` |
| `lq-exchange` | Instrument specs, fee schedules, venue metadata |
| `lq-orderbook` | `OrderBook` (BTreeMap tick-based), `BookStore`, `MarketStateEngine` |
| `lq-market-data` | `FeedDecoder` trait, WebSocket transports (OKX, Binance, Bybit) |
| `lq-strategy` | `Strategy` trait, `MarketMakingStrategy`, `StrategyEngine` |
| `lq-risk` | `RiskEngine`, `RiskDecision`, configurable limits |
| `lq-execution` | `ExecutionVenue` trait, `PaperExecutionVenue`, `OrderStateMachine`, `PositionManager` |
| `lq-simulator` | `SyntheticMarketData`, `PaperExchange` (local matching) |
| `lq-backtest` | `BacktestRunner`, deterministic replay, `PerfMetrics` |
| `lq-persistence` | `PostgresStore`, `RedisHotState`, `PersistenceSink` |
| `lq-telemetry` | `Metrics`, `MetricsServer`, structured tracing |
| `lq-api` | Axum router, auth, control-plane handlers |
| `lq-solana-types` | Solana-specific types: `SolanaProgram`, `AmmPoolState`, `Slot` |
| `lq-solana-data` | `SolanaDataAdapter`, `SolanaNormalizer`, slot tracking, reconnect |
| `lq-solana-execution` | `OrderIntent`, transaction building, submission, reconciliation |

### Application binaries

| Binary | Purpose |
|---|---|
| `trading-engine` | Full pipeline: feeds, books, strategy, risk, execution, API, metrics |
| `market-data-service` | Market data collection and persistence only |
| `backtest-runner` | Deterministic backtest (prints result, exits) |
| `simulator` | Synthetic market + paper matching |
| `api-server` | Control-plane API backed by empty engine state |

## Data Flow

1. A `FeedDecoder` per venue normalizes venue-native JSON into `MarketEvent`s (Snapshot, Delta, Trade, Tick, Status) and publishes to the market bus topic.
2. `BookStore::ingest` applies snapshots/deltas with sequence-number checks. Gaps produce `IngestOutcome::Gap`; the book is flagged suspect and quoting halts.
3. `MarketStateEngine` computes `MarketState` (mid, microprice, imbalance, volatility, regime) from the book and trade flow.
4. `StrategyEngine` invokes registered `Strategy` implementations. Strategies are pure functions: they receive a `StrategyContext` and return `StrategyDecision`s (Quote, MarketOrder, StandDown, Hold).
5. Every order passes `RiskEngine::validate_order` which returns Allow, Reduce, Reject, or Halt.
6. `ExecutionVenue::place_order` submits the order. `PositionManager` tracks fills, positions, and realized PnL.

## Order Book

`OrderBook` uses `BTreeMap<u64, u64>` (tick -> quantity) for each side. Prices are stored as integer ticks derived from `Decimal` prices via the instrument's `tick_size`. This gives O(1) per-level upsert/delete and avoids floating-point comparison issues.

`BookStore` manages per-(venue, symbol) books and checks sequence contiguity on every delta. An out-of-order delta yields `IngestOutcome::Gap`, the book is flagged suspect, and quoting halts until a resync snapshot arrives. Binance depth20 (full snapshots every 100ms) needs no sequence bookkeeping; OKX/Bybit (incremental) do.

The book is disposable. Positions and PnL come from the `FillEvent` stream (`PositionManager` is the system of record). A wrong book cannot corrupt accounting; it only stops trading until resync.

## Solana Integration

Three crates under `crates/solana/`:

**`lq-solana-types`** defines `SolanaProgram` (Raydium AMM V4, CLMM, Cpmm, OpenBook V2, Jupiter V6, Pump.fun), `AmmPoolState` (reserves, sqrt_price, fee), `SolanaTrade`, `Slot`, and `OrderIntent`.

**`lq-solana-data`** provides `SolanaDataAdapter` which:
- Receives raw `MarketDataEvent`s (PoolState, Swap, LiquidityChange, Heartbeat)
- Normalizes them through `SolanaNormalizer` into Aegis `MarketEvent`s
- Tracks slot lag per market for staleness detection
- Manages connection state with exponential-backoff reconnect
- Publishes to the shared `EventBus` market topic

**`lq-solana-execution`** provides:
- `OrderIntent` — transport-agnostic order description with slippage protection and TTL
- Transaction building and submission
- Reconciliation of on-chain fills against local order state

## Event Model

All inter-component communication goes through `EventBus` topics:

| Topic | Capacity | Policy | Events |
|---|---|---|---|
| `market` | 4096 | `DropNewest` | Snapshot, Delta, Trade, Tick, Status |
| `execution` | 4096 | `Block` | New, Acknowledged, Fill, CancelRequested, Cancelled, Rejected, Expired, Trade |
| `control` | 64 | `Block` | Start, Stop, KillSwitch, Reset |

`DropNewest` on the market topic means events may be dropped under extreme load. This is safe because sequence gaps allow detection and resync. The counter `lq_topic_dropped_total` makes drops visible.

Execution and control topics use `Block`: dropping a fill, an ack, or a kill command corrupts state in a way that cannot be recovered. Backpressure is applied to the producer instead.

## Deterministic Replay

`BacktestRunner` replays a sequence of `MarketEvent`s through the full engine stack. Reproducibility guarantees:

1. The paper venue is created with `reject_prob = 0` (forced, not configurable).
2. Latency is disabled (no artificial delay).
3. Bus publishing is disabled (`with_publishing(false)`) so fills are applied synchronously instead of arriving via the async fan-out broker. The broker's delivery timing is not deterministic.
4. The venue RNG is seeded from `BacktestConfig::seed`.
5. The synthetic market generator (`SyntheticMarketData`) is a seeded random walk.

Same input events produce byte-identical `BacktestResult`. Tests assert this: `deterministic_across_runs`.

## Latency Methodology

The critical path is single-threaded: WS decode -> bus -> book ingest -> analytics -> strategy -> risk -> venue place. No `await` on this path, no locks on the hot path.

`Metrics` records `lq_latency_ns{stage}` histograms with per-stage labels. Benchmarked stage costs (approximate):

| Stage | Cost |
|---|---|
| Decode | ~20 us |
| Analytics | ~1 us |
| Strategy | ~0.9 us |
| Risk | ~7.5 us |

Against a default 250ms quote cadence, pipeline latency is sub-100 microseconds end-to-end.

Paper venue latency (`base_latency_ms` + jitter) is deliberately disabled in backtests for determinism.

## Benchmark Methodology

Criterion benchmarks live in each crate:

```sh
cargo bench --workspace
```

| Crate | Benchmark | What it measures |
|---|---|---|
| `lq-orderbook` | `orderbook`, `orderbook_benchmarks` | Snapshot apply, delta apply, analytics compute |
| `lq-market-data` | `decode` | Binance 20-level depth JSON decode |
| `lq-strategy` | `strategy` | Market-making decision cost |
| `lq-risk` | `risk` | Order validation cost |

Benchmarks use `criterion` with `harness = false`. The `bench` profile includes debug symbols (`debug = 1`).

## Failure Recovery

- **WebSocket disconnect:** The transport emits `FeedStatus::Disconnected`. If `kill_switch_on_reconnect` is set, the risk engine halts and the engine cancels all working orders. Orders are suspect after disconnect; you stop, cancel, and resume only after `Resync` plus a fresh snapshot.
- **Stale data:** `kill_switch_on_stale` engages when no market event arrives within `stale_market_ms`. The book is flagged suspect and quoting halts.
- **Sequence gap:** `BookStore::ingest` detects out-of-order deltas. The book is flagged suspect until a resync snapshot arrives.
- **Solana slot lag:** `SolanaDataAdapter` tracks per-market slot lag. At `stale_slot_lag` it logs a warning; at `critical_slot_lag` it emits a warning suitable for alerting.
- **Kill switch:** Manual engagement via `POST /api/v1/control/kill`. Releases only via explicit `POST /api/v1/control/reset`.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

Test categories:

- **Unit tests:** Every crate has inline tests. The order book tests gap detection, delta application, and book consistency. Strategy tests verify quote generation. Risk tests verify limit enforcement.
- **Determinism tests:** `deterministic_across_runs` runs the backtest twice with identical inputs and asserts byte-identical outputs (events seen, orders placed, fills, final equity, max drawdown).
- **Property-based:** `fills_when_book_crosses_quote` uses a crafted event sequence (snapshot + delta that crosses a resting bid) to verify fill generation.
- **Bus tests:** Verify subscriber ordering, drop counting for slow consumers, no-subscriber counting, and subscriber unregistration on drop.
- **Solana tests:** `processes_unique_events`, `drops_duplicate_slots`, `tracks_connection_state`, `intent_creation`, `intent_expiry`, `pool_mid_price`, `pool_fee_bps`.

## Running Locally

```sh
# Build
cargo build --release

# Deterministic backtest (20k synthetic events, seed 13)
cargo run -p backtest-runner --release -- --events 20000

# Paper trading engine + control-plane API + metrics
cargo run -p trading-engine --release
#   GET  http://localhost:8080/api/v1/state
#   POST http://localhost:8080/api/v1/control/start
#   GET  http://localhost:9100/metrics

# Standalone simulator
cargo run -p simulator --release

# Standalone market data collector
cargo run -p market-data-service --release
```

### Web dashboard

```sh
cd web
npm install
npm run dev          # http://localhost:5173 (proxies to engine API on :8080)
```

### Full stack with Docker

```sh
docker compose -f docker/docker-compose.yml up --build
#   http://localhost:18000   dashboard
#   http://localhost:18080   engine API
#   http://localhost:19100   engine metrics
#   http://localhost:9090    Prometheus
#   http://localhost:3000    Grafana
```

## Configuration

All configuration lives in a single `EngineConfig` struct, deserialized from TOML. Every knob is explicit; nothing is magic.

```toml
mode = "paper"            # "paper" only. "live" is refused.
symbols = ["BTC-USDT"]
venues = ["paper", "simulated"]

[paper]
base_latency_ms = 2.0
latency_jitter_ms = 1.0
fill_fraction = 0.8
fee_rate_bps = 2.5
maker_rebate_bps = 0.5

[strategy.market_making]
enabled = true
half_spread_bps = 5.0
quote_qty = 0.01
inventory_max_qty = 0.5
quote_refresh_ms = 250

[risk]
max_position_qty = 1.0
max_order_qty = 0.1
max_open_orders = 50
max_daily_loss = 1000
kill_switch_on_stale = true
kill_switch_on_reconnect = true

[persistence]
enabled = false
postgres_url = "postgres://lq:lq@localhost:5432/liquidity"
redis_url = "redis://localhost:6379"
```

### Environment overrides

Every config field can be overridden by environment variable. Notable:

| Variable | Overrides |
|---|---|
| `LQ_CONFIG` | Path to TOML config |
| `API_TOKEN` | `[api] token` |
| `POSTGRES_URL` / `DATABASE_URL` | `[persistence] postgres_url` |
| `REDIS_URL` | `[persistence] redis_url` |
| `API_BIND` | `[api] bind` |
| `METRICS_BIND` | `[telemetry] metrics_bind` |
| `LQ_LOG_LEVEL` | `[telemetry] log_level` |
| `LQ_PERSISTENCE_ENABLED` | `[persistence] enabled` |

## Documentation

- `docs/ARCHITECTURE.md` -- system diagram, crate graph, event bus design
- `docs/DATA.md` -- event model, order book, persistence schema
- `docs/TRADING.md` -- strategy, execution, fee accounting
- `docs/RISK.md` -- limits, trip-wires, kill switch
- `docs/OPERATIONS.md` -- config reference, endpoints, metrics
- `docs/DEPLOYMENT.md` -- Docker, compose, Kubernetes
- `docs/DEVELOPMENT.md` -- workspace layout, conventions, pitfalls

## License

Apache-2.0
