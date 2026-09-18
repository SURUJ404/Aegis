//! Latency measurement model.

use lq_types::TimestampMs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

/// Which stage of the pipeline a latency measurement covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyStage {
    /// WebSocket message received from the exchange.
    WebSocketReceive,
    /// Raw message decode.
    Decode,
    /// Normalization of decoded message.
    Normalize,
    /// Sequence number check.
    SequenceCheck,
    /// Published to internal queue/topic.
    QueuePublish,
    /// Consumed from internal queue/topic.
    QueueConsume,
    /// Order-book ingestion.
    BookIngest,
    /// Order-book analytics / MarketState computation.
    BookAnalytics,
    /// Strategy decision.
    StrategyDecision,
    /// Risk validation.
    RiskValidation,
    /// Execution submission (client -> venue).
    ExecutionSubmit,
    /// Submission -> acknowledgement.
    ExecutionAck,
    /// Ack -> fill.
    ExecutionFill,
    /// Venue receive -> fill observed (end-to-end).
    EndToEnd,
}

impl LatencyStage {
    /// Human-readable label for Prometheus metrics.
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::WebSocketReceive => "websocket_receive",
            Self::Decode => "decode",
            Self::Normalize => "normalize",
            Self::SequenceCheck => "sequence_check",
            Self::QueuePublish => "queue_publish",
            Self::QueueConsume => "queue_consume",
            Self::BookIngest => "book_ingest",
            Self::BookAnalytics => "book_analytics",
            Self::StrategyDecision => "strategy_decision",
            Self::RiskValidation => "risk_validation",
            Self::ExecutionSubmit => "execution_submit",
            Self::ExecutionAck => "execution_ack",
            Self::ExecutionFill => "execution_fill",
            Self::EndToEnd => "end_to_end",
        }
    }
}

/// A single latency observation (nanoseconds).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    pub stage: LatencyStage,
    /// Duration in nanoseconds.
    pub nanos: u64,
    pub event_ts: TimestampMs,
}

/// Tracks start times for named pipeline stages and computes elapsed nanos.
///
/// Usage:
/// ```ignore
/// let mut rec = LatencyRecorder::new();
/// rec.start(LatencyStage::WebSocketReceive);
/// // ... do work ...
/// let elapsed = rec.stop(LatencyStage::WebSocketReceive);
/// ```
#[derive(Debug, Clone)]
pub struct LatencyRecorder {
    starts: HashMap<LatencyStage, Instant>,
}

impl LatencyRecorder {
    pub fn new() -> Self {
        Self {
            starts: HashMap::new(),
        }
    }

    /// Record the start time for a stage.
    pub fn start(&mut self, stage: LatencyStage) {
        self.starts.insert(stage, Instant::now());
    }

    /// Stop a stage and return elapsed nanoseconds. Returns `None` if the
    /// stage was never started.
    pub fn stop(&mut self, stage: LatencyStage) -> Option<u64> {
        let start = self.starts.remove(&stage)?;
        Some(start.elapsed().as_nanos() as u64)
    }

    /// Record a start and immediately build a [`LatencyMeasurement`] for the
    /// given stage, using the current wall-clock as the `event_ts`.
    pub fn record(&mut self, stage: LatencyStage) -> Option<LatencyMeasurement> {
        let nanos = self.stop(stage)?;
        Some(LatencyMeasurement {
            stage,
            nanos,
            event_ts: TimestampMs::now(),
        })
    }
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_stage_labels() {
        assert_eq!(LatencyStage::WebSocketReceive.as_label(), "websocket_receive");
        assert_eq!(LatencyStage::EndToEnd.as_label(), "end_to_end");
    }

    #[test]
    fn recorder_start_stop() {
        let mut rec = LatencyRecorder::new();
        rec.start(LatencyStage::Decode);
        let _nanos = rec.stop(LatencyStage::Decode).unwrap();
    }

    #[test]
    fn recorder_record_returns_measurement() {
        let mut rec = LatencyRecorder::new();
        rec.start(LatencyStage::RiskValidation);
        let m = rec.record(LatencyStage::RiskValidation).unwrap();
        assert_eq!(m.stage, LatencyStage::RiskValidation);
    }

    #[test]
    fn recorder_stop_without_start_returns_none() {
        let mut rec = LatencyRecorder::new();
        assert!(rec.stop(LatencyStage::ExecutionFill).is_none());
    }
}
