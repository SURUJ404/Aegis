// Deterministic order-log simulator. Used only for ?demo. Mirrors how lq-sequencer + lq-clob behave:
// an append-only log, a pure apply(), and a state hash that depends only on the log prefix.
// The apply/hash/levels half lives in replay.ts, shared with the live log reader.
import {
  apply, fresh, hash, levels, TICK, WIN,
  type Entry, type Side, type Tick, type View,
} from "./replay";

export { apply, fresh, hash, levels, TICK, WIN } from "./replay";
export type { Entry, Lvl, Mine, Row, Side, St, Tick, View } from "./replay";

export function createSim() {
  let seed = 13, nid = 1, gm = 64250;
  const rnd = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296;
  const log: Entry[] = [], hs = fresh();
  const push = (e: Entry) => { log.push(e); apply(hs, e); };
  const gen = () => {
    gm = Math.round((gm + (rnd() - 0.5) * TICK * 3) / TICK) * TICK; const r = rnd();
    if (r < 0.5 || hs.orders.size < 40) {
      const sd: Side = rnd() < 0.5 ? "b" : "a", o = TICK * (1 + Math.floor(rnd() * 14));
      push({ t: "place", side: sd, px: sd === "b" ? gm - o : gm + o, q: +(0.01 + rnd() * 0.4).toFixed(2), tif: "GTC", id: "m" + nid++ });
    } else if (r < 0.85) {
      const l = [...hs.orders.values()].filter((o) => !o.own);
      if (l.length) push({ t: "cancel", id: l[Math.floor(rnd() * l.length)].id });
    } else {
      const sd: Side = rnd() < 0.5 ? "b" : "a";
      push({ t: "place", side: sd, px: sd === "b" ? gm + TICK * 8 : gm - TICK * 8, q: +(0.05 + rnd() * 0.3).toFixed(2), tif: "IOC", id: "m" + nid++ });
    }
  };
  for (let i = 0; i < 500; i++) gen();
  return { log, hs, gen, push, uid: () => "u" + nid++ };
}
export type Sim = ReturnType<typeof createSim>;

const txt = (e: Entry) => e.t === "cancel" ? `cancel ${e.id}` : e.t === "kill" ? `kill switch ${e.on ? "engaged" : "released"}`
  : `${e.side === "b" ? "buy" : "sell"} ${e.q!.toFixed(2)} @ ${e.px!.toFixed(1)} ${e.tif}`;
export function viewOf(sim: Sim, p: number): View {
  let s = sim.hs;
  if (p !== sim.log.length) { s = fresh(); for (let i = 0; i < p; i++) apply(s, sim.log[i]); }
  const asks = levels(s, "a"), bids = levels(s, "b");
  const st = Math.max(0, sim.log.length - WIN);
  return {
    mid: asks[0] && bids[0] ? (asks[0].px + bids[0].px) / 2 : null, last: s.last || null,
    spread: asks[0] && bids[0] ? asks[0].px - bids[0].px : null, asks, bids, hash: hash(s), halted: s.halted, pos: s.pos, seq: p, len: sim.log.length,
    ticks: sim.log.slice(st).map((e): Tick => e.t === "cancel" ? "x" : e.t === "kill" ? "k" : e.tif === "IOC" || e.tif === "FOK" ? "f" : e.side!),
    tickFrom: st + 1,
    log: sim.log.slice(Math.max(0, p - 14), p).map((e, i, a) => ({ n: p - a.length + i + 1, text: txt(e), rej: e.rej, side: e.side })).reverse(),
    fills: s.fills.slice(-14).reverse().map((f) => ({ n: f.n, text: `${f.q.toFixed(2)} @ ${f.px.toFixed(1)}`, side: f.side, own: f.own })),
    mine: [...s.orders.values()].filter((o) => o.own),
  };
}
