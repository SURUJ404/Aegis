import { useEffect, useRef, type RefObject } from "react";

/** Read a design token from the root so charts follow the active theme. */
export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
}

type Painter = (ctx: CanvasRenderingContext2D, w: number, h: number) => void;

/**
 * Canvas plumbing shared by every chart: device-pixel-ratio aware sizing, a
 * repaint after each render (theme, data) and a ResizeObserver so the chart
 * follows its container (sidebar collapses, window resizes, tab switches).
 */
export function useCanvas(paint: Painter): RefObject<HTMLCanvasElement> {
  const ref = useRef<HTMLCanvasElement>(null);
  const paintRef = useRef(paint);
  paintRef.current = paint;

  const drawNow = () => {
    const c = ref.current;
    if (!c) return;
    const rect = c.getBoundingClientRect();
    const w = Math.max(1, Math.round(rect.width));
    const h = Math.max(1, Math.round(rect.height));
    const dpr = window.devicePixelRatio || 1;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    paintRef.current(ctx, w, h);
  };

  const drawRef = useRef(drawNow);
  drawRef.current = drawNow;

  useEffect(() => {
    drawRef.current();
    const ro = new ResizeObserver(() => drawRef.current());
    if (ref.current) ro.observe(ref.current);
    window.addEventListener("resize", drawRef.current);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", drawRef.current);
    };
  });

  return ref;
}

export function font(ctx: CanvasRenderingContext2D, size: number): void {
  ctx.font = `${size}px "Geist Mono", ui-monospace, Menlo, monospace`;
}

/** Axis labels: first / middle / last date under a chart. */
export function drawXAxis(
  ctx: CanvasRenderingContext2D,
  labels: string[],
  left: number,
  width: number,
  height: number,
  padBottom: number,
): void {
  if (!labels.length || width <= 0) return;
  font(ctx, 10);
  ctx.fillStyle = cssVar("--mute");
  ctx.textAlign = "left";
  ctx.fillText(labels[0], left, height - padBottom + 12);
  ctx.textAlign = "center";
  if (labels.length > 1) ctx.fillText(labels[1], left + width / 2, height - padBottom + 12);
  ctx.textAlign = "right";
  if (labels.length > 2) ctx.fillText(labels[2], left + width, height - padBottom + 12);
  ctx.textAlign = "left";
}

export function drawYTicks(
  ctx: CanvasRenderingContext2D,
  min: number,
  max: number,
  left: number,
  width: number,
  top: number,
  plotH: number,
  format: (v: number) => string,
  grid = true,
): void {
  font(ctx, 10);
  ctx.fillStyle = cssVar("--mute");
  for (let i = 0; i < 3; i++) {
    const v = min + ((max - min) * i) / 2;
    const y = top + plotH - (plotH * i) / 2;
    if (grid) {
      ctx.strokeStyle = cssVar("--line");
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(left, Math.round(y) + 0.5);
      ctx.lineTo(left + width, Math.round(y) + 0.5);
      ctx.stroke();
    }
    ctx.fillText(format(v), 4, Math.min(top + plotH, Math.max(top + 8, y + 3)));
  }
}
