import { EM_DASH, fmtDate, fmtNum } from "../format";
import { cssVar, drawXAxis, drawYTicks, useCanvas } from "../useCanvas";

interface Props {
  dates: string[];
  equity: number[];
  buyHold: number[];
  position: number[];
  label: string;
}

/** Growth of 1 with the days the strategy was in the market shaded behind it. */
export default function EquityChart({ dates, equity, buyHold, position, label }: Props) {
  const ref = useCanvas((ctx, w, h) => {
    if (equity.length < 2) return;
    const left = 54;
    const right = 8;
    const top = 8;
    const bottom = 22;
    const plotW = Math.max(1, w - left - right);
    const plotH = Math.max(1, h - top - bottom);
    const n = equity.length;

    let min = Infinity;
    let max = -Infinity;
    for (const v of equity) {
      min = Math.min(min, v);
      max = Math.max(max, v);
    }
    for (const v of buyHold) {
      min = Math.min(min, v);
      max = Math.max(max, v);
    }
    if (!Number.isFinite(min) || !Number.isFinite(max)) return;
    const pad = (max - min) * 0.05 || 0.01;
    min -= pad;
    max += pad;

    const x = (i: number) => left + (i / (n - 1)) * plotW;
    const y = (v: number) => top + plotH - ((v - min) / (max - min)) * plotH;

    // In-market shading first so both lines sit on top of it.
    ctx.fillStyle = cssVar("--bidbg");
    let start = -1;
    for (let i = 0; i <= n; i++) {
      const inMarket = i < n && position[i] > 0.5;
      if (inMarket && start < 0) start = i;
      if (!inMarket && start >= 0) {
        ctx.fillRect(x(start), top, Math.max(1, x(i - 1) - x(start) + plotW / (n - 1)), plotH);
        start = -1;
      }
    }

    drawYTicks(ctx, min, max, left, plotW, top, plotH, (v) => fmtNum(v));

    const line = (series: number[], color: string, dash: number[]) => {
      ctx.strokeStyle = color;
      ctx.lineWidth = 1.75;
      ctx.setLineDash(dash);
      ctx.beginPath();
      series.forEach((v, i) => (i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
      ctx.stroke();
      ctx.setLineDash([]);
    };
    line(equity, cssVar("--bid"), []);
    line(buyHold, cssVar("--ask"), [5, 4]);

    drawXAxis(
      ctx,
      [fmtDate(dates[0] ?? EM_DASH), fmtDate(dates[Math.floor(n / 2)] ?? EM_DASH), fmtDate(dates[n - 1] ?? EM_DASH)],
      left,
      plotW,
      h,
      bottom,
    );
  });

  if (equity.length < 2) return <div className="empty">No equity curve yet.</div>;
  const aria =
    `${label}: equity from ${fmtNum(equity[0])} to ${fmtNum(equity[equity.length - 1])}, ` +
    `buy and hold ${fmtNum(buyHold[0])} to ${fmtNum(buyHold[buyHold.length - 1])}, ` +
    `shaded bands mark days in the market.`;
  return <canvas ref={ref} className="chart tall" role="img" aria-label={aria} data-testid="equity" />;
}
