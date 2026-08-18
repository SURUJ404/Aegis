//! Market event and execution event type discriminators.

use serde::{Deserialize, Serialize};

/// Kind of market event flowing through the market-data pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketEventType {
    /// Full order-book snapshot.
    Snapshot,
    /// Incremental order-book update.
    Delta,
    /// A trade (aggressor fill) printed on the tape.
    Trade,
    /// Lightweight price tick (derived, e.g. from a trade stream).
    Tick,
    /// Venue connectivity / book status change.
    Status,
}

/// Kind of execution event emitted by an execution venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    /// Order accepted for processing (venue ack).
    New,
    /// Order placed on the book.
    Acknowledged,
    /// Partial fill against this order.
    PartialFill,
    /// Order fully filled.
    Fill,
    /// Cancel request accepted.
    Cancel,
    /// Order cancelled.
    Cancelled,
    /// Order rejected.
    Rejected,
    /// Order expired.
    Expired,
}

impl ExecutionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Acknowledged => "acknowledged",
            Self::PartialFill => "partial_fill",
            Self::Fill => "fill",
            Self::Cancel => "cancel",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}