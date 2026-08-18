//! Domain models, event system and shared engine state.
//!
//! `lq-core` is the single source of truth for the *shape* of data flowing through
//! the engine. It contains no I/O and no strategy logic: it defines types, events,
//! a bounded event bus and the shared in-memory state that observability and the
//! control API read from.

pub mod bus;
pub mod config;
pub mod event;
pub mod models;
pub mod state;

pub use bus::{EventBus, PublishResult, Topic};
pub use config::{
    EngineConfig, MarketMakingConfig, PaperSimConfig, RiskConfig, StrategyConfig,
};
pub use event::{
    ControlEvent, ExecutionEvent, MarketEvent, MarketEventKind, PublishStats,
};
pub use models::{
    Execution, FillEvent, Inventory, LatencyMeasurement, LatencyStage, MarketOrderSignal,
    MarketRegime, MarketState, MarketTick, Order, OrderBookDelta, OrderBookLevel,
    OrderBookSnapshot, Position, Quote, QuoteIntent, QuoteLeg, StrategyDecision,
    StrategySignal, Trade,
};
pub use state::EngineState;