"""Independent plain-Python reference for app.quant on a small fixed series.

Loops reimplement sma / momentum / rsi / backtest from scratch (no pandas math)
and compare every output field against app.quant.
"""
import math
import random

import pandas as pd

from app import quant


def _series(n: int = 40, seed: int = 7) -> list:
    rnd = random.Random(seed)
    px = [100.0]
    for _ in range(n - 1):
        px.append(px[-1] * (1.0 + rnd.uniform(-0.05, 0.05)))
    return px


def ref_sma(vals: list, n: int, t: int):
    if t + 1 < n:
        return None
    return sum(vals[t - n + 1 : t + 1]) / n


def ref_rsi(vals: list, n: int) -> list:
    """Wilder ewm (adjust=False, min_periods=n); None = warmup window."""
    alpha = 1.0 / n
    ag = al = None
    seen = 0
    out: list[float | None] = []
    for t in range(len(vals)):
        if t == 0:
            out.append(None)
            continue
        d = vals[t] - vals[t - 1]
        gain = d if d > 0 else 0.0
        loss = -d if d < 0 else 0.0
        ag = gain if ag is None else (1.0 - alpha) * ag + alpha * gain
        al = loss if al is None else (1.0 - alpha) * al + alpha * loss
        seen += 1
        if seen < n:
            out.append(None)
        elif al == 0.0:
            out.append(100.0 if ag > 0 else 50.0)
        else:
            out.append(100.0 - 100.0 / (1.0 + ag / al))
    return out


def ref_signals(vals: list, strategy: str, a: int, b: int) -> list:
    n = len(vals)
    if strategy == "ma":
        out = []
        for t in range(n):
            f, s = ref_sma(vals, a, t), ref_sma(vals, b, t)
            out.append(1 if f is not None and s is not None and f > s else 0)
        return out
    if strategy == "mom":
        return [1 if t >= a and vals[t] > vals[t - a] else 0 for t in range(n)]
    if strategy == "rsi":
        held = 0
        out = []
        for v in ref_rsi(vals, a):
            if v is not None and not held and v < b:
                held = 1
            elif v is not None and held and v > 55:
                held = 0
            out.append(held)
        return out
    raise ValueError(strategy)


def ref_stats(returns: list) -> dict:
    eq = 1.0
    for x in returns:
        eq *= 1.0 + x
    n = len(returns)
    mean = sum(returns) / n
    sd = math.sqrt(sum((x - mean) ** 2 for x in returns) / n) or 1e-9
    peak, maxdd, e = -math.inf, 0.0, 1.0
    for x in returns:
        e *= 1.0 + x
        peak = max(peak, e)
        maxdd = min(maxdd, e / peak - 1.0)
    return {
        "cagr": eq ** (252.0 / n) - 1.0,
        "sharpe": mean / sd * math.sqrt(252.0),
        "max_drawdown": maxdd,
    }


def ref_backtest(vals: list, strategy: str, a: int, b: int, cost_bps: float) -> dict:
    pos = ref_signals(vals, strategy, a, b)
    n = len(vals)
    held = [0.0] + [float(p) for p in pos[:-1]]
    turnover = [abs(held[0])] + [abs(held[t] - held[t - 1]) for t in range(1, n)]
    rets = [0.0] + [vals[t] / vals[t - 1] - 1.0 for t in range(1, n)]
    strat_ret = [0.0] + [
        held[t] * rets[t] - turnover[t] * cost_bps / 1e4 for t in range(1, n)
    ]
    equity, buy_hold = [1.0], [1.0]
    for t in range(1, n):
        equity.append(equity[-1] * (1.0 + strat_ret[t]))
        buy_hold.append(buy_hold[-1] * (1.0 + rets[t]))
    return {
        "position": pos,
        "equity": equity,
        "buy_hold_equity": buy_hold,
        "trades": sum(1 for t in range(1, n) if turnover[t] > 0.0),
        "exposure": sum(pos) / n,
        "strategy": ref_stats(strat_ret[1:]),
        "buy_hold": ref_stats(rets[1:]),
    }


def _no_ties(vals: list, strategy: str, a: int, b: int) -> None:
    """Guard: the fixed series must sit away from every decision threshold,
    so a 1-ulp sum-order difference cannot legitimately flip a position."""
    n = len(vals)
    if strategy == "ma":
        for t in range(n):
            f, s = ref_sma(vals, a, t), ref_sma(vals, b, t)
            if f is not None and s is not None:
                assert abs(f - s) > 1e-9, f"ma tie at t={t}"
    elif strategy == "rsi":
        for v in ref_rsi(vals, a):
            if v is not None:
                assert min(abs(v - b), abs(v - 55.0)) > 1e-6, f"rsi tie: {v}"


def _check(vals: list, strategy: str, a: int, b: int, cost: float) -> None:
    _no_ties(vals, strategy, a, b)
    paid = ref_backtest(vals, strategy, a, b, cost)
    free = ref_backtest(vals, strategy, a, b, 0.0)
    got = quant.backtest(pd.Series(vals), strategy, a, b, cost)
    tol = 1e-9
    assert got["position"] == paid["position"]
    assert got["trades"] == paid["trades"]
    assert abs(got["exposure"] - paid["exposure"]) <= tol
    assert len(got["equity"]) == len(paid["equity"])
    for i, (x, y) in enumerate(zip(got["equity"], paid["equity"])):
        assert abs(x - y) <= tol, f"equity[{i}]: {x} != {y}"
    for i, (x, y) in enumerate(zip(got["buy_hold_equity"], paid["buy_hold_equity"])):
        assert abs(x - y) <= tol, f"buy_hold_equity[{i}]: {x} != {y}"
    for k in ("cagr", "sharpe", "max_drawdown"):
        assert abs(got["strategy"][k] - paid["strategy"][k]) <= tol, f"strategy.{k}"
        assert abs(got["buy_hold"][k] - paid["buy_hold"][k]) <= tol, f"buy_hold.{k}"
    drag = free["strategy"]["cagr"] - paid["strategy"]["cagr"]
    assert abs(got["cost_drag_cagr"] - drag) <= tol, "cost_drag_cagr"


def test_reference_ma_matches_quant():
    _check(_series(), "ma", 5, 15, 10)


def test_reference_mom_matches_quant():
    _check(_series(), "mom", 5, 0, 10)


def test_reference_rsi_matches_quant():
    _check(_series(), "rsi", 14, 30, 10)


def test_reference_zero_cost_matches_quant():
    _check(_series(), "ma", 5, 15, 0)
