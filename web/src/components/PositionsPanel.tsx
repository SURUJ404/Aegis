import { fmtPrice, fmtQty, fmtPnl } from "../api";
import type { Inventory, Position } from "../types";

export default function PositionsPanel({ positions, inventory }: { positions: Position[]; inventory: Inventory[] }) {
  return (
    <section className="card">
      <div className="card-title">Positions & inventory</div>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              <th>Venue</th>
              <th>Symbol</th>
              <th>Net qty</th>
              <th>Avg entry</th>
              <th>Realized PnL</th>
            </tr>
          </thead>
          <tbody>
            {positions.length === 0 && (
              <tr>
                <td colSpan={5} className="muted">no positions yet</td>
              </tr>
            )}
            {positions.map((p) => (
              <tr key={`${p.venue}:${p.symbol}`}>
                <td><span className="venue-badge">{p.venue}</span></td>
                <td>{p.symbol}</td>
                <td className={Number(p.net_qty) >= 0 ? "pos" : "neg"}>{fmtQty(p.net_qty)}</td>
                <td>{fmtPrice(p.avg_entry)}</td>
                <td className={Number(p.realized_pnl) >= 0 ? "pos" : "neg"}>{fmtPnl(p.realized_pnl)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="card-title sub">Inventory (aggregated across venues)</div>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              <th>Symbol</th>
              <th>Net qty</th>
              <th>Avg entry</th>
              <th>Realized PnL</th>
            </tr>
          </thead>
          <tbody>
            {inventory.length === 0 && (
              <tr>
                <td colSpan={4} className="muted">flat</td>
              </tr>
            )}
            {inventory.map((i) => (
              <tr key={i.symbol}>
                <td>{i.symbol}</td>
                <td className={Number(i.net_qty) >= 0 ? "pos" : "neg"}>{fmtQty(i.net_qty)}</td>
                <td>{fmtPrice(i.avg_entry)}</td>
                <td className={Number(i.realized_pnl) >= 0 ? "pos" : "neg"}>{fmtPnl(i.realized_pnl)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}