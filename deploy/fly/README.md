# Deploy to Fly.io

Fly runs the Rust engine as a Machine and the dashboard behind nginx — a good
fit because both are long-running processes. Managed Postgres and Redis come
from `fly postgres` / `fly redis`; the engine's Prometheus endpoint is
scraped by Fly's built-in metrics.

## 1. Prerequisites

```sh
fly auth login
fly version          # needs flyctl v0.2+
```

## 2. Managed Postgres and Redis

```sh
fly postgres create            # pick a name; note the connection string
fly redis create               # note the REDIS_URL
```

## 3. Engine

```sh
# Create the app (config is at deploy/fly/engine/fly.toml)
fly launch --name aegis-engine --no-deploy --region iad --config deploy/fly/engine/fly.toml

# Point persistence at the managed services
fly secrets set DATABASE_URL=postgres://... --config deploy/fly/engine/fly.toml
fly secrets set REDIS_URL=rediss://...      --config deploy/fly/engine/fly.toml

fly deploy --config deploy/fly/engine/fly.toml
```

- Serves the control-plane API on `:8080` (`/healthz`, `/api/v1/state`).
- `fly logs` shows the paper engine streaming market data.
- The built-in config (`docker/config/lq.toml`) enables persistence; the
  `DATABASE_URL` / `REDIS_URL` secrets override the baked-in URLs. Tweak
  settings via `fly secrets set LQ_LOG_LEVEL=debug` etc.

## 4. Dashboard

```sh
fly launch --name aegis-dashboard --no-deploy --region iad --config deploy/fly/dashboard/fly.toml
fly deploy --config deploy/fly/dashboard/fly.toml
```

The dashboard nginx proxies `/api` to `http://aegis-engine.internal:8080`
(Fly private network). If you chose a different app name, override it:

```sh
fly secrets set ENGINE_API_HOST=aegis-engine.internal --config deploy/fly/dashboard/fly.toml
```

## 5. Open

```sh
fly apps open aegis-dashboard     # dashboard
fly apps open aegis-engine        # engine API
```

## Alternatives / notes

- Fly's free tier sleeps machines; the engine sets
  `auto_stop_machines = false` so it keeps trading. That costs you
  availability credits — set `min_machines_running = 1` or accept the charge.
- For a public Prometheus/Grafana stack instead of Fly's metrics, reuse
  `docker/prometheus.yml` + `docker/grafana` against the public engine URL.
