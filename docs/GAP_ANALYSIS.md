# Gap Analysis: Aegis → dYdX v4-Style Perpetuals Core

**Status:** Phase 0 complete; Stages 1–4 (`lq-sequencer`, `lq-clob`, `lq-perps`, `lq-oracle`) implemented.  
**Reference:** [dYdX v4-chain](https://github.com/dydxprotocol/v4-chain) — `protocol/x/clob`, `x/perpetuals`, `x/subaccounts`, `x/prices`, `x/liquidations`, `indexer/`, `v4-clients`.  
**Date:** 2026-09-24

---

## 1. Current Architecture

### 1.1 What Aegis is today

A **single-process, multi-venue market-making / liquidity engine** focused on:

- Normalizing external CEX (and Solana) market data into one event model
- Maintaining a **disposable aggregated local book** (level totals, not order-level)
- Running a **pure strategy** behind an in-process **risk gate** and **paper execution**
- Deterministic **backtesting** and a control-plane **API + dashboard**

It is a *trading bot platform*, not an *exchange*. There is no matching engine for client orders, no subaccounts, no margin, no liquidation, no consensus, and no ordered global log.

### 1.2 Workspace map (as-built)

| Layer | Members |
|---|---|
| Apps | `trading-engine`, `market-data-service`, `backtest-runner`, `simulator` (`simulate`), `api-server` |
| Crates (workspace) | `lq-types`, `lq-core`, `lq-exchange`, `lq-orderbook`, `lq-market-data`, `lq-strategy`, `lq-risk`, `lq-execution`, `lq-simulator`, `lq-backtest`, `lq-persistence`, `lq-telemetry`, `lq-api`, `lq-sequencer`, `lq-solana-market-data` |
| Crates (on disk, **not** workspace members) | `crates/solana/{types,data,execution}` → `lq-solana-types`, `lq-solana-data`, `lq-solana-execution` |
| Read path (today) | In-process `EngineState` (DashMap) → Axum REST + Prometheus; React dashboard polls `/api/v1/state` |
| Infra | Tokio, Postgres (audit sink), Redis (hot keys), Prometheus/Grafana, Docker/K8s/Fly/Railway |

### 1.3 Data flow (today)

```
External WS feeds (OKX/Binance/Bybit) or SyntheticMarketData
        │  MarketEvent (Snapshot|Delta|Trade|Tick|Status)
        ▼
EventBus.market  ── capacity 4096, DropNewest (lossy OK: seq gap + resync)
        │
        ▼  single event-loop task (apps/trading-engine/src/engine.rs)
BookStore::ingest  (venue feed book; sequence contiguity; gap → suspect)
        │
MarketStateEngine::compute  (mid, microprice, imbalance, vol, regime — some f64 analytics)
        │
StrategyEngine  (pure: Quote | MarketOrder | StandDown | Hold)
        │
RiskEngine::validate_order_at  (Allow | Reduce | Reject | Halt + kill switch)
        │
PaperExecutionVenue::place_order  (async; latency/fill_fraction RNG; optional bus publish)
        │
sweep_working_orders  (cross local feed book vs resting paper orders → report_fill)
        │
PositionManager / EngineState  (fills → positions, inventory, realized PnL)
        │
        ├─► EventBus.execution (Block) → strategy.on_execution_event, state updates
        ├─► PersistenceSink → Postgres (best-effort collection) / Redis hot state
        └─► API + metrics readers (DashMap snapshots; sorted on read)
```

Control path: `POST /api/v1/control/*` → `EventBus.control` (Block, cap 64) → same loop.

Backtest path: `BacktestRunner` replays `MarketEvent`s **synchronously** with `with_publishing(false)`, `reject_prob=0`, latency off, seeded RNG → byte-identical `BacktestResult`.

### 1.4 Threading / concurrency model

| Component | Model |
|---|---|
| Critical path | One `tokio::select!` loop task: market / execution / control. No `await` between ingest and risk on the hot path (venue place is `async` but paper). |
| Feeds | Separate Tokio tasks per venue/symbol; publish via `try_publish` (market DropNewest). |
| API + metrics | Axum + metrics server tasks; read `EngineState` via DashMap (sharded reads). |
| Bus | Per-topic broker task; fan-out with `try_send` to subscribers; atomic stats. |
| Simulator `PaperExchange` | Documented `!Send`-friendly, driven from one task; matching uses `parking_lot` + seeded `StdRng`. |
| Shared state | `EngineState` = DashMap + `parking_lot::RwLock`; engine is sole writer (API is reader). |
| Persistence | Optional background sinks; **not** on the money path; may drop market events. |

**Implication for target:** money path must become *stricter* — one state machine per market (or global), fed only by an ordered log; no concurrent writers; no wall-clock or RNG inside transitions.

### 1.5 Public API surface (crates)

| Crate | Primary exports (abridged) |
|---|---|
| `lq-types` | `Price`/`Qty`/`Amount` = `rust_decimal::Decimal`; `Money`; `Side`; `OrderType` (Limit, Market, PostOnly, IOC, FOK); `OrderStatus`; `TimeInForce`; `Exchange`; `Symbol`; `TimestampMs` |
| `lq-core` | `EventBus`, `Topic`, `MarketEvent`/`ExecutionEvent`/`ControlEvent`, `EngineConfig`, `EngineState`, models (`Order`, `Position`, `Inventory`, `MarketState`, `StrategyDecision`, `FillEvent`, …) |
| `lq-exchange` | `InstrumentSpec`, `FeeSchedule`, `VenueMeta` |
| `lq-orderbook` | `OrderBook` (+ impls), `BookStore`, `IngestOutcome`, `MarketStateEngine` |
| `lq-market-data` | `run_ws`, `FeedDecoder`, `WsConfig`, adapters (okx/binance/bybit) |
| `lq-strategy` | `Strategy`, `StrategyContext`, `StrategyEngine`, `MarketMakingStrategy`, `CrossVenueAnalyzer` |
| `lq-risk` | `RiskEngine`, `RiskDecision`, `RiskCode` |
| `lq-execution` | `ExecutionVenue`, `PaperExecutionVenue`, `OrderStateMachine`, `PositionManager` |
| `lq-simulator` | `PaperExchange`, `SyntheticMarketData`, `SimulatedFeed` |
| `lq-backtest` | `BacktestRunner`, `BacktestConfig`, `PerfMetrics` |
| `lq-persistence` | `PostgresStore`, `RedisHotState`, `PersistenceSink` |
| `lq-telemetry` | `init_logging`, `Metrics`, `MetricsServer` |
| `lq-api` | `build_router`, `ApiState`, control + state handlers, optional bearer auth |
| Solana | `SolanaDataAdapter`, Geyser/logs/RPC stubs; `OrderIntent` (sub-crates not in workspace) |

### 1.6 Existing guarantees (what we keep)

- **Paper default; live refused** (`mode = Mode::Live` → bail).
- **Deterministic backtest** (seeded RNG, sync fills, no bus timing).
- **Strategy purity** (single risk choke-point `place_checked`).
- **Sequence-gap detection** on external books; book is disposable vs fills as SoR.
- **Bounded typed bus** with explicit DropNewest vs Block policies + drop counters.
- **Fixed-point money types** via `rust_decimal` (not IEEE floats for Price/Qty).
- **`unsafe_code = "forbid"`** workspace-wide; Clippy `all = warn`.
- Rich docs (`docs/*`), Prometheus histograms `lq_latency_ns{stage}`, Grafana.

---

## 2. Target Architecture (dYdX v4-style)

**Principle:** everything that touches money is a **deterministic state machine fed by an ordered log**. Everything else is a **disposable read replica**.

### 2.1 Write path

```
Client (v4-style / MM bot)
   │  ed25519-signed orders, rate limits, schema validation
   ▼
Gateway (Axum/WS)
   │  admit → sequencer
   ▼
lq-sequencer ── global monotonic seq, WAL, snapshots, replay
   │  ordered LogEntries (Place/Cancel/Replace/Oracle/Daemon actions)
   ▼
lq-clob  (per-market single-threaded book: price-time priority,
   Limit/Market/PostOnly/IOC/FOK/ReduceOnly, cancel/replace, STP,
   short-term orders expire-by-sequence, stateful/conditional orders)
   │  fills + order lifecycle entries
   ▼
lq-perps (subaccounts, IM/MM margin, mark/index, funding,
   liquidation @ bankruptcy price, insurance fund, ADL;
   fill + margin update = one atomic transition)
   │
   ├── state hash every N entries; halt on divergence
   └── block invariants (collateral, Σpositions=0, margin health)

lq-oracle: multi-venue feeds (reuse lq-market-data) → median,
   outlier rejection, staleness → **log entries** (state never
   reads live externals)

Daemons (liquidator, funding, oracle) submit actions as log entries.
```

### 2.2 Read path

```
Engine emits domain events (fill, order_update, position, funding)
   → Redpanda/Kafka
   → lq-indexer (ingester → Postgres; REST; WS fan-out via Redis)
```

Indexer may lag/crash without affecting trading.

### 2.3 Replication

- **Phase 1:** Raft (`openraft`), 3 nodes, hot standby, failover; identical state machines.
- **Phase 2 (design only):** CometBFT/ABCI compatibility.

### 2.4 Hard requirements (normative)

1. Fixed-point integer math for prices, sizes, margin — **no floats in state**.
2. No wall-clock, RNG, or HashMap iteration order **inside the state machine**. Same log ⇒ byte-identical state hash.
3. State-hash comparison across replicas every N entries; **halt on divergence**.
4. Per-block invariants: collateral conserved; Σ positions = 0; no subaccount below maintenance without pending liquidation.
5. Per-market circuit breakers: oracle deviation, stale price, liquidation cascade.
6. Paper mode default; live disabled.
7. No `unsafe` without justification; Clippy clean.

---

## 3. Gap Table: Current → dYdX v4 Target

Legend: **P** = present (partial), **A** = absent, **R** = present but wrong shape (must rework).

| # | Capability | dYdX v4 analogue | Aegis today | Gap | Target stage |
|---|---|---|---|---|---|
| **Sequencing / log** | | | | | |
| 1 | Global monotonic sequence | CometBFT block height + `OrderSeq` | **Done:** `lq-sequencer` global + per-market seq | **P** | Stage 1 ✅ |
| 2 | Write-ahead log + replay | ABCI txs / block store | **Done:** CRC-framed WAL, tail recovery | **P** | Stage 1 ✅ |
| 3 | Snapshots + rebuild-from-empty-log | CometBFT state sync | **Done:** snapshot + WAL suffix; empty-log hash equality | **P** | Stage 1 ✅ |
| 4 | Deterministic state hash | App hash | **Done:** SHA-256 canonical `StateHash` (Decimal normalized) | **P** | Stage 1 ✅ (+7 wire across replicas) |
| 5 | Event-sourced money state | Msg-based state machine | **Done:** `LedgerState` (Stage 1) + `ClobState` (Stage 2); engine still uses `EngineState` on live path until Stage 5+ cutover | **P** | Stage 1–3 |
| **CLOB** | | | | | |
| 6 | Order-level book, price-time priority | `x/clob` memclob | **Done:** `lq-clob` `Book` = `BTreeMap<Price, VecDeque<Uuid>>` per side; FIFO within level | **P** | Stage 2 ✅ |
| 7 | Continuous matching of client orders | CLOB match | **Done:** matching inside `apply`; maker-price execution; outputs = fills | **P** | Stage 2 ✅ |
| 8 | Order types: Limit, Market, PostOnly, IOC, FOK | CLOB order types | **Done:** `exec_policy` enforces all TIF branches; market never rests | **P** | Stage 2 ✅ |
| 9 | Reduce-only orders | CLOB reduce-only | **Done:** place-time cap (`PreTradeVerdict::Reduce`), auto-cancel (`CancelReason::ReduceOnly`) when position shrinks, end-of-apply revalidation | **A** | Stage 3 ✅ |
| 10 | Cancel / replace (atomic) | CLOB cancel/replace | **Done:** `ReplaceOrder` entry, no-mutation on failure | **P** | Stage 2 ✅ |
| 11 | Self-trade prevention | STP policies | **Done:** none / cancel-resting / cancel-taker / cancel-both via `owner` (normalized to `sub:<n>` per subaccount) | **P** | Stage 2 ✅ + Stage 3 ✅ |
| 12 | Short-term orders (memory, expire by seq) | ST order window | **Done:** `expiration_ms` vs entry `ts_ms`, global sweep; stateful = `None` | **P** | Stage 2 ✅ |
| 13 | Stateful / conditional long-term orders | Conditional orders | Stateful = no expiry (done); conditional orders still absent | **P** | Stage 2 (stateful ✅; conditional later) |
| 14 | Price-time priority correctness tests / proptest | protocol tests | **Done:** 28 behavior tests + proptest invariants (consistency, conservation, replay hash) | **P** | Stage 2 ✅ |
| **Perps / margin** | | | | | |
| 15 | Subaccounts (USDC collateral, positions) | `x/subaccounts` | **Done:** `lq-perps` `Subaccount { collateral, positions }`, `Transfer` deposits/withdrawals, insurance ledger reserved at `u64::MAX` | **A** | Stage 3 ✅ |
| 16 | Initial / maintenance margin | `x/perpetuals` | **Done:** `MarketParams` IM/MM ratios; requirement = `|qty|·price·ratio` + open-order reservation; limits absorbed as pre-trade checks | **A** | Stage 3 ✅ |
| 17 | Mark price / index price | oracle + mark | **Done:** mark = last trade/tick inside the SM; **reference price** now prefers the Stage 4 oracle median (`price_of` = oracle → tick → trade) | **R** | Stage 3 ✅ (mark) / Stage 4 ✅ (oracle) |
| 18 | Funding payments | funding daemon | **Done:** `SettleFunding` zero-sum payments + funding index (operator-triggered and oracle-gated; interval daemon moved to Stage 5) | **A** | Stage 3 ✅ |
| 19 | Liquidation @ bankruptcy price | `x/liquidations` | **Done:** limit price `mark + (fee − equity)/qty`, synthetic IOC vs book, insurance residual, cascade breaker | **A** | Stage 3 ✅ |
| 20 | Insurance fund | `x/insurance` | **Done:** fees credit the insurance ledger; residual closes settle against it at the limit price | **A** | Stage 3 ✅ |
| 21 | ADL (auto-deleveraging) | ADL | **Done (full, not stub):** while insurance equity < 0, close largest opposite position at the insurance-neutral price | **A** | Stage 3 ✅ |
| 22 | Atomic fill + margin transition | single state transition | **Done:** `PerpsState` composes `ClobState`; pre-trade check → match → cash/margin in one `apply` | **A** | Stage 3 ✅ |
| 23 | Risk limits absorbed as pre-trade margin checks | checkTx / place order | **Done:** `pre_trade_check` = validation + `PerpsConfig` limits + margin, verdicts Allow/Reduce/Reject; `RiskEngine` remains only on the legacy engine path | **A** | Stage 3 ✅ |
| 24 | Block invariants (collateral, Σpos, margin) | EndBlocker | **Done:** `check_invariants` — collateral conserved, `Σ positions = 0`, pending = recomputed below-maintenance set — asserted after every apply in tests | **A** | Stage 3 ✅ |
| **Oracle** | | | | | |
| 25 | Multi-venue aggregation, median | `x/prices` | **Done:** `ObservationBook` (newest-per-venue, reuses `lq-market-data` events) + pure `aggregate()` — freshness filter → median → outlier rejection; `OraclePrice` log entries publish the result | **A** | Stage 4 ✅ |
| 26 | Outlier rejection | price feed slashing | **Done:** median ± `outlier_band_bps` (1 % default); too few survivors ⇒ `ConsensusLost`, no publish; deviation breaker rejects publications moving > `max_deviation_bps` (10 % default) and halts the market | **A** | Stage 4 ✅ |
| 27 | Staleness checks → halt | exchange params | **Done (log-driven):** `oracle_gate` = `oracle_halted` (latched deviation) / `oracle_stale` (`ts_ms − published_ts_ms > max_staleness_ms`); gates place/replace/liquidate/settle-funding; no wall clock in the SM | **P** | Stage 4 ✅ |
| 28 | Oracle results as log entries | vote/price txs | **Done:** `EntryPayload::OraclePrice(OraclePriceCmd { price, observation_ts_ms, sources, override_band })` validated in-state → `ApplyOutput::OraclePublished`; rejections are `Ok` + `Rejected` outputs | **R** | Stage 4 ✅ |
| **Gateway / auth** | | | | | |
| 29 | Ed25519-signed orders | Cosmos secp256k1 / eth | Bearer token on **control API only**; orders unsigned | **A** | Stage 5 |
| 30 | Order schema validation + rate limits at edge | CheckTx | Risk rate limit **after** strategy, in-process | **P** | Stage 5 |
| 31 | WS order gateway | indexer/gRPC + REST | Control REST only; no order ingress WS | **A** | Stage 5 |
| 32 | Client auth ≠ operator auth | separate concerns | Single optional `API_TOKEN` | **R** | Stage 5 |
| **Read path / indexer** | | | | | |
| 33 | Kafka/Redpanda event stream | indexer Kafka | None | **A** | Stage 6 |
| 34 | `lq-indexer` → Postgres | indexer/services/ender | `PostgresStore` best-effort market/exec dump | **R** | Stage 6 |
| 35 | REST read API for fills/orders/positions | indexer REST | Engine `/api/v1/*` (live engine state, not derived) | **P** | Stage 6 |
| 36 | WS fan-out via Redis | indexer WS | Dashboard 1s polling | **R** | Stage 6 |
| 37 | Indexer crash-safe (async lag OK) | indexer isolation | Same process as trading today if wired naively | **A** | Stage 6 |
| **Replication** | | | | | |
| 38 | Multi-node identical state | CometBFT validators | Single process | **A** | Stage 7 |
| 39 | Raft (openraft) hot standby | consensus | None | **A** | Stage 7 |
| 40 | Failover + hash compare / halt | evidence / app hash | None | **A** | Stage 7 |
| 41 | CometBFT/ABCI design | ABCI | N/A | **A** | Stage 7 design note |
| **Strategy as client** | | | | | |
| 42 | MM bot external to core | v4-clients | Strategy **in-process** in engine loop | **R** | Stage 8 |
| 43 | Optional dYdX venue adapter | v4-clients / full node | Solana/CEX adapters only; Solana not in workspace | **P** | Stage 8 optional |
| **Determinism / numerics** | | | | | |
| 44 | Fixed-point only in state | int prices | `rust_decimal::Decimal` (fixed-point) for money; **OK foundation** | **P** | keep; tighten |
| 45 | No f64 in state | — | `MarketState` analytics fields are `f64` (imbalance, vol, …); risk uses `as_f64()` for bps; paper fill uses `f64` RNG | **R** | Stages 1–3 (move analytics out of money SM) |
| 46 | No wall-clock in SM | block time from consensus | **Done in `lq-sequencer`:** `ts_ms` on entry; `LedgerState` never calls `now()`. Legacy risk/API still use wall clock outside SM | **P** | Stage 1 ✅ (legacy paths later) |
| 47 | No HashMap iteration in SM | deterministic iteration | **Done in `LedgerState`:** `BTreeMap` only. `EngineState` still DashMap until cutover | **P** | Stage 1 ✅ |
| 48 | No RNG in SM | deterministic | **Done in `LedgerState`/sequencer:** no RNG. Paper venue RNG remains simulation-only | **P** | Stage 1 ✅ |
| 49 | Replica hash comparison | app hash | Hash primitive ready; cross-node compare is Stage 7 | **A** | Stage 1 primitive + Stage 7 |
| **Testing** | | | | | |
| 50 | proptest matching + margin invariants | property tests | **Stage 1:** proptest seq monotonicity + codec roundtrip; **Stage 2:** book/order consistency; **Stage 3:** collateral conservation, Σpos=0, flag freshness, replay determinism on seeded logs | **P** | each stage ✅ (1–3) |
| 51 | Fuzz order parser + log decoder | fuzzing | **Stage 1:** proptest + structured garbage on `decode_entry` (no panic) | **P** | Stages 1 ✅, 5 |
| 52 | Replay: empty log → same hash | state sync | **Done:** `rebuild_empty_log` / snapshot rebuild vs live hash; backtest cross-check | **P** | Stage 1 ✅ |
| 53 | Chaos: kill leader, partition, corrupt WAL | e2e | **Partial:** corrupt WAL tail recovery tested; network chaos in Stage 7 | **P** | Stage 7 |
| 54 | Criterion p50/p99 match + liquidation budgets | — | **Stage 1:** sequencer append/replay/apply benches; **Stage 2:** match benches; **Stage 3:** `apply_place_with_margin_check` ~2.1 µs, `liquidate_underwater_position` ~648 ns, `state_hash_2000_subaccounts` ~253 µs | **P** | Stages 2–3 ✅ |
| **Observability** | | | | | |
| 55 | Per-stage latency histograms | — | `lq_latency_ns{stage}` exists | **P** | extend stages |
| 56 | Indexer sequence-lag gauges | — | Topic drop counters only | **A** | Stage 6 |
| 57 | State-hash mismatch alert | — | None | **A** | Stages 1, 7 |
| 58 | Order/fill counters | — | Metrics exist for events/fills | **P** | extend |
| **Platform constraints (must hold)** | | | | | |
| 59 | Paper default, live disabled | — | Enforced | **P** | keep |
| 60 | unsafe forbid / Clippy | — | `unsafe_code = forbid`, clippy all=warn | **P** | keep |

---

## 4. Architectural Mismatch Summary (the hard deltas)

These are not “missing features” — they are **shape mismatches** that force redesign rather than incremental patches.

### 4.1 Aggregate book ≠ CLOB

`lq-orderbook::OrderBook` stores **level totals** for *external* feeds. A dYdX-style CLOB needs **per-order** storage with price-time priority (price level → FIFO of `Order` with client OID, side, remaining qty, flags). Matching must be a pure function of (book, incoming order) → fills + book mutation, with **no RNG** and **no wall clock** (expiry driven by sequence or log-supplied time).

**Reuse path:** generalize `PaperExchange` matching + `OrderStateMachine` into `lq-clob`; keep feed `BookStore` as *market data* (oracle/mark inputs), not as the client book.

### 4.2 Engine loop ≠ event-sourced state machine

Today the loop applies events to mutable state with async side effects (venue `place_order`, bus, persistence). Target: **apply(LogEntry) → (State', Outputs)** pure transitions; I/O only at edges (gateway in, Kafka out). `lq-sequencer` becomes the only writer clock.

### 4.3 Risk limits ≠ margin system

`RiskEngine` enforces notional/qty/rate/kill-switch. dYdX needs **per-subaccount collateral math**: free collateral, IM/MM requirements, liquidation threshold, bankruptcy price. **Closed in Stage 3:** `lq-perps`'s `pre_trade_check` enforces the limits *as* margin-derived pre-trade verdicts (Allow/Reduce/Reject) inside `apply` — the parallel `RiskEngine` limit list survives only on the legacy in-process engine path.

### 4.4 In-process strategy ≠ external client

Stage 8 removes `lq-strategy` from the money path: MM bot talks signed orders to Gateway (like `v4-clients`). In-process strategy may remain as a *paper-only* demo client.

### 4.5 Read path entanglement

Today API/metrics read the same process state. Stage 6 splits: trading publishes to Kafka; indexer is the only REST/WS source for clients; engine metrics remain local Prometheus.

### 4.6 Determinism debt (must clear early)

| Debt | Location | Disposition |
|---|---|---|
| `f64` analytics | `MarketState`, risk bps | Keep **outside** money state machine (read-path / strategy inputs); margin/price in SM stay Decimal/int |
| `TimestampMs::now()` | risk, API uptime, models | SM time = log timestamp only |
| `HashMap`/`DashMap` iteration | `StrategyEngine`, `EngineState` | Money SM: `BTreeMap` only; API may sort |
| `StdRng` fill model | paper venue | Non-deterministic paper fills stay in *simulation*, not CLOB SM; CLOB matching pure |
| Market bus DropNewest | `EventBus` | Acceptable for **feed** data only; never for log/WAL |
| Postgres sink | `PersistenceSink` | Not a journal; superseded by WAL + indexer |

### 4.7 Workspace hygiene

- Add `crates/solana/{types,data,execution}` to workspace members **or** document exclusion (docs currently claim they are first-class).
- Geyser TODO (`tonic`) and placeholder program IDs remain out of scope for Stages 1–8 unless needed for oracle adapters.

---

## 5. Proposed Stage Plan (for approval)

Each stage = one PR-sized change: design note + code + invariant tests + docs + residual risks. **Do not start next stage until approved.**

| Stage | Deliverable | Primary new crates / changes | Key tests |
|---|---|---|---|
| **1** | Sequencer + event-sourced state | **✅ Done** — `lq-sequencer` (WAL, snapshot, replay, `StateHash`, `LedgerState`); `LogEntry` with global+market seq; logical `ts_ms` | Replay empty→hash equality; hash stability; decoder fuzz; proptest seq monotonic; backtest cross-check |
| **2** | CLOB | **✅ Done** — `lq-clob`: order-level book, TIF, STP, cancel/replace, ST vs stateful; `apply` returns `ApplyOutput`s | proptest price-time priority, conservation of qty; no-RNG match; p50/p99 match bench |
| **3** | Perps + margin | **✅ Done** — `lq-perps`: subaccounts, IM/MM, pre-trade checks (limits absorbed from `lq-risk`), liquidation @ bankruptcy price, insurance, ADL, funding, block invariants; `Transfer`/`Liquidate`/`SettleFunding` entries | proptest collateral conservation + Σpositions=0 + replay determinism; 23 behavior tests; liquidation bench |
| **4** | Oracle | **✅ Done** — `lq-oracle`: multi-source median + outlier reject (`ObservationBook`, pure `aggregate()`), `OraclePrice` log entries, `OracleBook` embedded in `PerpsState` with deviation (halt + override re-baseline) and log-driven staleness gates on place/replace/liquidate/settle-funding | prop: median/outlier + order independence + never-panics; deviation halt/override; staleness gate; replay determinism on oracle logs; 21 behavior + 34 unit tests |
| **5** | Gateway | Signed orders (ed25519), rate limits, validation, WS ingress → sequencer | Signature accept/reject fuzz; rate limit props; parser fuzz |
| **6** | Indexer + stream | Emit fill/order/position/funding → Kafka; `lq-indexer` → Postgres + REST + Redis WS | Lag gauges; crash-restart from offset; API contract tests |
| **7** | Raft | `openraft` 3-node, hot standby, failover, periodic hash compare + halt on mismatch | Chaos: leader kill, partition, WAL tail corruption |
| **8** | MM as client | Extract `MarketMakingStrategy` to external binary using gateway client; optional dYdX venue adapter | E2E paper: bot quotes via gateway; backtest parity story |

**Explicit non-goals until later:** real live trading mode, on-chain Solana execution, full ADL sophistication, CometBFT implementation (design note only in Stage 7).

---

## 6. What existing pieces map to which stage

| Existing asset | Fate |
|---|---|
| `EventBus` (feed topics) | Keep for **market data + metrics**; not for money path |
| `BookStore` / feed `OrderBook` | Keep as external book for analytics/oracle inputs |
| `PaperExchange` matching skeleton | Seed for `lq-clob` (strip RNG, add order-level priority) |
| `OrderStateMachine` | Keep; align statuses with CLOB lifecycle |
| `PositionManager` | **Superseded by `lq-perps`** (subaccount-centric positions in the state machine) |
| `RiskEngine` | **Stage 3:** money-path limits moved into `lq-perps` pre-trade checks; legacy `RiskEngine` remains only for the old engine path until Stage 8 |
| `lq-sequencer` | **Stage 1 done:** WAL/snapshot/replay/state hash for money-path commands |
| `lq-clob` | **Stage 2 done:** order-level matching as `StateMachine` over the sequencer log |
| `lq-perps` | **Stage 3 done:** subaccounts, margin, liquidation, insurance, ADL, funding composed over `lq-clob` |
| `lq-orderbook` | Feed-level book stays market-data only (analytics/oracle inputs); not the client book |
| `lq-backtest` | Stage 1 cross-checked via shared determinism; later re-based on sequencer log |
| `lq-api` control routes | Remain for ops; order ingress moves to Gateway (Stage 5) |
| `lq-persistence` Postgres | Audit/debug only; indexer owns client-facing history (Stage 6) |
| `lq-telemetry` | Extend histograms/gauges/alerts per stage |
| `web/` dashboard | Later: point at indexer REST/WS (post Stage 6); auth header still missing |
| Solana crates | Optional Stage 8 adapter; fix workspace membership separately |

---

## 7. Decisions (locked) and residual risks

### 7.1 Decisions — approved

| # | Question | Decision |
|---|---|---|
| 1 | Numeric type in money state machine | **Keep `rust_decimal::Decimal`** (fixed-point) for prices, sizes, margin, and state hash inputs. No switch to raw i128 scaled integers. Still: **no `f64`/`float` in state**. |
| 2 | Sequencing | **One global monotonic sequence** (primary log order, WAL, Raft, state hash) **along with per-market sequences** (tagged on entries / order lifecycle for per-book ordering and ST expiry). Global seq is authoritative; per-market seq is derived/monotonic within the market. |
| 3 | Concurrency | **Single-threaded apply for all markets initially** (one state-machine thread consuming the global log). Shard per market later only if needed. |

### 7.2 Residual risks / open items

1. **Kafka in Stage 6:** Redpanda single broker in docker-compose (vs external cluster) — default plan: one broker in compose.
2. **openraft maturity / version:** pin at Stage 7 design time.
3. **`Mode::Live`:** remains refused through Stage 8; paper/settlement-only.
4. **Funding interval & insurance parameters:** **resolved** — defaults documented in `docs/stages/STAGE_3_PERPS.md` (`MarketParams` 10 % IM / 5 % MM, `liquidation_window_ms = 60_000`, fee/insurance behavior); interval scheduling itself moves to the Stage 5 funding daemon (Stage 4 supplies the oracle gate it must respect).
5. **Existing `deterministic_across_runs`:** re-expressed as sequencer replay tests; old backtest API may break — acceptable.
6. **Decimal hashing:** state hash must use a canonical Decimal serialization (normalized scale/representation) so equal values always hash equal across replicas.

---

## 8. Approval gate

Phase 0 complete. **Plan approved with §7.1 decisions locked.**

- Gap table (§3) and stage plan (§5): approved
- Open questions (§7): resolved per §7.1

**Stage 1: complete** — see `docs/stages/STAGE_1_SEQUENCER.md`.

**Stage 2: complete** — see `docs/stages/STAGE_2_CLOB.md`.

**Stage 3: complete** — see `docs/stages/STAGE_3_PERPS.md`.

**Stage 4: complete** — see `docs/stages/STAGE_4_ORACLE.md`.

**Next:** Stage 5 (Gateway) — start only on explicit go-ahead.
