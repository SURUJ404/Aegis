//! Deterministic backtest runner.
//!
//! Generates a reproducible synthetic market sequence (or replays events you
//! provide), runs it through the real strategy / risk / paper-execution stack
//! with latency disabled and zero rejection, and reports PnL, drawdown and
//! Sharpe. Same inputs → same result, every time.

use clap::Parser;
use lq_backtest::{BacktestConfig, BacktestResult, BacktestRunner};
use lq_core::config::EngineConfig;
use lq_core::event::MarketEvent;
use lq_exchange::spec::InstrumentSpec;
use lq_simulator::market_gen::{SyntheticDataConfig, SyntheticMarketData};
use lq_types::{Amount, Exchange, Symbol, TimestampMs};
use rust_decimal_macros::dec;

#[derive(Parser)]
#[command(
    name = "backtest",
    about = "Deterministic backtest of the market-making stack"
)]
struct Cli {
    /// Path to a TOML config file (strategy/risk/paper sections are used).
    #[arg(long, env = "LQ_CONFIG")]
    config: Option<String>,

    /// Number of synthetic market events to generate.
    #[arg(long, default_value_t = 20_000)]
    events: u64,

    /// RNG seed (synthetic data + paper venue).
    #[arg(long, default_value_t = 13)]
    seed: u64,

    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

fn load_config(cli: &Cli) -> anyhow::Result<EngineConfig> {
    let _ = dotenvy::dotenv();
    match &cli.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            EngineConfig::from_toml(&text).map_err(|e| anyhow::anyhow!("config parse error: {e}"))
        }
        None => Ok(EngineConfig::default()),
    }
}

/// Build the synthetic event sequence. Deterministic given `seed`.
fn generate_events(seed: u64, count: u64, cfg: &EngineConfig) -> Vec<MarketEvent> {
    let symbol = Symbol(cfg.symbols.first().cloned().unwrap_or_else(|| "BTC-USDT".into()));
    let spec = InstrumentSpec::new(dec!(0.1), dec!(0.01));

    let mut gen = SyntheticMarketData::new(
        Exchange::Paper,
        symbol,
        spec,
        SyntheticDataConfig {
            seed,
            interval_ms: 100,
            ..SyntheticDataConfig::default()
        },
    );

    let mut events = Vec::with_capacity(count as usize + 1);
    events.push(gen.initial_snapshot(TimestampMs(0)));
    for i in 1..=count {
        events.extend(gen.next_events(TimestampMs(i * 100)));
    }
    events
}

/// Translate the engine config into backtest parameters.
fn backtest_config(cfg: &EngineConfig, seed: u64) -> BacktestConfig {
    let venue = cfg.venues.first().copied().unwrap_or(Exchange::Paper);
    let symbol = Symbol(cfg.symbols.first().cloned().unwrap_or_else(|| "BTC-USDT".into()));
    BacktestConfig {
        venue,
        symbol,
        spec: InstrumentSpec::new(dec!(0.1), dec!(0.01)),
        paper: cfg.paper.clone(),
        mm: cfg.strategy.market_making.clone(),
        risk: cfg.risk.clone(),
        initial_capital: Amount::from(100_000),
        equity_sample_every: 100,
        periods_per_year: 252.0 * 6.0,
        seed,
    }
}

fn print_summary(result: &BacktestResult) {
    let m = &result.metrics;
    println!("backtest result");
    println!("  events            : {}", result.events_seen);
    println!("  orders placed     : {}", result.orders_placed);
    println!("  rejected          : {}", result.rejected_orders);
    println!("  open orders at end: {}", result.open_orders_at_end);
    println!("  fills             : {}", m.fills);
    println!("  round-trip trades : {}", m.trades);
    println!("  win rate          : {:.2}%", m.win_rate * 100.0);
    println!("  fees total        : {}", m.fees_total);
    println!("  net pnl           : {}", m.net_pnl);
    println!("  max drawdown      : {} ({:.2}%)", m.max_drawdown, m.max_drawdown_pct * 100.0);
    println!("  sharpe (annual)   : {:.2}", m.sharpe);
    println!("  final equity      : {}", m.final_equity);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    lq_telemetry::init_logging(&cfg.telemetry)?;

    tracing::info!(events = cli.events, seed = cli.seed, "generating synthetic market sequence");
    let events = generate_events(cli.seed, cli.events, &cfg);
    tracing::info!(n = events.len(), "events generated");

    let mut runner = BacktestRunner::new(backtest_config(&cfg, cli.seed));
    let result = runner.run_async(&events).await;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_summary(&result);
    }
    Ok(())
}