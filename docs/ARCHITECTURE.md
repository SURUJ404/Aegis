# Architecture

This document describes the component model, data flow, and threading model of
the engine. It is deliberately written against the code: every crate maps to a
single responsibility, and every boundary is a type.

## Component model

The system is a single pipeline from market data to position/PnL. The
canonical view (annotated with the two deviations from the picture — the risk
fan-out includes `Reduce`/`Reject`, and the control plane + `EventBus` topics
sit alongside the pipeline):

```
REAL-TIME MARKET DATA
                     │
                     ▼
             WebSocket Gateway
                     │
                     ▼
              Order Book Engine
                     │
             ┌───────┴────────┐
             ▼                ▼
       Microstructure     Cross-Venue
          Engine            Analyzer
             │                │
             └───────┬────────┘
                     ▼
               Strategy Engine
                     │
                     ▼
                 Risk Engine
                     │
      ┌──────────────┼───────────────┐
      │              │               │
   ALLOW       REDUCE / REJECT     HALT
      │
      ▼
          Execution Engine
              │
              ▼
        Paper Exchange
              │
              ▼
       Position / PnL Engine
              │
       ┌──────┴─────────┐
       ▼                ▼
   PostgreSQL         Redis
       │
       ▼
 Prometheus → Grafana
```

Annotations:

- **Risk fan-out.** Beyond `ALLOW` and `HALT`, the risk engine returns
  `REDUCE` (place with a scaled quantity) and `REJECT` (do not place) —
  `crates/risk`.
- **Control plane.** `lq-api` (start / stop / reset / kill-switch) publishes
  `ControlEvent`s that the engine loop consumes; it is not shown above because
  it does not sit in the data path.
- **EventBus topics.** All stages communicate over three bus topics — `market`
  (bounded, drop-newest), `execution` (blocking), `control` (blocking). See
  "Event model" below.
- **Paper Exchange.** `lq-simulator`'s `PaperExchange` is the reference venue;
  `lq-execution`'s `PaperExecutionVenue` is the *client* side that places
  orders into it.

The crate-to-pipeline mapping:

```
                    ┌─────────────────────────────────────────────┐
                    │                  lq-telemetry                │
                    │   tracing logs   ·   Prometheus metrics      │
                    └─────────────────────────────────────────────┘
                    ▲                        ▲
                    │                        │
┌───────────────┐   │    ┌──────────────┐    │   ┌──────────────────┐
│ lq-market-data│───┼───▶│  lq-orderbook │───┼──▶│   lq-strategy    │
│ WS adapters   │market│  book + engine │    │   │  decisions       │
│ (okx/bin/byb) │events│  analytics     │ms  │   └────────┬─────────┘
└───────────────┘      └──────────────┘    │            │ Quote / MarketOrder
                                           │            ▼
┌───────────────┐     ┌──────────────┐     │   ┌──────────────────┐
│ lq-simulator  │────▶│ lq-execution │◀────┼───│    lq-risk       │
│ synthetic mk  │     │ paper venue  │exec.│   │  validate limits │
│ paper match   │     │ positions    │events│   └──────────────────┘
└───────────────┘     └──────────────┘     │
                                           │
                    ┌──────────────┐        │   ┌──────────────────┐
                    │ lq-backtest  │────────┴──▶│  lq-api          │
                    │ deterministic│            │ control plane    │
                    └──────────────┘            └──────────────────┘
```

The application binaries compose these crates:

- `trading-engine` — feeds → books → analytics → strategy → risk → paper
  venues, plus the API and metrics servers.
- `market-data-service` — feeds + `lq-persistence` sink only.
- `simulate` — synthetic market + `PaperExchange` matching.
- `backtest` — `BacktestRunner` over a recorded/synthetic event sequence.
- `api-server` — the control-plane API backed by an empty `EngineState`.

## Data flow

1. **Ingestion.** A `FeedDecoder` (per venue) normalizes venue-native JSON into
   `MarketEvent`s and publishes them to the shared market topic. Synthetic
   venues publish from `SyntheticMarketData`.
2. **Book keeping.** `BookStore.ingest` applies snapshots/deltas with sequence
   checks. Gaps are detected (`IngestOutcome::Gap`) and trading is treated as
   suspect until a resync.
3. **Analytics.** `MarketStateEngine` computes a `MarketState` (mid, microprice,
   imbalance, regime, volatility) from the book and trade flow.
4. **Strategy.** `StrategyEngine` turns `MarketState` + inventory/position into
   `StrategyDecision`s. Strategies are pure: no I/O, no bus access.
5. **Risk.** Every order is validated by `RiskEngine` before touching a venue.
   The result is `Allow`, `Reduce`, `Reject` or `Halt`.
6. **Execution.** `PaperExecutionVenue` (or a real venue later) places,
   tracks, and fills orders. Fill events drive `PositionManager` (the system of
   record for PnL) via the execution topic.
7. **Observation.** Every event and state change feeds `Metrics`; the control
   plane reads `EngineState` directly.

## Event model

All inter-component communication goes through `EventBus` topics:

| Topic | Policy | Producer | Consumer |
|---|---|---|---|
| `market` | `DropNewest` (bounded) | feeds, synthetic venues | book store, exchange matcher, persistence |
| `execution` | `Block` | venues | `PositionManager`, strategies |
| `control` | `Block` | API, operators | engine loop |

`DropNewest` means market data may be dropped under extreme load; this is safe
because sequence numbers allow gap detection and resync. Execution and control
events are never silently dropped.

## Threading model

- The `trading-engine` runs one single-threaded event loop task that owns the
  strategy, risk, and books. Feeds, venues, the API server, the metrics server
  and the metrics sampler run on the multi-threaded Tokio runtime.
- Shared state (`EngineState`, `BookStore`, venue maps) lives behind
  `DashMap` / `parking_lot` locks. The event loop is the *writer*; the API and
  metrics are *readers*.
- `EventBus::new()` spawns broker tasks and must be called inside a Tokio
  runtime (see `docs/DEVELOPMENT.md` for the pitfall).
- `PaperExchange` (simulator matching) is `!Send`; it runs on the current
  thread inside a `LocalSet`.

## Application layout

```
crates/
  types/       primitive domain types (Exchange, Side, Price, Amount, Symbol)
  core/        config, event model, bus, EngineState, domain models
  exchange/    instrument specs and venue metadata
  orderbook/   OrderBook, BookStore, MarketStateEngine
  market-data/ FeedDecoder + WS transport (okx/binance/bybit)
  strategy/    Strategy trait, MarketMakingStrategy, cross-venue analyzer
  risk/        RiskEngine, RiskDecision
  execution/   PaperExecutionVenue, OrderStateMachine, PositionManager
  simulator/   SyntheticMarketData, SimulatedFeed, PaperExchange
  backtest/    BacktestRunner, PerfMetrics
  persistence/ PostgresStore, RedisHotState, PersistenceSink
  telemetry/   init_logging, Metrics, MetricsServer
  api/         build_router + ApiState
apps/
  trading-engine/     the full engine
  market-data-service/ collection + persistence only
  backtest-runner/    deterministic backtest binary
  simulator/          paper exchange + synthetic market
  api-server/         control plane only
```
