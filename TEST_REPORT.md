# TEST_REPORT — Trading Terminal (restapis/app)

QA log. Rule: failing test first → fix → proof → entry here.
**All phases 0–4 plus the final bug-sweep are complete — 18 bugs fixed (A–R).**
The consolidated all-phase error/fix log lives in `report.md` (repo root); this file carries the per-phase detail.

---

## 0. Baseline (recorded before any change)

| Check | Result |
|---|---|
| `pytest` | **5 passed**, 1 warning, exit 0 (`tests/test_quant.py`) |
| `ruff check` | **6 errors**, exit 1: `I001`×3 (`data.py`, `test_quant.py`×2), `F401`×2 (unused `numpy` in `data.py`, `test_quant.py`), `BLE001`×1 (`main.py:36` blind except) |
| `mypy app` | **3 errors**, exit 1 — all missing stubs: pandas (`quant.py:4`, `data.py:5`), yfinance (`data.py:41`) |
| Spec front end | `trading-terminal.html` **absent**; React dashboard exists in `web/` (start/stop/reset/kill + market/positions/orders panels). Spec's strategy/cost sliders, ticker field, screener sorting, tabs: not present in this repo → cannot be tested as written. Logged as spec discrepancy. |

## 1. Feature checklist (tick = proven by a test in this repo)

### Quant core — Phase 1
- [x] SMA crossover strategy (`ma`, a<b) — shapes, determinism, JSON-safe, independent-loop cross-check
- [x] Momentum strategy (`mom`) — same
- [x] RSI strategy (`rsi`) — same; entry strictly `< b`; exit strictly `> 55`
- [x] No-lookahead rule — property: rewriting prices after day t cannot change equity/position up to t
- [x] Cost model (`cost_bps`) — 0 ≤ cost ≤ 200; more cost never increases return; `cost_drag_cagr ≥ 0`
- [x] Buy & hold baseline equals price ratio (existing test)
- [x] Equity/position invariants — equity > 0 & finite, position ∈ {0,1}, lengths == input length, exposure ∈ [0,1]
- [x] Determinism — identical JSON for identical input
- [x] Edge cases — 1 bar, 0 bars, 2 bars, shorter than windows, constant price, missing close, all-missing series, `a ≥ b` for ma, unknown strategy, flat position, always-long position
- [x] RSI definition — all-gains window → 100, all-losses → 0, never-moves → 50, finite after warmup on real data
- [x] Results cross-checked field-by-field against a plain-Python reference loop (ma / mom / rsi / cost 0 and 10)

### API — Phase 2
- [x] `/health`, `/symbols`, `/prices` (bounds, unknown symbol, cache TTL), `/backtest` (validation matrix, aligned JSON), `/screen` (all params, per-field values, never NaN/500)
- [x] Data-source selection, CORS matrix (allow configured / block unconfigured / env list parsing)
- [x] ValueError from quant mapped to 502 (not 500); empty/1-bar providers → 502; 2 bars → 200
- [x] Non-finite numbers in JSON request bodies → clean 422 (renderer crash fixed)

### UI / Deploy / Security — Phases 3–4
- [x] Playwright: 19 tests (dashboard, control state machine, kill prompt, toast timers, offline/recovery, live api-server smoke)
- [x] Secret scan, CORS/auth review, deploy-chain review, config audit
- [x] Env-file audit: `.env.example` inventory + `API_TOKEN`/`API_BIND` actually enforced (Bugs Q, R)
- [x] Load probe: 1,050 requests across both services, 0 errors

---

## 2. Bugs found and fixed (Phase 1)

### Bug A — Backtest crashes on fewer than 2 bars
- **Severity:** High (unhandled `IndexError` → HTTP 500 for any short/empty price series)
- **Repro (failing test):** `test_quant_edges.py::test_one_bar_input_is_a_clear_error`
  `quant.backtest(pd.Series([100.0]), "ma", 20, 60, 10)` → `IndexError: single positional indexer is out-of-bounds` (from `_stats` reading `eq.iloc[-1]` of an empty frame); same for `pd.Series([], dtype=float)`.
- **Fix:** `run()` now raises `ValueError("need at least 2 bars, got N")` before any computation.
- **Proof:** both tests now pass; suite green.
- **Behavior change (called out):** contract is now *clear error* instead of *crash*. Public response shapes unchanged; API maps it to 502 (Bug H).

### Bug B — Missing (NaN) closes leak NaN into results → response 500
- **Severity:** High (Starlette renders with `allow_nan=False`; one NaN close anywhere → `ValueError` → HTTP 500)
- **Repro (failing test):** `test_quant_edges.py::test_missing_close_never_produces_nan_json`,
  `test_entirely_missing_series_is_flat_and_clean` — `json.dumps(result, allow_nan=False)` raised `Out of range float values are not JSON compliant: nan` (`equity` and `strategy.cagr`).
- **Fix:** `run()` sanitizes returns: non-finite `pct_change()` values become `0.0` (a missing/broken close counts as a 0% move).
- **Behavior change (called out):** only affects inputs containing NaN/inf (previously produced corrupt output); clean data path is bit-identical.

### Bug C — `rsi()` returns NaN when the window has no losing (or no moving) days
- **Severity:** Medium (silent wrong signals: NaN blocks both entry *and* the `RSI > 55` exit, so an open position can never close during an all-gains stretch; also feeds `/screen.rsi14`, which would 500)
- **Repro (failing tests):** `test_rsi_is_100_when_every_day_gains` (all 200 days up → every RSI value NaN), `test_rsi_is_neutral_50_when_price_never_moves` (constant price → all NaN). Cause: `al.replace(0, np.nan)` with zero average loss.
- **Fix:** `rsi()` masks the zero-loss case: average loss == 0 and average gain > 0 → **100**; both zero → **50** (neutral, conventional). Loss-only → 0 unchanged; warmup NaN unchanged.
- **Behavior change (called out):** for RSI inputs whose ewm window contains only gains, values change NaN → 100, so the `> 55` exit can now fire (previously blocked). Strategy rules themselves (`< b` entry, `> 55` exit) are untouched — see `test_rsi_entry_requires_value_strictly_below_threshold` and `test_rsi_exit_requires_value_strictly_above_55`.

No strategy rules, endpoints, or response shapes were changed. Existing tests untouched.

## 3. Open items (not fixed — deferred)
- ~~`ruff`: 6 pre-existing errors from baseline~~ **fixed** (see §6).
- `mypy`: 7 errors in `tests/`, **all** the environmental missing-stubs class (`pandas`/`yfinance`); `mypy app` still exactly the 3 baseline errors — installing `pandas-stubs` remains a nice-to-have (no code fix possible in-repo; yfinance ships no stubs at all).
- ~~`/screen` reads `rsi(...).iloc[-1]`, NaN when history < 14 bars → would 500~~ **fixed in Phase 2** (Bug F).
- Spec discrepancy: spec's single-page terminal UI does not exist in this repo (see §0).
- Security observations: ~~permissive CORS in `crates/api`~~ (intentional, documented in code), ~~non-constant-time token compare~~ **fixed (Bug P)**, ~~empty kill-switch reason accepted~~ **fixed (Bug O)**, ~~`API_TOKEN` silently ignored by api-server~~ **fixed (Bug Q)**, dev creds in `docker-compose.yml` (local-dev scope, left as-is).

## 4. Files touched (this QA session)
- `restapis/tests/test_quant_properties.py` — **new** (4 property tests)
- `restapis/tests/test_quant_edges.py` — **new** (17 edge/threshold tests)
- `restapis/tests/test_quant_reference.py` — **new** (4 independent-loop cross-checks)
- `restapis/tests/test_api_phase2.py` — **new** (27 API tests)
- `restapis/app/quant.py` — Bug A/B/C fixes
- `restapis/app/main.py` — Bugs D–I + lint `# noqa: BLE001`
- `restapis/app/data.py` — lint only
- `restapis/tests/test_quant.py` — lint only (assertions untouched)
- `restapis/requirements.txt` — added `hypothesis>=6`
- `crates/api/src/lib.rs` — Bugs O (kill 422), P (constant-time token)
- `crates/core/src/config.rs` — Bug R (`with_env_overrides`)
- `apps/api-server/src/main.rs` — Bugs Q + R; `apps/{trading-engine,market-data-service,simulator,backtest-runner}/src/main.rs` — Bug R
- `apps/api-server/tests/env_auth.rs` — **new** (Bug Q/R red-first spawn tests)
- `apps/tui/src/api.rs`, `apps/tui/src/ui.rs` — Bug J (`Option<f64>`)
- `web/src/{App.tsx,api.ts}`, `web/src/components/{StatCards,OrdersPanel}.tsx` — Bugs K, L, M, N + dead-branch cleanup
- `web/playwright.config.ts`, `web/e2e/*.spec.ts` — **new** (19 e2e tests)
- `.gitignore`, `.env.example` (new), `TEST_REPORT.md`, `report.md`
- Untouched: strategy rules, deploy configs.

## 5. Final suite output

```
$ pytest tests -q
57 passed, 1 warning in 3.57s

$ ruff check .
All checks passed!     # was 6 errors at Phase 0 baseline

$ mypy app
Found 3 errors in 2 files   # identical to Phase 0 baseline (all missing stubs)
```

## 6. Ruff cleanup (post-Phase-1, requested)

| Rule | Location | Fix |
|---|---|---|
| `I001` | `app/data.py:3` | split `import math, os, time` into sorted single imports |
| `F401` | `app/data.py:4` | removed unused `import numpy as np` (verified: zero `np.` references) |
| `BLE001` | `app/main.py:36` | `# noqa: BLE001` — deliberate API boundary: provider failures must answer 502, not crash; narrowing the catch would change behavior, so it is acknowledged, not narrowed |
| `I001` + `F401` | `tests/test_quant.py:1` | sorted imports, removed unused `numpy` (assertions untouched) |
| `I001` | `tests/test_quant.py:36` | sorted imports inside `test_api_contract` (assertions untouched) |

**Proof:** `ruff check .` → `All checks passed!` (exit 0); `pytest` → green (exit 0).

## 7. Bugs found and fixed (Phase 2 — API)

All failing tests in `restapis/tests/test_api_phase2.py` (27 tests, red first → green).

| # | Bug | Severity | Failing test | Fix |
|---|---|---|---|---|
| D | Non-finite JSON numbers in request body (`NaN`/`Infinity`) → pydantic 422 detail embeds `nan` → Starlette renderer crash → **HTTP 500** | High | `test_backtest_rejects_nonfinite_json_numbers` | custom `RequestValidationError` handler + `_scrub()` → clean 422 |
| E | `/screen` accepted reverse MA window (`a >= b`) → 200 with meaningless signal | Med | `test_screen_rejects_reverse_ma_window` | `_check_window()` → 422 (mirrors BacktestRequest validator) |
| F | `/screen` short history → `IndexError` (`c.iloc[-22]`) or NaN (`vs_sma50`, `rsi14` warmup) → **500** | High | `test_screen_short_history_is_clean_json` | `_finite_or_none()` nullable metrics, `ret_1m` only when `len(c) >= 22` → 200 with `null` |
| G | `/screen` empty provider df → unhandled `IndexError` → **500** | High | `test_screen_empty_provider_is_502_not_500` | explicit empty check → 502 |
| H | `/backtest` provider returning <2 bars → quant `ValueError` → **500** | High | `test_backtest_single_bar_provider_is_502_not_500`, `test_backtest_empty_provider_is_502_not_500` (+ `test_backtest_two_bars_is_200`) | try/except ValueError → 502; 2-bar path proven 200 |
| I | CORS `ALLOWED_ORIGINS` env with spaces/empty segments → malformed origin list silently wrong | Med | `test_cors_origin_list_is_stripped_and_cleaned` | `_origins()` strips + drops empties |
| J | TUI contract: new nullable `/screen` fields break `ScreenRow` deserialize → TUI screener dead | High | Rust `screen_row_parses_null_metrics` (red first) | `ScreenRow` fields → `Option<f64>`; `ui.rs` renders `None` as `--` |

## 8. Bugs found and fixed (Phase 3 — UI, Playwright)

Framework: `@playwright/test@1.63.0` (chromium rev 1243), `web/playwright.config.ts` starts
`../target/debug/api-server.exe` (healthz wait) and `npm run dev`; 4 suites / 19 tests.

| # | Bug | Severity | Failing test | Fix |
|---|---|---|---|---|
| K | `StatCards` counted open orders with `!o.status.endsWith("filled")` → **`partially_filled` counted as closed** (under-count; contradicts OrdersPanel which shows it as open/warn) | Med | `dashboard.spec.ts::partially filled order counts as open` | `o.status !== "filled"` (aligned with TERMINAL set) |
| L | `App.tsx` deduped PnL samples → flat PnL (idle engine) **never gets a 2nd point, stuck on "collecting…" forever** despite "last 2 min" and flat-line support (`range \|\| 1`) | Med | `dashboard.spec.ts::sparkline draws a flat line when pnl never changes` | append one sample per poll, `slice(-119)` cap |
| M | `api.ts postJson` never checked `res.ok` → non-2xx JSON answers rendered as **success toast `start: undefined`** | High | `controls.spec.ts::http error responses toast as failures` | throw `control request failed: <status> <body>` on `!res.ok` → `start failed: …` toast |
| N | each action scheduled a fresh 4 s toast timer without cancelling the previous one → **earlier timer wiped a later toast early** | Med | `controls.spec.ts::a later toast is not cleared early by an earlier action's timer` | single `toastTimer` ref (clear on re-arm + unmount) |

**Cleanup:** unreachable `pill-ok` arm in `OrdersPanel` (`filled ∈ TERMINAL`) — dead branch removed,
zero behavior change, locked by `dashboard.spec.ts::order status pills: terminal states dim, open states warn`.
Test-side fix: `getByText("KILL SWITCH")` → `{ exact: true }`.

## 9. Phase 4 — deploy / config / secret / load audit

Audit findings — four hardened, rest documented:

**Bug O — empty kill-switch reason accepted (fixed).** `POST /api/v1/control/kill {"reason":""}`
published with a blank audit trail although `reason` is required. Red-first Rust test
`kill_switch_rejects_empty_reason` (202 → expected 422, nothing published) → `publish_kill`
now answers **422 `{"error":"reason required"}`**. TUI unaffected (substitutes
`"manual halt from TUI"`); web UI surfaces it as a `kill failed:` toast.

**Bug P — non-constant-time bearer-token compare (fixed).** `token == expected` short-circuited
on the first differing byte. New `token_matches()` XOR-accumulate compare + correctness test
`token_matches_is_exact_and_leaks_nothing_but_length`; `auth_requires_bearer_token` proves
401/401/200/healthz behavior unchanged. (Timing itself cannot be red-tested — noted honestly.)

**Bug Q — api-server ignored `API_TOKEN` (fixed).** `main.rs` never passed `cfg.api.token` to
`ApiState`, so documented bearer auth could not activate on this binary (trading-engine was
correct). Red-first spawn test `env_auth.rs::api_token_env_enables_auth_when_config_file_present`
(200 → 401/401/200/healthz) → `.with_token(cfg.api.token.clone())`.

**Bug R — env overrides ignored without a config file (fixed).** All five binaries used bare
`EngineConfig::default()` with no env pass, so `.env`/shell variables (`API_BIND`, `API_TOKEN`,
`POSTGRES_URL`…) only worked when a TOML file was supplied. Red-first spawn test
`env_auth.rs::api_bind_env_is_honored_without_config_file` (10 s bind-timeout panic) → new
`EngineConfig::with_env_overrides()` applied in the `None` branch of all five binaries.

Other findings (observations, unchanged — documented scope "local/paper tool"):

- **Secret scan:** no AWS/`ghp_`/`sk-`/private-key/high-entropy password matches in source, config, compose — **no real secrets exist in the repo**.
- **Deploy chain coherent:** `railway.json` → `docker/Dockerfile` + `healthcheckPath: /healthz`; `web/Dockerfile` + `nginx.conf.template` + lockfile; compose consistent (`trading-engine` really serves `lq_api::build_router`).
- **CORS:** Rust API reflects any `Origin` (crates/api, documented intentional + recommendation); Python API default-restricted to `localhost:5173/3000`, env-overridable, stripping fixed (Bug I).
- **Compose dev defaults:** postgres `lq:lq`, Grafana anonymous Viewer, published 5432/6379 — local-dev only, noted.
- **Env file:** `.env.example` inventories every env var in the repo; `.env` (same content, fill real values) is auto-loaded by dotenvy and **gitignored**.
- **Load probe** (httpx async, caches warm):

| Endpoint | n | conc | ok | p50 | p95 | errors |
|---|---|---|---|---|---|---|
| RUST `GET /api/v1/state` | 200 | 1 | 200/200 | 1.8ms | 2.4ms | 0 |
| RUST `GET /api/v1/state` | 300 | 50 | 300/300 | 147.2ms | 539.2ms | 0 |
| RUST `GET /healthz` | 200 | 50 | 200/200 | 155.7ms | 457.5ms | 0 |
| PY `GET /screen` | 30 | 1 | 30/30 | 15.0ms | 16.1ms | 0 |
| PY `GET /screen` | 80 | 20 | 80/80 | 334.3ms | 468.4ms | 0 |
| PY `POST /backtest` | 40 | 10 | 40/40 | 95.8ms | 132.3ms | 0 |
| PY `GET /health` | 200 | 50 | 200/200 | 147.8ms | 647.2ms | 0 |

**1,050 requests, 0 errors.** Env note: `target\debug\api-server.exe` needs the VC143 CRT dir on
`PATH` (else spawn fails with 0xC0000135) — PowerShell session fix, same as the build.

## 10. Final full-suite output (all phases)

```
$ pytest tests -q                              (restapis, venv python)
57 passed, 1 warning in 3.57s                  exit 0

$ ruff check .                                 (restapis)
All checks passed!                             exit 0   # baseline was 6 errors

$ mypy app                                     (restapis)
Found 3 errors in 2 files (checked 4 source files)      # identical to baseline, all missing stubs

$ cargo test --workspace                       (CRT PATH applied)
TOTAL passed=151 failed=0 ignored=1            (39 test binaries)  exit 0

$ npx tsc -b                                   (web)
exit 0

$ npx playwright test                          (web; api-server + vite auto-started)
19 passed (11.7s)                              exit 0
```
