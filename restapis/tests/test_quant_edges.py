"""Phase 1 edge cases: crashes, missing values, degenerate series, decision thresholds.

Every case must give a clean result (finite, JSON-safe) or a clear error (ValueError).
"""
import json
import math

import pandas as pd
import pytest

from app import data, quant


def test_one_bar_input_is_a_clear_error():
    with pytest.raises(ValueError, match="at least 2"):
        quant.backtest(pd.Series([100.0]), "ma", 20, 60, 10)


def test_empty_input_is_a_clear_error():
    with pytest.raises(ValueError, match="at least 2"):
        quant.backtest(pd.Series(dtype=float), "mom", 5, 0, 10)


def test_two_bars_is_clean_and_json_safe():
    c = pd.Series([100.0, 103.0])
    r = quant.backtest(c, "ma", 2, 20, 10)
    assert len(r["equity"]) == len(r["position"]) == 2
    assert all(math.isfinite(x) and x > 0 for x in r["equity"])
    json.dumps(r, allow_nan=False)


def test_series_shorter_than_windows_is_flat_and_clean():
    c = pd.Series([100.0 + i for i in range(10)])
    r = quant.backtest(c, "ma", 20, 60, 10)
    assert r["position"] == [0] * 10
    assert all(math.isfinite(x) and x > 0 for x in r["equity"])
    json.dumps(r, allow_nan=False)


def test_constant_price_is_clean_for_every_strategy():
    c = pd.Series([100.0] * 60)
    for s, a, b in [("ma", 5, 20), ("mom", 5, 0), ("rsi", 14, 30)]:
        r = quant.backtest(c, s, a, b, 10)
        assert r["position"] == [0] * 60, s
        assert r["equity"] == [1.0] * 60, s
        assert r["trades"] == 0 and r["exposure"] == 0.0, s
        json.dumps(r, allow_nan=False)


def test_missing_close_never_produces_nan_json():
    px = [100.0 * (1.001**i) for i in range(60)]
    px[30] = float("nan")
    r = quant.backtest(pd.Series(px), "ma", 5, 15, 10)
    json.dumps(r, allow_nan=False)  # Starlette rejects NaN while rendering
    assert all(math.isfinite(x) and x > 0 for x in r["equity"])
    assert all(math.isfinite(x) for x in r["buy_hold_equity"])
    assert all(math.isfinite(v) for v in r["strategy"].values())
    assert set(r["position"]) <= {0, 1}


def test_entirely_missing_series_is_flat_and_clean():
    r = quant.backtest(pd.Series([float("nan")] * 40), "mom", 5, 0, 10)
    json.dumps(r, allow_nan=False)
    assert r["position"] == [0] * 40
    assert r["equity"] == [1.0] * 40


def test_reverse_ma_window_is_clean():
    c = pd.Series([100.0 * (1.002**i) for i in range(80)])
    r = quant.backtest(c, "ma", 60, 20, 10)
    json.dumps(r, allow_nan=False)
    assert len(r["position"]) == 80
    assert set(r["position"]) <= {0, 1}


def test_rsi_is_100_when_every_day_gains():
    up = pd.Series([float(i) for i in range(1, 201)])
    vals = quant.rsi(up, 14).dropna()
    assert len(vals) > 0
    assert (vals == 100.0).all()


def test_rsi_is_0_when_every_day_loses():
    down = pd.Series([float(i) for i in range(200, 0, -1)])
    vals = quant.rsi(down, 14).dropna()
    assert len(vals) > 0
    assert (vals == 0.0).all()


def test_rsi_is_neutral_50_when_price_never_moves():
    vals = quant.rsi(pd.Series([100.0] * 60), 14).dropna()
    assert len(vals) > 0
    assert (vals == 50.0).all()


def test_rsi_is_finite_after_warmup_on_a_real_series():
    r = quant.rsi(data.synthetic("AAPL")["c"], 14)
    assert r.iloc[14:].notna().all()


def test_rsi_entry_requires_value_strictly_below_threshold(monkeypatch):
    c = pd.Series([100.0] * 40)
    monkeypatch.setattr(quant, "rsi", lambda cc, n=14: pd.Series([30.0] * len(cc)))
    assert quant.signals(c, "rsi", 14, 30).eq(0.0).all()
    monkeypatch.setattr(quant, "rsi", lambda cc, n=14: pd.Series([29.999] * len(cc)))
    assert quant.signals(c, "rsi", 14, 30).eq(1.0).all()


def test_rsi_exit_requires_value_strictly_above_55(monkeypatch):
    c = pd.Series([100.0] * 40)
    vals = [20.0] * 5 + [55.0] * 5 + [56.0] + [20.0] * 29
    monkeypatch.setattr(quant, "rsi", lambda cc, n=14: pd.Series(vals))
    pos = quant.signals(c, "rsi", 14, 30).tolist()
    assert pos == [1, 1, 1, 1, 1] + [1] * 5 + [0] + [1] * 29


def test_all_flat_position_costs_nothing():
    r = quant.run(pd.Series([100.0] * 30), pd.Series([0.0] * 30), 50)
    assert r["equity"] == [1.0] * 30
    assert r["trades"] == 0
    assert r["exposure"] == 0.0


def test_always_long_at_zero_cost_equals_buy_hold():
    c = pd.Series([100.0, 105.0, 99.75, 110.0, 110.0, 121.0])
    r = quant.run(c, pd.Series([1.0] * 6), 0.0)
    assert r["equity"] == r["buy_hold_equity"]
    assert r["trades"] == 1
    assert r["exposure"] == 1.0


def test_unknown_strategy_is_a_clear_error():
    with pytest.raises(ValueError, match="unknown strategy"):
        quant.signals(pd.Series([1.0, 2.0]), "nope", 5, 10)
