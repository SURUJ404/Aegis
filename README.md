# rust-liquidity-engine

A production-oriented, multi-venue crypto liquidity / market-making engine
written in Rust. It is **paper-first**: every binary runs against a simulated
exchange by default, and live trading is a deliberate, explicit opt-in that is
not implemented yet (the engine refuses to run in `mode = "live"`).

The workspace is a set of small, independently testable crates wired together
by thin application binaries:

| Area | Crate / app | Purpose |
|---|---|---|
| Market data | `lq-market-data` | OKX / Binance / Bybit WebSocket adapters, normalize to `MarketEvent`, resilient reconnect |
| Order book | `lq-orderbook` | Sequence-managed local book, delta application, microstructure analytics |
| Strategy | `lq-strategy` | `Strategy` trait, baseline market-making strategy, cross-venue analyzer |
| Risk | `lq-risk` | Configurable limits + kill switches between strategy and execution |
| Execution | `lq-execution` | Order state machine, position/inventory tracking, paper venue |
| Simulator | `lq-simulator` | Synthetic random-walk market + paper matching exchange |
| Backtest | `lq-backtest` | Deterministic replay of the full engine stack |
| Telemetry | `lq-telemetry` | Tracing/logging, Prometheus metrics + `/metrics` server |
| Persistence | `lq-persistence` | Postgres (SQLX), Redis hot state, bus→store sink |
| API | `lq-api` | Axum control-plane: state reads + start/stop/reset/kill |
| `trading-engine` | app | Full engine: feeds → books → analytics → strategy → risk → execution |
| `market-data-service` | app | Standalone market data collection + persistence (no trading) |
| `backtest` | app | Deterministic backtest runner |
| `simulate` | app | Standalone paper-exchange simulator |
| `api-server` | app | Standalone control-plane API server |
| `dashboard` | web | React/Vite control dashboard over the REST API |

## Quick start

```sh
cargo build --release

# Deterministic backtest (20k synthetic events, seed 13)
cargo run -p backtest-runner --release -- --events 20000

# Paper trading engine + control-plane API + metrics
cargo run -p trading-engine --release
#   GET  http://localhost:8080/api/v1/state
#   POST http://localhost:8080/api/v1/control/start
#   GET  http://localhost:9100/metrics

# Standalone simulator and market-data collector
cargo run -p simulator --release
cargo run -p market-data-service --release
```

## Web dashboard

A React/Vite dashboard (`web/`) visualizes engine state and drives the
control plane. It polls `/api/v1/state` and posts to the control endpoints;
decimal fields render with full precision.

```sh
# Dev (proxy to the engine API on :8080)
cd web
npm install
npm run dev          # http://localhost:5173

# Or run the whole stack with Docker (engine + API + dashboard +
# Postgres + Redis + Prometheus + Grafana)
docker compose -f docker/docker-compose.yml up --build
#   http://localhost:18000   dashboard
#   http://localhost:18080   engine API
#   http://localhost:19100   engine metrics
#   http://localhost:9090    Prometheus
#   http://localhost:3000    Grafana
```

For the same stack on a container platform: `deploy/fly/` (Fly.io,
`fly.toml` + managed Postgres/Redis) and `deploy/railway/` (bundled
`railway.json` + Postgres/Redis plugins). The image picks up
`DATABASE_URL`/`REDIS_URL` and the dashboard proxies the API via
`ENGINE_API_HOST`.

## Design principles

- **Explicit configuration.** Every knob lives in a TOML file
  (`EngineConfig`) with no hidden assumptions. See `docs/OPERATIONS.md`.
- **Deterministic backtests.** Latency and rejection are disabled in
  backtests; the venue RNG is seeded. Same input events → same result.
- **Strategy / risk / execution separation.** Strategies are pure (no
  networking, no I/O). Risk sits between a decision and an order. Execution
  is the only thing that talks to a venue.
- **Bounded topics.** Market data can be dropped under load (sequence gaps
  recover it); execution and control events block until published.
- **Observability first.** Every binary emits structured logs, Prometheus
  metrics, and a health/state API.

## Documentation

- `docs/ARCHITECTURE.md` — component model, data flow, threading
- `docs/TRADING.md` — strategy and execution model, fee accounting
- `docs/RISK.md` — risk limits, trip-wires, kill switches
- `docs/DATA.md` — event model, order book, persistence schema
- `docs/OPERATIONS.md` — configuration reference, endpoints, metrics
- `docs/DEPLOYMENT.md` — Docker, docker-compose, Kubernetes
- `docs/DEVELOPMENT.md` — workspace layout, testing, benchmarking

## Tests & benchmarks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo bench --workspace
```

## License

Apache-2.0.
# Aegis
