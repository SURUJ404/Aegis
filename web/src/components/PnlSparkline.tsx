export function PnlSparkline({ data }: { data: number[] }) {
  if (data.length < 2) {
    return <div className="muted">collecting…</div>;
  }
  const w = 560;
  const h = 90;
  const min = Math.min(...data);
  const max = Math.max(...data);
  const range = max - min || 1;
  const step = w / (data.length - 1);
  const pts = data.map((v, i) => `${(i * step).toFixed(1)},${(h - ((v - min) / range) * (h - 8) - 4).toFixed(1)}`);
  const last = data[data.length - 1];
  const color = last >= 0 ? "#2ecc71" : "#e74c3c";

  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="sparkline" preserveAspectRatio="none" aria-hidden>
      <line x1="0" y1={h / 2} x2={w} y2={h / 2} stroke="rgba(255,255,255,0.12)" strokeWidth="1" />
      <polyline points={pts.join(" ")} fill="none" stroke={color} strokeWidth="2" />
      <circle cx={w} cy={Number(pts[pts.length - 1].split(",")[1])} r="3" fill={color} />
    </svg>
  );
}