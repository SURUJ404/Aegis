//! Log entry types: global + per-market sequencing and command payloads.

use lq_types::{Exchange, OrderType, Price, Qty, Side, Symbol, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identity of a market within the sequencer (venue + symbol).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MarketId {
    pub venue: Exchange,
    pub symbol: Symbol,
}

impl MarketId {
    pub fn new(venue: Exchange, symbol: Symbol) -> Self {
        Self { venue, symbol }
    }
}

impl std::fmt::Display for MarketId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.venue, self.symbol)
    }
}

/// One sequenced command. `global_seq` is assigned by the sequencer and is the
/// sole total order; `market_seq` is monotonic within `market`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub global_seq: u64,
    pub market_seq: u64,
    pub market: MarketId,
    /// Logical timestamp supplied by the producer. The state machine must not
    /// read the wall clock; time always comes from the entry.
    pub ts_ms: u64,
    pub payload: EntryPayload,
}

/// Command payloads that may enter the log (Stage 1 set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryPayload {
    PlaceOrder(PlaceOrderCmd),
    CancelOrder { order_id: Uuid },
    Fill(FillCmd),
    MarketTick(MarketTickCmd),
}

/// Client order placement command (unsigned in Stage 1; gateway signs in Stage 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceOrderCmd {
    pub order_id: Uuid,
    pub client_order_id: String,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Qty,
    pub time_in_force: TimeInForce,
}

/// Execution result logged so position updates are event-sourced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillCmd {
    pub order_id: Uuid,
    pub price: Price,
    pub quantity: Qty,
    pub fee: Decimal,
    pub liquidity: FillLiquidity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillLiquidity {
    Maker,
    Taker,
}

/// Mark/last price observation (oracle results land here from Stage 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketTickCmd {
    pub last: Price,
    pub bid: Option<Price>,
    pub ask: Option<Price>,
}

impl LogEntry {
    pub fn payload_kind(&self) -> &'static str {
        match self.payload {
            EntryPayload::PlaceOrder(_) => "place_order",
            EntryPayload::CancelOrder { .. } => "cancel_order",
            EntryPayload::Fill(_) => "fill",
            EntryPayload::MarketTick(_) => "market_tick",
        }
    }
}
