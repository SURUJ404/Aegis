//! Order-book snapshot and delta models.

use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};

/// One price level of an order book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBookLevel {
    pub price: Price,
    pub qty: Qty,
}

impl OrderBookLevel {
    pub fn new(price: Price, qty: Qty) -> Self {
        Self { price, qty }
    }
}

/// Full L2 snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub venue: Exchange,
    pub symbol: Symbol,
    /// Venue sequence number; used for gap detection and resync.
    pub sequence: u64,
    /// Wall clock observed arrival.
    pub event_ts: TimestampMs,
    /// Wall clock reported by the venue (if available).
    pub exchange_ts: TimestampMs,
    /// Bids sorted best-first (descending price).
    pub bids: Vec<OrderBookLevel>,
    /// Asks sorted best-first (ascending price).
    pub asks: Vec<OrderBookLevel>,
}

/// A single level change inside a delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelChange {
    pub side: Side,
    pub price: Price,
    /// Quantity after the change. `0` means "remove the level".
    pub qty: Qty,
}

/// Incremental L2 update. Applied to a local book in sequence order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookDelta {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub sequence: u64,
    pub event_ts: TimestampMs,
    pub exchange_ts: TimestampMs,
    pub changes: Vec<LevelChange>,
    /// If true, this delta replaces the book entirely (e.g. after a resync).
    pub clear: bool,
}