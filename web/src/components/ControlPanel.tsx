interface Props {
  running: boolean;
  halted: boolean;
  busy: boolean;
  onStart: () => void;
  onStop: () => void;
  onReset: () => void;
  onKill: () => void;
}

export default function ControlPanel({ running, halted, busy, onStart, onStop, onReset, onKill }: Props) {
  return (
    <section className="control-panel">
      <button className="btn btn-ok" onClick={onStart} disabled={busy || running || halted}>
        Start
      </button>
      <button className="btn btn-warn" onClick={onStop} disabled={busy || !running}>
        Stop
      </button>
      <button className="btn" onClick={onReset} disabled={busy || !halted}>
        Reset kill switch
      </button>
      <button className="btn btn-danger" onClick={onKill} disabled={busy || halted}>
        Kill switch
      </button>
    </section>
  );
}