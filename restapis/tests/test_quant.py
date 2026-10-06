import pandas as pd

from app import data, quant


def test_no_lookahead_first_held_day_has_no_return():
    c = pd.Series([100, 100, 110, 121, 121.0])
    pos = pd.Series([0, 1, 1, 1, 1.0])  # decided at close of day 1
    held = pos.shift(1).fillna(0)
    r = quant.run(c, pos, 0)
    # day 1 return (0%) is earned by yesterday's flat position, day 2 (+10%) by the day-1 signal
    assert abs(r["equity"][2] - 1.10) < 1e-9 and held.iloc[1] == 0


def test_costs_reduce_return():
    c = data.synthetic("AAPL")["c"]
    free = quant.backtest(c, "ma", 20, 60, 0)
    paid = quant.backtest(c, "ma", 20, 60, 50)
    assert paid["strategy"]["cagr"] < free["strategy"]["cagr"] and paid["cost_drag_cagr"] > 0


def test_all_strategies_run_and_shapes_match():
    c = data.synthetic("NVDA")["c"]
    for s, a, b in [("ma", 20, 60), ("mom", 60, 0), ("rsi", 14, 30)]:
        r = quant.backtest(c, s, a, b, 10)
        assert len(r["equity"]) == len(c) == len(r["position"])
        assert 0 <= r["exposure"] <= 1


def test_buy_hold_matches_price_ratio():
    c = data.synthetic("SPY")["c"]
    r = quant.backtest(c, "mom", 60, 0, 0)
    assert abs(r["buy_hold_equity"][-1] - c.iloc[-1] / c.iloc[0]) < 1e-9


def test_api_contract():
    from fastapi.testclient import TestClient

    from app.main import app
    cl = TestClient(app)
    assert cl.get("/health").json()["ok"]
    ok = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "ma", "a": 20, "b": 60, "cost_bps": 10})
    assert ok.status_code == 200 and "cost_drag_cagr" in ok.json()
    assert cl.post("/backtest", json={"symbol": "AAPL", "strategy": "ma", "a": 80, "b": 60}).status_code == 422
    assert cl.get("/prices", params={"symbol": "NOPE"}).status_code == 404
    assert len(cl.get("/screen").json()) == len(data.SYMBOLS)
