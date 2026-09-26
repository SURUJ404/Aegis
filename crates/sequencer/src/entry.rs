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

/// Command payloads that may enter the log (Stage 1–2 set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryPayload {
    PlaceOrder(PlaceOrderCmd),
    CancelOrder {
        order_id: Uuid,
    },
    /// Atomic cancel/replace (Stage 2): cancel `old_order_id`, then place `new`.
    ReplaceOrder {
        old_order_id: Uuid,
        new: Box<PlaceOrderCmd>,
    },
    Fill(FillCmd),
    MarketTick(MarketTickCmd),
}

/// Self-trade prevention action when an incoming order would match a resting
/// order with the same non-empty `owner`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StpPolicy {
    /// Match normally (no self-trade prevention).
    #[default]
    None,
    /// Cancel the resting (maker) order and let the taker continue past it.
    CancelResting,
    /// Abort the taker when it would hit a resting self order.
    CancelTaker,
    /// Cancel both the resting order and the remainder of the taker.
    CancelBoth,
}

/// Client order placement command (unsigned until the gateway signs in Stage 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceOrderCmd {
    pub order_id: Uuid,
    pub client_order_id: String,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Qty,
    pub time_in_force: TimeInForce,
    /// Owner identity used for self-trade prevention. Empty = no owner
    /// (STP never triggers against an empty owner).
    #[serde(default)]
    pub owner: String,
    /// Self-trade prevention policy (default: none).
    #[serde(default)]
    pub stp: StpPolicy,
    /// Short-term orders carry an absolute logical expiration (entry `ts_ms`
    /// domain). `None` = stateful (persists until cancel/fill/explicit term).
    #[serde(default)]
    pub expiration_ms: Option<u64>,
}

impl Default for PlaceOrderCmd {
    fn default() -> Self {
        Self {
            order_id: Uuid::nil(),
            client_order_id: String::new(),
            side: Side::Bid,
            order_type: OrderType::Limit,
            price: None,
            quantity: Decimal::ONE,
            time_in_force: TimeInForce::Gtc,
            owner: String::new(),
            stp: StpPolicy::None,
            expiration_ms: None,
        }
    }
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
            EntryPayload::ReplaceOrder { .. } => "replace_order",
            EntryPayload::Fill(_) => "fill",
            EntryPayload::MarketTick(_) => "market_tick",
        }
    }
}
