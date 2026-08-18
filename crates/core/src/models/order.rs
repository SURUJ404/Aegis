//! Order model.

use lq_types::{Exchange, OrderStatus, OrderType, Price, Qty, Side, Symbol, TimeInForce, TimestampMs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A client-side order record. The canonical copy lives in the order state
/// machine (`lq-execution`); this is the durable, observable projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: Uuid,
    pub client_order_id: String,
    /// Venue-side order id (populated after acknowledgement).
    pub venue_order_id: String,
    pub venue: Exchange,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Qty,
    pub filled_quantity: Qty,
    pub avg_fill_price: Option<Price>,
    pub status: OrderStatus,
    pub time_in_force: TimeInForce,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

impl Order {
    pub fn new(
        venue: Exchange,
        symbol: Symbol,
        side: Side,
        order_type: OrderType,
        price: Option<Price>,
        quantity: Qty,
    ) -> Self {
        let now = TimestampMs::now();
        Self {
            order_id: Uuid::new_v4(),
            client_order_id: Uuid::new_v4().to_string(),
            venue_order_id: String::new(),
            venue,
            symbol,
            side,
            order_type,
            price,
            quantity,
            filled_quantity: Qty::default(),
            avg_fill_price: None,
            status: OrderStatus::Created,
            time_in_force: TimeInForce::Gtc,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn remaining(&self) -> Qty {
        self.quantity - self.filled_quantity
    }

    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}