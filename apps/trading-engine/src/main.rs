//! Main engine binary: wires market data, books, strategy, risk, execution,
//! observability and the control-plane API.

mod engine;

use clap::Parser;
use lq_core::config::EngineConfig;

#[derive(Parser)]
#[command(name = "trading-engine", about = "Multi-venue liquidity / market-making engine (paper mode)")]
struct Cli {
    /// Path to a TOML config file. Defaults to built-in defaults.
    #[arg(long, env = "LQ_CONFIG")]
    config: Option<String>,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    lq_telemetry::init_logging(&cfg.telemetry)?;
    engine::run(cfg).await
}