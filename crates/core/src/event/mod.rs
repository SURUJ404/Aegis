//! Event definitions.

pub mod control;
pub mod execution;
pub mod market;

pub use control::ControlEvent;
pub use execution::ExecutionEvent;
pub use market::{FeedStatus, MarketEvent, MarketEventKind};

/// Counters attached to each topic.
#[derive(Debug, Clone, Default)]
pub struct PublishStats {
    pub published: u64,
    pub dropped: u64,
    pub subscribers: usize,
}