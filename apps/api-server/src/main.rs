//! Standalone control-plane API server.
//!
//! Serves the engine's HTTP API (state, positions, inventory, orders,
//! market-state, risk, control) backed by an empty `EngineState` plus an
//! `EventBus`. Control posts are published onto the control topic and echoed to
//! the log; consumers (the trading engine or a test harness) subscribe on that
//! topic. This is the smallest control plane for inspecting state and driving
//! control flows without running an engine.

use std::sync::Arc;

use clap::Parser;
use lq_core::bus::EventBus;
use lq_core::config::EngineConfig;
use lq_core::event::ControlEvent;
use lq_core::state::EngineState;
use lq_api::ApiState;

#[derive(Parser)]
#[command(
    name = "api-server",
    about = "Standalone control-plane API server"
)]
struct Cli {
    /// Path to a TOML config file. Defaults to built-in defaults.
    #[arg(long, env = "LQ_CONFIG")]
    config: Option<String>,

    /// Override the API bind address.
    #[arg(long)]
    bind: Option<String>,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    lq_telemetry::init_logging(&cfg.telemetry)?;

    let bus = Arc::new(EventBus::new());
    let state = EngineState::new();

    // Echo control events so a standalone server is observable.
    {
        let ctrl_bus = Arc::clone(&bus);
        tokio::spawn(async move {
            let mut sub = ctrl_bus.control().subscribe();
            while let Some(event) = sub.recv().await {
                match event {
                    ControlEvent::Start => tracing::info!("control: start"),
                    ControlEvent::Stop => tracing::info!("control: stop"),
                    ControlEvent::Reset => tracing::info!("control: reset"),
                    ControlEvent::KillSwitch { reason } => {
                        tracing::warn!(reason = %reason, "control: kill switch")
                    }
                }
            }
        });
    }

    // Prometheus metrics endpoint.
    let metrics = Arc::new(lq_telemetry::Metrics::new());
    let metrics_server = lq_telemetry::MetricsServer::new(Arc::clone(&metrics));
    let metrics_bind = cfg.telemetry.metrics_bind.clone();
    let metrics_task = tokio::spawn(async move {
        if let Err(e) = metrics_server.serve(&metrics_bind).await {
            tracing::error!(err = %e, "metrics server failed");
        }
    });

    // Control-plane API.
    let api_bind = cli.bind.clone().unwrap_or_else(|| cfg.api.bind.clone());
    let app = lq_api::build_router(ApiState::new(state, Arc::clone(&bus)));
    let listener = tokio::net::TcpListener::bind(&api_bind).await?;
    tracing::info!(bind = %api_bind, "api server listening");
    axum::serve(listener, app).await?;

    metrics_task.abort();
    Ok(())
}