//! Store contracts and error type.

use lq_core::event::{ExecutionEvent, MarketEvent};

/// Errors surfaced by the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("postgres error: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("store is not connected: {0}")]
    NotConnected(String),
}

/// Persist normalized market events (snapshots, deltas, trades, ticks).
#[async_trait::async_trait]
pub trait MarketDataStore: Send + Sync {
    async fn save_market_event(&self, event: &MarketEvent) -> Result<(), StoreError>;
}

/// Persist execution events (order lifecycle + fills).
#[async_trait::async_trait]
pub trait ExecutionStore: Send + Sync {
    async fn save_execution_event(&self, event: &ExecutionEvent) -> Result<(), StoreError>;
}