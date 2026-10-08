"""Price providers. Default is a seeded synthetic walk (same generator as the web demo).
Set DATA_SOURCE=yfinance for real daily closes."""
import math
import os
import time

import pandas as pd

SYMBOLS = ["AAPL", "MSFT", "NVDA", "TSLA", "AMZN", "SPY", "XOM", "BTC-USD"]
_TTL = 15 * 60
_cache: dict[str, tuple[float, pd.DataFrame]] = {}


def synthetic(symbol: str, n: int = 520) -> pd.DataFrame:
    seed = 7
    for ch in symbol:
        seed = (seed * 31 + ord(ch)) & 0xFFFFFFFF
    state = [seed]

    def r() -> float:
        state[0] = (state[0] * 1664525 + 1013904223) & 0xFFFFFFFF
        return state[0] / 4294967296

    def gauss() -> float:
        a = math.sqrt(-2 * math.log(r() or 1e-9))
        return a * math.cos(6.2831853 * r())

    mu = 0.0003 + (r() - 0.3) * 0.0006
    v = 0.011 + r() * 0.016
    p = 50 + r() * 250
    rows = []
    for _ in range(n):
        o = p
        p *= 1 + mu + v * gauss()
        h = max(o, p) * (1 + r() * v / 2)
        l = min(o, p) * (1 - r() * v / 2)
        rows.append((o, h, l, p))
    idx = pd.bdate_range(end=pd.Timestamp.today().normalize(), periods=n)
    return pd.DataFrame(rows, columns=["o", "h", "l", "c"], index=idx)


def _yahoo(symbol: str) -> pd.DataFrame:
    import yfinance as yf
    df = yf.download(symbol, period="3y", auto_adjust=True, progress=False)
    if df.empty:
        raise KeyError(symbol)
    if isinstance(df.columns, pd.MultiIndex):
        df.columns = df.columns.get_level_values(0)
    df = df.rename(columns={"Open": "o", "High": "h", "Low": "l", "Close": "c"})[["o", "h", "l", "c"]]
    return df.dropna()


def source() -> str:
    return os.getenv("DATA_SOURCE", "synthetic")


def get_prices(symbol: str) -> pd.DataFrame:
    symbol = symbol.upper()
    hit = _cache.get(symbol)
    if hit and time.time() - hit[0] < _TTL:
        return hit[1]
    if source() == "yfinance":
        df = _yahoo(symbol)
    else:
        if symbol not in SYMBOLS:
            raise KeyError(symbol)
        df = synthetic(symbol)
    _cache[symbol] = (time.time(), df)
    return df
