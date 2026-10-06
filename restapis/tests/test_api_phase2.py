"""Phase 2 API tests: validation matrix, error mapping, screener robustness, CORS, cache.

Rule under test: every request must give a clean200/4xx or a deliberate 502 for
provider failures — never an unhandled 500, never NaN in JSON.
"""
import json
import math
import time

import pytest
from fastapi.testclient import TestClient

from app import data
from app import main as api


@pytest.fixture()
def cl():
    return TestClient(api.app)


# ---------- guards: already correct, locked against regression ----------


def test_health_reports_data_source(cl):
    r = cl.get("/health")
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is True and body["data_source"] == data.source()


def test_symbols_lists_all_supported_tickers(cl):
    r = cl.get("/symbols")
    assert r.status_code == 200
    assert r.json() == data.SYMBOLS


def test_prices_rejects_days_outside_bounds(cl):
    for days in (-10, 0, 4, 1001):
        r = cl.get("/prices", params={"symbol": "AAPL", "days": days})
        assert r.status_code == 422, days


def test_prices_is_case_insensitive_and_bars_are_well_formed(cl):
    r = cl.get("/prices", params={"symbol": "aapl", "days": 30})
    assert r.status_code == 200
    body = r.json()
    assert body["symbol"] == "AAPL"
    bars = body["bars"]
    assert len(bars) == 30
    for bar in bars:
        assert set(bar) == {"t", "o", "h", "l", "c"}
        assert len(bar["t"]) == 10 and bar["t"][4] == "-"
        assert bar["h"] >= max(bar["o"], bar["c"])
        assert bar["l"] <= min(bar["o"], bar["c"])
        assert all(math.isfinite(bar[k]) for k in ("o", "h", "l", "c"))


def test_prices_empty_provider_returns_empty_bars_not_500(cl, monkeypatch):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").iloc[0:0])
    r = cl.get("/prices", params={"symbol": "AAPL"})
    assert r.status_code == 200
    assert r.json()["bars"] == []


def test_backtest_unknown_symbol_is_404(cl):
    r = cl.post("/backtest", json={"symbol": "NOPE", "strategy": "ma", "a": 20, "b": 60})
    assert r.status_code == 404


def test_backtest_provider_failure_is_502(cl, monkeypatch):
    def boom(_symbol):
        raise ConnectionError("wire cut")

    monkeypatch.setattr(api.data, "get_prices", boom)
    r = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "ma", "a": 20, "b": 60})
    assert r.status_code == 502
    assert "data provider error" in r.json()["detail"]


def test_backtest_validation_matrix_is_422(cl):
    base = {"symbol": "AAPL", "strategy": "ma", "a": 20, "b": 60, "cost_bps": 10}
    bad = [
        {"a": 1},
        {"a": 251},
        {"b": 1},
        {"b": 251},
        {"cost_bps": -0.1},
        {"cost_bps": 200.1},
        {"strategy": "bollinger"},
        {"strategy": "ma", "a": 60, "b": 20},
    ]
    for over in bad:
        body = {**base, **over}
        assert cl.post("/backtest", json=body).status_code == 422, body


def test_backtest_rejects_nonfinite_json_numbers(cl):
    contents = [
        f'{{"symbol":"AAPL","strategy":"ma","a":20,"b":60,"cost_bps":{tok}}}'
        for tok in ("NaN", "Infinity", "-Infinity")
    ]
    contents.append('{"symbol":"AAPL","strategy":"ma","a":NaN,"b":60,"cost_bps":10}')
    for content in contents:
        r = cl.post("/backtest", content=content, headers={"content-type": "application/json"})
        assert r.status_code == 422, content


def test_backtest_response_is_aligned_and_json_safe(cl):
    r = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "rsi", "a": 14, "b": 30, "cost_bps": 5})
    assert r.status_code == 200
    body = r.json()
    n = len(body["dates"])
    assert n == len(body["equity"]) == len(body["buy_hold_equity"]) == len(body["position"])
    assert set(body["position"]) <= {0, 1}
    json.dumps(body, allow_nan=False)


def test_screen_returns_all_symbols_with_clean_fields(cl):
    r = cl.get("/screen")
    assert r.status_code == 200
    rows = r.json()
    assert [row["symbol"] for row in rows] == data.SYMBOLS
    for row in rows:
        assert row["signal"] in ("long", "flat")
        assert row["price"] is not None and math.isfinite(row["price"]) and row["price"] > 0
        for k in ("ret_1m", "rsi14", "vs_sma50"):
            assert row[k] is None or math.isfinite(row[k]), (row["symbol"], k)
    json.dumps(rows, allow_nan=False)


def test_screen_accepts_every_strategy(cl):
    for s in ("ma", "mom", "rsi"):
        r = cl.get("/screen", params={"strategy": s, "a": 20, "b": 60})
        assert r.status_code == 200 and len(r.json()) == len(data.SYMBOLS), s


def test_screen_validation_matrix_is_422(cl):
    bad = [
        {"strategy": "ma", "a": 1},
        {"strategy": "ma", "a": 251},
        {"strategy": "ma", "b": 1},
        {"strategy": "nope"},
    ]
    for params in bad:
        assert cl.get("/screen", params=params).status_code == 422, params


def test_cors_allows_configured_origin(cl):
    r = cl.options(
        "/backtest",
        headers={"Origin": "http://localhost:5173", "Access-Control-Request-Method": "POST"},
    )
    assert r.headers.get("access-control-allow-origin") == "http://localhost:5173"


def test_cors_blocks_unconfigured_origin(cl):
    r = cl.options(
        "/backtest",
        headers={"Origin": "http://evil.example", "Access-Control-Request-Method": "POST"},
    )
    assert "access-control-allow-origin" not in r.headers


def test_cache_serves_fresh_entries_without_refetch():
    sentinel = data.synthetic("AAPL").tail(3)
    api.data._cache["AAPL"] = (time.time(), sentinel)
    try:
        assert api.data.get_prices("AAPL") is sentinel
    finally:
        api.data._cache.pop("AAPL", None)


def test_cache_refreshes_stale_entries():
    sentinel = data.synthetic("AAPL").tail(3)
    api.data._cache["MSFT"] = (time.time() - (data._TTL + 1), sentinel)
    try:
        assert api.data.get_prices("MSFT") is not sentinel
    finally:
        api.data._cache.pop("MSFT", None)


# ---------- failing tests: bugs found in Phase 2 ----------


def test_screen_rejects_reverse_ma_window(cl):
    r = cl.get("/screen", params={"strategy": "ma", "a": 60, "b": 20})
    assert r.status_code == 422


@pytest.mark.parametrize(
    "n,rsi_ok,sma_ok,ret_ok",
    [(10, False, False, False), (20, True, False, False), (40, True, False, True), (60, True, True, True)],
)
def test_screen_short_history_is_clean_json(cl, monkeypatch, n, rsi_ok, sma_ok, ret_ok):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").tail(n))
    r = cl.get("/screen")
    assert r.status_code == 200, r.text
    rows = r.json()
    json.dumps(rows, allow_nan=False)
    for row in rows:
        assert (row["price"] is not None) == (n >= 1), row
        assert (row["rsi14"] is not None) == rsi_ok, (row, n)
        assert (row["vs_sma50"] is not None) == sma_ok, (row, n)
        assert (row["ret_1m"] is not None) == ret_ok, (row, n)
        assert row["signal"] in ("long", "flat")


def test_screen_empty_provider_is_502_not_500(cl, monkeypatch):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").iloc[0:0])
    r = cl.get("/screen")
    assert r.status_code == 502


def test_backtest_single_bar_provider_is_502_not_500(cl, monkeypatch):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").tail(1))
    r = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "ma", "a": 20, "b": 60})
    assert r.status_code == 502


def test_backtest_two_bars_is_200(cl, monkeypatch):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").tail(2))
    r = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "ma", "a": 20, "b": 60})
    assert r.status_code == 200
    body = r.json()
    assert len(body["dates"]) == 2 == len(body["equity"])


def test_backtest_empty_provider_is_502_not_500(cl, monkeypatch):
    monkeypatch.setattr(api.data, "get_prices", lambda s: data.synthetic("AAPL").iloc[0:0])
    r = cl.post("/backtest", json={"symbol": "AAPL", "strategy": "mom", "a": 5, "b": 10})
    assert r.status_code == 502


def test_cors_origin_list_is_stripped_and_cleaned(monkeypatch):
    monkeypatch.setenv("ALLOWED_ORIGINS", " http://a.example , http://b.example ,, ")
    assert api._origins() == ["http://a.example", "http://b.example"]
