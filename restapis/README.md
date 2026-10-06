# Trading Terminal API

    pip install -r requirements.txt
    uvicorn app.main:app --reload          # http://localhost:8000/docs
    pytest

Data is synthetic by default. For real daily prices: `DATA_SOURCE=yfinance uvicorn app.main:app`.
CORS origins: `ALLOWED_ORIGINS=https://your.site,http://localhost:5173`.

| Endpoint | Purpose |
|---|---|
| `GET /health`, `GET /symbols` | status, available tickers |
| `GET /prices?symbol=AAPL&days=120` | OHLC bars |
| `POST /backtest` | `{symbol, strategy: ma\|mom\|rsi, a, b, cost_bps}` returns metrics, equity, positions, `cost_drag_cagr` |
| `GET /screen?strategy=ma&a=20&b=60` | screener rows with current signal |

Adding a strategy: add a branch in `quant.signals`, extend `Strategy` in `main.py`. Logic stays in `quant.py`, free of web code.
