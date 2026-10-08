import { useEffect, useReducer, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as RPE } from "react";
import { killSwitch, postControl, startPolling } from "./api";
import type { Snapshot } from "./types";
import { createSim, viewOf, type Entry, type Lvl, type Side } from "./sim";
import { liveView } from "./live";

const demo = new URLSearchParams(location.search).has("demo");
const sim = demo ? createSim() : null;
const f = (n: number | null, d = 1) => (n === null ? "–" : n.toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d }));
const css = (n: string) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
const color = (s: Side) => `var(--${s === "b" ? "bid" : "ask"})`;

export default function EngineApp() {
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const [P, setPRaw] = useState(sim ? sim.log.length : 0);
  const [follow, setFollow] = useState(true);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tab, setTab] = useState<"log" | "fills" | "mine">("log");
  const [side, setSide] = useState<Side>("b");
  const [px, setPx] = useState(""), [q, setQ] = useState("0.10"), [tif, setTif] = useState("GTC"), [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);
  const cv = useRef<HTMLCanvasElement>(null), pRef = useRef(P), followRef = useRef(true);
  pRef.current = P;
  followRef.current = follow;
  const len = sim ? sim.log.length : snap?.log?.head ?? 0;
  const st = snap?.state ?? null;

  // Scrub to `p`; parking on the head resumes following the live log.
  const setP = (p: number) => {
    const hi = sim ? sim.log.length : len;
    const c = Math.max(0, Math.min(hi, p));
    setPRaw(c);
    if (!sim) setFollow(c >= hi);
  };

  useEffect(() => {
    if (sim) {
      const id = setInterval(() => { const live = pRef.current === sim.log.length; sim.gen(); if (live) setPRaw(sim.log.length); bump(); }, 900);
      return () => clearInterval(id);
    }
    return startPolling(
      (s) => {
        setSnap(s);
        setErr(null);
        if (followRef.current) setPRaw(s.log?.head ?? 0);
      },
      setErr,
    );
  }, []);

  // A control answer clears itself after a few seconds. One timer per message:
  // setting a new one re-arms it, so an earlier message's timer can never wipe
  // a later one early.
  useEffect(() => {
    if (!msg) return;
    const t = setTimeout(() => setMsg(""), 5_000);
    return () => clearTimeout(t);
  }, [msg]);

  const v = sim ? viewOf(sim, P) : snap ? liveView(snap.state, snap.book, snap.log, follow ? null : P) : null;
  const live = sim ? P === len : follow, canTrade = !!sim && live;

  useEffect(() => {
    const c = cv.current; if (!c || !v) return;
    const r = c.getBoundingClientRect(), d = devicePixelRatio || 1, x = c.getContext("2d")!;
    c.width = r.width * d; c.height = r.height * d; x.setTransform(d, 0, 0, d, 0, 0);
    const n = v.ticks.length, w = r.width / Math.max(1, n), off = v.tickFrom - 1;
    v.ticks.forEach((k, i) => {
      const h = k === "x" ? 0.3 : k === "k" ? 1 : k === "f" ? 0.9 : 0.55;
      x.fillStyle = css(k === "b" ? "--bid" : k === "a" ? "--ask" : k === "x" ? "--mute" : "--ink");
      x.fillRect(i * w, r.height * (1 - h), Math.max(1, w - 0.6), r.height * h);
    });
    x.fillStyle = css("--ink"); x.fillRect(Math.max(0, Math.min(r.width - 2, (P - off) * w - 1)), 0, 2, r.height);
  });

  const at = (e: RPE<HTMLCanvasElement>) => {
    const r = cv.current!.getBoundingClientRect(), n = v!.ticks.length;
    setP(v!.tickFrom - 1 + Math.round(((e.clientX - r.left) / r.width) * n));
  };
  const place = () => {
    const pr = parseFloat(px), qq = parseFloat(q);
    if (!sim) return;
    if (!(pr > 0) || !(qq > 0)) { setMsg("Enter a price and a size above zero."); return; }
    const e: Entry = { t: "place", side, px: pr, q: qq, tif, id: sim.uid(), own: true };
    sim.push(e); setP(sim.log.length);
    setMsg(e.rej ? `Rejected: ${e.rej.replace(/_/g, " ")}. The rejection is in the log.` : "Order accepted and logged.");
  };
  const control = (action: "start" | "stop" | "reset") => {
    setBusy(true);
    postControl(action)
      .then((r) => setMsg(r.accepted ? `${action}: ${r.message}` : `${action} failed: ${r.message}`))
      .catch((e) => setMsg(`${action} failed: ${e instanceof Error ? e.message : String(e)}`))
      .finally(() => setBusy(false));
  };
  const kill = (halted: boolean) => {
    if (sim) { sim.push({ t: "kill", on: !sim.hs.halted, own: true }); setP(sim.log.length); return; }
    // The engine clears the halt on `reset`; there is no separate release.
    if (halted) { control("reset"); return; }
    const reason = window.prompt("Kill switch reason:");
    if (reason === null) return;
    setBusy(true);
    killSwitch(reason)
      .then((r) => setMsg(`kill: ${r.message}`))
      .catch((e) => setMsg(`kill failed: ${e instanceof Error ? e.message : String(e)}`))
      .finally(() => setBusy(false));
  };
  const rows = (ls: Lvl[], s: Side, max: number) => {
    let c = 0;
    return ls.map((l) => { c += l.q; return (
      <div key={l.px} className={`row ${s} num${l.own ? " own" : ""}`} style={{ "--w": `${(c / max) * 100}%` } as CSSProperties}
        onClick={() => { setPx(String(l.px)); setSide(s === "a" ? "b" : "a"); }}>
        <span>{f(l.px)}</span><span>{f(l.q, 2)}</span><span>{f(c, 2)}</span>
      </div>); });
  };

  if (!v) return (
    <div className="app"><div className="banner">
      {err ? <>The API at /api/v1/state is unreachable ({err}). <a href="?demo">Open the demo data</a> to see the terminal without a backend.</> : "Connecting to the engine."}
    </div></div>
  );
  const max = Math.max(v.asks.reduce((a, l) => a + l.q, 0), v.bids.reduce((a, l) => a + l.q, 0), 1);
  const mine = v.mine.filter((o) => o.id);
  return (
    <div className="app">
      <header>
        <h1>Aegis</h1><span className="mkt num">{demo ? "BTC-USDT-PERP" : st?.market_state[0]?.symbol ?? "–"}</span>
        <span className="pill">{demo ? "Demo data" : "Paper mode"}</span>
        {!sim && st && <span className={`pill${st.strategy_running ? " run" : ""}`}>{st.strategy_running ? "Running" : "Stopped"}</span>}
        {v.halted && <span className="pill halt">Halted{st?.risk.halt_reason ? `: ${st.risk.halt_reason}` : ""}</span>}
        <div className="mid num">{f(v.mid)}</div>
      </header>

      <section className={`panel tape${live ? "" : " replay"}`} aria-label="Order log">
        <div className="trow">
          <span><span className="lbl">Log position</span> <b className="num">{f(v.seq, 0)}</b> <span className="lbl">of</span> <span className="num">{f(v.len, 0)}</span></span>
          <span><span className="lbl">State hash</span> <b className="num hash" title={v.hash ?? undefined}>{v.hash ? (v.hash.length > 16 ? `${v.hash.slice(0, 16)}…` : v.hash) : "–"}</b></span>
          <span className="lbl">{!snap?.log && !sim ? "No order log: the engine opened no WAL." : live ? "Live: new entries stream in" : "Replaying history: orders are disabled"}</span>
          {!live && <button className="btn primary sm" onClick={() => setP(len)}>Return to live</button>}
        </div>
        {(sim || v.ticks.length > 0) && <canvas ref={cv} tabIndex={0} role="slider" aria-label="Replay position in the order log" aria-valuemin={0} aria-valuemax={len} aria-valuenow={P}
          onPointerDown={(e) => { e.currentTarget.setPointerCapture(e.pointerId); at(e); }}
          onPointerMove={(e) => { if (e.currentTarget.hasPointerCapture(e.pointerId)) at(e); }}
          onKeyDown={(e) => { const k = ({ ArrowLeft: -1, ArrowRight: 1, Home: -1e9, End: 1e9 } as Record<string, number>)[e.key]; if (k) { e.preventDefault(); setP(P + k * (e.shiftKey ? 10 : 1)); } }} />}
      </section>

      <main>
        <section className="panel ladder" aria-label="Order book">
          <div className="colh"><span>Price</span><span>Size</span><span>Total</span></div>
          {[...rows(v.asks, "a", max)].reverse()}
          <div className="spread num"><span>Spread <b>{f(v.spread)}</b></span><span>Last <b>{f(v.last)}</b></span></div>
          {rows(v.bids, "b", max)}
          {!sim && <div className="note">{live
            ? "Live depth from /api/v1/book, merged with the engine's own open orders."
            : "Tape, orders and hash at the replayed sequence; market depth stays live."}</div>}
        </section>

        <section className="panel">
          <div className="tabs" role="tablist">
            {(["log", "fills", "mine"] as const).map((t) => (
              <button key={t} role="tab" aria-selected={tab === t} onClick={() => setTab(t)}>{t === "log" ? "Log" : t === "fills" ? "Fills" : "My orders"}</button>))}
          </div>
          <div className="list num">
            {tab === "mine" ? (mine.length ? mine.map((o) => (
              <div className="li" key={o.id}><span style={{ color: color(o.side) }}>{o.side === "b" ? "buy" : "sell"}</span><span>{f(o.q, 2)} @ {f(o.px)}</span>
                {canTrade && <button className="btn sm" onClick={() => { sim!.push({ t: "cancel", id: o.id }); setP(sim!.log.length); }}>Cancel</button>}</div>))
              : <div className="empty">No open orders.</div>)
            : (tab === "log" ? v.log : v.fills).length ? (tab === "log" ? v.log : v.fills).map((r, i) => (
              <div className="li" key={i}><span className="s">{r.n === null ? "" : `#${r.n}`}</span>
                {tab === "fills" && r.side && <span style={{ color: color(r.side) }}>{r.side === "b" ? "buy" : "sell"}</span>}
                <span>{r.text}</span>{r.rej && <span className="rj">rejected: {r.rej}</span>}{r.own && <span className="s">yours</span>}</div>))
            : <div className="empty">Nothing here yet.</div>}
          </div>
        </section>

        <aside className="side">
          <form className="panel form" onSubmit={(e) => e.preventDefault()}>
            <div className="seg" role="group" aria-label="Side">
              <button type="button" className="b" aria-pressed={side === "b"} onClick={() => setSide("b")}>Buy</button>
              <button type="button" className="a" aria-pressed={side === "a"} onClick={() => setSide("a")}>Sell</button>
            </div>
            <label>Price<input className="num" inputMode="decimal" value={px} onChange={(e) => setPx(e.target.value)} /></label>
            <div className="two">
              <label>Size<input className="num" inputMode="decimal" value={q} onChange={(e) => setQ(e.target.value)} /></label>
              <label>Time in force<select value={tif} onChange={(e) => setTif(e.target.value)}>
                <option value="GTC">Good till cancel</option><option value="IOC">Immediate or cancel</option>
                <option value="FOK">Fill or kill</option><option value="PO">Post only</option></select></label>
            </div>
            <button className="btn primary" disabled={!canTrade} onClick={place}>Place order</button>
            <div id="msg" role="status">{sim ? msg : "Order entry arrives with the signed gateway (Stage 6)."}</div>
          </form>
          <div className="panel form" aria-label="Risk and control">
            <div className="meter"><div><span>Position</span><b className="num">{f(Math.abs(v.pos), 2)}{sim ? " / 1.00" : ""}</b></div>{sim && <div className="bar"><i style={{ width: `${Math.min(100, Math.abs(v.pos) * 100)}%` }} /></div>}</div>
            <div className="meter"><div><span>Open orders</span><b className="num">{mine.length}{sim ? " / 50" : ""}</b></div></div>
            {!sim && (
              <div className="ctrl">
                <div className="ctrl-row">
                  <button className="btn sm" disabled={!st || st.strategy_running || v.halted || busy} onClick={() => control("start")}>Start</button>
                  <button className="btn sm" disabled={!st || !st.strategy_running || busy} onClick={() => control("stop")}>Stop</button>
                  <button className="btn sm" disabled={busy} onClick={() => control("reset")}>Reset</button>
                </div>
              </div>
            )}
            <button className="btn" disabled={sim ? !live : busy} onClick={() => kill(v.halted)}>{v.halted ? "Release kill switch" : "Engage kill switch"}</button>
            {!sim && msg && <div id="msg" role="status">{msg}</div>}
          </div>
        </aside>
      </main>
    </div>
  );
}
