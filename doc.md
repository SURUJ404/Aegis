# Aegis — Interview Answers

Every answer below is grounded in the actual implementation. These are the
questions that separate a component count from an engineering story.

## Why is this channel bounded?

`EventBus` topics are bounded `mpsc` channels. An unbounded channel under a
slow consumer is a latency bomb — memory grows without limit and every
consumer serves stale data. For market data that is the wrong tradeoff: a
dropped delta is recoverable (sequence gaps), so the market topic is bounded
with `DropNewest` and counts the drop (`lq_topic_dropped_total`). Execution
and control topics are also bounded but `Block`: dropping a fill, an ack, or
a kill command corrupts state in a way you cannot recover, so those apply
backpressure to the producer instead. Bounded + counted is visibility, not a
bug.

## What happens if I miss an order-book sequence?

`BookStore::ingest` checks contiguity on every delta. An out-of-order delta
yields `IngestOutcome::Gap`, the book is flagged suspect, and quoting halts
(risk trip-wire if `kill_switch_on_stale`). The book never applies a delta
onto a book it cannot prove is contiguous — it waits for a resync snapshot.
This is exactly why Binance depth20 (full-snapshot-every-100ms) needs no
sequence bookkeeping while OKX/Bybit (incremental) do.

## Why can't the strategy directly submit an order?

`Strategy` is a pure trait: it takes a `StrategyContext` and returns
`StrategyDecision`s — no bus, no venue handle. That buys four things:
(1) determinism, so backtests are replayable; (2) a single choke-point —
every order passes `RiskEngine::validate_order` in exactly one place
(`place_checked`), so risk is enforced by construction, not convention;
(3) strategies unit-test with no runtime; (4) a buggy or compromised
strategy physically cannot bypass the kill switch.

## How do you prevent inventory runaway?

Two independent layers. Primary: `MarketMakingStrategy` skews quotes —
inventory skew widens the side you are already filled on, and it stops
quoting the aggressive side when `|inv| >= inventory_max_qty`. Backstop:
`RiskEngine` hard limits (`max_position_qty`, `max_daily_loss`) that do not
trust the strategy at all. The strategy can be naive; risk is the trip-wire.

## What happens if the exchange WebSocket dies while we have open orders?

The transport emits `FeedStatus::Disconnected`; if `kill_switch_on_reconnect`
is set, the risk engine halts and the engine cancels all working orders.
Orders are *suspect* after a disconnect — you cannot trust resting state you
did not hear about — so you stop, cancel, and only resume after `Resync` plus
a fresh snapshot confirms the book and order state. Reconnection is not "just
reconnect".

## Where exactly is your latency budget?

The decision path is one single-threaded event loop: WS decode -> bus -> book
ingest -> analytics -> strategy -> risk -> venue place, with no `await` on
that critical path and no locks (DashMap reads are cheap). `Metrics` records
`lq_latency_ns{stage}` histograms so you can *measure* it. Benchmarked:
decode ~20µs, analytics ~1µs, strategy ~0.9µs, risk ~7.5µs — microseconds
against a 250ms quote cadence. Paper latency (`base_latency_ms` + jitter) is
deliberately off in backtests for determinism.

## Why Tokio?

The workload is I/O-bound and concurrent: several WebSocket streams, axum API,
metrics server, timers (pings, refresh cadence), task-per-venue. Tokio gives
work-stealing async, `mpsc`, `select!`, timers, and composes with
axum/tungstenite/sqlx. It also supports both sides of the design: the
multithreaded runtime for feeds/venues/servers, and a current-thread
`LocalSet` for the `!Send` matching engine and for deterministic backtests.

## Why Redis here but PostgreSQL there?

Postgres is the durable, queryable system of record — market_data,
executions, order_events — for audit, reconciliation, and backtest analysis.
Redis is the in-memory hot-state mirror (last price, halt flag, open-order
counts) for low-latency reads. The tell: Redis is disposable. Lose it, and
nothing is wrong — it rebuilds from the bus. Lose Postgres, and you have lost
your audit trail. Cache vs. source of truth.

## How do you know your order book is correct?

By construction, not trust: contiguous sequences or gap-flag-and-suspend;
full-snapshot resync; deterministic replay in tests
(`deltas_keep_book_consistent`). And finally — the book is *disposable*.
Positions and PnL come from the `FillEvent` stream (`PositionManager` is the
system of record), so even a wrong book cannot corrupt accounting; it can
only stop you from trading until it resyncs. A broker-grade system adds
exchange book checksums (OKX) as a final cross-check.

## What does p99 latency look like under load?

It stays flat — and that is the design. Under overload the bounded market
topic drops events (counted in `lq_topic_dropped_total`) instead of growing a
queue, so queue latency does not blow up; you see *throughput loss with a
metric*, not silent p99 degradation. Execution/control `Block`, so their p99
grows only if the engine keeps producing while blocked — which is the correct
failure mode for events you cannot drop. You do not ask "what is p99?" — you
graph `lq_latency_ns` against `lq_topic_dropped_total` and the tradeoff is
visible.

## What happens when the consumer is slower than the producer?

Exactly the backpressure story above, now from the consumer's side: a slow
subscriber (say `PersistenceSink` during a burst) falls behind, drops market
events, and the counter shows it — the engine never waits on it.
`PersistenceSink` is explicitly documented as a *collection* workload, not a
lossless journal. If you need lossless, you write fills/positions directly
through the store instead of via the bus. The honest tradeoff is stated in
the code, which is what an interviewer wants to hear.
