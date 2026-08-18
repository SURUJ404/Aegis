//! Foundational value types shared across the whole engine.
//!
//! This crate holds only *pure value types*: enums, money aliases, instruments and
//! timestamps. Nothing in this crate has side effects or knows about networking,
//! storage or the async runtime. It is the stable base of the dependency graph.

pub mod exchange;
pub mod instrument;
pub mod market;
pub mod money;
pub mod orders;
pub mod side;
pub mod time;

pub use exchange::{Exchange, VenueInstrument};
pub use instrument::{Instrument, Symbol};
pub use market::{ExecutionType, MarketEventType};
pub use money::{Amount, Money, Price, Qty, QUOTE_DECIMALS};
pub use orders::{OrderType, OrderStatus, TimeInForce};
pub use side::Side;
pub use time::TimestampMs;