# Deploy to Railway

Railway builds Dockerfiles and gives you managed Postgres + Redis plugins.
The repo already ships `railway.json` (engine) and `web/railway.json`
(dashboard), so the services auto-configure when connected.

## 1. Create a project

`railway init` or use the dashboard → **New Project**.

## 2. Add Postgres and Redis

- **New → Database → PostgreSQL** → provides `DATABASE_URL`.
- **New → Database → Redis** → provides `REDIS_URL`.

## 3. Add the engine service

- **New → GitHub Repo** → select this repo, service name `engine`.
- Settings:
  - Root Directory: `/` (repo root)
  - Dockerfile Path: `docker/Dockerfile`
  - Builder: Dockerfile (from `railway.json`)
- Link `DATABASE_URL` and `REDIS_URL` to the service (Railway auto-links
  plugin variables; verify in Variables).
- The engine is private-only — **do not** generate a public domain for it.
- Health check: `GET /healthz` (from `railway.json`).

## 4. Add the dashboard service

- **New → GitHub Repo** → same repo, service name `dashboard`.
- Settings:
  - Root Directory: `web`
  - Dockerfile Path: `Dockerfile`
  - Builder: Dockerfile (from `web/railway.json`)
- Variables:
  - `ENGINE_API_HOST` = `${{engine.RAILWAY_PRIVATE_DOMAIN}}`
    (or the engine service's name, e.g. `engine`)
- Generate a public domain for the dashboard — that is the URL users open.

## 5. Open

```sh
railway up
railway open          # dashboard
```

## Notes

- The engine's baked config enables persistence; `DATABASE_URL` /
  `REDIS_URL` override the baked-in URLs at boot.
- Prometheus/Grafana are optional on Railway. To scrape the engine, give it a
  public domain and add a Prometheus service configured against
  `https://<engine-domain>:9100/metrics` — or keep observability to
  `docker-compose` locally and use Railway purely for the demo.
