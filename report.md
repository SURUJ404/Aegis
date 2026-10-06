# QA Bug-Fix Report — Trading Terminal (Aegis)

Method: every entry below started as a **failing test**, then the fix, then a green proof.
No test was ever loosened or deleted; no strategy rule, endpoint contract, or response shape
was changed except where explicitly called out. Session phases: 0 baseline → 1 quant core →
2 API → 3 UI (Playwright) → 4 deploy/config/secret/load → final sweep + env audit.

**18 bugs found and fixed (A–R).**

## Suite status (final)

| Suite | Result |
|---|---|
| `pytest tests -q` (restapis) | **57 passed**, 1 warning, exit 0 |
| `ruff check .` (restapis) | **All checks passed!** (baseline: 6 errors) |
| `mypy app` (restapis) | 3 errors — unchanged baseline, all missing pandas/yfinance stubs |
| `cargo test --workspace` | **151 passed, 0 failed, 1 ignored** (39 binaries), exit 0 |
| `npx tsc -b` (web) | exit 0 |
| `npx playwright test` (web) | **19 passed**, exit 0 |
| Live feature checks | 19/19 passed |
| Load probe (both services) | 1,050 requests, **0 errors** |

---

## Baseline (Phase 0, before any change)

- `pytest`: 5 passed (`tests/test_quant.py`).
- `ruff check`: **6 errors** — `I001`×3 (`data.py`, `test_quant.py`×2), `F401`×2 (unused `numpy`), `BLE001`×1 (deliberate 502 boundary catch).
- `mypy app`: 3 errors, all missing-stub class.
- Spec discrepancy: `trading-terminal.html` does not exist; the UI is the React dashboard in `web/`.

---

## Errors found and their fixes

### Phase 1 — quant core (`restapis/app/quant.py`)

**Bug A — Backtest crashes on <2 bars.**
`quant.backtest(pd.Series([100.0]), ...)` → `IndexError: single positional indexer is out-of-bounds` (HTTP 500 path).
Failing test: `test_quant_edges.py::test_one_bar_input_is_a_clear_error`.
Fix: `run()` raises `ValueError("need at least 2 bars, got N")` before computing.
*Behavior change called out:* clear error instead of crash; no shape change.

**Bug B — NaN closes leak NaN into results → JSON 500.**
One missing close produced `nan` in `equity`/`cagr`; Starlette renders with `allow_nan=False` → `ValueError` → HTTP 500.
Failing tests: `test_quant_edges.py::test_missing_close_never_produces_nan_json`, `..._entirely_missing_series_is_flat_and_clean`.
Fix: non-finite `pct_change()` values coerced to `0.0` in `run()`.
*Behavior change called out:* only NaN/inf inputs are affected (previously corrupt); clean data identical.

**Bug C — `rsi()` all-NaN on all-gains / never-moving windows.**
All-gains window → RSI NaN forever → RSI-exit blocked, positions stuck open; `/screen.rsi14` would 500.
Failing tests: `test_rsi_is_100_when_every_day_gains`, `test_rsi_is_neutral_50_when_price_never_moves`.
Fix: masks — avg loss 0 & gain > 0 → **100**; both 0 → **50**; losses-only → 0 unchanged.
*Behavior change called out:* all-gains windows change NaN → 100 so the `> 55` exit can fire; entry/exit thresholds untouched (guarded by dedicated tests).

### Phase 1 tooling — lint baseline (6 errors → 0)

- `I001`/`F401` in `app/data.py`, `tests/test_quant.py`: sorted imports, removed unused `numpy` (assertions untouched).
- `BLE001` in `app/main.py`: `# noqa: BLE001` — deliberate API boundary (provider failure must answer 502, not crash).

### Phase 2 — API (`restapis/app/main.py`, TUI contract)

All failing tests in `restapis/tests/test_api_phase2.py` (27 tests).

**Bug D — Non-finite numbers in JSON bodies → HTTP 500.**
`{"symbol":"AAPL","cost_bps": NaN}` → pydantic 422 detail embeds `nan` → Starlette renderer raises → 500.
Fix: custom `RequestValidationError` handler + `_scrub()` → clean **422**.

**Bug E — `/screen` accepted reverse MA window (`a >= b`).**
Fix: `_check_window()` → **422**, mirroring `BacktestRequest` validator.

**Bug F — `/screen` short history → HTTP 500.**
`c.iloc[-22]` IndexError and NaN `vs_sma50`/`rsi14` warmup.
Fix: `_finite_or_none()` → **null** fields, `ret_1m` only when `len(c) >= 22` → **200**. TUI side: `ScreenRow` fields → `Option<f64>` rendered as `--` (Rust test `screen_row_parses_null_metrics`, red first).

**Bug G — `/screen` empty provider df → HTTP 500.**
Fix: explicit empty check → **502**.

**Bug H — `/backtest` provider <2 bars → quant `ValueError` → HTTP 500.**
Fix: try/except → **502** (empty and 1-bar); 2-bar input proven **200**.

**Bug I — CORS env list parsing.**
`ALLOWED_ORIGINS=" http://a.example , http://b.example ,, "` produced malformed origins.
Fix: `_origins()` strips and drops empty segments.

**Bug J — nullable `/screen` broke the TUI's `ScreenRow` deserialization.**
Fix: `Option<f64>` fields + `--` rendering; red Rust test first.

### Phase 3 — UI (`web/`, Playwright 1.63.0; 19 tests, api-server + vite auto-started)

**Bug K — "Open orders" stat under-counted.**
`StatCards` used `!o.status.endsWith("filled")` → `partially_filled` counted as *closed*, contradicting OrdersPanel (renders it as open/warn).
Failing test: `e2e/dashboard.spec.ts::partially filled order counts as open`.
Fix: `o.status !== "filled"` (aligned with the panel's TERMINAL set).

**Bug L — Sparkline stuck on "collecting…" whenever PnL is flat.**
`App.tsx` deduped samples (`pnl !== last`); an idle engine never reaches 2 points, so the "Realized PnL (last 2 min)" card never draws — even though the component supports flat lines (`range || 1`).
Failing test: `e2e/dashboard.spec.ts::sparkline draws a flat line when pnl never changes`.
Fix: append one sample per poll, keep the `slice(-119)` cap.

**Bug M — control POSTs ignored `res.ok` → success-shaped toast for failures.**
Any non-2xx JSON answer (503 `engine not listening`, 409, 500 `detail`) rendered as `start: undefined`.
Failing test: `e2e/controls.spec.ts::http error responses toast as failures`.
Fix: `postJson` throws `control request failed: <status> <body>` → toast `start failed: …`.

**Bug N — stale toast timer cleared newer toasts early.**
Each `act()` scheduled `setTimeout(() => setToast(null), 4000)` without cancelling the previous one, so a second action's toast was wiped when the *first* action's timer fired (kill at t=0, start at t=2 → start toast vanished at t=4 instead of t=6).
Failing test: `e2e/controls.spec.ts::a later toast is not cleared early by an earlier action's timer`.
Fix: single `toastTimer` ref — cleared before every re-arm and on unmount.

**Cleanup — unreachable branch in OrdersPanel pill logic** (static defect, zero behavior change).
`TERMINAL.has(o.status) ? "pill-dim" : o.status === "filled" ? "pill-ok" : "pill-warn"` — the `filled → pill-ok`
arm was unreachable (`filled ∈ TERMINAL`). Dead arm removed; rendering unchanged (terminal → dim,
open → warn), locked by characterization test `order status pills: terminal states dim, open states warn`.

Test-side (not product) fix: `getByText("KILL SWITCH")` strict-mode ambiguity → `{ exact: true }`.

### Phase 4 — deploy / config / secret / load

**Bug O — kill switch accepted an empty reason (audit-trail hole).**
`POST /api/v1/control/kill {"reason":""}` → 202 published an empty-reason KillSwitch event even
though `reason` is required (required yet unvalidated).
Failing test: Rust `kill_switch_rejects_empty_reason` (red: got 202, expected 422; also proved
nothing was published). Fix: `publish_kill` → **422 `{"error":"reason required"}`**. TUI already
substitutes `"manual halt from TUI"` for blank input; web UI surfaces the 422 as a `kill failed:` toast.
*Behavior change called out:* empty reasons now 422 instead of 202.

**Bug P — bearer-token comparison short-circuited (`==`).**
`token == expected` bails on the first differing byte, leaking a prefix of the secret through
response timing. Timing cannot be red-tested (flaky by nature), so: new `token_matches()`
(XOR-accumulate, length-check only) + correctness test
`token_matches_is_exact_and_leaks_nothing_but_length`, with `auth_requires_bearer_token`
(401/401/200/healthz) proving behavior unchanged.

**Bug Q — `api-server` never activated `API_TOKEN` auth.**
`apps/api-server/src/main.rs` built `ApiState::new(...)` without `.with_token(cfg.api.token)`,
so the documented bearer token (docs/OPERATIONS.md) was silently ignored by this binary
(trading-engine wired it correctly at `engine.rs:94`).
Failing test: `apps/api-server/tests/env_auth.rs::api_token_env_enables_auth_when_config_file_present`
(red: 200 instead of 401; wrong-token 401 and healthz-open asserted in the same test).
Fix: `build_router(ApiState::new(...).with_token(cfg.api.token.clone()))`.

**Bug R — env overrides (and `.env` files) ignored without a `--config` file.**
All five binaries fell back to `EngineConfig::default()` **without** `apply_env_overrides()`, so
`API_BIND`, `API_TOKEN`, `POSTGRES_URL`, etc. only worked when a TOML file was supplied —
dotenvy loaded `.env` values that were then never applied.
Failing test: `apps/api-server/tests/env_auth.rs::api_bind_env_is_honored_without_config_file`
(red: server never bound the env-specified port — 10 s timeout panic).
Fix: new `EngineConfig::with_env_overrides()` + `EngineConfig::default().with_env_overrides()`
in the `None` branch of **all five** binaries (api-server, trading-engine, market-data-service,
simulator, backtest-runner).

**Remaining audit results:**

- **Secret scan:** no AWS/`ghp_`/`sk-`/private-key/high-entropy password matches in source, config, compose. **No real secrets exist in this repo** — `.env.example` therefore ships placeholders/dev defaults only.
- **Deploy chain coherent:** `railway.json` → `docker/Dockerfile` (exists, multi-stage, `LQ_CONFIG` set) + `healthcheckPath: /healthz` (route exists, unit-tested); `web/Dockerfile` + `nginx.conf.template` (`${ENGINE_API_HOST}`) + `package-lock.json`; compose services consistent — `trading-engine` serves `lq_api::build_router`.
- **CORS:** Rust API reflects any `Origin` (documented intentional + recommendation); Python API default-restricted to `localhost:5173/3000`, env-overridable (Bug I).
- **Observations (unchanged, local-tool scope):** permissive CORS by design; compose dev creds `lq:lq` + published 5432/6379 + Grafana anonymous Viewer.
- **Load probe** (httpx async, caches warm): 1,050 requests, **0 errors** — rust `/api/v1/state` p50 1.8 ms (seq) / 147 ms (c=50); python `/screen` p50 15 ms (seq) / 334 ms (c=20); `/backtest` p50 96 ms (c=10).
- **Env prerequisite:** `target\debug\api-server.exe` needs the VC143 CRT directory on `PATH` (otherwise spawn fails with `0xC0000135`).

### Environment file (`.env.example` / `.env`)

Every environment variable read anywhere in the repo is inventoried in **`.env.example`** (committed
template): `API_TOKEN`, `API_BIND`, `METRICS_BIND`, `LQ_CONFIG`, `LQ_LOG_LEVEL`,
`LQ_PERSISTENCE_ENABLED`, `POSTGRES_URL`/`DATABASE_URL`, `REDIS_URL`, `LQ_BACKTEST_OUT`,
`LQ_BACKTEST_JSON`, `DATA_SOURCE`, `ALLOWED_ORIGINS`, `ENGINE_API_HOST`, plus docker-compose
dev services (`POSTGRES_*`, `GF_AUTH_*`). **`.env`** (same content, fill real values) is
auto-loaded by every Rust binary via dotenvy and is **gitignored** — it can never be committed.
Bugs Q and R are exactly what made such a file inert before this session.

---

## Behavior changes (explicitly called out)

1. Quant: <2 bars → `ValueError` (was `IndexError`); NaN returns → 0.0; RSI all-gains/flat → 100/50 (was NaN).
2. API: reverse ma window → 422; short/empty data → 200-with-nulls / 502 (was 500); non-finite JSON input → 422 (was 500); CORS env list now stripped.
3. TUI: screener fields nullable (`--` rendering).
4. UI: `partially_filled` counts as open; sparkline samples every poll; failed control POSTs toast as `… failed:`; toasts are no longer cleared early by a stale timer.
5. API control: empty/whitespace kill-switch reason → 422 (was 202); `API_TOKEN` now actually enforces 401s on api-server; env overrides apply without a config file.

## Files touched

- **Quant/API:** `restapis/app/quant.py`, `restapis/app/main.py`, `restapis/app/data.py` (lint)
- **Rust:** `crates/api/src/lib.rs` (Bug O, P), `crates/core/src/config.rs` (Bug R), `apps/api-server/src/main.rs` (Bug Q + R), `apps/{trading-engine,market-data-service,simulator,backtest-runner}/src/main.rs` (Bug R), `apps/api-server/tests/env_auth.rs` (new, Bug Q/R red tests)
- **TUI:** `apps/tui/src/api.rs`, `apps/tui/src/ui.rs`
- **Web:** `web/src/api.ts`, `web/src/App.tsx`, `web/src/components/StatCards.tsx`, `web/src/components/OrdersPanel.tsx`, `web/package.json`, `web/package-lock.json`, `web/playwright.config.ts` (new), `web/e2e/{support,dashboard,controls,offline,smoke}.spec.ts` (new)
- **Tests:** `restapis/tests/test_quant_properties.py`, `test_quant_edges.py`, `test_quant_reference.py`, `test_api_phase2.py` (all new), `test_quant.py` (lint only)
- **Root:** `.gitignore`, `.env.example` (new), `TEST_REPORT.md`, `report.md` (this file)
- **Unchanged:** strategy rules, endpoint paths (except fixes above), deploy configs.

## Full final suite output

```
$ pytest tests -q
57 passed, 1 warning in 3.57s                                exit 0

$ ruff check .
All checks passed!                                            exit 0

$ mypy app
Found 3 errors in 2 files (checked 4 source files)            # baseline stubs only

$ cargo test --workspace
TOTAL passed=151 failed=0 ignored=1 (39 test binaries)        exit 0

$ npx tsc -b                                                  (web)
                                                                exit 0

$ npx playwright test                                         (web)
19 passed (11.7s)                                             exit 0
```
