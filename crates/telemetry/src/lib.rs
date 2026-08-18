//! Observability infrastructure.
//!
//! `lq-telemetry` provides:
//! - [`logging::init_logging`] — global `tracing` setup (plain or JSON).
//! - [`metrics::Metrics`] — Prometheus registry wired to market / execution /
//!   latency / engine-state signals.
//! - [`metrics::MetricsServer`] — an axum endpoint exposing `/metrics` and
//!   `/healthz`.

pub mod logging;
pub mod metrics;

pub use logging::init_logging;
pub use metrics::{Metrics, MetricsServer, TelemetryError};