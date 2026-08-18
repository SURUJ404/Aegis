//! Latency measurement model.

use lq_types::TimestampMs;
use serde::{Deserialize, Serialize};

/// Which stage of the pipeline a latency measurement covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyStage {
    /// Network / venue -> our process.
    ExchangeReceive,
    /// Raw message decode + normalization.
    Decode,
    /// Order-book application.
    OrderBookUpdate,
    /// MarketState computation.
    MarketState,
    /// Strategy invocation.
    Strategy,
    /// Risk validation.
    Risk,
    /// Execution submission (client -> venue).
    ExecutionSubmit,
    /// Submission -> acknowledgement.
    ExecutionAck,
    /// Ack -> fill.
    ExecutionFill,
    /// Venue receive -> fill observed.
    EndToEnd,
}

/// A single latency observation (nanoseconds).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    pub stage: LatencyStage,
    /// Duration in nanoseconds.
    pub nanos: u64,
    pub event_ts: TimestampMs,
}