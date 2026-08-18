//! Execution / fill model.

use lq_types::{Amount, Exchange, ExecutionType, Price, Qty, Side, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single execution (fill, partial fill, cancel, reject) against an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub execution_id: Uuid,
    pub order_id: Uuid,
    pub client_order_id: String,
    pub venue: Exchange,
    pub symbol: Symbol,
    pub side: Side,
    pub exec_type: ExecutionType,
    pub price: Price,
    pub qty: Qty,
    pub fee: Amount,
    pub fee_currency: String,
    /// Wall clock the venue reported (may be zero for simulated events).
    pub exchange_ts: TimestampMs,
    /// Wall clock we observed the event.
    pub event_ts: TimestampMs,
}

impl Execution {
    /// Signed notional: positive for a buy, negative for a sell.
    pub fn signed_notional(&self) -> Amount {
        let notional = self.price * self.qty;
        match self.side {
            Side::Bid => notional,
            Side::Ask => -notional,
        }
    }
}

/// A fill event as emitted by venues; consumed by position tracking and PnL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEvent {
    pub execution_id: Uuid,
    pub order_id: Uuid,
    pub client_order_id: String,
    pub venue: Exchange,
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub fee: Amount,
    pub fee_currency: String,
    pub exchange_ts: TimestampMs,
    pub event_ts: TimestampMs,
}