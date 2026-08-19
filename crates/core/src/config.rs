//! Configuration for the engine and its components.
//!
//! All components read configuration from a single `EngineConfig` (deserialized
//! from TOML or built programmatically). Values are deliberately explicit:
//! nothing is "magic". Paper trading is the default; live trading requires an
//! explicit `mode = "live"` flag plus per-exchange credential environment
//! variables.

use lq_types::{Amount, Exchange, Qty};
use serde::Deserialize;

/// Engine operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Simulated orders only. Default.
    Paper,
    /// Live order routing. Requires `EXCHANGE_LIVE=true` and credentials.
    Live,
}

impl Default for Mode {
    fn default() -> Self {
        Self::Paper
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub mode: Mode,
    pub symbols: Vec<String>,
    pub venues: Vec<Exchange>,
    pub strategy: StrategyConfig,
    pub risk: RiskConfig,
    pub paper: PaperSimConfig,
    pub telemetry: TelemetryConfig,
    pub api: ApiConfig,
    pub persistence: PersistenceConfig,
    pub market_data: MarketDataConfig,
}

impl EngineConfig {
    /// Load from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load from a TOML string, then apply environment overrides for
    /// deployment-critical settings. Platform-provided connection strings
    /// (e.g. Railway/Fly Postgres and Redis) win over the file so one config
    /// can be baked into an image and adapted at boot.
    pub fn from_toml_with_env(s: &str) -> Result<Self, toml::de::Error> {
        let mut cfg: Self = toml::from_str(s)?;
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("LQ_PERSISTENCE_ENABLED") {
            self.persistence.enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("POSTGRES_URL").or_else(|_| std::env::var("DATABASE_URL")) {
            self.persistence.postgres_url = v;
        }
        if let Ok(v) = std::env::var("REDIS_URL") {
            self.persistence.redis_url = v;
        }
        if let Ok(v) = std::env::var("API_BIND") {
            self.api.bind = v;
        }
        if let Ok(v) = std::env::var("METRICS_BIND") {
            self.telemetry.metrics_bind = v;
        }
        if let Ok(v) = std::env::var("LQ_LOG_LEVEL") {
            self.telemetry.log_level = v;
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Paper,
            symbols: vec!["BTC-USDT".to_string()],
            venues: vec![Exchange::Paper, Exchange::Simulated],
            strategy: StrategyConfig::default(),
            risk: RiskConfig::default(),
            paper: PaperSimConfig::default(),
            telemetry: TelemetryConfig::default(),
            api: ApiConfig::default(),
            persistence: PersistenceConfig::default(),
            market_data: MarketDataConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StrategyConfig {
    pub market_making: MarketMakingConfig,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            market_making: MarketMakingConfig::default(),
        }
    }
}

/// Parameters for the baseline market-making strategy.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MarketMakingConfig {
    pub enabled: bool,
    /// Target half-spread in bps (distance from mid to each leg).
    pub half_spread_bps: f64,
    pub min_spread_bps: f64,
    pub max_spread_bps: f64,
    /// Quantity (base units) per resting quote.
    pub quote_qty: Qty,
    /// Inventory level we aim to keep (0 = flat).
    pub inventory_target_qty: Qty,
    /// Inventory threshold beyond which we stop quoting the aggressive side.
    pub inventory_max_qty: Qty,
    /// Exponent controlling inventory skew aggressiveness (0 = no skew).
    pub skew_exponent: f64,
    /// Minimum time between quote refreshes.
    pub quote_refresh_ms: u64,
    /// Whether to widen the spread with short-term volatility.
    pub vol_scale_half_spread: bool,
}

impl Default for MarketMakingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            half_spread_bps: 5.0,
            min_spread_bps: 2.0,
            max_spread_bps: 30.0,
            quote_qty: rust_decimal_macros::dec!(0.01),
            inventory_target_qty: rust_decimal_macros::dec!(0.0),
            inventory_max_qty: rust_decimal_macros::dec!(0.5),
            skew_exponent: 1.0,
            quote_refresh_ms: 250,
            vol_scale_half_spread: true,
        }
    }
}

/// Risk limits and trip-wires. See `RISK.md` for semantics.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    pub max_position_qty: Qty,
    pub max_order_qty: Qty,
    pub max_open_orders: u32,
    pub max_notional: Amount,
    pub max_daily_loss: Amount,
    pub max_order_rate_per_sec: f64,
    pub max_exposure_per_venue: Amount,
    pub max_price_deviation_bps: f64,
    /// A book this old is "stale"; quoting halts.
    pub stale_market_ms: u64,
    /// Halt trading when the feed goes stale.
    pub kill_switch_on_stale: bool,
    /// Halt trading when a venue reconnects (order state is suspect).
    pub kill_switch_on_reconnect: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_qty: rust_decimal_macros::dec!(1.0),
            max_order_qty: rust_decimal_macros::dec!(0.1),
            max_open_orders: 50,
            max_notional: rust_decimal_macros::dec!(100_000),
            max_daily_loss: rust_decimal_macros::dec!(1_000),
            max_order_rate_per_sec: 20.0,
            max_exposure_per_venue: rust_decimal_macros::dec!(100_000),
            max_price_deviation_bps: 100.0,
            stale_market_ms: 5_000,
            kill_switch_on_stale: true,
            kill_switch_on_reconnect: true,
        }
    }
}

/// Parameters controlling the paper execution simulator. Every knob exists so
/// the simulation's assumptions are visible and tunable (see `TRADING.md`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PaperSimConfig {
    /// Base one-way latency added to each order submission.
    pub base_latency_ms: f64,
    /// Uniform jitter added on top of the base latency.
    pub latency_jitter_ms: f64,
    /// Probability a resting maker order fills when the book crosses it.
    pub fill_fraction: f64,
    /// Taker fee in bps.
    pub fee_rate_bps: f64,
    /// Maker rebate in bps (negative fee).
    pub maker_rebate_bps: f64,
    /// Slippage applied to market/taker fills in bps.
    pub slippage_bps: f64,
    /// Probability an order is partially filled rather than fully.
    pub partial_fill_prob: f64,
    /// Fraction filled on a partial fill.
    pub partial_fill_fraction: f64,
    /// Probability an order is rejected by the venue.
    pub reject_prob: f64,
    /// Assumed queue position of a resting order (0 = front).
    pub queue_position: f64,
    /// Number of book levels the paper exchange simulates.
    pub depth_levels: usize,
}

impl Default for PaperSimConfig {
    fn default() -> Self {
        Self {
            base_latency_ms: 2.0,
            latency_jitter_ms: 1.0,
            fill_fraction: 0.8,
            fee_rate_bps: 2.5,
            maker_rebate_bps: 0.5,
            slippage_bps: 1.0,
            partial_fill_prob: 0.3,
            partial_fill_fraction: 0.5,
            reject_prob: 0.001,
            queue_position: 0.5,
            depth_levels: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    pub log_level: String,
    pub metrics_bind: String,
    pub json_logs: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            metrics_bind: "0.0.0.0:9100".to_string(),
            json_logs: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ApiConfig {
    pub bind: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub postgres_url: String,
    pub redis_url: String,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            postgres_url: "postgres://lq:lq@localhost:5432/liquidity".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MarketDataConfig {
    pub ws_reconnect_base_ms: u64,
    pub ws_reconnect_max_ms: u64,
    pub ping_interval_ms: u64,
    pub stale_after_ms: u64,
}

impl Default for MarketDataConfig {
    fn default() -> Self {
        Self {
            ws_reconnect_base_ms: 500,
            ws_reconnect_max_ms: 30_000,
            ping_interval_ms: 15_000,
            stale_after_ms: 5_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_paper_only() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.mode, Mode::Paper);
        assert!(cfg.venues.iter().all(|v| !v.is_live()));
    }

    #[test]
    fn parses_from_toml() {
        let toml = r#"
            [strategy.market_making]
            half_spread_bps = 8.0
            quote_qty = 0.05
        "#;
        let cfg = EngineConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.strategy.market_making.half_spread_bps, 8.0);
        assert_eq!(cfg.strategy.market_making.quote_qty, rust_decimal_macros::dec!(0.05));
    }

    #[test]
    fn live_mode_is_explicit() {
        let toml = r#"
            mode = "live"
            venues = ["okx"]
        "#;
        let cfg = EngineConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.mode, Mode::Live);
        assert_eq!(cfg.venues, vec![Exchange::Okx]);
    }

    #[test]
    fn env_overrides_win_over_file() {
        std::env::set_var("POSTGRES_URL", "postgres://override");
        std::env::set_var("REDIS_URL", "redis://override");
        std::env::set_var("API_BIND", "0.0.0.0:9999");
        std::env::set_var("LQ_PERSISTENCE_ENABLED", "true");
        let toml = r#"
            [persistence]
            enabled = false
            postgres_url = "postgres://file"
            redis_url = "redis://file"
            [api]
            bind = "0.0.0.0:8080"
        "#;
        let cfg = EngineConfig::from_toml_with_env(toml).unwrap();
        assert!(cfg.persistence.enabled);
        assert_eq!(cfg.persistence.postgres_url, "postgres://override");
        assert_eq!(cfg.persistence.redis_url, "redis://override");
        assert_eq!(cfg.api.bind, "0.0.0.0:9999");
        std::env::remove_var("POSTGRES_URL");
        std::env::remove_var("REDIS_URL");
        std::env::remove_var("API_BIND");
        std::env::remove_var("LQ_PERSISTENCE_ENABLED");
    }
}


