import math
import os
from typing import Literal

from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.encoders import jsonable_encoder
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field, model_validator

from . import data, quant

DEFAULT_ORIGINS = "http://localhost:5173,http://localhost:3000"


def _origins() -> list[str]:
    raw = os.getenv("ALLOWED_ORIGINS", DEFAULT_ORIGINS)
    return [o.strip() for o in raw.split(",") if o.strip()]


app = FastAPI(title="Trading Terminal API", version="0.1.0")
app.add_middleware(CORSMiddleware, allow_origins=_origins(), allow_methods=["*"], allow_headers=["*"])


def _scrub(obj):
    """Make a payload JSON-renderable: Starlette refuses NaN/Infinity literals."""
    if isinstance(obj, float):
        return obj if math.isfinite(obj) else str(obj)
    if isinstance(obj, dict):
        return {k: _scrub(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [_scrub(v) for v in obj]
    return obj


@app.exception_handler(RequestValidationError)
async def _validation_error(request: Request, exc: RequestValidationError):
    # 422 bodies echo the offending input; a NaN input must not crash the renderer.
    return JSONResponse(status_code=422, content={"detail": _scrub(jsonable_encoder(exc.errors()))})

Strategy = Literal["ma", "mom", "rsi"]


class BacktestRequest(BaseModel):
    symbol: str
    strategy: Strategy = "ma"
    a: int = Field(20, ge=2, le=250, description="ma: fast days, mom: lookback, rsi: period")
    b: int = Field(60, ge=2, le=250, description="ma: slow days, rsi: buy below, mom: unused")
    cost_bps: float = Field(10, ge=0, le=200)

    @model_validator(mode="after")
    def _check(self):
        if self.strategy == "ma" and self.a >= self.b:
            raise ValueError("for ma, fast days (a) must be less than slow days (b)")
        return self


def _prices(symbol: str):
    try:
        return data.get_prices(symbol)
    except KeyError:
        raise HTTPException(404, f"unknown symbol {symbol}")
    except Exception as e:  # noqa: BLE001 - deliberate API boundary: any provider failure must answer 502, not crash
        raise HTTPException(502, f"data provider error: {e}")


@app.get("/health")
def health():
    return {"ok": True, "data_source": data.source()}


@app.get("/symbols")
def symbols():
    return data.SYMBOLS


@app.get("/prices")
def prices(symbol: str, days: int = Query(120, ge=5, le=1000)):
    df = _prices(symbol).tail(days)
    return {"symbol": symbol.upper(), "bars": [
        {"t": t.strftime("%Y-%m-%d"), "o": r.o, "h": r.h, "l": r.l, "c": r.c} for t, r in df.iterrows()]}


def _check_window(strategy: str, a: int, b: int) -> None:
    if strategy == "ma" and a >= b:
        raise HTTPException(422, "for ma, fast days (a) must be less than slow days (b)")


def _finite_or_none(x) -> float | None:
    v = float(x)
    return v if math.isfinite(v) else None


@app.post("/backtest")
def backtest(req: BacktestRequest):
    df = _prices(req.symbol)
    try:
        stats = quant.backtest(df["c"], req.strategy, req.a, req.b, req.cost_bps)
    except ValueError as e:
        raise HTTPException(502, f"data provider error: {e}") from e
    return {"symbol": req.symbol.upper(), "dates": [t.strftime("%Y-%m-%d") for t in df.index],
            **stats}


@app.get("/screen")
def screen(strategy: Strategy = "ma", a: int = Query(20, ge=2, le=250), b: int = Query(60, ge=2, le=250)):
    _check_window(strategy, a, b)
    rows = []
    for s in data.SYMBOLS:
        df = _prices(s)
        if df.empty:
            raise HTTPException(502, f"data provider error: {s} returned no bars")
        c = df["c"]
        ret_1m = _finite_or_none(c.iloc[-1] / c.iloc[-22] - 1) if len(c) >= 22 else None
        rows.append({"symbol": s, "price": _finite_or_none(c.iloc[-1]), "ret_1m": ret_1m,
                     "rsi14": _finite_or_none(quant.rsi(c, 14).iloc[-1]),
                     "vs_sma50": _finite_or_none(quant.sma(c, 50).iloc[-1]),
                     "signal": "long" if quant.signals(c, strategy, a, b).iloc[-1] else "flat"})
    return rows
