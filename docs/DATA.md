# Data

Events, the order book, and persistence schema.

## Event model

### `MarketEvent` (market topic)

| Variant | Carries | Used for |
|---|---|---|
| `Snapshot` | full book levels | initial sync, resync, Binance depth pushes |
| `Delta` | level changes | incremental book updates (OKX, Bybit) |
| `Trade` | price/qty/aggressor | trade flow analytics |
| `Tick` | last trade + touch | lightweight quote surface |
| `Status` | `Healthy` / `Stale` / `Disconnected` / `Resync` | risk trip-wires |

Every payload carries `venue`, `symbol`, `sequence` (where the protocol has
one), `event_ts`, and `exchange_ts`. `sequence` is checked on ingest; a gap
produces `IngestOutcome::Gap` and the book is treated as suspect until a
resync snapshot.

### `ExecutionEvent` (execution topic)

`New`, `Acknowledged`, `CancelRequested`, `Cancelled`, `Rejected`, `Expired`,
`Fill(FillEvent)`, `Trade`. `PositionManager` turns these into order status,
positions, and realized PnL.

### `ControlEvent` (control topic)

`Start`, `Stop`, `Reset`, `KillSwitch { reason }`.

## Order book

`lq_orderbook::book::OrderBook`:

- Price levels are fixed-point (integral tick scale, `QTY_SCALE`), keyed by
  tick → O(1) upsert/delete per level.
- `apply_snapshot` replaces; `apply_delta` applies a `LevelChange` batch
  (qty 0 = delete).
- `BookStore` keys books by (venue, symbol) and performs sequence-gap
  detection on ingest.

`MarketStateEngine` reduces a book + trade flow to a `MarketState`:

`best_bid`, `best_ask`, `mid`, `spread` (abs + bps), `microprice`, `vwap`,
`depth_bid/ask`, `num_bid/ask_levels`, `buy/sell_volume`,
`trade_intensity`, `realized_volatility`, `price_impact_estimate`,
`regime` (`Normal` | `LowLiquidity` | `HighVolatility` | `Stale` | `Other`).

## Persistence

`lq_persistence` is used by `market-data-service` (and optionally the engine).

### Postgres (`PostgresStore`)

Schema created by `migrate()`:

```sql
market_data(
  id        BIGSERIAL PRIMARY KEY,
  venue     TEXT, symbol TEXT, kind TEXT,
  seq       BIGINT,
  ts        TIMESTAMPTZ,
  payload   TEXT            -- normalized event as text
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

### Redis (`RedisHotState`)

Hot state mirrors for cheap reads:

| Key | Value |
|---|---|
| `lq:last_price:<symbol>` | last trade price |
| `lq:halted` | "1"/"0" |
| `lq:open_orders:<venue>` | count |

### Sink

`PersistenceSink::spawn(bus, store)` subscribes to market + execution topics
and forwards every event to the store. Like any bus subscriber it can fall
behind under extreme load and drop market events (documented trade-off — see
`crates/persistence/src/sink.rs`). For lossless bookkeeping the engine should
write fills/positions directly through the store rather than via the bus.

## Backtest determinism

`BacktestRunner` forces `reject_prob = 0`, disables latency, and seeds the
venue RNG from `BacktestConfig::seed`. The synthetic market generator
(`SyntheticMarketData`) is a seeded random walk producing snapshots, deltas,
and trades. Identical inputs → identical `BacktestResult`.
