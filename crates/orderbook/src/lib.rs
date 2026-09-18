//! High-performance local order book, sequence management and microstructure
//! analytics.

pub mod analytics;
pub mod book;
pub mod book_impls;
pub mod engine;
pub mod replay;

pub use analytics::{AnalyticsConfig, MarketStateEngine};
pub use book::{DeltaOutcome, OrderBook, QTY_SCALE};
pub use book_impls::{ArrayBackedBook, BTreeMapBook, HashMapSortedVecBook, OrderBookImpl};
pub use engine::{BookStore, IngestOutcome};