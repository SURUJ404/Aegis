//! Strategy outputs: signals and quote intents.

use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A generic directional / informational signal produced by analyzers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySignal {
    pub id: Uuid,
    pub symbol: Symbol,
    pub venue: Exchange,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub kind: String,
    pub ts: TimestampMs,
}

/// One leg of a two-sided quote.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QuoteLeg {
    pub price: Price,
    pub qty: Qty,
}

/// A concrete two-sided quote a strategy wants to place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteIntent {
    pub id: Uuid,
    pub symbol: Symbol,
    pub venue: Exchange,
    pub bid: Option<QuoteLeg>,
    pub ask: Option<QuoteLeg>,
    pub strategy: String,
    pub reason: String,
    pub ts: TimestampMs,
}

impl QuoteIntent {
    pub fn is_two_sided(&self) -> bool {
        self.bid.is_some() && self.ask.is_some()
    }
}

/// A one-shot directional order a strategy wants to execute immediately
/// (e.g. cross-venue arbitrage).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketOrderSignal {
    pub symbol: Symbol,
    pub venue: Exchange,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub reason: String,
}

/// What a strategy returns from a market-state update.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StrategyDecision {
    /// Replace the current resting quote with this intent.
    Quote(QuoteIntent),
    /// Submit an immediate market/limit order (cross-venue arbitrage).
    MarketOrder(MarketOrderSignal),
    /// Pull all quotes and stand aside.
    StandDown { reason: String },
    /// Keep the current quote as-is.
    Hold,
}