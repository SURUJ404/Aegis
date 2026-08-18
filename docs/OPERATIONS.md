# Operations

Everything you need to run the binaries, configure them, and read their
output. Configuration is read from a TOML file passed with `--config` (or the
`LQ_CONFIG` environment variable). Every binary falls back to `EngineConfig`
defaults when no file is given.

## Configuration reference

The single `EngineConfig` schema (TOML) drives all binaries. Unknown fields
are ignored; missing fields default.

```toml
mode = "paper"            # "paper" only. "live" is refused by the engine.

symbols = ["BTC-USDT"]
venues = ["paper", "simulated"]   # live venues: "okx" | "binance" | "bybit"

[paper]
base_latency_ms = 2.0
latency_jitter_ms = 1.0
fill_fraction = 0.8       # P(maker fill) when the book crosses a resting order
fee_rate_bps = 2.5        # taker fee
maker_rebate_bps = 0.5    # maker rebate (negative fee)
slippage_bps = 1.0
partial_fill_prob = 0.3
partial_fill_fraction = 0.5
reject_prob = 0.001       # forced to 0 in backtests (determinism)
queue_position = 0.5
depth_levels = 10

[strategy.market_making]
enabled = true
half_spread_bps = 5.0
min_spread_bps = 2.0
max_spread_bps = 30.0
quote_qty = 0.01
inventory_target_qty = 0.0
inventory_max_qty = 0.5
skew_exponent = 1.0
quote_refresh_ms = 250
vol_scale_half_spread = true

[risk]
max_position_qty = 1.0
max_order_qty = 0.1
max_open_orders = 50
max_notional = 100000
max_daily_loss = 1000
max_order_rate_per_sec = 20.0
max_exposure_per_venue = 100000
max_price_deviation_bps = 100.0
stale_market_ms = 5000
kill_switch_on_stale = true
kill_switch_on_reconnect = true

[telemetry]
log_level = "info"        # trace | debug | info | warn | error
metrics_bind = "0.0.0.0:9100"
json_logs = false

[api]
bind = "0.0.0.0:8080"

[persistence]
enabled = false
postgres_url = "postgres://lq:lq@localhost:5432/liquidity"
redis_url = "redis://localhost:6379"

[market_data]
ws_reconnect_base_ms = 500
ws_reconnect_max_ms = 30000
ping_interval_ms = 15000
stale_after_ms = 5000
```

## Binaries

| Binary | Starts | Exits when |
|---|---|---|
| `trading-engine` | feeds, engine loop, API, metrics | ctrl-c |
| `market-data-service` | live feeds + persistence + metrics | ctrl-c |
| `simulate` | synthetic feeds + paper matching + metrics | ctrl-c |
| `api-server` | control-plane API + metrics | ctrl-c |
| `backtest` | runs a backtest, prints summary, exits | run finishes |

### `backtest`

```
backtest --events 20000 --seed 13 [--config path.toml] [--json]
```

Output (or JSON with `--json`): events seen, orders placed, rejections,
open orders at end, fills, round-trip trades, win rate, total fees, net PnL,
max drawdown, annualized Sharpe, final equity.

## Control-plane API

Served by `trading-engine` and `api-server`:

| Method | Path | Meaning |
|---|---|---|
| GET | `/healthz` | liveness probe |
| GET | `/api/v1/state` | aggregate snapshot |
| GET | `/api/v1/positions` | per-venue positions |
| GET | `/api/v1/inventory` | per-symbol net inventory |
| GET | `/api/v1/orders` | order history |
| GET | `/api/v1/market-state` | latest `MarketState` per venue/symbol |
| GET | `/api/v1/risk` | risk status + halt reason |
| POST | `/api/v1/control/start` | start strategies |
| POST | `/api/v1/control/stop` | stop strategies + cancel all |
| POST | `/api/v1/control/reset` | release kill switch |
| POST | `/api/v1/control/kill` | engage kill switch (body `{"reason": "..."}`) |

## Metrics

Every binary exposes Prometheus metrics at `telemetry.metrics_bind` (`/metrics`):

- `lq_market_events_total{kind}` — market events by kind
- `lq_execution_events_total{kind}` — execution events by kind
- `lq_fills_total{venue}`, `lq_fees_total{venue}`
- `lq_latency_ns{stage}` — pipeline latency histogram
- `lq_open_orders{venue}`, `lq_net_position{venue,symbol}`,
  `lq_inventory_qty{symbol}`, `lq_realized_pnl{symbol}`
- `lq_halted`, `lq_strategy_running`
- `lq_topic_published_total{topic}`, `lq_topic_dropped_total{topic}`,
  `lq_topic_no_subscribers_total{topic,subscribers}`

A Prometheus + Grafana stack is included in `docker/docker-compose.yml`.

## Runbook

- **No fills in a live paper run.** Quotes refresh every `quote_refresh_ms`
  (default 250 ms). A refresh cancels and re-places; the synthetic book rarely
  crosses a quoted price within that window. This is expected behaviour, not a
  bug. Use `backtest` with a crafted sequence to see fills
  (`crates/backtest` `fills_when_book_crosses_quote` test).
- **"there is no reactor running".** `EventBus::new()` must be called inside a
  Tokio runtime. Application code always is; tests must use `#[tokio::test]`.
- **Feed goes stale / reconnects.** `lq_risk` halt switches engage if
  `kill_switch_on_stale` / `kill_switch_on_reconnect` are enabled. Recover by
  POSTing `control/reset` after the feed resyncs.
- **Order rate limit.** `risk.max_order_rate_per_sec` counts order
  submissions per second. A quote refresh places 2 orders per refresh; with
  `quote_refresh_ms = 250` that is 8 orders/s — under the default 20/s.
