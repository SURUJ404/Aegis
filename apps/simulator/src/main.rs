//! Standalone paper-exchange simulator.
//!
//! Runs a synthetic market on one or more venues, drives a `PaperExchange`
//! (order book + fill matching) from that market stream, and exposes a
//! Prometheus metrics endpoint. There is no strategy and no API: this is the
//! minimal execution environment for exercising venue behaviour and running
//! order-placement experiments against a live-ish simulated book.

use std::sync::Arc;

use clap::Parser;
use lq_core::bus::EventBus;
use lq_core::config::EngineConfig;
use lq_exchange::spec::InstrumentSpec;
use lq_execution::paper::PaperExecutionVenue;
use lq_simulator::exchange::PaperExchange;
use lq_simulator::market_gen::{SimulatedFeed, SyntheticDataConfig};
use lq_types::Symbol;
use rust_decimal_macros::dec;

#[derive(Parser)]
#[command(
    name = "simulate",
    about = "Run the paper exchange with synthetic market data"
)]
struct Cli {
    /// Path to a TOML config file. Defaults to built-in defaults.
    #[arg(long, env = "LQ_CONFIG")]
    config: Option<String>,

    /// RNG seed for the synthetic market and venue.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Milliseconds between synthetic book updates.
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
}

fn load_config(cli: &Cli) -> anyhow::Result<EngineConfig> {
    let _ = dotenvy::dotenv();
    match &cli.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            EngineConfig::from_toml_with_env(&text).map_err(|e| anyhow::anyhow!("config parse error: {e}"))
        }
        None => Ok(EngineConfig::default()),
    }
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run())
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    lq_telemetry::init_logging(&cfg.telemetry)?;

    let bus = Arc::new(EventBus::new());
    let metrics = Arc::new(lq_telemetry::Metrics::new());
    let mut handles = Vec::new();

    // The paper venues used by the simulated exchanges, for the final summary.
    let mut venues_desc = Vec::new();

    for &venue in &cfg.venues {
        for symbol_str in &cfg.symbols {
            let symbol = Symbol(symbol_str.clone());
            let spec = InstrumentSpec::new(dec!(0.1), dec!(0.01));

            let ven = Arc::new(PaperExecutionVenue::with_seed(
                venue,
                cfg.paper.clone(),
                Arc::clone(&bus),
                cli.seed,
                true,
            ));
            let exchange = PaperExchange::new(
                venue,
                symbol.clone(),
                spec,
                Arc::clone(&ven),
                cfg.paper.clone(),
                cli.seed,
            );
            exchange.connect_prices();

            // Synthetic feed: publishes snapshots/deltas/trades on the market topic.
            let feed = SimulatedFeed::new(
                SyntheticDataConfig {
                    seed: cli.seed ^ cfg_symbol_seed(&symbol),
                    interval_ms: cli.interval_ms,
                    ..SyntheticDataConfig::default()
                },
                venue,
                symbol.clone(),
                spec,
            );
            handles.push(feed.spawn(Arc::clone(&bus)));

            // Match resting orders against the moving book. The paper exchange
            // is not Send, so it must live on the current-thread task set.
            let mut exchange = exchange;
            let market_bus = Arc::clone(&bus);
            let task_metrics = Arc::clone(&metrics);
            handles.push(tokio::task::spawn_local(async move {
                let mut sub = market_bus.market().subscribe();
                while let Some(event) = sub.recv().await {
                    let _ = exchange.on_market_event(&event).await;
                    task_metrics.record_market_event(&event);
                }
            }));

            // Mirror execution events into metrics.
            let metric_bus = Arc::clone(&bus);
            let metrics_task_metrics = Arc::clone(&metrics);
            handles.push(tokio::spawn(async move {
                let mut sub = metric_bus.execution().subscribe();
                while let Some(event) = sub.recv().await {
                    metrics_task_metrics.record_execution(&event);
                }
            }));

            tracing::info!(venue = %venue, symbol = %symbol, "simulated exchange running");
            venues_desc.push(format!("{venue}:{symbol}"));
        }
    }

    // Metrics endpoint.
    let metrics_server = lq_telemetry::MetricsServer::new(Arc::clone(&metrics));
    let metrics_bind = cfg.telemetry.metrics_bind.clone();
    let metrics_task = tokio::spawn(async move {
        if let Err(e) = metrics_server.serve(&metrics_bind).await {
            tracing::error!(err = %e, "metrics server failed");
        }
    });

    tracing::info!(venues = ?venues_desc, "simulator started");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    metrics_task.abort();
    for h in &handles {
        h.abort();
    }
    Ok(())
}

fn cfg_symbol_seed(symbol: &Symbol) -> u64 {
    symbol.as_str().bytes().fold(0x5EED, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as u64)
    })
}