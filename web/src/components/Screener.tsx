import type { ScreenRow } from "../quantApi";
import { EM_DASH, fmtNum, fmtPct, fmtText } from "../format";

export type SortKey = "symbol" | "price" | "ret_1m" | "rsi14" | "vs_sma50" | "signal";
export type SortDir = "asc" | "desc";

export const COLUMNS: { key: SortKey; label: string; kind: "text" | "num" | "pct" }[] = [
  { key: "symbol", label: "Symbol", kind: "text" },
  { key: "price", label: "Price", kind: "num" },
  { key: "ret_1m", label: "1M return", kind: "pct" },
  { key: "rsi14", label: "RSI 14", kind: "num" },
  // The API returns the 50-day average close itself (a price level), not a
  // distance from it, so it is rendered as a number and never as a percent.
  { key: "vs_sma50", label: "SMA 50", kind: "num" },
  { key: "signal", label: "Signal", kind: "text" },
];

interface Props {
  rows: ScreenRow[];
  sort: { key: SortKey; dir: SortDir };
  onSort: (key: SortKey) => void;
  selected: string | null;
  onSelect: (symbol: string) => void;
}

function cell(row: ScreenRow, col: (typeof COLUMNS)[number]): string {
  const v = row[col.key];
  if (col.kind === "pct") return fmtPct(v);
  if (col.kind === "num") return fmtNum(v);
  return fmtText(v, 16);
}

/** The screener: every column sortable both ways, rows clickable into a backtest. */
export default function Screener({ rows, sort, onSort, selected, onSelect }: Props) {
  if (!rows.length) return <div className="empty">No screener rows yet.</div>;
  return (
    <div className="table-scroll">
      <table className="screen" data-testid="screener">
        <thead>
          <tr>
            {COLUMNS.map((c) => (
              <th
                key={c.key}
                scope="col"
                aria-sort={sort.key === c.key ? (sort.dir === "asc" ? "ascending" : "descending") : "none"}
              >
                <button
                  type="button"
                  className="sort"
                  data-col={c.key}
                  onClick={() => onSort(c.key)}
                  aria-label={`Sort by ${c.label}${sort.key === c.key ? (sort.dir === "asc" ? ", currently ascending" : ", currently descending") : ""}`}
                >
                  {c.label}
                  <span className="arrow" aria-hidden="true">
                    {sort.key === c.key ? (sort.dir === "asc" ? "▲" : "▼") : "↕"}
                  </span>
                </button>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => {
            const isSel = selected === r.symbol;
            return (
              <tr
                key={r.symbol}
                className={isSel ? "sel" : undefined}
                aria-selected={isSel}
                data-symbol={r.symbol}
                tabIndex={0}
                onClick={() => onSelect(r.symbol)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    onSelect(r.symbol);
                  }
                }}
              >
                {COLUMNS.map((c) => (
                  <td key={c.key} className={c.kind === "text" ? undefined : "num"} data-col={c.key}>
                    {c.key === "signal" ? (
                      <span className={`pill sig-${fmtText(r.signal, 8).toLowerCase()}`}>{cell(r, c)}</span>
                    ) : (
                      cell(r, c)
                    )}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="hint">
        {EM_DASH} missing · click a row to load it into the backtest
      </p>
    </div>
  );
}
