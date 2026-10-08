"""Pure strategy and backtest logic. No web or UI imports, so it can be reused anywhere.
Rule: a signal computed from the close on day t is only held from day t+1 (no lookahead)."""
import numpy as np
import pandas as pd


def sma(c: pd.Series, n: int) -> pd.Series:
    return c.rolling(n).mean()


def rsi(c: pd.Series, n: int = 14) -> pd.Series:
    d = c.diff()
    ag = d.clip(lower=0).ewm(alpha=1 / n, adjust=False, min_periods=n).mean()
    al = (-d).clip(lower=0).ewm(alpha=1 / n, adjust=False, min_periods=n).mean()
    out = 100 - 100 / (1 + ag / al.replace(0, np.nan))
    out = out.mask(al.eq(0) & ag.gt(0), 100.0)  # only gains in the window -> RSI 100
    out = out.mask(al.eq(0) & ag.eq(0), 50.0)  # price never moved -> neutral 50
    return out


def signals(c: pd.Series, strategy: str, a: int, b: int) -> pd.Series:
    """Desired position (0 or 1) decided at each day's close."""
    if strategy == "ma":
        f, s = sma(c, a), sma(c, b)
        return ((f > s) & f.notna() & s.notna()).astype(float)
    if strategy == "mom":
        return (c > c.shift(a)).astype(float)
    if strategy == "rsi":
        out, held = [], 0.0
        for v in rsi(c, a).to_numpy():
            if not np.isnan(v):
                if not held and v < b:
                    held = 1.0
                elif held and v > 55:
                    held = 0.0
            out.append(held)
        return pd.Series(out, index=c.index)
    raise ValueError(f"unknown strategy {strategy}")


def _stats(r: pd.Series) -> dict:
    eq = (1 + r).cumprod()
    sd = r.std(ddof=0) or 1e-9
    return {
        "cagr": float(eq.iloc[-1] ** (252 / len(r)) - 1),
        "sharpe": float(r.mean() / sd * np.sqrt(252)),
        "max_drawdown": float((eq / eq.cummax() - 1).min()),
    }


def run(c: pd.Series, pos: pd.Series, bps: float) -> dict:
    if len(c) < 2:
        raise ValueError(f"need at least 2 bars, got {len(c)}")
    ret = c.pct_change()
    ret = ret.where(np.isfinite(ret), 0.0)  # a missing/broken close counts as a 0% move
    held = pos.shift(1).fillna(0.0)
    turnover = held.diff().abs().fillna(held.abs())
    strat = (held * ret - turnover * bps / 1e4).iloc[1:]
    bh = ret.iloc[1:]
    return {
        "strategy": _stats(strat), "buy_hold": _stats(bh),
        "trades": int((turnover.iloc[1:] > 0).sum()), "exposure": float(pos.mean()),
        "equity": [1.0] + (1 + strat).cumprod().tolist(),
        "buy_hold_equity": [1.0] + (1 + bh).cumprod().tolist(),
        "position": pos.astype(int).tolist(),
    }


def backtest(c: pd.Series, strategy: str, a: int, b: int, bps: float) -> dict:
    pos = signals(c, strategy, a, b)
    res, free = run(c, pos, bps), run(c, pos, 0.0)
    res["cost_drag_cagr"] = free["strategy"]["cagr"] - res["strategy"]["cagr"]
    return res
