//! Domain models.

pub mod book;
pub mod execution;
pub mod latency;
pub mod market_state;
pub mod order;
pub mod position;
pub mod signal;
pub mod trade;

pub use book::{LevelChange, OrderBookDelta, OrderBookLevel, OrderBookSnapshot};
pub use execution::{Execution, FillEvent};
pub use latency::{LatencyMeasurement, LatencyStage};
pub use market_state::{MarketRegime, MarketState};
pub use order::Order;
pub use position::{Inventory, Position, Quote};
pub use signal::{MarketOrderSignal, QuoteIntent, QuoteLeg, StrategyDecision, StrategySignal};
pub use trade::{MarketTick, Trade};