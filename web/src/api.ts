import type { BookResponse, ControlResponse, LogEntry, LogResponse, Snapshot, StateSummary } from "./types";

const POLL_MS = 1000;
/// Entries per page (the server clamps to 5000): big enough to catch up from
/// seq 1 on first load, and every poll after that asks only for what the head
/// has gained since the last sequence we saw.
const LOG_LIMIT = 5000;

// Bearer token for an API started with `API_TOKEN`. `?token=...` in the URL
// wins and is kept in localStorage so reloads stay authenticated.
const params = new URLSearchParams(location.search);
if (params.get("token")) localStorage.setItem("lq_token", params.get("token")!);
const token = params.get("token") ?? localStorage.getItem("lq_token");

function authHeaders(): Record<string, string> {
  return token ? { authorization: `Bearer ${token}` } : {};
}

export async function fetchState(): Promise<StateSummary> {
  const res = await fetch("/api/v1/state", { headers: authHeaders() });
  if (!res.ok) throw new Error(`state request failed: ${res.status}`);
  return res.json();
}

export async function fetchBook(depth = 50): Promise<BookResponse> {
  const res = await fetch(`/api/v1/book?depth=${depth}`, { headers: authHeaders() });
  if (!res.ok) throw new Error(`book request failed: ${res.status}`);
  return res.json();
}

/// `from` = first sequence to return (omitted: the window of `limit` entries
/// ending at the head). The page never depends on where the replay scrubber is
/// parked, so a client watching seq N keeps appending what the head gained.
export async function fetchLog(from?: number): Promise<LogResponse> {
  const qs = new URLSearchParams({ limit: String(LOG_LIMIT) });
  if (from != null) qs.set("from", String(from));
  const res = await fetch(`/api/v1/log?${qs}`, { headers: authHeaders() });
  if (!res.ok) throw new Error(`log request failed: ${res.status}`);
  return res.json();
}

export async function postControl(action: "start" | "stop" | "reset", reason?: string): Promise<ControlResponse> {
  return postJson(`/api/v1/control/${action}`, reason ? { reason } : undefined);
}

export async function killSwitch(reason: string): Promise<ControlResponse> {
  return postJson("/api/v1/control/kill", { reason });
}

async function postJson(url: string, body?: unknown): Promise<ControlResponse> {
  const res = await fetch(url, {
    method: "POST",
    headers: { ...authHeaders(), ...(body ? { "content-type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  return (await res.json()) as ControlResponse;
}

/// Poll the control plane once a second: state and book in full, the log as
/// an append of everything since the last sequence we saw (so the replay
/// strip has the whole history without re-downloading it). The accumulated
/// log is handed to the shared replay module, which rebuilds the state at any
/// sequence from it.
export function startPolling(
  onSnapshot: (s: Snapshot) => void,
  onError: (e: string) => void,
): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout>;
  /// Everything from seq 1 onwards. Pushed into (never rebuilt), so the
  /// array's identity is stable and the replay module can memoise on it.
  let acc: LogEntry[] = [];
  let seen = 0;
  /// Whether the last page actually gained rows (drives the catch-up loop).
  let advanced = false;
  /// Last good log response, held across a failed log poll.
  let last: LogResponse | null = null;

  const merge = (page: LogResponse): LogResponse => {
    advanced = false;
    // The WAL restarted behind us: the history we hold no longer exists.
    // A new array resets the replay module's memoised fold with it.
    if (page.head < seen) { acc = []; seen = 0; }
    const next = page.entries;
    // A gap (the WAL never skips a seq) means we cannot bridge it: start the
    // window at the first entry we can trust.
    if (next.length && next[0].seq !== seen + 1) { acc = []; seen = next[0].seq - 1; }
    if (next.length) {
      for (const e of next) acc.push(e);
      seen = next[next.length - 1].seq;
      advanced = true;
    }
    return { ...page, entries: acc };
  };

  const tick = async () => {
    if (stopped) return;
    let drained = false;
    try {
      const [state, book, page] = await Promise.all([
        fetchState(),
        fetchBook().catch(() => null),
        fetchLog(seen + 1).catch(() => null),
      ]);
      if (page) last = merge(page);
      onSnapshot({ state, book, log: last });
      // Keep pulling while the head is still ahead of us, so a cold load
      // catches up as fast as the server can page instead of one page a
      // second. A page that gains nothing falls back to the timer.
      drained = advanced && !!last && seen < last.head;
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
    if (drained && !stopped) {
      void tick();
      return;
    }
    timer = setTimeout(tick, POLL_MS);
  };

  tick();
  return () => {
    stopped = true;
    clearTimeout(timer);
  };
}

export function fmtPrice(v: string | null | undefined, digits = 2): string {
  if (v === null || v === undefined) return "—";
  const n = Number(v);
  if (!Number.isFinite(n)) return v;
  return n.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

export function fmtQty(v: string | null | undefined): string {
  if (v === null || v === undefined) return "—";
  const n = Number(v);
  if (!Number.isFinite(n)) return v;
  return n.toLocaleString("en-US", { maximumFractionDigits: 8 });
}

export function fmtPnl(v: string | null | undefined): string {
  if (v === null || v === undefined) return "—";
  const n = Number(v);
  if (!Number.isFinite(n)) return v;
  return n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 4 });
}

export function fmtPct(v: number | null | undefined): string {
  if (v === null || v === undefined) return "—";
  return `${(v * 100).toFixed(2)}%`;
}

export function fmtUptime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return `${h}h ${m}m ${sec}s`;
}

export function shortId(id: string): string {
  return id.slice(0, 8);
}

export function timeAgo(ts: number): string {
  const delta = Date.now() - ts;
  if (delta < 1000) return "now";
  if (delta < 60_000) return `${Math.floor(delta / 1000)}s ago`;
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  return `${Math.floor(delta / 3_600_000)}h ago`;
}