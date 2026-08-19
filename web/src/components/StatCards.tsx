import { fmtPnl, fmtPrice, fmtQty } from "../api";
import type { StateSummary } from "../types";

export default function StatCards({ state, totalPnl }: { state: StateSummary; totalPnl: number }) {
  const openOrders = state.orders.filter((o) => !o.status.endsWith("filled") && !["cancelled", "rejected", "expired"].includes(o.status)).length;
  const grossExposure = state.inventory.reduce((acc, i) => acc + Math.abs(Number(i.net_qty)), 0);
  const quotable = state.market_state.filter((m) => m.regime !== "no_liquidity" && m.regime !== "stale" && !m.stale).length;

  return (
    <section className="stat-grid">
      <Stat label="Positions" value={String(state.positions.length)} />
      <Stat label="Net inventory" value={fmtQty(String(grossExposure))} />
      <Stat label="Open orders" value={String(openOrders)} sub={`${state.orders.length} total`} />
      <Stat label="Quotable markets" value={String(quotable)} sub={`${state.market_state.length} tracked`} />
      <Stat label="Realized PnL" value={fmtPnl(String(totalPnl))} positive={totalPnl >= 0} />
      <Stat label="Mark price (BTC-USDT)" value={markPrice(state)} />
    </section>
  );
}

function markPrice(state: StateSummary): string {
  const m = state.market_state.find((x) => x.symbol === "BTC-USDT");
  return m ? fmtPrice(m.mid) : "—";
}

function Stat({ label, value, sub, positive }: { label: string; value: string; sub?: string; positive?: boolean }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className={`stat-value ${positive === false ? "neg" : positive === true ? "pos" : ""}`}>{value}</div>
      {sub && <div className="stat-sub">{sub}</div>}
    </div>
  );
}