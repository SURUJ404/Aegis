//! Market data.
//!
//! Asynchronous WebSocket adapters for OKX, Binance and Bybit that decode
//! venue-native payloads into the engine's normalized
//! [`MarketEvent`](lq_core::event::MarketEvent) stream. A transport layer
//! ([`ws_client`]) handles connect / subscribe / app-level ping / staleness
//! detection / exponential-backoff reconnect with resubscribe.

pub mod adapters;
pub mod ws_client;

pub use ws_client::{run_ws, FeedDecoder, WsConfig};