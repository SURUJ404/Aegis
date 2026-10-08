# Shipping Report — Production Deployment Readiness

**Repository:** `SURUJ404/Aegis` · **Branch:** `feat/solana-integration-and-engineering-overhaul`
**HEAD (pushed, 0 ahead / 0 behind):** `4c89379` — *test: full QA pass — fix 18 bugs (A-R), add Playwright e2e suite and .env.example*
**Report date:** 2026-10-06 · **Planned production deploy:** ~week of 2026-10-13
**Scope of this report:** every change that will ship with this branch, every change that will **never** ship, all production behavior changes (contract call-outs), and the decisions required **before** deploy.

---

## 1. TL;DR

| | |
|---|---|
| **Ships** | 43 files / +5,587 lines in `4c89379`: 18 bug fixes (A–R), 57 pytest + 151 cargo + 19 Playwright tests, new TUI crate, `.env.example` template, docs (`report.md`, `TEST_REPORT.md`) |
| **Never ships** | `.env`, `secrets/**`, logs, caches, build artifacts — all gitignored, absent from git history and remote |
| **Contract changes in prod** | 4 endpoints change behavior (kill reason validation, auth enforcement-if-enabled, error codes 500→422/502, nullable short-history fields) |
| **Blocking decisions** | **#1 `API_TOKEN`** (dashboard sends no auth header → would 401), **#2 `.dockerignore`** (local `.env`/`secrets/` enter the Docker build context) |
| **Verification** | All suites green: cargo **151/0**, pytest **57**, Playwright **19**, ruff clean, tsc 0, live smoke **19/19**, load probe **1,050 req / 0 errors**, secret scan clean |

---

## 2. Ship matrix

### 2.1 Ships with this deploy (committed & pushed)

| Category | Contents |
|---|---|
| **Bug fixes** | Phase 1 quant (A–C), Phase 2 API (D–J), Phase 3 UI (K–N + dead-branch cleanup), Phase 4 security (O–R) — full detail in §4 and `report.md` / `TEST_REPORT.md` |
| **Tests (new)** | `restapis/tests/` (6 files), `apps/api-server/tests/env_auth.rs`, `web/e2e/` (5 files) + `web/playwright.config.ts` — all failing-test-first |
| **New binary** | Entire `apps/tui/` crate (1,752 lines: `main.rs`, `app.rs`, `ui.rs`, `api.rs`) — renders `--` for missing values (Bug J) |
| **Config template** | `.env.example` (placeholders only) + `.gitignore` exception `!.env.example` |
| **Docs** | `report.md` (18 bugs A–R + fixes), `TEST_REPORT.md` (per-phase QA detail), `restapis/README.md` |
| **Dependency/lock** | `Cargo.lock`, `Cargo.toml` (+`apps/tui` member), `web/package*.json` (+`@playwright/test`) |
| **Python API** | `restapis/app/{main,quant,data}.py` + `restapis/Dockerfile`, `requirements.txt` |
| **Web UI** | `web/src/{App.tsx,api.ts,components/StatCards.tsx,components/OrdersPanel.tsx}` |

### 2.2 Never ships (verified ignored — `git status --ignored`)

```
.env                 ← root env (auto-loaded locally, gitignored)
secrets/             ← secrets.env, load-secrets.ps1, README (gitignored, cannot be pushed)
target/, web/.vite/  ← build outputs
api-server.log, build_final.log, restapis/quant.log
restapis/.pytest_cache/, .ruff_cache/, .mypy_cache/, .hypothesis/, __pycache__/
```

- Only `.env.example` is committed — placeholders, **zero real secrets** (repo-wide secret scan clean: no AWS/`ghp_`/`sk-`/private keys).
- `git check-ignore` confirms `.env` and all `secrets/*` files are ignored; none appear in any commit or on the remote.

---

## 3. Deploy chain (validated)

```
railway.json ──► docker build -f docker/Dockerfile (builder: COPY . . + cargo build --release)
                 └─► runtime stage: debian-slim + /usr/local/bin/app + /config/lq.toml
                     ENV LQ_CONFIG=/config/lq.toml   EXPOSE 8080 9100
                     healthcheck /healthz (300 s) · restart ON_FAILURE (max 10)

web/Dockerfile ──► nginx:1.27-alpine + vite dist + nginx.conf.template
                   (proxies /api, /healthz → ENGINE_API_HOST)

docker/docker-compose.yml ──► local-dev only (Postgres/Grafana/Prometheus, dev creds) — not a prod path
restapis/Dockerfile ──► Python analysis API
```

- Runtime image contains **only** the compiled binary + `lq.toml` — no `.env`, no `secrets/`, no source.
- ⚠️ **`.dockerignore` gap:** it excludes `target/ .git/ docs/ deploy/ docker/ tests/ benches/` but **not** `.env` or `secrets/`. With `COPY . .` (docker/Dockerfile:10) your *local* secret files are uploaded in the build context to Railway's builder. Final image is clean, but the context itself carries them → **see pre-ship action #2**.

---

## 4. Production behavior changes (call-outs)

### 4.1 Rust control-plane API (`crates/api`, `apps/api-server`)

| Endpoint | Before | After | When it applies | Risk / mitigation |
|---|---|---|---|---|
| **All `/api/v1/*`** (except `/healthz`) — Bug **Q** | `API_TOKEN` env **silently dead** (auth never enforced) | With `API_TOKEN` set → missing/wrong `Authorization: Bearer …` ⇒ **401**; correct ⇒ 200. Unset ⇒ open (unchanged) | **Only if prod sets `API_TOKEN`** | **HIGH — decision #1.** Dashboard and TUI send **no** auth header today ⇒ they would break with 401. Leave token unset in prod until UI sends it, or wire header injection later. Constant-time compare now (Bug **P**, no contract change). |
| **POST `/api/v1/control/kill`** — Bug **O** | Empty/whitespace `reason` accepted → **202** | **422** `{"error":"reason required"}` | Always | **MEDIUM.** Any automation sending blank reasons starts failing. Send a non-empty reason (UI already does). |
| **Config/env loading** — Bug **R** | Env overrides skipped when **no** `--config` file | `EngineConfig::with_env_overrides()` applied when config absent | **No-op on Railway** (bakes `LQ_CONFIG=/config/lq.toml`, overrides already applied pre-fix). Affects bare-binary deploys only | **LOW.** Audit prod env vars: they are now honored where they were previously ignored (documented intent: platform vars win over TOML). |
| GET `/api/v1/state` etc. | — | Unchanged shape/status | — | — |

### 4.2 Python analysis API (`restapis`)

| Endpoint | Before | After | Risk |
|---|---|---|---|
| `POST /backtest`, `GET /screen` — Bug **D** | NaN in JSON body ⇒ **500** | Clean **422** | LOW (error path only) |
| reverse `ma` window — Bug **E** | 500 | **422** | LOW |
| `GET /screen` short history — Bug **F** | 500 | **200** with `null` indicator fields (nullable) | **MEDIUM** — clients assuming numbers-or-500 must accept `null` |
| empty dataframe — Bug **G** | 500 | **502** | LOW |
| internal `ValueError` — Bug **H** | 500 | **502** | LOW |
| indicator math — Bugs **A/B/C** | `<2 bars` ⇒ IndexError/500; NaN closes ⇒ NaN outputs; RSI edge cases wrong | `ValueError → 502` with message; NaN closes ⇒ 0.0 returns; RSI all-gains ⇒ 100, flat ⇒ 50 | **MEDIUM** — backtest/screen results on dirty/edge data can differ from previous runs (old runs crashed or produced NaN) |
| CORS parsing — Bug **I** | raw split | origins stripped/filtered | LOW — stricter/cleaner matching of configured origins |
| `GET /health /symbols /prices` | — | Unchanged (200) | — |

### 4.3 Web dashboard (`web/src`)

| Area | Change | Risk |
|---|---|---|
| `StatCards.tsx` — Bug **K** | Order status filter fixed (`o.status !== "filled"`) — counts/labels now correct | Display-only; numbers change to correct values |
| `App.tsx` — Bug **L** | PnL series appends a sample per poll — chart actually fills | Display-only |
| `api.ts` — Bug **M** | `postJson` now **throws on non-2xx** — failed control actions surface error toasts instead of failing silently | Display/behavior: users now *see* failures (intended) |
| `App.tsx` — Bug **N** | Single toast timer ref — no stacked/flickering toasts | Display-only |
| `OrdersPanel.tsx` | Unreachable dead branch removed; rendering locked by characterization test | None (identical output) |

### 4.4 Intentional non-changes (documented, not bugs)

- Permissive CORS default in `crates/api` — deliberate (code comment), reviewed OK.
- `docker-compose` dev creds (`lq:lq`), Grafana anonymous, published 5432/6379 — local-dev scope only, not a prod path.
- mypy: 3 missing-stub errors — environment baseline (yfinance ships no stubs), unchanged by this work.

---

## 5. Secrets & configuration inventory

- **Local files (never pushed):** root `.env` (dotenvy auto-load defaults, no real values) and `secrets/` (`secrets.env` for real keys + `load-secrets.ps1` per-shell loader).
- **Loader precedence verified live:** shell-loaded secrets win over `.env` (dotenvy does not override) — api-server answered **401** without header / **200** with `Bearer …` while `.env` had an empty token.
- **Committed template:** `.env.example` lists every variable (`API_TOKEN`, `API_BIND`, `METRICS_BIND`, `LQ_CONFIG`, `LQ_LOG_LEVEL`, `LQ_PERSISTENCE_ENABLED`, `POSTGRES_URL`/`DATABASE_URL`, `REDIS_URL`, `LQ_BACKTEST_OUT/JSON`, `DATA_SOURCE`, `ALLOWED_ORIGINS`, `ENGINE_API_HOST`, compose `POSTGRES_*`/`GF_*`) — **values empty/placeholder**.
- **Production:** set secrets via Railway dashboard env vars (or `LQ_CONFIG` file), never via committed files.

---

## 6. Verification evidence

| Suite | Result |
|---|---|
| `cargo test --workspace` | **151 passed / 0 failed / 1 ignored** (39 test binaries) |
| `python -m pytest tests -q` | **57 passed** |
| `python -m ruff check .` | **All checks passed!** |
| `python -m mypy app` | 3 baseline stub errors only (yfinance, unchanged) |
| `npx tsc -b` (web) | exit 0 |
| `npx playwright test` | **19 passed** (dashboard, controls, offline, smoke) |
| Live smoke (both APIs, real binaries) | **19/19** — health/state 200; start/reset/kill 202; empty/whitespace/missing kill 422; bounds/reverse-ma 422; NaN body 422; unknown symbol 404; CORS allow/block |
| Load probe | **1,050 requests, 0 errors** |
| Secret scan (repo + staged) | **Clean** — no keys/credentials |

Every fix followed red-first TDD; no test was loosened or deleted. Full bug log: `report.md` (A–R) · per-phase detail: `TEST_REPORT.md`.

---

## 7. Pre-ship decisions & checklist

**P0 — decide before deploy**

- [ ] **1. `API_TOKEN` in production.** Setting it turns on real 401 enforcement (Bug Q) but the dashboard/TUI send **no** `Authorization` header ⇒ UI would break. Choose: (a) leave `API_TOKEN` unset (status quo, auth off), (b) add header support in UI first (a change to schedule later — none made now), or (c) inject header at nginx (bakes secret into config — not recommended).
- [ ] **2. `.dockerignore` does not exclude `.env` / `secrets/`.** Local secrets are uploaded in the build context (final image stays clean). Recommended add — **not applied yet**:
  ```
  .env
  .env.*
  secrets/
  ```
  Confirm whether you want this applied (it only hardens builds; no runtime change).

**P1 — audit**

- [ ] **3. Kill-switch callers** send non-empty `reason` (422 otherwise — Bug O).
- [ ] **4. Prod env vars audit** — any var previously ignored without a config file now works (Bug R; no-op on Railway's baked `LQ_CONFIG`).
- [ ] **5. Python API clients** tolerate `null` indicator fields on short histories (Bug F) and 422/502 instead of 500 (D/E/G/H).
- [ ] **6. Backtest comparability** — results on NaN/edge data intentionally differ from broken old outputs (A/B/C).

**P2 — post-deploy smoke**

- [ ] `GET /healthz` 200 · `GET /api/v1/state` 200 · `GET /health` 200 (python)
- [ ] `POST /api/v1/control/kill` with reason → 202; with empty reason → 422
- [ ] If token enabled: no-header → 401, `Bearer` → 200
- [ ] Dashboard loads, order cards & PnL chart populate, CORS origin matches

---

## 8. Rollback

1. **Git:** `git revert 4c89379` (or redeploy previous image/commit) — branch pushed 0/0 with origin, so any commit is reproducible.
2. **Railway:** automatic — `/healthz` healthcheck, `ON_FAILURE` restart ×10; redeploy prior build from dashboard.
3. **Contract rollback note:** reverting restores dead auth (Q), silent empty kill reasons (O), and 500-crashes (D–H) — keep fixes unless they break a P1 audit item.

---

*Generated for the planned deploy week of 2026-10-13. No code/config changes were made while writing this report — items in §7 are documented actions awaiting your decision.*
