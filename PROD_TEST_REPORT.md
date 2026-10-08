# Production-Readiness Test Report — Aegis Trading Terminal

Date: 2026-10-08
Scope: the new Trading Terminal front end (`web/`), the quant REST API (`restapis/`), the Rust
control-plane API (`apps/api-server`), and the free-tier Docker production stack
(`docker-compose.yml`, `docker/web/`, `restapis/Dockerfile`).

## 1. What was run

| Stage | Command | Result |
|---|---|---|
| Type check + bundle | `cd web && npm run build` (`tsc -b && vite build`) | clean, exit 0 |
| Python API tests | `cd restapis && python -m pytest tests -q` | **57 passed** |
| Rust engine build | `cargo build --bin api-server` | ok, 53 s (fresh `target/`) |
| Dev e2e (all specs) | `cd web && npx playwright test` | **43 passed** (33 s, cold start) |
| Production e2e (built bundle + containers) | `E2E_BASE_URL=http://localhost:4173 E2E_API_URL=http://localhost:10000 npx playwright test terminal*.spec.ts` | **24 passed** |
| Production smoke | `scripts/smoke.sh` (in `alpine/curl`) | all checks passed, exit 0 |
| Clean-checkout run | full copy of `git ls-files -co --exclude-standard` to an empty dir, then `npm ci`, `npm run build`, `cargo build`, `pytest`, `npx playwright test`, `docker compose up --build`, smoke, prod e2e | **all green** (see §5) |

Production e2e intentionally runs only the three terminal specs: the production bundle talks to the
API host directly (`http://localhost:10000`), so the `/api` proxy tests in `dashboard`/`smoke`/
`offline`/`controls` are dev-server tests and stay in the dev run (their only change this session is
the route `page.goto("/")` → `page.goto("/engine")`; no assertion was touched).

## 2. Production stack profile (free-tier limits)

| Item | Configured | Measured |
|---|---|---|
| api container | `cpus: 0.10`, `mem_limit: 512m`, `read_only: true`, tmpfs `/tmp`, `PORT=10000` | 67–71 MB RSS (13%), 0.12% CPU idle |
| web container | `mem_limit: 64m`, `nginx-unprivileged`, port `4173→8080` | 9.7 MB RSS (15%), 0.0% CPU idle |
| image sizes | — | api 558 MB, web 74 MB |
| `POST /backtest` latency @0.1 CPU | — | min 24 ms / avg 112 ms / max 191 ms (10 samples) |
| healthcheck | `healthz` every 5 s | healthy on first start |
| runtime user | `USER 10001(app)` | `uid=10001(app)` confirmed in container |
| read-only rootfs | `read_only: true` | `touch /srv/x` → `Read-only file system`; `/tmp` writable (tmpfs) |

Bundle: `index.html` 0.77 kB, CSS 9.19 kB (2.61 gz), JS 187.76 kB (61.54 gz).

## 3. Bugs found

### Fixed in this session

| # | Sev | Symptom | Root cause | Repro | Fix |
|---|---|---|---|---|---|
| B1 | High | Every dev/engine test failed: `/api` proxy returned 404, `.banner` visible in `smoke.spec.ts` | `web/vite.config.ts` proxied to `http://localhost:8080`; on this host `localhost` resolves to `::1` first, where a foreign listener (WSL relay / docker-proxy) answers 404. Same listener produced the "smoke 404" seen during the Docker test. | `npm run dev`, open `/engine`, watch proxy fail; `curl -6 http://localhost:8080/healthz` → 404 while `curl -4 http://127.0.0.1:8080/healthz` → 200 | `web/vite.config.ts:9` → default `http://127.0.0.1:8080` (still overridable with `LQ_API_PROXY`) |
| B2 | High | `npx playwright test` died with `Error: Timed out waiting 30000ms from config.webServer` | `api-server.exe` links the debug CRT dynamically; without the VC143 redist DLLs on PATH it exits `0xC0000135 STATUS_DLL_NOT_FOUND`, so Playwright's `webServer` never becomes ready. The same failure breaks `cargo build` on a fresh shell (build scripts are affected too). | unset PATH additions, `npx playwright test` → timeout; `target\debug\build\serde-*\build-script-build.exe` → `STATUS_DLL_NOT_FOUND` | `web/playwright.config.ts:31` prepends `Microsoft.VC143.CRT` to the child `PATH`; for cargo, add the same dir to PATH before building |
| B3 | High (env) | The Rust control-plane could not bind port 8080; intermittent 404s from `/api` | A leftover container from an unrelated session (`services-build-api-1`, project `zara/services`) published host `0.0.0.0:8080`, racing the locally running `api-server.exe`. | `netstat -ano \| findstr :8080` → two LISTENING owners; `docker ps` → `services-build-api-1` publishing 8080 | Stopped the leftover container (`docker stop services-build-api-1`). Not a repo bug — port 8080 must be free for this project's tests. |
| B4 | Medium | After a redeploy, browsers could keep serving the stale `index.html` shell → blank/old UI | nginx default `expires` rule was not defined for the HTML entry point (assets were already immutable) | `curl -I http://localhost:4173/` → no explicit `Cache-Control` | `docker/web/nginx.conf`: `location = /index.html { add_header Cache-Control "no-cache" always; }` (assets keep `public, immutable`) |
| B5 | Medium | Front end rendered the screener's `vs_sma50` column as a percentage of nothing meaningful | The API returns the **raw 50-day average close**, not a distance/variant from it (`restapis/app/main.py:118` = `sma(c,50).iloc[-1]`), while the column was labelled and formatted as a % distance | call `/screen?strategy=ma` and inspect `vs_sma50` vs the SMA of close | Front end only (no backend change): `web/src/components/Screener.tsx` column `{ label: "SMA 50", kind: "num" }`, rendered as a price. See B6 for the backend recommendation. |
| B6 | Low | `RefObject` mismatch: JSX `ref` props rejected `useRef<T>(null)` results (`Type 'RefObject<T \| null>' is not assignable…`) | React 18 types: `RefObject<T \| null>` ≠ `RefObject<T>` for invariance | `npm run build` → TS error | `useCanvas` returns `RefObject<HTMLCanvasElement>`; `Controls.inputRef: RefObject<HTMLInputElement>`; `tickerRef = useRef<HTMLInputElement>(null)` |

### Not fixed (called out, no backend change made)

| # | Sev | Issue | Repro | Recommendation |
|---|---|---|---|---|
| N1 | Medium | `_scrub()` ( `restapis/app/main.py:26` ) converts non-finite floats to the **strings** `"nan"` / `"inf"` / `"-inf"`, contradicting the declared `float \| null` response schema | seed a path that yields `inf` (e.g. division by a degenerate window) and read the JSON body | return `null` instead of `str(obj)`; the front end already maps null/NaN to `—` (`web/e2e/terminal-screen.spec.ts` covers it) |
| N2 | Low | `b` is documented `mom: unused` (`main.py:49`) but is range-validated `le=250` for **every** strategy, so `strategy=mom&b=999` → 422 even though the backtest ignores `b` | `POST /backtest {"strategy":"mom","a":250,"b":999}` → 422 | ignore `b` for `mom`, or update the field description to say it is always validated |
| N3 | Low | No auth / rate limiting on the quant API (documented as by design for a local tool; CORS is restricted to the configured origin) | — | put it behind a reverse proxy with auth before exposing it publicly |
| N4 | Low | `vs_sma50` semantics are ambiguous for API consumers (raw price level, name suggests a distance) | see B5 | either return `close/sma - 1` or rename to `sma50` |

No HTTP 500s were observed in any edge probe: unknown/empty/over-long symbol → 404
(`{"detail":"unknown symbol …"}`), lowercase symbols are normalized → 200, `a=2&b=250`, RSI
`b=95`, reverse `ma`, `cost_bps=201`, NaN literals, `/screen a=1`, bad strategy, `/prices days=0`
and `days=100000`, missing params → all 422.

## 4. Regression/behavioural notes for the report consumer

* Existing e2e specs were changed **URL-only**: `/` → `/engine`, `/?demo` → `/engine?demo`. The
  engine dashboard now lives behind `/engine` (`web/src/App.tsx` router → `web/src/EngineApp.tsx`);
  the new Trading Terminal is `/`. No assertions were deleted or weakened.
* The cost message contract is frozen: `Fees cost {X} points of annual return (cost {Y} bps).` with
  testids `cost-message` and `data-points` (4 dp).
* Formatting: `—` for null/NaN/undefined/inf, signed 2-decimal percentages, `fmtPct(..., 2, false)`
  for exposure.
* Production e2e needs two env vars: `E2E_BASE_URL` (switches Playwright from vite dev to the
  preview/production server) and `E2E_API_URL` (defaults to `http://127.0.0.1:8000`).

## 5. Clean-checkout run (proof the tree is self-contained)

Copied exactly `git ls-files -co --exclude-standard` (272 files, no `node_modules`, no `target/`,
no `dist/`) into an empty directory:

1. `python -m pytest tests -q` → **57 passed**
2. `npm ci` → ok; `npm run build` → clean (identical bundle hashes/size)
3. `cargo build --bin api-server` (fresh `target/`) → ok in **53 s** with the VC143 CRT on PATH
4. `npx playwright test` (cold: it launched uvicorn, `api-server.exe` and vite itself) → **43 passed**
5. `docker compose up -d --build` → api healthy, web up
6. `scripts/smoke.sh` → all checks passed (health, symbols, backtest fields/finite payload, screen,
   prices, CORS allow + reject, shell, hashed bundle, bundle API host, SPA fallback `/engine`)
7. `E2E_BASE_URL=http://localhost:4173 E2E_API_URL=http://localhost:10000 npx playwright test
   terminal*.spec.ts` → **24 passed**

## 6. Gaps / not covered

* `@axe-core/playwright` accessibility audit not executed this session (structural `sr-only`
  labels, focus order and ARIA are covered by hand-written tests only).
* `DATA_SOURCE=yfinance` path not exercised — the container runs `DATA_SOURCE=synthetic`
  (documented free-tier default).
* Long soak / sustained-load testing beyond the 0.1-CPU latency sample; no memory-leak sweep.
* TLS and anything beyond localhost: the stack assumes an external proxy for HTTPS.

## 7. Reproduce

```powershell
# dev
cd web; npm ci; npm run build; npx playwright test        # 43 tests
cd ..\restapis; python -m pytest tests -q                # 57 tests

# production
docker compose up -d --build
docker run --rm -v "${PWD}\scripts:/scripts:ro" `
  -e API_URL=http://host.docker.internal:10000 -e API_ORIGIN=http://localhost:10000 `
  -e WEB_URL=http://host.docker.internal:4173 -e WEB_ORIGIN=http://localhost:4173 `
  alpine/curl:latest sh /scripts/smoke.sh
cd web
$env:E2E_BASE_URL="http://localhost:4173"; $env:E2E_API_URL="http://localhost:10000"
npx playwright test terminal.spec.ts terminal-screen.spec.ts terminal-failure.spec.ts
```

Note (Windows): if the shell has no VC143 redist on PATH, prepend
`C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Redist\MSVC\14.44.35112\x64\Microsoft.VC143.CRT`
before running `cargo build` or `api-server.exe` (Playwright does this automatically for its
webServer).

**Verdict:** production-ready for the free-tier / local deployment target — all suites green in both
dev and containerised production modes, resource ceilings met with ~8× headroom on the API and the
five findings above either fixed or documented as intentional.
