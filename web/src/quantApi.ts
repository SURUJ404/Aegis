import { API_URL, REQUEST_TIMEOUT_MS } from "./config";

export type Strategy = "ma" | "mom" | "rsi";

export interface Stats {
  cagr: number;
  sharpe: number;
  max_drawdown: number;
}

/** `POST /backtest` — every field the API returns. */
export interface BacktestResult {
  symbol: string;
  dates: string[];
  strategy: Stats;
  buy_hold: Stats;
  trades: number;
  exposure: number;
  equity: number[];
  buy_hold_equity: number[];
  position: number[];
  cost_drag_cagr: number;
}

export interface Bar {
  t: string;
  o: number;
  h: number;
  l: number;
  c: number;
}

/** `GET /prices`. */
export interface Prices {
  symbol: string;
  bars: Bar[];
}

/** `GET /screen` — indicator fields are nullable on short histories. */
export interface ScreenRow {
  symbol: string;
  price: number | null;
  ret_1m: number | null;
  rsi14: number | null;
  vs_sma50: number | null;
  signal: string;
}

export interface Health {
  ok: boolean;
  data_source: string;
}

export type ApiErrorKind = "network" | "timeout" | "http" | "invalid";

/** Anything that stopped a call from producing data. Drives the error + fallback UI. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly kind: ApiErrorKind,
    readonly status?: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

function messageFor(status: number, detail?: string): string {
  const detailText = detail ? ` ${detail}` : "";
  if (status === 404) return `Unknown ticker or endpoint (404).${detailText}`;
  if (status === 422) return `The API rejected these parameters (422).${detailText}`;
  if (status === 502 || status === 503) return `The data provider failed (${status}).${detailText}`;
  return `The API returned an error (${status}).${detailText}`;
}

/**
 * One GET/POST helper for the whole quant API: timeout, abort wiring, JSON
 * parsing and error classification live here so no call site can forget them.
 */
export async function request<T>(
  path: string,
  options: { method?: "GET" | "POST"; body?: unknown; signal?: AbortSignal } = {},
): Promise<T> {
  const { method = "GET", body, signal } = options;
  const ctrl = new AbortController();
  const onAbort = () => ctrl.abort();
  signal?.addEventListener("abort", onAbort, { once: true });
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    ctrl.abort();
  }, REQUEST_TIMEOUT_MS);

  let res: Response;
  try {
    res = await fetch(`${API_URL}${path}`, {
      method,
      signal: ctrl.signal,
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch (e) {
    if (signal?.aborted) throw e; // caller cancelled: not a real failure
    if (timedOut) {
      throw new ApiError(
        `The API did not answer within ${Math.round(REQUEST_TIMEOUT_MS / 1000)}s.`,
        "timeout",
      );
    }
    throw new ApiError(`Could not reach the API at ${API_URL}.`, "network");
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", onAbort);
  }

  if (!res.ok) {
    let detail = "";
    try {
      const payload = await res.json();
      if (payload && typeof payload === "object" && "detail" in payload) {
        const d = (payload as { detail: unknown }).detail;
        detail = typeof d === "string" ? d : JSON.stringify(d);
      }
    } catch {
      /* body is not JSON: the status alone is the message */
    }
    throw new ApiError(messageFor(res.status, detail), "http", res.status);
  }

  try {
    return (await res.json()) as T;
  } catch {
    throw new ApiError("The API returned a response that is not valid JSON.", "invalid");
  }
}

export function fetchHealth(signal?: AbortSignal): Promise<Health> {
  return request<Health>("/health", { signal });
}

export function fetchSymbols(signal?: AbortSignal): Promise<string[]> {
  return request<string[]>("/symbols", { signal });
}

export function fetchPrices(symbol: string, days: number, signal?: AbortSignal): Promise<Prices> {
  return request<Prices>(`/prices?symbol=${encodeURIComponent(symbol)}&days=${days}`, { signal });
}

export interface BacktestParams {
  symbol: string;
  strategy: Strategy;
  a: number;
  b: number;
  cost_bps: number;
}

export function postBacktest(p: BacktestParams, signal?: AbortSignal): Promise<BacktestResult> {
  return request<BacktestResult>("/backtest", { method: "POST", body: p, signal });
}

export function fetchScreen(
  params: { strategy: Strategy; a: number; b: number },
  signal?: AbortSignal,
): Promise<ScreenRow[]> {
  const qs = `strategy=${params.strategy}&a=${params.a}&b=${params.b}`;
  return request<ScreenRow[]>(`/screen?${qs}`, { signal });
}
