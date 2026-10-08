import { useEffect, useMemo, useRef, useState, type KeyboardEvent as RKeyboardEvent } from "react";
import Controls, { SLIDERS } from "./components/Controls";
import CandleChart from "./components/CandleChart";
import EquityChart from "./components/EquityChart";
import Metrics from "./components/Metrics";
import Screener, { type SortDir, type SortKey } from "./components/Screener";
import { DEBOUNCE_MS, WAKING_MS } from "./config";
import { EM_DASH, fmtDate, fmtInt, fmtNum, isMissing } from "./format";
import {
  fetchHealth,
  fetchPrices,
  fetchScreen,
  fetchSymbols,
  postBacktest,
  type BacktestResult,
  type Bar,
  type Health,
  type ScreenRow,
  type Strategy,
} from "./quantApi";
import { backtestLocal, closes, sma, synthetic } from "./quantLocal";

type TabId = "backtest" | "screener" | "market";
type Phase = "loading" | "waking" | "ready" | "error";
type Source = "api" | "local" | null;

const TABS: { id: TabId | "live" | "portfolio"; label: string; disabled?: boolean }[] = [
  { id: "backtest", label: "Backtest" },
  { id: "screener", label: "Screener" },
  { id: "market", label: "Market" },
  { id: "live", label: "Live trading", disabled: true },
  { id: "portfolio", label: "Portfolio", disabled: true },
];

const errText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

function alignBars(all: Bar[], dates: string[]): Bar[] {
  if (!dates.length || !all.length) return all;
  const map = new Map(all.map((b) => [b.t, b]));
  const matched = dates.map((d) => map.get(d)).filter((b): b is Bar => !!b);
  if (matched.length && matched.length >= Math.min(dates.length, all.length) * 0.9) return matched;
  return all.slice(-dates.length);
}

function compare(x: unknown, y: unknown): number {
  const nx = typeof x === "number" ? x : Number(x);
  const ny = typeof y === "number" ? y : Number(y);
  if (Number.isFinite(nx) && Number.isFinite(ny)) return nx - ny;
  return String(x).localeCompare(String(y));
}

export default function Terminal() {
  const [tab, setTab] = useState<TabId>("backtest");
  const [symbol, setSymbol] = useState("AAPL");
  const [symbols, setSymbols] = useState<string[]>([]);
  const [health, setHealth] = useState<Health | null>(null);
  const [strategy, setStrategy] = useState<Strategy>("ma");
  const [a, setA] = useState(20);
  const [b, setB] = useState(60);
  const [cost, setCost] = useState(10);
  const [result, setResult] = useState<BacktestResult | null>(null);
  const [bars, setBars] = useState<Bar[]>([]);
  const [source, setSource] = useState<Source>(null);
  const [phase, setPhase] = useState<Phase>("loading");
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [screen, setScreen] = useState<ScreenRow[]>([]);
  const [screenError, setScreenError] = useState<string | null>(null);
  const [screenLoading, setScreenLoading] = useState(false);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: "symbol", dir: "asc" });
  const [selected, setSelected] = useState<string | null>(null);
  const [theme, setTheme] = useState<"light" | "dark">(() => {
    const saved = localStorage.getItem("aegis-theme");
    if (saved === "light" || saved === "dark") return saved;
    return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  });

  const tickerRef = useRef<HTMLInputElement>(null);
  const seqRef = useRef(0);
  const barsRef = useRef<{ symbol: string; bars: Bar[] }>({ symbol: "", bars: [] });

  const blocked =
    strategy === "ma" && a >= b
      ? `Fast MA (${a}) must be below slow MA (${b}) — move a slider before running.`
      : null;

  // Theme is a document-level concern: one attribute drives every token.
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("aegis-theme", theme);
  }, [theme]);

  // `/` jumps to the ticker box from anywhere that is not already a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName)) return;
      if (t?.isContentEditable) return;
      e.preventDefault();
      tickerRef.current?.focus();
      tickerRef.current?.select();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Health + symbol universe (re-checked whenever the user retries).
  useEffect(() => {
    const ctrl = new AbortController();
    void (async () => {
      const [h, s] = await Promise.allSettled([fetchHealth(ctrl.signal), fetchSymbols(ctrl.signal)]);
      if (ctrl.signal.aborted) return;
      setHealth(h.status === "fulfilled" ? h.value : null);
      if (s.status === "fulfilled") setSymbols(s.value);
    })();
    return () => ctrl.abort();
  }, [retry]);

  // Debounced, cancellable backtest + price fetch. Stale answers are dropped by
  // both the abort and the sequence counter, so a slow response can never
  // overwrite a newer one.
  useEffect(() => {
    if (blocked) return undefined;
    const ctrl = new AbortController();
    const seq = ++seqRef.current;
    const wake = setTimeout(() => {
      if (seq === seqRef.current) setPhase("waking");
    }, WAKING_MS);
    const start = setTimeout(() => {
      if (seq !== seqRef.current) return;
      setPhase("loading");
      setError(null);
      void (async () => {
        try {
          const [bt, px] = await Promise.all([
            postBacktest({ symbol, strategy, a, b, cost_bps: cost }, ctrl.signal).then(
              (v) => ({ ok: true as const, v }),
              (e) => ({ ok: false as const, e }),
            ),
            fetchPrices(symbol, 1000, ctrl.signal).then(
              (v) => ({ ok: true as const, v }),
              () => ({ ok: false as const }),
            ),
          ]);
          if (ctrl.signal.aborted || seq !== seqRef.current) return;
          if (!bt.ok) throw bt.e;
          const aligned = px.ok ? alignBars(px.v.bars, bt.v.dates) : [];
          if (aligned.length) barsRef.current = { symbol, bars: aligned };
          setResult(bt.v);
          setBars(aligned);
          setSource("api");
          setPhase("ready");
          setError(null);
        } catch (e) {
          if (ctrl.signal.aborted || seq !== seqRef.current) return;
          if (e instanceof DOMException && e.name === "AbortError") return;
          const held = barsRef.current.symbol === symbol ? barsRef.current.bars : [];
          const previewBars = held.length ? held : synthetic(symbol);
          barsRef.current = { symbol, bars: previewBars };
          setResult(backtestLocal(symbol, previewBars, strategy, a, b, cost));
          setBars(previewBars);
          setSource("local");
          setError(errText(e));
          setPhase("error");
        } finally {
          clearTimeout(wake);
        }
      })();
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(start);
      clearTimeout(wake);
      ctrl.abort();
    };
  }, [symbol, strategy, a, b, cost, retry, blocked]);

  // The screener only talks to the API while it is on screen.
  useEffect(() => {
    if (blocked || tab !== "screener") return undefined;
    const ctrl = new AbortController();
    const start = setTimeout(() => {
      setScreenLoading(true);
      setScreenError(null);
      void (async () => {
        try {
          const rows = await fetchScreen({ strategy, a, b }, ctrl.signal);
          if (ctrl.signal.aborted) return;
          setScreen(rows);
        } catch (e) {
          if (ctrl.signal.aborted) return;
          if (e instanceof DOMException && e.name === "AbortError") return;
          setScreenError(errText(e));
        } finally {
          if (!ctrl.signal.aborted) setScreenLoading(false);
        }
      })();
    }, DEBOUNCE_MS);
    return () => {
      clearTimeout(start);
      ctrl.abort();
    };
  }, [tab, strategy, a, b, retry, blocked]);

  const sorted = useMemo(() => {
    const rows = [...screen];
    const { key, dir } = sort;
    rows.sort((x, y) => {
      const mx = isMissing(x[key]);
      const my = isMissing(y[key]);
      if (mx && my) return 0;
      if (mx) return 1;
      if (my) return -1;
      const c = compare(x[key], y[key]);
      return dir === "asc" ? c : -c;
    });
    return rows;
  }, [screen, sort]);

  const overlays = useMemo(() => {
    if (strategy !== "ma" || !bars.length) return [];
    const c = closes(bars);
    return [
      { n: a, values: sma(c, a) },
      { n: b, values: sma(c, b) },
    ].filter((o) => o.n > 1 && o.n <= bars.length);
  }, [strategy, a, b, bars]);

  const toggleSort = (key: SortKey) =>
    setSort((s) => (s.key === key ? { key, dir: s.dir === "asc" ? "desc" : "asc" } : { key, dir: "asc" }));

  const pickSymbol = (next: string) => {
    setSelected(next);
    setSymbol(next);
    setTab("backtest");
  };

  const onStrategy = (next: Strategy) => {
    setStrategy(next);
    if (next === "ma" && a >= b) setB(Math.min(250, a + 40));
    if (next === "rsi" && b < SLIDERS.rsi.b!.min) setB(30);
  };

  const onTabKey = (e: RKeyboardEvent<HTMLDivElement>) => {
    const enabled = TABS.filter((t) => !t.disabled);
    const ids = enabled.map((t) => t.id);
    const current = ids.indexOf(tab);
    let next = current;
    if (e.key === "ArrowRight") next = (current + 1) % ids.length;
    else if (e.key === "ArrowLeft") next = (current - 1 + ids.length) % ids.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = ids.length - 1;
    else return;
    e.preventDefault();
    const id = ids[next] as TabId;
    setTab(id);
    document.getElementById(`tab-${id}`)?.focus();
  };

  const statusText =
    phase === "waking"
      ? "Waking the server, this can take a minute…"
      : phase === "loading"
        ? "Running backtest…"
        : null;

  return (
    <div className="app terminal">
      <header className="term-header">
        <h1>Aegis</h1>
        <span className="tag">Trading Terminal</span>
        <span className="pill" data-testid="health">
          {health ? `API ok · ${health.data_source}` : "API unreachable"}
        </span>
        <span className="pill num">{symbol}</span>
        {source === "local" && (
          <span className="pill offline" data-testid="offline-badge">
            offline preview
          </span>
        )}
        <div className="head-right">
          <a className="head-link" href="/engine">
            Engine dashboard
          </a>
          <button
            type="button"
            className="btn sm"
            data-testid="theme"
            aria-label={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
            onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
          >
            {theme === "dark" ? "Light" : "Dark"}
          </button>
        </div>
      </header>

      <nav aria-label="Sections">
        <div className="tabs" role="tablist" aria-label="Terminal sections" onKeyDown={onTabKey}>
          {TABS.map((t) => {
            const active = t.id === tab;
            return (
              <button
                key={t.id}
                type="button"
                role="tab"
                id={`tab-${t.id}`}
                aria-selected={active}
                aria-controls={t.disabled ? undefined : `panel-${t.id}`}
                aria-disabled={t.disabled || undefined}
                disabled={t.disabled}
                title={t.disabled ? "not ported yet" : undefined}
                tabIndex={active ? 0 : -1}
                onClick={() => !t.disabled && setTab(t.id as TabId)}
              >
                {t.label}
                {t.disabled && <span className="tab-note">not ported yet</span>}
              </button>
            );
          })}
        </div>
      </nav>

      <main>
        {statusText && (
          <div className="banner status" role="status" data-testid={phase === "waking" ? "waking" : "loading"}>
            {statusText}
          </div>
        )}
        {phase === "error" && error && (
          <div className="banner alert" role="alert" data-testid="error">
            <span>{error}</span>
            <button type="button" className="btn sm" data-testid="retry" onClick={() => setRetry((r) => r + 1)}>
              Retry
            </button>
          </div>
        )}
        {blocked && (
          <div className="banner alert" role="alert" data-testid="blocked">
            {blocked}
          </div>
        )}

        <div
          role="tabpanel"
          id={`panel-${tab}`}
          aria-labelledby={`tab-${tab}`}
          tabIndex={-1}
          className={tab === "backtest" ? "term-main" : undefined}
        >
          {tab === "backtest" && (
            <>
              <Controls
                symbol={symbol}
                symbols={symbols}
                onSymbol={setSymbol}
                strategy={strategy}
                onStrategy={onStrategy}
                a={a}
                b={b}
                onA={setA}
                onB={setB}
                cost={cost}
                onCost={setCost}
                inputRef={tickerRef}
              />
              <div className="term-content">
                <section
                  className="panel results"
                  data-testid="results"
                  data-source={source === "local" ? "offline" : source === "api" ? "api" : "none"}
                  aria-label="Backtest results"
                >
                  <div className="trow">
                    <b className="num">{result?.symbol || symbol}</b>
                    <span className="lbl">
                      {strategy} · a {a}
                      {strategy !== "mom" ? ` · b ${b}` : ""} · {fmtNum(cost, 1)} bps
                    </span>
                    <span className="lbl">
                      {result ? `${fmtInt(result.dates.length)} bars` : EM_DASH}
                    </span>
                  </div>
                  {result ? (
                    <>
                      <Metrics result={result} costBps={cost} />
                      <h2 className="chart-title">Price and moving averages</h2>
                      <CandleChart bars={bars} overlays={overlays} label={`${result.symbol} daily`} />
                      <div className="legend">
                        <span>
                          <i className="sw up" /> up
                        </span>
                        <span>
                          <i className="sw down" /> down
                        </span>
                        {overlays.map((o, i) => (
                          <span key={o.n}>
                            <i className={`sw ma ${i === 0 ? "fast" : "slow"}`} /> MA {o.n}
                          </span>
                        ))}
                      </div>
                      <h2 className="chart-title">Equity — growth of 1</h2>
                      <EquityChart
                        dates={result.dates}
                        equity={result.equity}
                        buyHold={result.buy_hold_equity}
                        position={result.position}
                        label={`${result.symbol} equity`}
                      />
                      <div className="legend">
                        <span>
                          <i className="sw strat" /> strategy
                        </span>
                        <span>
                          <i className="sw hold" /> buy &amp; hold
                        </span>
                        <span>
                          <i className="sw shade" /> in the market
                        </span>
                      </div>
                    </>
                  ) : (
                    <div className="empty">No results yet.</div>
                  )}
                </section>
              </div>
            </>
          )}

          {tab === "screener" && (
            <section className="panel screener-panel" aria-label="Screener">
              <div className="trow">
                <b>Screener</b>
                <span className="lbl">
                  {strategy} · a {a}
                  {strategy !== "mom" ? ` · b ${b}` : ""}
                </span>
                <span className="lbl">{screen.length} rows</span>
                <button
                  type="button"
                  className="btn sm"
                  data-testid="screen-retry"
                  onClick={() => setRetry((r) => r + 1)}
                >
                  Refresh
                </button>
              </div>
              {screenError ? (
                <div className="banner alert" role="alert" data-testid="screen-error">
                  <span>{screenError}</span>
                  <button
                    type="button"
                    className="btn sm"
                    onClick={() => setRetry((r) => r + 1)}
                  >
                    Retry
                  </button>
                </div>
              ) : screenLoading && !screen.length ? (
                <div className="empty" role="status">
                  Screening…
                </div>
              ) : (
                <Screener
                  rows={sorted}
                  sort={sort}
                  onSort={toggleSort}
                  selected={selected}
                  onSelect={pickSymbol}
                />
              )}
            </section>
          )}

          {tab === "market" && (
            <section className="panel market" aria-label="Market">
              <div className="trow">
                <b className="num">{symbol}</b>
                <span className="lbl">{bars.length} bars from /prices</span>
                <span className="lbl">
                  last close {bars.length ? fmtNum(bars[bars.length - 1].c) : EM_DASH}
                </span>
              </div>
              {bars.length ? (
                <div className="table-scroll">
                  <table className="screen bars">
                    <thead>
                      <tr>
                        <th scope="col">Date</th>
                        <th scope="col">Open</th>
                        <th scope="col">High</th>
                        <th scope="col">Low</th>
                        <th scope="col">Close</th>
                      </tr>
                    </thead>
                    <tbody>
                      {[...bars]
                        .slice(-12)
                        .reverse()
                        .map((bar) => (
                          <tr key={bar.t}>
                            <td className="num">{fmtDate(bar.t)}</td>
                            <td className="num">{fmtNum(bar.o)}</td>
                            <td className="num">{fmtNum(bar.h)}</td>
                            <td className="num">{fmtNum(bar.l)}</td>
                            <td className="num">{fmtNum(bar.c)}</td>
                          </tr>
                        ))}
                    </tbody>
                  </table>
                </div>
              ) : (
                <div className="empty">No bars loaded for this ticker.</div>
              )}
            </section>
          )}
        </div>
      </main>
    </div>
  );
}

