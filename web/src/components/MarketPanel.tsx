import { fmtPct, fmtPrice, fmtQty } from "../api";
import type { MarketState } from "../types";

export default function MarketPanel({ market }: { market: MarketState[] }) {
  if (market.length === 0) {
    return (
      <section className="card">
        <div className="card-title">Market state</div>
        <div className="muted">no market data yet</div>
      </section>
    );
  }

  return (
    <section className="card">
      <div className="card-title">Market state</div>
      <div className="market-grid">
        {market.map((m) => (
          <div className="market-card" key={`${m.venue}:${m.symbol}`}>
            <div className="market-head">
              <span className="venue-badge">{m.venue}</span>
              <span>{m.symbol}</span>
              <span className={`pill ${m.stale || m.regime === "stale" ? "pill-danger" : m.regime === "normal" ? "pill-ok" : "pill-warn"}`}>
                {m.regime}
              </span>
            </div>
            <div className="market-mid">
              <span className="muted">mid</span>
              <span className="mid">{fmtPrice(m.mid)}</span>
            </div>
            <div className="market-grid-2">
              <div><span className="muted">bid</span> {fmtPrice(m.best_bid)}</div>
              <div><span className="muted">ask</span> {fmtPrice(m.best_ask)}</div>
              <div><span className="muted">spread</span> {m.spread_bps.toFixed(1)} bps</div>
              <div><span className="muted">imbalance</span> {fmtPct(m.orderbook_imbalance)}</div>
              <div><span className="muted">depth bid</span> {fmtQty(m.depth_bid)}</div>
              <div><span className="muted">depth ask</span> {fmtQty(m.depth_ask)}</div>
              <div><span className="muted">buy vol</span> {fmtQty(m.buy_volume)}</div>
              <div><span className="muted">sell vol</span> {fmtQty(m.sell_volume)}</div>
              <div><span className="muted">vwap</span> {fmtPrice(m.vwap, 4)}</div>
              <div><span className="muted">intensity</span> {m.trade_intensity.toFixed(1)}</div>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}