import type { BacktestResult } from "../quantApi";
import { EM_DASH, fmtInt, fmtNum, fmtPct } from "../format";

interface Props {
  result: BacktestResult;
  costBps: number;
}

interface Row {
  key: string;
  label: string;
  strategy: string;
  buyHold: string;
}

/** Strategy vs buy & hold, the numbers the original design put in one table. */
export default function Metrics({ result, costBps }: Props) {
  const inMarket = result.position.filter((p) => p > 0.5).length;
  const rows: Row[] = [
    {
      key: "annual-return",
      label: "Annual return",
      strategy: fmtPct(result.strategy.cagr),
      buyHold: fmtPct(result.buy_hold.cagr),
    },
    {
      key: "sharpe",
      label: "Sharpe",
      strategy: fmtNum(result.strategy.sharpe),
      buyHold: fmtNum(result.buy_hold.sharpe),
    },
    {
      key: "worst-drawdown",
      label: "Worst drawdown",
      strategy: fmtPct(result.strategy.max_drawdown),
      buyHold: fmtPct(result.buy_hold.max_drawdown),
    },
    { key: "trades", label: "Trades", strategy: fmtInt(result.trades), buyHold: EM_DASH },
    {
      key: "time-in-market",
      label: "Time in market",
      strategy: fmtPct(result.exposure, 2, false),
      buyHold: EM_DASH,
    },
    {
      key: "in-market-days",
      label: "In market days",
      strategy: `${fmtInt(inMarket)} / ${fmtInt(result.position.length)}`,
      buyHold: EM_DASH,
    },
    { key: "bars", label: "Bars", strategy: fmtInt(result.dates.length), buyHold: EM_DASH },
  ];

  const points = result.cost_drag_cagr * 100;

  return (
    <div className="metrics-wrap">
      <table className="metrics" data-testid="metrics">
        <caption className="sr-only">
          Backtest results for {result.symbol}: strategy versus buy and hold
        </caption>
        <thead>
          <tr>
            <th scope="col">Metric</th>
            <th scope="col">Strategy</th>
            <th scope="col">Buy &amp; hold</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.key}>
              <th scope="row">{r.label}</th>
              <td className="num" data-metric={`${r.key}-strategy`}>
                {r.strategy}
              </td>
              <td className="num" data-metric={`${r.key}-buyhold`}>
                {r.buyHold}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="cost-line" data-testid="cost-message" data-points={points.toFixed(4)}>
        Fees cost {fmtNum(points)} points of annual return (cost {fmtNum(costBps, 1)} bps).
      </p>
    </div>
  );
}
