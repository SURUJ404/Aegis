import { apply, hash, levels, replay, WIN, type Entry, type Lvl, type Mine, type Row, type Side, type St, type Tick, type View } from "./replay";
import type { BookLevel, BookResponse, LogEntry, LogResponse, StateSummary } from "./types";

const num = (x?: string | null) => (x == null ? null : Number(x));
const side = (s: string | null | undefined): Side => (s === "bid" ? "b" : "a");
const short = (id: string | null) => (id ? id.slice(0, 8) : "?");
/// WAL tif values are lowercase; the model's time-in-force checks are uppercase.
const TIF: Record<string, string> = { gtc: "GTC", ioc: "IOC", fok: "FOK", post_only: "PO" };

const tickOf = (e: LogEntry): Tick =>
  e.kind === "fill" || (e.kind === "place_order" && TIF[(e.tif ?? "").toLowerCase()] !== "GTC")
    ? "f"
    : e.kind === "place_order"
      ? side(e.side)
      : "x";

function entryText(e: LogEntry): string {
  const q = e.qty ?? "?";
  const p = e.price ?? "market";
  switch (e.kind) {
    case "place_order":
      return `${side(e.side) === "b" ? "buy" : "sell"} ${q} @ ${p} ${(e.tif ?? "").toUpperCase()}`.trim();
    case "replace_order":
      return `replace ${side(e.side) === "b" ? "buy" : "sell"} ${q} @ ${p} for ${short(e.old_id)}`;
    case "cancel_order":
      return `cancel ${short(e.id)}`;
    case "fill":
      return `fill ${q} @ ${p}`;
    case "market_tick":
      return `tick ${p}`;
    case "oracle_price":
      return `oracle ${p}`;
    case "settle_funding":
      return "settle funding";
    case "liquidate":
      return "liquidation";
    case "transfer":
      return "transfer";
    default:
      return e.kind.replace(/_/g, " ");
  }
}

/// One WAL row as the shared model sees it. Exported so the same mapping
/// can be replayed outside the browser (checks that the client fold agrees
/// with the engine's own snapshot). A cancel/replace becomes two
/// entries (cancel the old id, place the new) so the model matches the engine;
/// rows that never rest (market orders, funding, transfers) map to nothing.
export function toEntries(e: LogEntry): Entry[] {
  switch (e.kind) {
    case "place_order": {
      if (e.side == null || e.price == null || e.qty == null) return [];
      const px = Number(e.price), q = Number(e.qty);
      if (!Number.isFinite(px) || !Number.isFinite(q) || q <= 0) return [];
      return [{
        t: "place",
        seq: e.seq,
        side: side(e.side),
        px,
        q,
        tif: TIF[(e.tif ?? "").toLowerCase()] ?? "GTC",
        id: e.id ?? undefined,
        own: true,
        rej: e.reject_reason ?? undefined,
      }];
    }
    case "replace_order": {
      const out: Entry[] = [];
      if (e.old_id) out.push({ t: "cancel", seq: e.seq, id: e.old_id, own: true });
      out.push(...toEntries({ ...e, kind: "place_order" }));
      return out;
    }
    case "cancel_order":
      return e.id ? [{ t: "cancel", seq: e.seq, id: e.id, own: true }] : [];
    case "fill": {
      const px = Number(e.price ?? 0), q = Number(e.qty ?? 0);
      if (!Number.isFinite(px) || !(q > 0)) return [];
      return [{
        t: "fill",
        seq: e.seq,
        side: e.side ? side(e.side) : undefined,
        px,
        q,
        id: e.id ?? undefined,
        own: true,
        rej: e.reject_reason ?? undefined,
      }];
    }
    case "market_tick":
      return e.price == null ? [] : [{ t: "tick", seq: e.seq, px: Number(e.price) }];
    default:
      return [];
  }
}

/// Rows -> model entries, memoised on the rows array so a scrub (same rows)
/// only pays for the fold, not the mapping.
let mapped: { src: LogEntry[]; out: Entry[] } | null = null;
function modelEntries(rows: LogEntry[]): Entry[] {
  if (mapped?.src === rows) return mapped.out;
  const out: Entry[] = [];
  for (const e of rows) out.push(...toEntries(e));
  mapped = { src: rows, out };
  return out;
}

/// Replay state at `seq`. While the rows are unchanged (the live path) the
/// cached state is advanced by the few entries that arrived; scrubbing back
/// folds the prefix from scratch — the same fold the demo runs for `?demo`.
let folded: { rows: LogEntry[]; seq: number; st: St } | null = null;
function stateAt(rows: LogEntry[], seq: number): St {
  if (folded && folded.rows === rows && folded.seq <= seq) {
    const upto = folded.seq, st = folded.st;
    for (const e of rows) {
      if (e.seq <= upto) continue;
      if (e.seq > seq) break;
      for (const m of toEntries(e)) apply(st, m);
    }
    folded = { rows, seq, st };
    return st;
  }
  const st = replay(modelEntries(rows), seq);
  folded = { rows, seq, st };
  return st;
}

/// Market depth merged with the model's own open levels, aggregated to one
/// row per price and trimmed to the nine best per side.
function mergeLevels(market: BookLevel[], own: Lvl[], s: Side): Lvl[] {
  const map = new Map<number, Lvl>();
  for (const l of market) {
    const px = Number(l.price), q = Number(l.qty);
    if (!Number.isFinite(px) || !(q > 0)) continue;
    map.set(px, { px, q, own: false });
  }
  for (const l of own) {
    const cur = map.get(l.px);
    if (cur) {
      cur.q += l.q;
      cur.own = true;
    } else {
      map.set(l.px, { px: l.px, q: l.q, own: true });
    }
  }
  return [...map.values()]
    .sort((a, b) => (s === "a" ? a.px - b.px : b.px - a.px))
    .slice(0, 9);
}

/// Maps `GET /api/v1/state` + `GET /api/v1/book` + `GET /api/v1/log` onto the
/// terminal's view model. `at` is the replayed sequence (`null` = the head).
/// Orders, position, fills, the tape and the hash are all rebuilt by the
/// shared apply() module over the log prefix ending at `at` — the same fold
/// `?demo` runs — so scrubbing behaves exactly like the demo against real
/// data, and the hash tracks the scrub position. Market depth stays live
/// (market data is not in the order log).
export function liveView(
  s: StateSummary,
  book: BookResponse | null,
  log: LogResponse | null,
  at: number | null,
): View {
  const rows = log?.entries ?? [];
  const head = log?.head ?? 0;
  // While the first load is still paging the log in, the playhead follows the
  // entries actually loaded rather than claiming the head.
  const loaded = rows.length ? rows[rows.length - 1].seq : head;
  const seq = at ?? Math.min(head, loaded);

  const st = stateAt(rows, seq);
  const ownAsk = levels(st, "a", 20), ownBid = levels(st, "b", 20);
  const asks = mergeLevels(book?.asks ?? [], ownAsk, "a");
  const bids = mergeLevels(book?.bids ?? [], ownBid, "b");
  const m = s.market_state[0];
  const bid = bids[0]?.px ?? num(m?.best_bid);
  const ask = asks[0]?.px ?? num(m?.best_ask);
  const mid = num(m?.mid) ?? (bid != null && ask != null ? (bid + ask) / 2 : null);
  const spread = num(m?.spread) ?? (bid != null && ask != null ? ask - bid : null);

  const upto = rows.filter((e) => e.seq <= seq);
  const window = upto.slice(Math.max(0, upto.length - WIN));
  const mine: Mine[] = [...st.orders.values()].filter((o) => o.own);

  // Rejection reasons for the tape: whatever the shared fold concluded for the
  // entries it applied, falling back to the engine's own reason on the row.
  const rej = new Map<number, string>();
  for (const e of modelEntries(rows)) {
    if (e.rej && e.seq != null) rej.set(e.seq, e.rej);
  }

  const tape: Row[] = upto
    .slice(-14)
    .map((e) => ({
      n: e.seq,
      text: entryText(e),
      side: e.side ? side(e.side) : undefined,
      rej: rej.get(e.seq) ?? e.reject_reason ?? undefined,
    }))
    .reverse();

  const fillRows: Row[] = st.fills
    .slice(-14)
    .reverse()
    .map((f) => ({ n: f.n, text: `${f.q.toFixed(2)} @ ${f.px.toFixed(1)}`, side: f.side, own: f.own }));

  return {
    mid,
    last: st.last || null,
    spread,
    asks,
    bids,
    hash: log ? hash(st) : null,
    halted: s.risk.halted || st.halted,
    pos: log ? st.pos : s.inventory.reduce((a, i) => a + Number(i.net_qty), 0),
    seq,
    len: head,
    ticks: window.map(tickOf),
    tickFrom: window.length ? window[0].seq : seq,
    log: tape,
    fills: fillRows,
    mine,
  };
}
