import type { Bar } from "../quantApi";
import { EM_DASH, fmtDate, fmtNum } from "../format";
import { cssVar, drawXAxis, drawYTicks, font, useCanvas } from "../useCanvas";

export interface Overlay {
  n: number;
  values: (number | null)[];
}

interface Props {
  bars: Bar[];
  overlays: Overlay[];
  label: string;
}

/** Daily candles plus optional moving-average overlays (MA strategy only). */
export default function CandleChart({ bars, overlays, label }: Props) {
  const ref = useCanvas((ctx, w, h) => {
    if (!bars.length) return;
    const left = 54;
    const right = 8;
    const top = 8;
    const bottom = 22;
    const plotW = Math.max(1, w - left - right);
    const plotH = Math.max(1, h - top - bottom);

    let min = Infinity;
    let max = -Infinity;
    for (const b of bars) {
      min = Math.min(min, b.l);
      max = Math.max(max, b.h);
    }
    for (const o of overlays) {
      for (const v of o.values) {
        if (v !== null && Number.isFinite(v)) {
          min = Math.min(min, v);
          max = Math.max(max, v);
        }
      }
    }
    if (!Number.isFinite(min) || !Number.isFinite(max)) return;
    const pad = (max - min) * 0.05 || Math.max(0.01, max * 0.01);
    min -= pad;
    max += pad;

    const y = (v: number) => top + plotH - ((v - min) / (max - min)) * plotH;
    drawYTicks(ctx, min, max, left, plotW, top, plotH, (v) => fmtNum(v));

    const slot = plotW / bars.length;
    const body = Math.max(1, slot * 0.62);
    const up = cssVar("--bid");
    const down = cssVar("--ask");
    for (let i = 0; i < bars.length; i++) {
      const b = bars[i];
      const x = left + i * slot + slot / 2;
      const rising = b.c >= b.o;
      ctx.strokeStyle = ctx.fillStyle = rising ? up : down;
      ctx.lineWidth = Math.max(1, Math.min(2, slot * 0.2));
      ctx.beginPath();
      ctx.moveTo(x, y(b.h));
      ctx.lineTo(x, y(b.l));
      ctx.stroke();
      const yo = y(b.o);
      const yc = y(b.c);
      ctx.fillRect(x - body / 2, Math.min(yo, yc), body, Math.max(1, Math.abs(yc - yo)));
    }

    overlays.forEach((o, idx) => {
      ctx.strokeStyle = idx === 0 ? cssVar("--ink") : cssVar("--mute");
      ctx.lineWidth = idx === 0 ? 1.5 : 1.5;
      ctx.setLineDash(idx === 0 ? [] : [4, 3]);
      ctx.beginPath();
      let started = false;
      o.values.forEach((v, i) => {
        if (v === null || !Number.isFinite(v)) return;
        const px = left + i * slot + slot / 2;
        const py = y(v);
        if (!started) {
          ctx.moveTo(px, py);
          started = true;
        } else ctx.lineTo(px, py);
      });
      ctx.stroke();
      ctx.setLineDash([]);
    });

    drawXAxis(
      ctx,
      [fmtDate(bars[0].t), fmtDate(bars[Math.floor(bars.length / 2)].t), fmtDate(bars[bars.length - 1].t)],
      left,
      plotW,
      h,
      bottom,
    );

    font(ctx, 10);
    ctx.fillStyle = cssVar("--mute");
    ctx.fillText(label, left + 2, top + 10);
  });

  if (!bars.length) return <div className="empty">No price bars for this ticker yet.</div>;
  const last = bars[bars.length - 1];
  const aria =
    `${label}: ${bars.length} daily candles, ${fmtDate(bars[0].t)} to ${fmtDate(last.t)}, ` +
    `last close ${fmtNum(last.c)}${overlays.length ? `, overlays ${overlays.map((o) => `MA ${o.n}`).join(" and ")}` : ""}.`;
  return <canvas ref={ref} className="chart" role="img" aria-label={aria} data-testid="candles" />;
}

export function overlayLabel(overlays: Overlay[]): string {
  if (!overlays.length) return EM_DASH;
  return overlays.map((o) => `MA ${o.n}`).join(" / ");
}
