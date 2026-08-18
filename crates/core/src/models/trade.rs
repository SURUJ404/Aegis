//! Trade and tick models.

use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};

/// An aggressive trade printed on the tape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub price: Price,
    pub qty: Qty,
    /// Side of the aggressor (maker/taker).
    pub aggressor: Side,
    pub event_ts: TimestampMs,
    pub exchange_ts: TimestampMs,
}

/// A lightweight market tick: last price + touch. Derived from trade and
/// book streams; the cheap thing strategies subscribe to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketTick {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub last_price: Price,
    pub last_qty: Qty,
    pub best_bid: Price,
    pub best_ask: Price,
    pub event_ts: TimestampMs,
}