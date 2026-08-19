import { useEffect, useRef, useState } from "react";
import { killSwitch, postControl, startPolling } from "./api";
import type { StateSummary } from "./types";
import Header from "./components/Header";
import ControlPanel from "./components/ControlPanel";
import StatCards from "./components/StatCards";
import MarketPanel from "./components/MarketPanel";
import PositionsPanel from "./components/PositionsPanel";
import OrdersPanel from "./components/OrdersPanel";
import { PnlSparkline } from "./components/PnlSparkline";

export default function App() {
  const [state, setState] = useState<StateSummary | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const pnlHistory = useRef<number[]>([]);

  useEffect(() => {
    return startPolling(
      (s) => {
        setState(s);
        setConnected(true);
        setError(null);
        const pnl = s.inventory.reduce((acc, i) => acc + Number(i.realized_pnl), 0);
        const last = pnlHistory.current[pnlHistory.current.length - 1];
        if (last === undefined || pnl !== last) {
          pnlHistory.current = [...pnlHistory.current.slice(-119), pnl];
        }
      },
      (e) => {
        setConnected(false);
        setError(e);
      },
    );
  }, []);

  const act = async (action: () => Promise<{ accepted: boolean; message: string }>, label: string) => {
    setBusy(true);
    try {
      const r = await action();
      setToast(`${label}: ${r.message}`);
    } catch (e) {
      setToast(`${label} failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
      setTimeout(() => setToast(null), 4000);
    }
  };

  const totalPnl = state?.inventory.reduce((acc, i) => acc + Number(i.realized_pnl), 0) ?? 0;

  return (
    <div className="app">
      <Header connected={connected} strategyRunning={state?.strategy_running ?? false} halted={state?.risk.halted ?? false} uptimeMs={state?.uptime_ms ?? 0} />
      {error && !state && <div className="error-banner">API unreachable — {error}</div>}
      {toast && <div className="toast">{toast}</div>}

      <ControlPanel
        running={state?.strategy_running ?? false}
        halted={state?.risk.halted ?? false}
        busy={busy}
        onStart={() => act(() => postControl("start"), "start")}
        onStop={() => act(() => postControl("stop"), "stop")}
        onReset={() => act(() => postControl("reset"), "reset")}
        onKill={async () => {
          const reason = window.prompt("Kill switch reason:");
          if (reason === null) return;
          await act(() => killSwitch(reason), "kill");
        }}
      />

      {state && (
        <>
          <StatCards state={state} totalPnl={totalPnl} />
          <div className="card-row">
            <div className="card">
              <div className="card-title">Realized PnL (last 2 min)</div>
              <PnlSparkline data={pnlHistory.current} />
            </div>
            <div className="card">
              <div className="card-title">Risk status</div>
              <div className="risk-box">
                <div>
                  <span className="muted">Armed</span>{" "}
                  <span className={`pill ${state.risk.armed ? "pill-ok" : "pill-warn"}`}>
                    {state.risk.armed ? "armed" : "disarmed"}
                  </span>
                </div>
                <div>
                  <span className="muted">Halted</span>{" "}
                  <span className={`pill ${state.risk.halted ? "pill-danger" : "pill-ok"}`}>
                    {state.risk.halted ? "halted" : "normal"}
                  </span>
                </div>
                {state.risk.halt_reason && <div className="muted">Reason: {state.risk.halt_reason}</div>}
              </div>
            </div>
          </div>
          <MarketPanel market={state.market_state} />
          <PositionsPanel positions={state.positions} inventory={state.inventory} />
          <OrdersPanel orders={state.orders} />
        </>
      )}
    </div>
  );
}