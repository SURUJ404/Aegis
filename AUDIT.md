# AUDIT — Trading Terminal front end vs FastAPI backend

Date: 2026-10-08 · Method: static read of `restapis/app/{main,quant,data}.py`, static read of
`web/src/*`, then live run (uvicorn on `http://127.0.0.1:8000`, vite on `http://localhost:5173`)
with the page opened in Playwright and the same inputs curled from the shell.

**Headline:** the repo contains two different products. The FastAPI backend
(`restapis`, title *"Trading Terminal API"*) exposes `/health /symbols /prices /backtest /screen`.
The web front end (`web/`, title *"Aegis terminal"*) is the **liquidity-engine dashboard**: it talks
to the Rust control plane at `/api/v1/state|book|log|control` (`web/src/api.ts:19-62`) and never
issues a single request to the FastAPI backend. Not one quant field is rendered anywhere.

---

## 1. Backend contract (every endpoint, request field, response field)

### `GET /health` — `restapis/app/main.py:68`
| direction | field | type |
|---|---|---|
| resp | `ok` | bool |
| resp | `data_source` | str (`synthetic` \| `yfinance`) |

### `GET /symbols` — `main.py:73`
| resp | (array of str) | `["AAPL","MSFT","NVDA","TSLA","AMZN","SPY","XOM","BTC-USD"]` |

### `GET /prices` — `main.py:78`
| req | `symbol` | str, required |
| req | `days` | int, default 120, `ge=5`, `le=1000` |
| resp | `symbol` | str (upper-cased) |
| resp | `bars[]` | array |
| resp | `bars[].t` | `YYYY-MM-DD` |
| resp | `bars[].o`,`h`,`l`,`c` | float |
| err | 404 unknown symbol · 422 bad `days` · 502 provider failure | |

### `POST /backtest` — `main.py:95`, body `BacktestRequest` (`main.py:45`)
| req | `symbol` | str, required |
| req | `strategy` | `"ma"` \| `"mom"` \| `"rsi"` (default `ma`) |
| req | `a` | int, 2..250 — ma: fast days, mom: lookback, rsi: period |
| req | `b` | int, 2..250 — ma: slow days, rsi: buy below, mom: unused |
| req | `cost_bps` | float, 0..200 |
| resp | `symbol` | str (upper-cased) |
| resp | `dates[]` | `YYYY-MM-DD`, len 520 on synthetic data |
| resp | `strategy.cagr` | float — annual return |
| resp | `strategy.sharpe` | float |
| resp | `strategy.max_drawdown` | float (negative) |
| resp | `buy_hold.cagr` | float |
| resp | `buy_hold.sharpe` | float |
| resp | `buy_hold.max_drawdown` | float |
| resp | `trades` | int |
| resp | `exposure` | float 0..1 — time in market |
| resp | `equity[]` | float, growth of 1, len == `dates` |
| resp | `buy_hold_equity[]` | float, len == `dates` |
| resp | `position[]` | int 0/1, len == `dates` |
| resp | `cost_drag_cagr` | float — free CAGR − paid CAGR (fee drag) |
| err | 422 invalid body / reverse ma window (`a>=b`) · 404 unknown symbol · 502 provider | |

### `GET /screen` — `main.py:106`
| req | `strategy` | `"ma"`\|`"mom"`\|`"rsi"`, default `ma` |
| req | `a` | int 2..250, default 20 |
| req | `b` | int 2..250, default 60 |
| resp | `rows[]` | one per symbol |
| resp | `rows[].symbol` | str |
| resp | `rows[].price` | float \| **null** (short history) |
| resp | `rows[].ret_1m` | float \| **null** (needs ≥22 bars) |
| resp | `rows[].rsi14` | float \| **null** |
| resp | `rows[].vs_sma50` | float \| **null** |
| resp | `rows[].signal` | `"long"` \| `"flat"` |
| err | 422 reverse ma window · 502 empty/provider | |

Non-finite floats are stringified (`"nan"`, `"inf"`) by `_scrub()` (`main.py:26`) rather than
emitted as invalid JSON — the UI must treat those as missing too.

---

## 2. Front-end status per backend field

The front end that exists today (`web/src/App.tsx`, 212 lines) renders the engine dashboard only.
`web/src/api.ts` has no quant functions at all (`api.ts:19-62` covers `/api/v1/*` exclusively).

| Backend field / feature | Status | Where it is / where it should be |
|---|---|---|
| `GET /health` → `ok`, `data_source` | **missing** | no caller; belongs in the terminal header status pill (`web/src/Terminal.tsx`, to be created) |
| `GET /symbols` → symbol list | **missing** | belongs in the ticker `<datalist>`/validation (`Terminal.tsx`) |
| `GET /prices` → `symbol`, `bars[].t/o/h/l/c` | **missing** | belongs in the candlestick chart (`Terminal.tsx` / `CandleChart.tsx`) |
| `POST /backtest` → `symbol` | **missing** | backtest header |
| `dates[]` | **missing** | x-axis of both charts |
| `strategy.cagr` | **missing** | metrics table "annual return" column |
| `strategy.sharpe` | **missing** | metrics table |
| `strategy.max_drawdown` | **missing** | metrics table "worst drawdown" |
| `buy_hold.cagr` / `.sharpe` / `.max_drawdown` | **missing** | metrics table "buy & hold" column |
| `trades` | **missing** | metrics table |
| `exposure` | **missing** | metrics table "time in market" |
| `equity[]` | **missing** | equity chart series + in-market shading driver |
| `buy_hold_equity[]` | **missing** | equity chart second series |
| `position[]` | **missing** | shaded in-market periods on the equity chart |
| `cost_drag_cagr` | **missing** | "fees cost X points" message |
| `GET /screen` → 6 columns × 8 rows | **missing** | screener table (`Screener.tsx`) |
| `screen[].… = null` → em dash | **missing** | formatter (`format.ts`) |
| request `strategy/a/b/cost_bps` controls | **missing** | strategy dropdown + sliders + cost slider |
| debounce 250 ms / cancel stale | **missing** | `useBacktest.ts` |
| loading / "Waking the server" / error+Retry / offline preview | **missing** | `Terminal.tsx` state machine |
| ticker box, Enter, `/` shortcut | **missing** | `Terminal.tsx` |
| light + dark toggle | **partly** | tokens exist at `web/src/styles.css:1-5`, but **no toggle** — only OS preference; nothing sets `data-theme` |
| design tokens / fonts | **correct** | `styles.css:1-9`, `index.html:8-10` — keep |
| API base URL config | **missing** | quant calls must read `VITE_API_URL` (`web/src/config.ts`, to be created); the existing engine calls use relative `/api/v1/...` on purpose |
| disabled "not ported yet" tabs | **missing** | tab strip in `Terminal.tsx` |
| `position` strip / in-market days / bars rows | **missing** | metrics table extra rows (parity with `apps/tui/src/ui.rs:472-519`) |

The engine dashboard fields (`web/src/App.tsx:126-209`) are **out of scope**: they belong to the
Rust control plane, are covered by `web/e2e/*.spec.ts`, and are correct against `/api/v1/*`.

---

## 3. Live comparison — page vs curl (same inputs)

Inputs used: `symbol=AAPL strategy=ma a=20 b=60 cost_bps=10` and
`/screen?strategy=ma&a=20&b=60`.

Curl reference (live, 2026-10-08):

```
POST /backtest -> {"symbol":"AAPL","dates":[520],
  "strategy":{"cagr":0.06019318965005249,"sharpe":0.34669386356370663,"max_drawdown":-0.2634411811597155},
  "buy_hold":{"cagr":0.42809520684620317,"sharpe":1.1169293801753226,"max_drawdown":-0.307360272579153},
  "trades":10,"exposure":0.6096153846153847,"cost_drag_cagr":0.0051497472526333965,
  equity/buy_hold_equity/position: 520 values each}
GET /screen?strategy=ma&a=20&b=60 -> 8 rows (AAPL price 586.21, ret_1m -0.1263, rsi14 32.97,
  vs_sma50 670.83, signal "flat"; AMZN signal "long"; …)
GET /health -> {"ok":true,"data_source":"synthetic"}
GET /prices?symbol=AAPL&days=5 -> 5 bars, t 2026-10-02 … 2026-10-08
```

Playwright, same moment, `http://localhost:5173/`:

```
VISIBLE TEXT: "The API at /api/v1/state is unreachable (state request failed: 404).
               Open the demo data to see the terminal without a backend."
TITLE: Aegis terminal
CONSOLE ERRORS: 9 × "Failed to load resource: the server responded with a status of 404"
screenshot: web/e2e/screenshots/audit-before.png
```

### Mismatches (every one is total: value absent, not merely wrong)

| # | Screen | JSON | Mismatch |
|---|---|---|---|
| 1 | annual return (strategy) | `strategy.cagr = 6.02%` | absent |
| 2 | sharpe (strategy) | `0.35` | absent |
| 3 | worst drawdown (strategy) | `-26.34%` | absent |
| 4 | annual return (buy & hold) | `42.81%` | absent |
| 5 | sharpe (buy & hold) | `1.12` | absent |
| 6 | worst drawdown (buy & hold) | `-30.74%` | absent |
| 7 | trades | `10` | absent |
| 8 | time in market | `60.96%` | absent |
| 9 | cost message | `cost_drag_cagr = 0.0051497` → "fees cost 0.51 points" | absent |
| 10 | equity curve | 520 points | absent |
| 11 | buy & hold curve | 520 points | absent |
| 12 | in-market shading | `position[]` 520 × {0,1} | absent |
| 13 | x-axis dates | `dates[0..519]` | absent |
| 14 | candlesticks + MA(20)/MA(60) | `prices.bars` 5+ | absent |
| 15 | screener table (8 rows × 6 cols) | `/screen` | absent |
| 16 | data source pill | `data_source="synthetic"` | absent |
| 17 | symbol list | 8 tickers | absent |
| 18 | console on load | — | **9 spurious 404 console errors** from the wrong backend |

---

## 4. Features of the original design lost in the port

The original design file `trading-terminal.html` **does not exist in this repository** (confirmed by
`git grep` over all refs; recorded as a spec discrepancy in `report.md` @4c89379). The surviving
reference implementation of the design is the TUI (`apps/tui/src/ui.rs`) plus the spec bullet list.
Status against that design:

| Design feature | Status today |
|---|---|
| Candlestick chart with moving averages on the MA strategy | **lost** — no chart of `/prices` exists; the only canvas is the engine's replay strip (`App.tsx:65-81`) |
| Equity chart with shaded in-market periods | **lost** — TUI has equity (`ui.rs:521`) but no shading; web has nothing |
| Metrics table (annual return, Sharpe, worst drawdown, trades, time in market, strategy vs buy & hold) | **lost** — TUI `ui.rs:472-519` is the reference; web has none |
| "fees cost X points" message | **lost** — TUI `ui.rs:433-448`; web has none |
| Sortable screener with row click → backtest | **lost** — TUI screener has selection (`ui.rs:301`) but no sort; web has none |
| Ticker box with Enter and `/` shortcut | **lost** — web has no symbol input |
| Light and dark mode | **partly** — tokens + `prefers-color-scheme` exist (`styles.css:1-5`); no user toggle, no `data-theme` ever set |
| Disabled "not ported yet" tabs | **lost** — no tab strip exists outside the engine's Log/Fills/My orders tabs (`App.tsx:160-163`) |

---

## 5. Verdict

1. **18 backend fields/features have no front-end counterpart** (table 2) and **18 live mismatches**
   (table 3) — all of the "value missing" kind.
2. The page additionally emits **9 console errors on load** because it targets the wrong backend.
3. Two design features are only half-there (theme toggle) and six are entirely absent.

Fix plan (Step 2 of the task) is executed in `web/src/Terminal.tsx` + friends, keeps
`styles.css` tokens/fonts untouched, and is proven by `web/e2e/*.spec.ts`.
