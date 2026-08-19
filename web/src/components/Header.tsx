import { fmtUptime } from "../api";

interface Props {
  connected: boolean;
  strategyRunning: boolean;
  halted: boolean;
  uptimeMs: number;
}

export default function Header({ connected, strategyRunning, halted, uptimeMs }: Props) {
  return (
    <header className="header">
      <div className="brand">
        <span className="logo">Æ</span>
        <div>
          <h1>Aegis</h1>
          <span className="subtitle">Multi-venue liquidity engine</span>
        </div>
      </div>
      <div className="header-right">
        <span className={`pill ${connected ? "pill-ok" : "pill-danger"}`}>
          {connected ? "api connected" : "api offline"}
        </span>
        <span className={`pill ${strategyRunning ? "pill-ok" : "pill-warn"}`}>
          {strategyRunning ? "strategy running" : "strategy stopped"}
        </span>
        {halted && <span className="pill pill-danger">KILL SWITCH</span>}
        <span className="muted">uptime {fmtUptime(uptimeMs)}</span>
      </div>
    </header>
  );
}