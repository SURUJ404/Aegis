//! Market events flowing from the market-data pipeline.

use lq_types::{Exchange, Symbol, TimestampMs};

use crate::models::{MarketTick, OrderBookDelta, OrderBookSnapshot, Trade};

/// Connectivity / data-quality status of a feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedStatus {
    /// Feed connected and delivering in-sequence data.
    Healthy,
    /// Feed is stale (no recent events).
    Stale,
    /// Feed disconnected; book should be treated as suspect.
    Disconnected,
    /// Feed resumed; consumers should expect a resync snapshot.
    Resync,
}

/// Every market-data event the bus carries.
#[derive(Debug, Clone)]
pub enum MarketEvent {
    Snapshot(OrderBookSnapshot),
    Delta(OrderBookDelta),
    Trade(Trade),
    Tick(MarketTick),
    Status {
        venue: Exchange,
        symbol: Symbol,
        status: FeedStatus,
        ts: TimestampMs,
    },
}

/// Event-kind discriminator for metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketEventKind {
    Snapshot,
    Delta,
    Trade,
    Tick,
    Status,
}

impl MarketEvent {
    pub fn kind(&self) -> MarketEventKind {
        match self {
            Self::Snapshot(_) => MarketEventKind::Snapshot,
            Self::Delta(_) => MarketEventKind::Delta,
            Self::Trade(_) => MarketEventKind::Trade,
            Self::Tick(_) => MarketEventKind::Tick,
            Self::Status { .. } => MarketEventKind::Status,
        }
    }

    pub fn event_ts(&self) -> TimestampMs {
        match self {
            Self::Snapshot(s) => s.event_ts,
            Self::Delta(d) => d.event_ts,
            Self::Trade(t) => t.event_ts,
            Self::Tick(t) => t.event_ts,
            Self::Status { ts, .. } => *ts,
        }
    }
}