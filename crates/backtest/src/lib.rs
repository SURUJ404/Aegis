//! Deterministic backtesting.
//!
//! Replays a recorded [`MarketEvent`] sequence through the real engine
//! components (order book, strategy, risk, paper venue with **latency
//! disabled and zero rejection**) so results are byte-for-byte reproducible
//! given the same inputs. PnL is computed from the venue's fill accounting
//! plus a mark-to-market equity curve.
//!
//! No networking, no wall-clock sleeps, no hidden randomness: the venue's RNG
//! is seeded from [`BacktestConfig::seed`].

pub mod metrics;
pub mod runner;

pub use metrics::{BacktestResult, EquitySample, PerfMetrics};
pub use runner::{BacktestConfig, BacktestRunner};