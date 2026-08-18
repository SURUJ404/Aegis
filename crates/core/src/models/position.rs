//! Quote, position and inventory models.

use lq_types::{Amount, Exchange, Price, Qty, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A resting two-sided quote published by a strategy. Risk-engine validated
/// before it may become orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: Uuid,
    pub venue: Exchange,
    pub symbol: Symbol,
    pub bid_price: Price,
    pub bid_qty: Qty,
    pub ask_price: Price,
    pub ask_qty: Qty,
    pub strategy: String,
    pub event_ts: TimestampMs,
}

/// An open position on a venue for a symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub venue: Exchange,
    pub symbol: Symbol,
    /// Positive = long, negative = short.
    pub net_qty: Qty,
    pub avg_entry: Price,
    pub realized_pnl: Amount,
    pub event_ts: TimestampMs,
}

impl Default for Position {
    fn default() -> Self {
        Self {
            venue: Exchange::Paper,
            symbol: Symbol("".into()),
            net_qty: Qty::default(),
            avg_entry: Price::default(),
            realized_pnl: Amount::default(),
            event_ts: TimestampMs::default(),
        }
    }
}

impl Position {
    pub fn is_flat(&self) -> bool {
        self.net_qty.is_zero()
    }

    pub fn notional(&self, mark: Price) -> Amount {
        (self.net_qty * mark).abs()
    }
}

/// Per-symbol inventory aggregated across venues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    pub symbol: Symbol,
    /// Signed: positive long, negative short.
    pub net_qty: Qty,
    pub avg_entry: Price,
    pub realized_pnl: Amount,
    pub event_ts: TimestampMs,
}

impl Default for Inventory {
    fn default() -> Self {
        Self {
            symbol: Symbol("".into()),
            net_qty: Qty::default(),
            avg_entry: Price::default(),
            realized_pnl: Amount::default(),
            event_ts: TimestampMs::default(),
        }
    }
}

impl Inventory {
    pub fn signed_notional(&self, mark: Price) -> Amount {
        self.net_qty * mark
    }

    /// Absolute exposure in quote currency at the given mark.
    pub fn abs_notional(&self, mark: Price) -> Amount {
        self.signed_notional(mark).abs()
    }
}