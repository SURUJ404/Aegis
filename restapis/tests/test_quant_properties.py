"""Phase 1 property tests: invariants that must hold for every possible input.

Bounded daily moves (+/-35%) make equity > 0 mathematically guaranteed:
worst daily factor = 1 + held*(-0.35) - turnover*0.02 >= 0.63 > 0 with cost_bps <= 200.
"""
import json
import math

import pandas as pd
from hypothesis import given, settings
from hypothesis import strategies as st

from app import quant

BOUND = 0.35


@st.composite
def closes(draw, min_size: int = 6, max_size: int = 100):
    n = draw(st.integers(min_value=min_size, max_value=max_size))
    start = draw(st.floats(min_value=1.0, max_value=500.0, allow_nan=False, allow_infinity=False))
    moves = draw(
        st.lists(
            st.floats(min_value=-BOUND, max_value=BOUND, allow_nan=False, allow_infinity=False),
            min_size=n - 1,
            max_size=n - 1,
        )
    )
    px = [start]
    for m in moves:
        px.append(px[-1] * (1.0 + m))
    return px


def _frame(strategy: str, a: int, b: int) -> tuple:
    if strategy == "ma" and a > b:
        a, b = b, a  # API only accepts a < b for ma
    return strategy, a, b


COMMON = {
    "prices": closes(),
    "strategy": st.sampled_from(["ma", "mom", "rsi"]),
    "a": st.integers(2, 40),
    "b": st.integers(2, 40),
}
WITH_COST = {
    **COMMON,
    "cost": st.floats(0.0, 200.0, allow_nan=False, allow_infinity=False),
}


@settings(max_examples=50, deadline=None)
@given(**WITH_COST)
def test_every_result_is_json_safe_and_well_formed(prices, strategy, a, b, cost):
    strategy, a, b = _frame(strategy, a, b)
    c = pd.Series(prices)
    r = quant.backtest(c, strategy, a, b, cost)
    assert len(r["equity"]) == len(r["buy_hold_equity"]) == len(r["position"]) == len(c)
    assert set(r["position"]) <= {0, 1}
    assert 0.0 <= r["exposure"] <= 1.0
    assert r["trades"] >= 0
    for name in ("equity", "buy_hold_equity"):
        assert all(math.isfinite(x) and x > 0 for x in r[name]), name
    for name in ("strategy", "buy_hold"):
        assert all(math.isfinite(v) for v in r[name].values()), name
    assert math.isfinite(r["cost_drag_cagr"])
    json.dumps(r, allow_nan=False)  # exactly how Starlette renders every response


@settings(max_examples=50, deadline=None)
@given(**COMMON)
def test_more_cost_never_increases_return(prices, strategy, a, b):
    strategy, a, b = _frame(strategy, a, b)
    c = pd.Series(prices)
    cheap = quant.backtest(c, strategy, a, b, 0.0)
    dear = quant.backtest(c, strategy, a, b, 200.0)
    tol = 1e-9
    assert all(paid <= free + tol for paid, free in zip(dear["equity"], cheap["equity"]))
    assert dear["strategy"]["cagr"] <= cheap["strategy"]["cagr"] + tol
    assert dear["cost_drag_cagr"] >= -tol


@settings(max_examples=50, deadline=None)
@given(
    prices=closes(min_size=10),
    strategy=st.sampled_from(["ma", "mom", "rsi"]),
    a=st.integers(2, 40),
    b=st.integers(2, 40),
    cost=st.floats(0.0, 200.0, allow_nan=False, allow_infinity=False),
    frac=st.floats(0.0, 1.0, allow_nan=False, allow_infinity=False),
)
def test_prices_after_day_t_never_change_the_past(prices, strategy, a, b, cost, frac):
    strategy, a, b = _frame(strategy, a, b)
    t = int(frac * (len(prices) - 1))
    future = list(prices)
    for i in range(t + 1, len(future)):
        future[i] = future[i] * 1.9 + 1.0
    past = quant.backtest(pd.Series(prices), strategy, a, b, cost)
    altered = quant.backtest(pd.Series(future), strategy, a, b, cost)
    tol = 1e-9
    assert all(
        abs(x - y) <= tol
        for x, y in zip(past["equity"][: t + 1], altered["equity"][: t + 1])
    )
    assert past["position"][: t + 1] == altered["position"][: t + 1]


@settings(max_examples=30, deadline=None)
@given(**WITH_COST)
def test_backtest_is_deterministic(prices, strategy, a, b, cost):
    strategy, a, b = _frame(strategy, a, b)
    one = quant.backtest(pd.Series(prices), strategy, a, b, cost)
    two = quant.backtest(pd.Series(list(prices)), strategy, a, b, cost)
    assert json.dumps(one, sort_keys=True) == json.dumps(two, sort_keys=True)
