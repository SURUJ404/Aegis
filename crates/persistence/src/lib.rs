//! Durability and hot-state layers.
//!
//! `lq-persistence` provides:
//! - [`MarketDataStore`] / [`ExecutionStore`] — async trait contracts for
//!   persisting market and execution events.
//! - [`postgres::PostgresStore`] — a PostgreSQL-backed durable store.
//! - [`redis::RedisHotState`] — Redis-backed hot state (last price, halt
//!   flags, working-order counts) readable by the control API and dashboards.
//! - [`sink::PersistenceSink`] — bus subscribers that forward events to a
//!   store without blocking producers.

pub mod postgres;
pub mod redis;
pub mod sink;
pub mod store;

pub use postgres::PostgresStore;
pub use redis::RedisHotState;
pub use sink::PersistenceSink;
pub use store::{ExecutionStore, MarketDataStore, StoreError};