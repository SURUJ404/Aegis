//! High-performance local order book, sequence management and microstructure
//! analytics.

pub mod analytics;
pub mod book;
pub mod engine;

pub use analytics::{AnalyticsConfig, MarketStateEngine};
pub use book::{DeltaOutcome, OrderBook, QTY_SCALE};
pub use engine::{BookStore, IngestOutcome};