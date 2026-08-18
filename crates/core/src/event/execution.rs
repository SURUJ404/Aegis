//! Execution events emitted by venues.

use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use uuid::Uuid;

use crate::models::FillEvent;

/// Every execution event the bus carries.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
    /// Order accepted for processing.
    New {
        order_id: Uuid,
        venue: Exchange,
        ts: TimestampMs,
    },
    /// Order acked and resting on the book.
    Acknowledged {
        order_id: Uuid,
        venue: Exchange,
        ts: TimestampMs,
    },
    /// A (partial or full) fill.
    Fill(FillEvent),
    /// Cancel request accepted.
    CancelRequested {
        order_id: Uuid,
        venue: Exchange,
        ts: TimestampMs,
    },
    /// Order cancelled.
    Cancelled {
        order_id: Uuid,
        venue: Exchange,
        ts: TimestampMs,
    },
    /// Order rejected. `reason` is always populated.
    Rejected {
        order_id: Uuid,
        venue: Exchange,
        reason: String,
        ts: TimestampMs,
    },
    /// Order expired.
    Expired {
        order_id: Uuid,
        venue: Exchange,
        ts: TimestampMs,
    },
    /// A reported trade we did not submit (position reconciliation).
    Trade {
        venue: Exchange,
        symbol: Symbol,
        side: Side,
        price: Price,
        qty: Qty,
        ts: TimestampMs,
    },
}

impl ExecutionEvent {
    pub fn order_id(&self) -> Option<Uuid> {
        match self {
            Self::New { order_id, .. }
            | Self::Acknowledged { order_id, .. }
            | Self::CancelRequested { order_id, .. }
            | Self::Cancelled { order_id, .. }
            | Self::Rejected { order_id, .. }
            | Self::Expired { order_id, .. } => Some(*order_id),
            Self::Fill(f) => Some(f.order_id),
            Self::Trade { .. } => None,
        }
    }

    pub fn venue(&self) -> Exchange {
        match self {
            Self::Fill(f) => f.venue,
            Self::Trade { venue, .. } => *venue,
            Self::New { venue, .. }
            | Self::Acknowledged { venue, .. }
            | Self::CancelRequested { venue, .. }
            | Self::Cancelled { venue, .. }
            | Self::Rejected { venue, .. }
            | Self::Expired { venue, .. } => *venue,
        }
    }
}