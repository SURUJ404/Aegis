//! Standalone market data collection service.
//!
//! Connects to live venue WebSocket streams, normalizes them into the shared
//! `MarketEvent` stream, and (optionally) persists every event to Postgres
//! plus hot state to Redis. Unlike the trading engine it never places orders:
//! it is a pure market-data appliance.

use std::sync::Arc;

use clap::Parser;
use lq_core::bus::EventBus;
use lq_core::config::EngineConfig;
use lq_core::event::{FeedStatus, MarketEvent};
use lq_market_data::adapters::{binance, bybit, okx};
use lq_market_data::{run_ws, WsConfig};
use lq_persistence::postgres::PostgresStore;
use lq_persistence::sink::PersistenceSink;
use lq_types::{Exchange, Symbol, TimestampMs};

#[derive(Parser)]
#[command(
    name = "market-data-service",
    about = "Venue market data collection and persistence (no trading)"
)]
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

/// Build the WS transport settings from the shared market-data section.
fn ws_config(cfg: &EngineConfig, url: String) -> WsConfig {
    WsConfig {
        url,
        reconnect_base_ms: cfg.market_data.ws_reconnect_base_ms,
        reconnect_max_ms: cfg.market_data.ws_reconnect_max_ms,
        stale_after_ms: cfg.market_data.stale_after_ms,
        ping_interval_ms: cfg.market_data.ping_interval_ms,
    }
}

/// Publish a single `Status` transition for every feed on the market topic.
fn publish_status(
    bus: &Arc<EventBus>,
    venue: Exchange,
    symbol: &Symbol,
    status: FeedStatus,
) {
    let _ = bus.market().try_publish(MarketEvent::Status {
        venue,
        symbol: symbol.clone(),
        status,
        ts: TimestampMs::now(),
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_tls_provider();
    let cli = Cli::parse();
    let cfg = load_config(&cli)?;
    lq_telemetry::init_logging(&cfg.telemetry)?;

    // Live exchanges only: there is nothing to collect from paper/simulated.
    let live_venues: Vec<(Exchange, Symbol)> = cfg
        .venues
        .iter()
        .filter(|v| v.is_live())
        .flat_map(|venue| {
            cfg.symbols
                .iter()
                .map(move |sym| (*venue, Symbol(sym.clone())))
        })
        .collect();

    if live_venues.is_empty() {
        tracing::warn!("no live venues configured; nothing to collect");
    }

    let bus = Arc::new(EventBus::new());

    // Describe feeds for the startup log before moving `live_venues`.
    let feed_desc: Vec<String> = live_venues
        .iter()
        .map(|(v, s)| format!("{v}:{s}"))
        .collect();

    // One resilient read loop per (venue, symbol) stream. The transport config
    // and decoder are built before the async block so `cfg` is only borrowed.
    let mut handles = Vec::new();
    for (venue, symbol) in live_venues {
        let feed_bus = Arc::clone(&bus);
        let (ws, decoder): (WsConfig, Box<dyn lq_market_data::FeedDecoder>) = match venue {
            Exchange::Okx => (
                ws_config(&cfg, okx::OKX_PUBLIC_WS.to_string()),
                Box::new(okx::OkxDecoder::new(feed_bus, symbol.clone())),
            ),
            Exchange::Binance => (
                ws_config(&cfg, binance::stream_url(&symbol)),
                Box::new(binance::BinanceDecoder::new(feed_bus, symbol.clone())),
            ),
            Exchange::Bybit => (
                ws_config(&cfg, bybit::BYBIT_PUBLIC_WS.to_string()),
                Box::new(bybit::BybitDecoder::new(feed_bus, symbol.clone())),
            ),
            Exchange::Paper | Exchange::Simulated => {
                continue;
            }
        };
        let task_bus = Arc::clone(&bus);
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_ws(ws, task_bus, decoder).await {
                tracing::error!(err = %e, "feed exited");
            }
        }));
        publish_status(&bus, venue, &symbol, FeedStatus::Disconnected);
    }

    // Optional persistence: forwards every bus event to Postgres and mirrors
    // hot state (last price, halt flag) into Redis. The sink lives for the
    // whole process so the forwarding workers stay alive.
    let _sink = if cfg.persistence.enabled {
        if cfg.persistence.postgres_url.is_empty() {
            tracing::error!("persistence enabled but postgres_url is empty");
            None
        } else {
            match PostgresStore::connect(&cfg.persistence.postgres_url).await {
                Ok(store) => match store.migrate().await {
                    Ok(()) => {
                        tracing::info!("postgres connected");
                        Some(PersistenceSink::spawn(Arc::clone(&bus), Arc::new(store)))
                    }
                    Err(e) => {
                        tracing::error!(err = %e, "postgres migrate failed");
                        None
                    }
                },
                Err(e) => {
                    tracing::error!(err = %e, "postgres connect failed");
                    None
                }
            }
        }
    } else {
        tracing::info!("persistence disabled");
        None
    };

    // Prometheus metrics endpoint.
    let metrics = Arc::new(lq_telemetry::Metrics::new());
    let metrics_server = lq_telemetry::MetricsServer::new(Arc::clone(&metrics));
    let metrics_bind = cfg.telemetry.metrics_bind.clone();

    let metrics_task = tokio::spawn(async move {
        if let Err(e) = metrics_server.serve(&metrics_bind).await {
            tracing::error!(err = %e, "metrics server failed");
        }
    });
    tracing::info!(venues = ?feed_desc, "market data service started");

    if handles.is_empty() {
        tracing::info!("no feeds to supervise; waiting for ctrl-c");
        tokio::signal::ctrl_c().await?;
        tracing::info!("shutting down");
    } else {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
            }
            _ = async {
                // Wait for all feed loops to finish (they only exit on unrecoverable errors).
                for h in handles {
                    let _ = h.await;
                }
            } => {
                tracing::error!("a feed exited; shutting down");
            }
        }
    }

    metrics_task.abort();
    Ok(())
}

/// Pick a single rustls crypto provider so TLS connections do not panic at
/// runtime when rustls cannot auto-detect a provider.
fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}