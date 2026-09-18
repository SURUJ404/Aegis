# Architecture

This document describes the system design, component responsibilities, data flow, and implementation details of the Aegis liquidity engine.

## System Diagram

```
+--------------------------------------------------------------------+
|                         Application Layer                           |
|  trading-engine  |  market-data-service  |  backtest-runner  | ... |
+--------------------------------------------------------------------+
                              |
+--------------------------------------------------------------------+
|                           Crate Layer                              |
|                                                                    |
|  lq-types  lq-core  lq-exchange  lq-orderbook  lq-market-data     |
|  lq-strategy  lq-risk  lq-execution  lq-simulator  lq-backtest    |
|  lq-persistence  lq-telemetry  lq-api                              |
|  lq-solana-types  lq-solana-data  lq-solana-execution              |
+--------------------------------------------------------------------+
                              |
+--------------------------------------------------------------------+
|                        Infrastructure                              |
|  Tokio runtime  |  Postgres  |  Redis  |  Prometheus  |  Grafana  |
+--------------------------------------------------------------------+
```

## Crate Dependency Graph

```
lq-types
   |
lq-core  (depends on lq-types)
   |
lq-exchange  (depends on lq-types)
   |
lq-orderbook  (depends on lq-types, lq-core, lq-exchange)
   |
lq-market-data  (depends on lq-types, lq-core, lq-exchange)
   |
lq-strategy  (depends on lq-types, lq-core)
   |
lq-risk  (depends on lq-types, lq-core)
   |
lq-execution  (depends on lq-types, lq-core)
   |
lq-simulator  (depends on lq-types, lq-core, lq-exchange, lq-orderbook, lq-execution)
   |
lq-backtest  (depends on lq-types, lq-core, lq-exchange, lq-orderbook,
              lq-strategy, lq-risk, lq-execution)
   |
lq-persistence  (depends on lq-types, lq-core)
   |
lq-telemetry  (depends on lq-types, lq-core)
   |
lq-api  (depends on lq-types, lq-core, lq-telemetry, lq-execution)
   |
lq-solana-types  (depends on lq-types, lq-core, lq-exchange)
   |
lq-solana-data  (depends on lq-types, lq-core, lq-exchange, lq-solana-types)
   |
lq-solana-execution  (depends on lq-types, lq-core, lq-execution, lq-solana-types)
```

### Dependency rules

- `lq-types` has no internal dependencies (leaf crate).
- `lq-core` depends only on `lq-types`.
- Strategy and risk crates depend on `lq-types` and `lq-core` only; they cannot depend on execution, persistence, or market-data. This enforces the purity constraint.
- `lq-backtest` depends on strategy, risk, execution, and orderbook but not on market-data or persistence (it replays events directly).
- `lq-solana-*` crates depend on the corresponding base crates (`lq-types`, `lq-core`, `lq-execution`) and on each other as needed.

## Event Bus Design

The event bus is defined in `crates/core/src/bus.rs`. All inter-component communication flows through it.

### Topics

Three typed, bounded topics:

| Topic | Type | Capacity | Policy |
|---|---|---|---|
| `market` | `Topic<MarketEvent>` | 4096 | `DropNewest` |
| `execution` | `Topic<ExecutionEvent>` | 4096 | `Block` |
| `control` | `Topic<ControlEvent>` | 64 | `Block` |

### PublishPolicy

**`DropNewest`**: When the inbound channel is full, `try_publish` returns `PublishResult::Dropped` and the event is counted. The producer never blocks. This is the correct tradeoff for market data: a dropped delta is recoverable via sequence gaps and resync snapshots.

**`Block`**: When the inbound channel is full, `publish_blocking` applies backpressure. The producer awaits until space is available. This is the correct tradeoff for execution and control events: dropping a fill, an ack, or a kill command corrupts state in a way that cannot be recovered.

### Fan-out broker

Each `Topic` spawns a broker task that:
1. Reads from the inbound channel.
2. Iterates over registered subscriber senders.
3. Uses `try_send` on each subscriber (non-blocking from the broker's perspective).
4. Counts drops and no-subscriber events via atomic counters.

Subscribers register via `topic.subscribe()` which returns a `TopicSubscriber<T>` guard. Dropping the guard unregisters the subscriber. Multiple subscribers per topic are supported.

### Observability counters

Per-topic atomic counters exposed via `topic.stats()`:
- `published`: total events accepted
- `dropped`: events dropped due to full subscriber channels
- `no_subscribers`: events published with no live subscribers

These map to Prometheus metrics: `lq_topic_published_total{topic}`, `lq_topic_dropped_total{topic}`, `lq_topic_no_subscribers_total{topic,subscribers}`.

## Order Book Implementation

### Price representation

Prices are stored as integer ticks. A `Decimal` price is converted to a tick via `price / tick_size`. The `tick_size` comes from the instrument's `InstrumentSpec`. This eliminates floating-point comparison issues and gives O(1) per-level operations.

### Data structure

`OrderBook` maintains two `BTreeMap<u64, u64>` maps (tick -> quantity), one per side. BTreeMap provides O(log n) iteration and O(1) access by key, with ordered traversal for best bid/ask.

Key fields:
- `venue: Exchange`
- `symbol: Symbol`
- `spec: InstrumentSpec` (tick_size, lot_size)
- `bids: BTreeMap<u64, u64>`
- `asks: BTreeMap<u64, u64>`
- `last_seq: u64` (last applied sequence number)
- `last_update_ms: TimestampMs`

### Operations

- `apply_snapshot(OrderBookSnapshot)`: Replaces all levels. Used for initial sync, Binance depth pushes, and resync after gaps.
- `apply_delta(OrderBookDelta)`: Applies a batch of `LevelChange`s. Qty 0 means delete. Sequence numbers are checked; out-of-order yields `IngestOutcome::Gap`.
- `best_bid()` / `best_ask()`: Return the best price via BTreeMap iteration.
- `imbalance(depth)`: Returns bid quantity / (bid + ask quantity) within `depth` levels.
- `depth(side, levels)`: Returns total quantity within `levels` levels.

### Sequence management

`BookStore` wraps per-(venue, symbol) books and checks contiguity:
- Each delta carries a `sequence: u64`.
- `BookStore::ingest` verifies `sequence == last_seq + 1`.
- A gap produces `IngestOutcome::Gap`.
- The book is flagged suspect; quoting halts until a resync snapshot arrives.

This is why Binance depth20 (full-snapshot-every-100ms) needs no sequence bookkeeping while OKX/Bybit (incremental deltas) do.

## Solana Adapter Architecture

Three crates under `crates/solana/`:

### lq-solana-types

Defines the Solana domain model:

- `SolanaProgram`: Enum of known DEX programs (Raydium AMM V4, CLMM, Cpmm, OpenBook V2, Jupiter V6, Pump.fun, Other).
- `SolanaMarketId`: Program + pool address.
- `AmmPoolState`: Reserves, sqrt_price, lp_supply, fee, slot, timestamp.
- `SolanaTrade`: Swap event with token mints, amounts, fee, price.
- `SolanaLiquidityChange`: Add/remove liquidity event.
- `Slot`: Newtype over `u64` for Solana slot numbers.
- `OrderIntent`: Transport-agnostic order description with slippage protection and TTL.

### lq-solana-data

The `SolanaDataAdapter` processes raw `MarketDataEvent`s:

```
MarketDataEvent (PoolState / Swap / LiquidityChange / Heartbeat)
       |
       v
  SlotTracker        -- deduplication, slot lag detection
       |
       v
  SolanaNormalizer   -- converts to Aegis MarketEvent
       |
       v
  EventBus.market()  -- publishes to the shared bus topic
```

Key behaviors:
- **Deduplication**: `SlotTracker` tracks the last seen slot per market. Events at or below the last slot are dropped.
- **Staleness detection**: Slot lag (global latest - per-market latest) is computed. Warnings at `stale_slot_lag`, critical alerts at `critical_slot_lag`.
- **Connection state**: `ConnectionState` manages disconnect/reconnect lifecycle with exponential-backoff reconnect delays.

### lq-solana-execution

Handles order submission on Solana:

- `OrderIntent` is the output of the risk engine and the input to the transaction builder. It contains market, side, token mints, amounts, slippage tolerance, and TTL.
- Transaction building constructs Solana instructions from the intent.
- Submission sends the transaction to the RPC endpoint.
- Reconciliation matches on-chain fills against local order state.

## Strategy Trait Design

The `Strategy` trait is defined in `crates/strategy/src/lib.rs`:

```rust
trait Strategy {
    fn on_market_state(
        &self,
        market: &MarketState,
        inventory: Option<&Inventory>,
        position: Option<&Position>,
        halted: bool,
        running: bool,
    ) -> Vec<StrategyDecision>;
}
```

### Purity constraint

Strategies are pure functions:
- No networking (no WebSocket, no HTTP)
- No database access
- No event bus access
- All input comes through `StrategyContext`
- All output is `StrategyDecision` values

This buys:
1. **Determinism**: Backtests are replayable because strategy output depends only on input.
2. **Single risk choke-point**: Every order passes `RiskEngine::validate_order` in exactly one place (`place_checked`).
3. **Trivial unit testing**: Strategies can be tested with no runtime, no mocks.
4. **Safety**: A buggy strategy physically cannot bypass the kill switch.

### StrategyDecision variants

- `Quote(QuoteIntent)`: Replace the two-sided quote for a symbol/venue. Contains bid and ask price/qty.
- `MarketOrder(MarketOrderSignal)`: Take liquidity now with a specific side, qty, and price.
- `StandDown { reason }`: Cancel working orders, wait.
- `Hold`: Do nothing.

### Baseline strategy: MarketMakingStrategy

Quotes both sides around the mid:
```
bid_price = mid * (1 - half_spread * (1 + skew))
ask_price = mid * (1 + half_spread * (1 - skew))
```

- `half_spread` adapts to volatility within `min_spread_bps` / `max_spread_bps`.
- `skew` is a function of inventory vs. `inventory_target_qty` and `inventory_max_qty`.
- Order-book imbalance further offsets each side.
- Quotes refresh at most every `quote_refresh_ms`; each refresh cancels and re-places.

## Risk Engine Architecture

`RiskEngine` is the only authority between a strategy decision and an order on a venue.

### Validation flow

```
StrategyDecision::Quote(intent)
       |
       v
RiskEngine::validate_order(order, mark_price, now)
       |
       +---> Allow         -> place as-is
       +---> Reduce {qty}  -> place with reduced quantity
       +---> Reject        -> do not place, count reject
       +---> Halt          -> do not place, engage kill switch
```

### Limits

| Limit | Bound |
|---|---|
| `max_position_qty` | Net absolute position per venue/symbol |
| `max_order_qty` | Single order quantity |
| `max_open_orders` | Concurrent working orders |
| `max_notional` | Notional of a single order |
| `max_daily_loss` | Realized + unrealized loss before halting |
| `max_order_rate_per_sec` | Order submissions per second |
| `max_exposure_per_venue` | Sum of working order notional per venue |
| `max_price_deviation_bps` | Order price vs. mark price deviation |

### Kill switch

Engages on:
- Stale feed (no market event for `stale_market_ms`)
- Venue reconnect (order state suspect)
- Loss limit exceeded (`max_daily_loss`)
- Manual (`POST /api/v1/control/kill`)

Releases only via explicit `POST /api/v1/control/reset`.

## Execution Pipeline

### ExecutionVenue trait

```rust
#[async_trait]
trait ExecutionVenue {
    async fn place_order(&self, order: &mut Order) -> Result<OrderPlacement>;
    async fn cancel_order(&self, order_id: Uuid) -> Result<()>;
    async fn cancel_all(&self, symbol: Option<&Symbol>) -> Result<()>;
    fn working_order_ids(&self) -> Vec<Uuid>;
    fn order_snapshot(&self, id: Uuid) -> Option<OrderSnapshot>;
}
```

### PaperExecutionVenue

The reference implementation. Configurable via `PaperSimConfig`:
- **Latency**: `base_latency_ms` + uniform `latency_jitter_ms` (disabled in backtests).
- **Fill model**: When the market crosses a resting order, fills with probability `fill_fraction` (times queue-position weighting). Partial fills possible.
- **Rejection**: `reject_prob` (forced to 0 in backtests).
- **Market orders**: Price against the venue's live touch with `slippage_bps`.

### OrderStateMachine

Orders move through a validated state machine:

```
Created -> Submitted -> Acknowledged -> Filled
                  \-> Cancelled / Expired / Rejected
```

Illegal transitions are rejected. Fills update `filled_quantity`, `avg_fill_price`, and status. Partial fills are tracked until remaining quantity is zero.

### PositionManager

The system of record for positions, inventory, and realized PnL:
- Processes `ExecutionEvent`s (primarily `Fill` events).
- Tracks per-(venue, symbol) position: `net_qty`, `avg_entry`, `realized_pnl`.
- Inventory is per-symbol, aggregated across venues.
- Opening fills (increasing |position|) charge fee against realized PnL.
- Closing fills (reducing |position|) realize PnL `(exit - entry) * qty`.

## Persistence Layer

### PostgresStore

Durable system of record for audit, reconciliation, and backtest analysis.

Schema (created by `migrate()`):

```sql
market_data(
  id        BIGSERIAL PRIMARY KEY,
  venue     TEXT, symbol TEXT, kind TEXT,
  seq       BIGINT,
  ts        TIMESTAMPTZ,
  payload   TEXT
);

executions(
  id        BIGSERIAL PRIMARY KEY,
  venue     TEXT, symbol TEXT, kind TEXT,
  order_id  UUID,
  ts        TIMESTAMPTZ,
  payload   TEXT
);

order_events(
  id        BIGSERIAL PRIMARY KEY,
  order_id  UUID,
  venue     TEXT, kind TEXT,
  ts        TIMESTAMPTZ
);
```

### RedisHotState

In-memory hot state mirror for low-latency reads:
- `lq:last_price:<symbol>`: last trade price
- `lq:halted`: "1"/"0"
- `lq:open_orders:<venue>`: count

Redis is disposable: lose it and nothing is wrong (rebuilds from the bus). Lose Postgres and you lose the audit trail.

### PersistenceSink

`PersistenceSink::spawn(bus, store)` subscribes to market + execution topics and forwards events to the store. It is a collection workload, not a lossless journal: under extreme load it falls behind and drops market events (documented tradeoff). For lossless bookkeeping, write fills/positions directly through the store.

## Control Plane API

Defined in `crates/api/src/lib.rs`, served by Axum.

### Endpoints

| Method | Path | Description |
|---|---|---|
| GET | `/healthz` | Liveness probe |
| GET | `/api/v1/state` | Aggregate engine snapshot |
| GET | `/api/v1/positions` | Per-venue positions |
| GET | `/api/v1/inventory` | Per-symbol net inventory |
| GET | `/api/v1/orders` | Order history |
| GET | `/api/v1/market-state` | Latest MarketState per venue/symbol |
| GET | `/api/v1/risk` | Risk status + halt reason |
| POST | `/api/v1/control/start` | Start strategies |
| POST | `/api/v1/control/stop` | Stop strategies + cancel all |
| POST | `/api/v1/control/reset` | Release kill switch |
| POST | `/api/v1/control/kill` | Engage kill switch (body: `{"reason": "..."}`) |

### Authentication

If `api.token` is set (or `API_TOKEN` env var), every `/api/*` endpoint requires `Authorization: Bearer <token>`. `/healthz` and CORS preflight `OPTIONS` are always exempt.

### State access pattern

The engine loop is the sole writer to `EngineState`. The API and metrics are readers. Shared state lives behind `DashMap` / `parking_lot` locks. The event loop holds write locks only during the critical path; the API reads are lock-free (DashMap snapshot reads).

## Threading Model

- The `trading-engine` runs one single-threaded event loop task owning strategy, risk, and books.
- Feeds, venues, API server, and metrics server run on the multi-threaded Tokio runtime.
- `EventBus::new()` spawns broker tasks and must be called inside a Tokio runtime.
- `PaperExchange` (simulator matching) is `!Send`; it runs on a current-thread `LocalSet`.
- DashMap reads from the API and metrics are cheap (sharded, lock-free reads).

## Latency Pipeline

The critical path has no `await` and no locks:

```
WS decode (~20us)
    |
    v
EventBus.market().try_publish()  [non-blocking, DropNewest]
    |
    v
BookStore::ingest()  [sequence check, BTreeMap update]
    |
    v
MarketStateEngine::compute()  [~1us, pure computation]
    |
    v
StrategyEngine::on_market_state()  [~0.9us, pure function]
    |
    v
RiskEngine::validate_order()  [~7.5us, limit checks]
    |
    v
ExecutionVenue::place_order()  [paper: simulated latency]
```

Total pipeline latency (excluding venue): sub-100 microseconds.

Per-stage histograms: `lq_latency_ns{stage}` (Prometheus).

## Deterministic Backtest Design

`BacktestRunner` in `crates/backtest/src/runner.rs`:

1. Creates a `PaperExecutionVenue` with:
   - `reject_prob = 0` (forced)
   - Latency disabled
   - `with_publishing(false)` (fills applied synchronously, not via bus)
   - Seeded RNG

2. Replays events sequentially:
   - Applies snapshots/deltas to the local book
   - Runs strategy on each event
   - Applies risk checks
   - Places orders through the paper venue
   - Sweeps maker fills (checks if resting orders are crossed by current book)
   - Applies fills synchronously via `report_fill` return value

3. Produces `BacktestResult` with: events seen, orders placed, rejections (by code), fills, trades, win rate, fees, net PnL, max drawdown, annualized Sharpe, equity curve.

The key determinism guarantee: bus publishing is disabled so fill events are never delivered asynchronously. This prevents the broker's non-deterministic delivery timing from perturbing inventory updates and cascading into different quotes and fill prices.
