import { useEffect, useState, type RefObject } from "react";
import type { Strategy } from "../quantApi";
import { EM_DASH, fmtNum } from "../format";

export interface SliderSpec {
  label: string;
  min: number;
  max: number;
  step: number;
}

/** Labels and ranges are strategy specific — the design requires it. */
export const SLIDERS: Record<Strategy, { a: SliderSpec; b: SliderSpec | null }> = {
  ma: {
    a: { label: "Fast MA (days)", min: 2, max: 249, step: 1 },
    b: { label: "Slow MA (days)", min: 3, max: 250, step: 1 },
  },
  mom: {
    a: { label: "Lookback (days)", min: 2, max: 250, step: 1 },
    b: null,
  },
  rsi: {
    a: { label: "RSI period (days)", min: 2, max: 100, step: 1 },
    b: { label: "Buy below RSI", min: 5, max: 95, step: 1 },
  },
};

export const STRATEGY_LABEL: Record<Strategy, string> = {
  ma: "Moving average crossover",
  mom: "Momentum",
  rsi: "RSI mean reversion",
};

interface Props {
  symbol: string;
  symbols: string[];
  onSymbol: (symbol: string) => void;
  strategy: Strategy;
  onStrategy: (s: Strategy) => void;
  a: number;
  b: number;
  onA: (v: number) => void;
  onB: (v: number) => void;
  cost: number;
  onCost: (v: number) => void;
  inputRef?: RefObject<HTMLInputElement>;
}

function Slider({
  spec,
  value,
  onChange,
  testid,
}: {
  spec: SliderSpec;
  value: number;
  onChange: (v: number) => void;
  testid: string;
}) {
  return (
    <label className="slider" data-testid={testid}>
      <span>
        {spec.label} <b className="num">{value}</b>
      </span>
      <input
        type="range"
        min={spec.min}
        max={spec.max}
        step={spec.step}
        value={value}
        aria-label={spec.label}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <span className="range num">
        {spec.min} … {spec.max}
      </span>
    </label>
  );
}

/** Ticker box, strategy dropdown, per-strategy sliders and the cost slider. */
export default function Controls({
  symbol,
  symbols,
  onSymbol,
  strategy,
  onStrategy,
  a,
  b,
  onA,
  onB,
  cost,
  onCost,
  inputRef,
}: Props) {
  const [draft, setDraft] = useState(symbol);
  useEffect(() => setDraft(symbol), [symbol]);
  const specs = SLIDERS[strategy];

  const commit = () => {
    const next = draft.trim().toUpperCase();
    if (next) onSymbol(next);
    else setDraft(symbol);
  };

  return (
    <form className="panel form" onSubmit={(e) => e.preventDefault()} aria-label="Backtest controls">
      <label className="ticker" data-testid="ticker-box">
        <span>Ticker — press Enter to load</span>
        <input
          className="num"
          list="ticker-list"
          ref={inputRef}
          value={draft}
          placeholder="AAPL"
          autoComplete="off"
          spellCheck={false}
          aria-label="Ticker symbol"
          data-testid="ticker"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commit();
            }
          }}
        />
      </label>
      <datalist id="ticker-list">
        {symbols.map((s) => (
          <option key={s} value={s} />
        ))}
      </datalist>

      <label>
        Strategy
        <select
          value={strategy}
          aria-label="Strategy"
          data-testid="strategy"
          onChange={(e) => onStrategy(e.target.value as Strategy)}
        >
          <option value="ma">ma — moving average</option>
          <option value="mom">mom — momentum</option>
          <option value="rsi">rsi — mean reversion</option>
        </select>
      </label>
      <p className="hint">{STRATEGY_LABEL[strategy]}</p>

      <Slider spec={specs.a} value={a} onChange={onA} testid="slider-a" />
      {specs.b ? (
        <Slider spec={specs.b} value={b} onChange={onB} testid="slider-b" />
      ) : (
        <p className="hint" data-testid="slider-b-hidden">
          Momentum uses one lookback window — the second slider does not apply.
        </p>
      )}

      <label className="slider" data-testid="cost-slider">
        <span>
          Trading cost <b className="num">{fmtNum(cost, 1)} bps</b>
        </span>
        <input
          type="range"
          min={0}
          max={200}
          step={1}
          value={cost}
          aria-label="Trading cost in basis points"
          onChange={(e) => onCost(Number(e.target.value))}
        />
        <span className="range num">0 … 200</span>
      </label>
      <p className="hint">
        Active ticker <b className="num">{symbol || EM_DASH}</b> · press <kbd>/</kbd> to jump to the
        ticker box.
      </p>
    </form>
  );
}
