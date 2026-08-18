//! Exchange metadata: fee schedules, instrument specifications and venue
//! connectivity parameters.
//!
//! This crate is pure data. Adapters in `lq-market-data` and execution code in
//! `lq-execution` consume it; nothing here touches the network.

pub mod fee;
pub mod spec;

pub use fee::{FeeSchedule, FeeTier};
pub use spec::{InstrumentSpec, PriceTick};

use lq_types::Exchange;

/// Static connectivity metadata per venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueMeta {
    pub venue: Exchange,
    /// Public market-data WebSocket URL.
    pub ws_market_data: &'static str,
    /// Private (authenticated) WebSocket URL, if any.
    pub ws_private: &'static str,
    /// REST base URL.
    pub rest_base: &'static str,
}

impl VenueMeta {
    /// Connectivity metadata for known venues.
    ///
    /// URLs are publicly documented endpoints. They are *not* credentials.
    /// Live order routing remains disabled unless explicitly enabled.
    pub fn for_venue(venue: Exchange) -> Option<VenueMeta> {
        let meta = match venue {
            Exchange::Paper => VenueMeta {
                venue,
                ws_market_data: "ws://127.0.0.1:9000/ws",
                ws_private: "ws://127.0.0.1:9000/ws",
                rest_base: "http://127.0.0.1:9000",
            },
            Exchange::Simulated => VenueMeta {
                venue,
                ws_market_data: "ws://127.0.0.1:9001/ws",
                ws_private: "ws://127.0.0.1:9001/ws",
                rest_base: "http://127.0.0.1:9001",
            },
            Exchange::Okx => VenueMeta {
                venue,
                ws_market_data: "wss://ws.okx.com:8443/ws/v5/public",
                ws_private: "wss://ws.okx.com:8443/ws/v5/private",
                rest_base: "https://www.okx.com",
            },
            Exchange::Binance => VenueMeta {
                venue,
                ws_market_data: "wss://stream.binance.com:9443/ws",
                ws_private: "wss://stream.binance.com:9443/ws",
                rest_base: "https://api.binance.com",
            },
            Exchange::Bybit => VenueMeta {
                venue,
                ws_market_data: "wss://stream.bybit.com/v5/public/spot",
                ws_private: "wss://stream.bybit.com/v5/private",
                rest_base: "https://api.bybit.com",
            },
        };
        Some(meta)
    }
}