import type { ControlResponse, StateSummary } from "./types";

const POLL_MS = 1000;

export async function fetchState(): Promise<StateSummary> {
  const res = await fetch("/api/v1/state");
  if (!res.ok) throw new Error(`state request failed: ${res.status}`);
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
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const payload = (await res.json()) as ControlResponse;
  return payload;
}

export function startPolling(onState: (s: StateSummary) => void, onError: (e: string) => void): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout>;

  const tick = async () => {
    if (stopped) return;
    try {
      onState(await fetchState());
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
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