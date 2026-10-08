/**
 * The in-browser fallback engine: a direct port of `restapis/app/data.py`
 * (`synthetic`) and `restapis/app/quant.py`, used only when the API cannot be
 * reached. It exists so a dead backend still shows a usable preview; results
 * are labelled "offline preview" and are never presented as the API's answer.
 */
import type { BacktestResult, Bar, Strategy } from "./quantApi";

function bdays(n: number): string[] {
  const out: string[] = [];
  const d = new Date();
  d.setHours(12, 0, 0, 0);
  while (out.length < n) {
    const day = d.getDay();
    if (day !== 0 && day !== 6) {
      out.push(
        `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`,
      );
    }
    d.setDate(d.getDate() - 1);
  }
  return out.reverse();
}

/** Seeded LCG walk — same constants as `data.synthetic`. */
export function synthetic(symbol: string, n = 520): Bar[] {
  let seed = 7;
  for (const ch of symbol) seed = (seed * 31 + ch.charCodeAt(0)) >>> 0;
  let state = seed >>> 0;
  const r = (): number => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
  const gauss = (): number => {
    const a = Math.sqrt(-2 * Math.log(r() || 1e-9));
    return a * Math.cos(6.2831853 * r());
  };
  const mu = 0.0003 + (r() - 0.3) * 0.0006;
  const v = 0.011 + r() * 0.016;
  let p = 50 + r() * 250;
  const dates = bdays(n);
  const bars: Bar[] = [];
  for (let i = 0; i < n; i++) {
    const o = p;
    p *= 1 + mu + v * gauss();
    const h = Math.max(o, p) * (1 + ((r() * v) / 2));
    const l = Math.min(o, p) * (1 - ((r() * v) / 2));
    bars.push({ t: dates[i], o, h, l, c: p });
  }
  return bars;
}

export function closes(bars: Bar[]): number[] {
  return bars.map((b) => b.c);
}

/** Simple moving average; `null` during the warm-up window. */
export function sma(c: number[], n: number): (number | null)[] {
  const out: (number | null)[] = [];
  let sum = 0;
  for (let i = 0; i < c.length; i++) {
    sum += c[i];
    if (i >= n) sum -= c[i - n];
    out.push(i >= n - 1 ? sum / n : null);
  }
  return out;
}

function ewmAlpha(values: (number | null)[], alpha: number, minPeriods: number): (number | null)[] {
  const out: (number | null)[] = [];
  let y = NaN;
  let obs = 0;
  for (const v of values) {
    if (v === null || !Number.isFinite(v)) {
      out.push(null);
      continue;
    }
    y = obs === 0 ? v : (1 - alpha) * y + alpha * v;
    obs++;
    out.push(obs >= minPeriods ? y : null);
  }
  return out;
}

/** Wilder RSI with the same edge cases as `quant.rsi` (all-gains 100, flat 50). */
export function rsi(c: number[], n = 14): (number | null)[] {
  const d: (number | null)[] = c.map((v, i) => (i === 0 ? null : v - c[i - 1]));
  const gain = d.map((v) => (v === null ? null : Math.max(v, 0)));
  const loss = d.map((v) => (v === null ? null : Math.max(-v, 0)));
  const ag = ewmAlpha(gain, 1 / n, n);
  const al = ewmAlpha(loss, 1 / n, n);
  return ag.map((g, i) => {
    const l = al[i];
    if (g === null || l === null) return null;
    if (l === 0 && g > 0) return 100;
    if (l === 0) return 50;
    return 100 - 100 / (1 + g / l);
  });
}

/** Desired position (0 or 1) decided at each day's close. */
export function signals(c: number[], strategy: Strategy, a: number, b: number): number[] {
  if (strategy === "ma") {
    const f = sma(c, a);
    const s = sma(c, b);
    return c.map((_, i) => (f[i] !== null && s[i] !== null && (f[i] as number) > (s[i] as number) ? 1 : 0));
  }
  if (strategy === "mom") {
    return c.map((v, i) => (i >= a && v > c[i - a] ? 1 : 0));
  }
  const out: number[] = [];
  let held = 0;
  for (const v of rsi(c, a)) {
    if (v !== null) {
      if (!held && v < b) held = 1;
      else if (held && v > 55) held = 0;
    }
    out.push(held);
  }
  return out;
}

function stats(r: number[]): { cagr: number; sharpe: number; max_drawdown: number } {
  const eq: number[] = [];
  let acc = 1;
  for (const x of r) {
    acc *= 1 + x;
    eq.push(acc);
  }
  let peak = -Infinity;
  let maxdd = 0;
  for (const v of eq) {
    peak = Math.max(peak, v);
    maxdd = Math.min(maxdd, v / peak - 1);
  }
  const mean = r.reduce((s, x) => s + x, 0) / r.length;
  const variance = r.reduce((s, x) => s + (x - mean) ** 2, 0) / r.length;
  const sd = Math.sqrt(variance) || 1e-9;
  const last = eq[eq.length - 1];
  return {
    cagr: last ** (252 / r.length) - 1,
    sharpe: (mean / sd) * Math.sqrt(252),
    max_drawdown: maxdd,
  };
}

function run(c: number[], pos: number[], bps: number) {
  const n = c.length;
  const ret: number[] = [];
  for (let i = 0; i < n; i++) {
    const v = i === 0 ? NaN : (c[i] - c[i - 1]) / c[i - 1];
    ret.push(Number.isFinite(v) ? v : 0);
  }
  const held = [0, ...pos.slice(0, n - 1)];
  const turnover = held.map((h, i) => (i === 0 ? Math.abs(h) : Math.abs(h - held[i - 1])));
  const strat = held.map((h, i) => h * ret[i] - (turnover[i] * bps) / 1e4);
  const body = strat.slice(1);
  const bh = ret.slice(1);
  const cum = (xs: number[]): number[] => {
    const out = [1];
    let acc = 1;
    for (const x of xs) {
      acc *= 1 + x;
      out.push(acc);
    }
    return out;
  };
  return {
    stats: stats(body),
    bhStats: stats(bh),
    equity: cum(body),
    buyHoldEquity: cum(bh),
    trades: turnover.slice(1).filter((t) => t > 0).length,
    exposure: pos.reduce((s, x) => s + x, 0) / n,
    position: pos.map((p) => (p > 0.5 ? 1 : 0)),
  };
}

/** `quant.backtest` — same numbers the API would return for the same bars. */
export function backtestLocal(
  symbol: string,
  bars: Bar[],
  strategy: Strategy,
  a: number,
  b: number,
  costBps: number,
): BacktestResult {
  const c = closes(bars);
  const pos = signals(c, strategy, a, b);
  const paid = run(c, pos, costBps);
  const free = run(c, pos, 0);
  return {
    symbol: symbol.toUpperCase(),
    dates: bars.map((x) => x.t),
    strategy: paid.stats,
    buy_hold: paid.bhStats,
    trades: paid.trades,
    exposure: paid.exposure,
    equity: paid.equity,
    buy_hold_equity: paid.buyHoldEquity,
    position: paid.position,
    cost_drag_cagr: free.stats.cagr - paid.stats.cagr,
  };
}
