//! Order-level order record held by [`crate::state::ClobState`].

use lq_sequencer::entry::{MarketId, StpPolicy};
use lq_types::{OrderStatus, OrderType, Price, Qty, Side, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single order as stored by the CLOB: lifecycle plus book location.
///
/// Invariants (checked by proptest):
/// - `filled_quantity <= quantity`
/// - `book_price.is_some()` ⇔ the order is resting in the book for its market
/// - terminal orders never rest
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClobOrder {
    pub order_id: Uuid,
    pub client_order_id: String,
    pub market: MarketId,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Qty,
    pub filled_quantity: Qty,
    pub status: OrderStatus,
    pub time_in_force: TimeInForce,
    /// Owner identity for self-trade prevention (empty = no owner).
    pub owner: String,
    pub stp: StpPolicy,
    /// Short-term expiry in the entry's logical time domain; `None` = stateful.
    pub expiration_ms: Option<u64>,
    /// Global sequence of the place/replace entry (time-priority tiebreak).
    pub place_global_seq: u64,
    pub created_ts_ms: u64,
    pub updated_ts_ms: u64,
    /// `Some(price)` while resting at that level of the book.
    pub book_price: Option<Price>,
}

impl ClobOrder {
    pub fn remaining(&self) -> Qty {
        (self.quantity - self.filled_quantity).max(Decimal::ZERO)
    }

    pub fn is_resting(&self) -> bool {
        self.book_price.is_some()
    }
}
