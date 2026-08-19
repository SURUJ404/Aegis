import { fmtPrice, fmtQty, shortId, timeAgo } from "../api";
import type { Order } from "../types";

const TERMINAL = new Set(["filled", "cancelled", "rejected", "expired"]);

export default function OrdersPanel({ orders }: { orders: Order[] }) {
  const sorted = [...orders].sort((a, b) => b.created_at - a.created_at);

  return (
    <section className="card">
      <div className="card-title">Orders ({orders.length})</div>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>Venue</th>
              <th>Symbol</th>
              <th>Side</th>
              <th>Type</th>
              <th>Price</th>
              <th>Qty</th>
              <th>Filled</th>
              <th>Avg fill</th>
              <th>Status</th>
              <th>Age</th>
            </tr>
          </thead>
          <tbody>
            {orders.length === 0 && (
              <tr>
                <td colSpan={11} className="muted">no orders yet</td>
              </tr>
            )}
            {sorted.map((o) => (
              <tr key={o.order_id}>
                <td className="mono">{shortId(o.order_id)}</td>
                <td><span className="venue-badge">{o.venue}</span></td>
                <td>{o.symbol}</td>
                <td className={o.side === "bid" ? "pos" : "neg"}>{o.side}</td>
                <td>{o.order_type}</td>
                <td>{fmtPrice(o.price)}</td>
                <td>{fmtQty(o.quantity)}</td>
                <td>{fmtQty(o.filled_quantity)}</td>
                <td>{fmtPrice(o.avg_fill_price)}</td>
                <td>
                  <span className={`pill ${TERMINAL.has(o.status) ? "pill-dim" : o.status === "filled" ? "pill-ok" : "pill-warn"}`}>
                    {o.status}
                  </span>
                </td>
                <td className="muted">{timeAgo(o.created_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}