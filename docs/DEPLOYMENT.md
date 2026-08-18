# Deployment

Containers, compose, and Kubernetes manifests live in `docker/` and `deploy/`.
The engine is stateless (all state lives in `EngineState` memory; persistence
is optional) so it scales horizontally behind the control-plane only for
readers. Start with a single replica.

## Docker

```sh
docker build -f docker/Dockerfile -t lq/trading-engine:0.1.0 .
```

The Dockerfile is multi-stage: a `rust` builder produces the release binary
and a `debian:bookworm-slim` runtime copies it in. Build any binary by passing
`--build-arg BIN=<name>` (default `trading-engine`):

```sh
docker build --build-arg BIN=market-data-service -t lq/market-data-service .
```

Binaries: `trading-engine`, `market-data-service`, `simulate`, `backtest`,
`api-server`.

## docker-compose

`docker/docker-compose.yml` runs the full stack:

- `postgres` (persistence), `redis` (hot state) — default creds `lq:lq`
- `trading-engine` with `PERSISTENCE_ENABLED=1`
- `prometheus` scraping `trading-engine:9100`
- `grafana` on `:3000`

```sh
docker compose -f docker/docker-compose.yml up --build
```

Use a config volume (`config/lq.toml`) to override engine settings.

## Kubernetes

Manifests in `deploy/k8s/`:

| File | What |
|---|---|
| `namespace.yaml` | `liquidity` namespace |
| `configmap.yaml` | engine TOML + telemetry settings |
| `postgres.yaml`, `redis.yaml` | persistence statefulsets |
| `trading-engine.yaml` | deployment + service (8080 API, 9100 metrics) |
| `market-data-service.yaml` | deployment + service |

```sh
kubectl apply -f deploy/k8s/
kubectl port-forward svc/trading-engine 8080:8080
kubectl port-forward svc/prometheus 9090:9090   # optional
```

Liveness: `/healthz`. Readiness: `GET /api/v1/state` returns 200.

## Config injection

- File: `--config /config/lq.toml` (or `LQ_CONFIG` env var).
- Secrets: `.env` via `dotenvy` (e.g. `DATABASE_URL`, Postgres/Redis URLs read
  from `[persistence]`).
- The `configmap` mounts `/config/lq.toml`.

## Operations notes

- `persistence.enabled = true` requires Postgres reachable at startup;
  otherwise the engine logs an error and runs without persistence.
- The engine holds no durable state: restarting a replica re-syncs books from
  snapshots. Do not run more than one engine writing to the same paper venue.
- Grafana dashboards should scrape `lq_*` metrics documented in
  `OPERATIONS.md`.
