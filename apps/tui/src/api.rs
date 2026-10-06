//! HTTP clients for the two backends the TUI integrates:
//!
//! * **Engine** — the Rust control-plane api-server (`apps/api-server`).
//! * **Quant** — the Python FastAPI service in `restapis/`.
//!
//! Every call returns `Result<T, String>` so results can travel through the
//! UI message channel without dragging error types around.

use std::time::Duration;

use lq_core::models::{Inventory, MarketState, Order, Position};
use lq_core::state::RiskStatus;
use serde::{Deserialize, Serialize};

const TIMEOUT: Duration = Duration::from_secs(5);

fn http() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(TIMEOUT).build()
}

// ---------------------------------------------------------------- engine api

#[derive(Debug, Clone, Deserialize)]
pub struct StateSummary {
    pub positions: Vec<Position>,
    pub inventory: Vec<Inventory>,
    pub orders: Vec<Order>,
    pub market_state: Vec<MarketState>,
    pub risk: RiskStatus,
    pub strategy_running: bool,
    pub uptime_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponse {
    pub accepted: bool,
    pub message: String,
}

#[derive(Clone)]
pub struct EngineClient {
    base: String,
    http: reqwest::Client,
}

impl EngineClient {
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self { base: base.into(), http: http()? })
    }

    pub async fn state(&self) -> Result<StateSummary, String> {
        self.get("/api/v1/state").await
    }

    pub async fn control(&self, action: &str) -> Result<ControlResponse, String> {
        self.http
            .post(format!("{}/api/v1/control/{action}", self.base))
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    pub async fn kill(&self, reason: &str) -> Result<ControlResponse, String> {
        self.http
            .post(format!("{}/api/v1/control/kill", self.base))
            .json(&serde_json::json!({ "reason": reason }))
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        self.http
            .get(format!("{}{}", self.base, path))
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }
}

// ----------------------------------------------------------------- quant api

pub const STRATEGIES: [&str; 3] = ["ma", "mom", "rsi"];

#[derive(Debug, Clone, Deserialize)]
pub struct QuantHealth {
    pub ok: bool,
    pub data_source: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScreenRow {
    pub symbol: String,
    /// null when the provider has no usable close
    pub price: Option<f64>,
    /// null when history is shorter than the 1-month window
    pub ret_1m: Option<f64>,
    /// null during the RSI warmup window
    pub rsi14: Option<f64>,
    /// null when history is shorter than the 50-day window
    pub vs_sma50: Option<f64>,
    pub signal: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestReq {
    pub symbol: String,
    pub strategy: String,
    pub a: i32,
    pub b: i32,
    pub cost_bps: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stats {
    pub cagr: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Backtest {
    pub symbol: String,
    pub dates: Vec<String>,
    pub strategy: Stats,
    pub buy_hold: Stats,
    pub trades: i64,
    /// Fraction of days held — the API's "time in market".
    pub exposure: f64,
    pub equity: Vec<f64>,
    pub buy_hold_equity: Vec<f64>,
    /// Daily position series (0 = flat, 1 = long).
    #[serde(default)]
    pub position: Vec<f64>,
    pub cost_drag_cagr: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bar {
    pub t: String,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Prices {
    pub symbol: String,
    pub bars: Vec<Bar>,
}

#[derive(Clone)]
pub struct QuantClient {
    base: String,
    http: reqwest::Client,
}

impl QuantClient {
    pub fn new(base: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self { base: base.into(), http: http()? })
    }

    pub async fn health(&self) -> Result<QuantHealth, String> {
        self.get("/health").await
    }

    pub async fn symbols(&self) -> Result<Vec<String>, String> {
        self.get("/symbols").await
    }

    pub async fn screen(&self, strategy: &str, a: i32, b: i32) -> Result<Vec<ScreenRow>, String> {
        self.http
            .get(format!("{}/screen", self.base))
            .query(&[("strategy", strategy), ("a", &a.to_string()), ("b", &b.to_string())])
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    pub async fn backtest(&self, req: &BacktestReq) -> Result<Backtest, String> {
        self.http
            .post(format!("{}/backtest", self.base))
            .json(req)
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    pub async fn prices(&self, symbol: &str, days: i32) -> Result<Prices, String> {
        self.http
            .get(format!("{}/prices", self.base))
            .query(&[("symbol", symbol), ("days", &days.to_string())])
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        self.http
            .get(format!("{}{}", self.base, path))
            .send()
            .await
            .map_err(err)?
            .json()
            .await
            .map_err(err)
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture mirrors a live `POST /backtest` response from `restapis`.
    #[test]
    fn backtest_parses_quant_api_response() {
        let json = r#"{
            "symbol":"AAPL","dates":["2024-01-01","2024-01-02"],
            "strategy":{"cagr":0.11,"sharpe":1.2,"max_drawdown":-0.18},
            "buy_hold":{"cagr":0.09,"sharpe":0.9,"max_drawdown":-0.31},
            "trades":42,"exposure":0.87,
            "equity":[1.0,1.01],"buy_hold_equity":[1.0,0.99],
            "position":[0,1],"cost_drag_cagr":0.00515
        }"#;
        let b: Backtest = serde_json::from_str(json).expect("backtest parses");
        assert_eq!(b.position, vec![0.0, 1.0]);
        assert!((b.cost_drag_cagr - 0.00515).abs() < 1e-12);
        assert_eq!(b.strategy.cagr, 0.11);
    }

    #[test]
    fn screen_parses_quant_api_response() {
        let json = r#"{"symbol":"AAPL","price":123.4,"ret_1m":0.05,
                       "rsi14":55.0,"vs_sma50":-0.01,"signal":"long"}"#;
        let row: ScreenRow = serde_json::from_str(json).expect("screen row parses");
        assert_eq!(row.signal, "long");
        assert_eq!(row.symbol, "AAPL");
    }

    #[test]
    fn screen_row_parses_null_metrics() {
        // The API answers null (not NaN) when a window doesn't fit the data.
        let json = r#"{"symbol":"XOM","price":null,"ret_1m":null,"rsi14":null,
                       "vs_sma50":null,"signal":"flat"}"#;
        let row: ScreenRow = serde_json::from_str(json).expect("null metrics parse");
        assert_eq!(row.signal, "flat");
        assert!(row.rsi14.is_none());
        assert!(row.price.is_none());
    }

    /// Fixture mirrors `GET /api/v1/state` from the Rust api-server.
    #[test]
    fn state_parses_api_server_response() {
        let json = r#"{
            "positions":[],"inventory":[],"orders":[],"market_state":[],
            "risk":{"armed":true,"halted":false,"halt_reason":null,"updated_at":1791255329986},
            "strategy_running":false,"uptime_ms":42
        }"#;
        let s: StateSummary = serde_json::from_str(json).expect("state parses");
        assert!(s.risk.armed);
        assert!(!s.risk.halted);
        assert_eq!(s.uptime_ms, 42);
    }
}
