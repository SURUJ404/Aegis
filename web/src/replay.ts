// Shared replay model. One apply() for both ?demo and the live engine log:
// an append-only sequence of entries, a pure fold over it, and a state hash
// that depends only on the prefix applied. sim.ts generates entries for the
// demo; live.ts maps GET /api/v1/log rows onto the same entries, so the
// ladder's own rows, the position meter, the fills and the tape all rebuild
// identically at any sequence.
export type Side = "b" | "a";
export interface Entry {
  t: "place" | "cancel" | "kill" | "fill" | "tick";
  /// WAL sequence; demo entries omit it and use their position in the log.
  seq?: number;
  side?: Side;
  px?: number;
  q?: number;
  tif?: string;
  id?: string;
  on?: boolean;
  own?: boolean;
  rej?: string;
}
export interface Lvl { px: number; q: number; own: boolean }
export interface Row { n: number | null; text: string; side?: Side; rej?: string; own?: boolean }
export interface Mine { id: string; side: Side; px: number; q: number }
export type Tick = "b" | "a" | "x" | "f" | "k";
export interface View {
  mid: number | null; last: number | null; spread: number | null; asks: Lvl[]; bids: Lvl[];
  hash: string | null; halted: boolean; pos: number; seq: number; len: number;
  ticks: Tick[]; tickFrom: number; log: Row[]; fills: Row[]; mine: Mine[];
}
export interface Ord { id: string; side: Side; px: number; q: number; own: boolean }
export interface Fill { n: number; px: number; q: number; side: Side; own: boolean }
export interface St {
  orders: Map<string, Ord>;
  /// Side of every order ever placed, kept after it leaves the book so a
  /// fill arriving later still moves the position.
  sides: Map<string, Side>;
  fills: Fill[];
  pos: number;
  halted: boolean;
  n: number;
  last: number;
}
export const TICK = 0.5;
export const WIN = 600;

export const fresh = (): St => ({
  orders: new Map(),
  sides: new Map(),
  fills: [],
  pos: 0,
  halted: false,
  n: 0,
  last: 0,
});

function best(s: St, side: Side): Ord | null {
  let b: Ord | null = null;
  for (const o of s.orders.values()) if (o.side === side && (!b || (side === "a" ? o.px < b.px : o.px > b.px))) b = o;
  return b;
}

export function apply(s: St, e: Entry): void {
  s.n = e.seq ?? s.n + 1;
  if (e.t === "cancel") { s.orders.delete(e.id!); return; }
  if (e.t === "kill") { s.halted = !!e.on; return; }
  if (e.t === "tick") { if (e.px != null) s.last = e.px; return; }
  if (e.t === "fill") {
    const q = e.q ?? 0, id = e.id, o = id ? s.orders.get(id) : undefined;
    const sd = e.side ?? o?.side ?? (id ? s.sides.get(id) : undefined);
    if (e.px != null) s.last = e.px;
    if (!sd || q <= 0) return;
    if (o) {
      o.q = +(o.q - q).toFixed(4);
      if (o.q <= 1e-9) s.orders.delete(o.id);
    }
    const own = !!(e.own || o?.own);
    s.fills.push({ n: s.n, px: e.px ?? s.last, q, side: sd, own });
    // A fill the engine refused to apply (`invalid_fill`: the order had
    // already been filled by the matching that placed it) leaves its order
    // where it was in the engine too — the book still tracks the fill, but it
    // does not move the position the engine reports.
    if (own && !e.rej) s.pos += sd === "b" ? q : -q;
    return;
  }
  const side = e.side!, px = e.px!, opp: Side = side === "b" ? "a" : "b";
  if (e.id) s.sides.set(e.id, side);
  const ok = (o: Ord) => (side === "b" ? o.px <= px : o.px >= px);
  const cross = () => { const b = best(s, opp); return b && ok(b) ? b : null; };
  if (e.own && s.halted) { e.rej = "halted"; return; }
  if (e.tif === "PO" && cross()) { e.rej = "post_only_cross"; return; }
  if (e.tif === "FOK") {
    let a = 0; for (const o of s.orders.values()) if (o.side === opp && ok(o)) a += o.q;
    if (a < e.q! - 1e-9) { e.rej = "fok_unfilled"; return; }
  }
  let q = e.q!, b: Ord | null;
  while (q > 1e-9 && (b = cross())) {
    const f = Math.min(q, b.q); q = +(q - f).toFixed(4); b.q = +(b.q - f).toFixed(4);
    s.fills.push({ n: s.n, px: b.px, q: f, side, own: !!(e.own || b.own) }); s.last = b.px;
    if (e.own) s.pos += side === "b" ? f : -f;
    if (b.own) s.pos += b.side === "b" ? f : -f;
    if (b.q <= 1e-9) s.orders.delete(b.id);
  }
  if (q > 1e-9 && e.tif !== "IOC" && e.tif !== "FOK") s.orders.set(e.id!, { id: e.id!, side, px, q, own: !!e.own });
}

export function hash(s: St): string {
  let a = 0x811c9dc5, b = 0x1b873593, t = `${s.pos}${s.halted}`;
  for (const o of s.orders.values()) t += o.id + o.side + o.px + o.q;
  for (let i = 0; i < t.length; i++) { const c = t.charCodeAt(i); a = Math.imul(a ^ c, 16777619); b = Math.imul(b ^ c, 2246822519); }
  return (a >>> 0).toString(16).padStart(8, "0") + (b >>> 0).toString(16).padStart(8, "0");
}

export function levels(s: St, sd: Side, top = 9): Lvl[] {
  const m = new Map<number, Lvl>();
  for (const o of s.orders.values()) if (o.side === sd) {
    const l = m.get(o.px) ?? { px: o.px, q: 0, own: false }; l.q += o.q; l.own = l.own || o.own; m.set(o.px, l);
  }
  return [...m.values()].sort((a, b) => (sd === "a" ? a.px - b.px : b.px - a.px)).slice(0, top);
}

/// Folds `entries` into a fresh state, stopping after sequence `upto`
/// (entries without a `seq`, i.e. demo entries, always apply).
export function replay(entries: Entry[], upto?: number): St {
  const s = fresh();
  for (const e of entries) {
    if (upto != null && e.seq != null && e.seq > upto) break;
    apply(s, e);
  }
  return s;
}
