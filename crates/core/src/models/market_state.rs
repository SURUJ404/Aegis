//! Market microstructure state consumed by strategies.

use lq_types::{Exchange, Price, Qty, Symbol, TimestampMs};
use serde::{Deserialize, Serialize};

/// Qualitative regime classification derived from microstructure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRegime {
    /// Liquid, balanced two-sided market.
    Normal,
    /// Spread expanded and/or volatility elevated.
    Volatile,
    /// Heavy buying pressure at the touch.
    OneSidedBid,
    /// Heavy selling pressure at the touch.
    OneSidedAsk,
    /// Data too old to act on; strategies must not quote.
    Stale,
    /// No usable two-sided liquidity.
    NoLiquidity,
}

/// A single, cheap-to-recompute snapshot of microstructure. Strategies receive
/// `MarketState` and must not compute anything they do not need from raw data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub event_ts: TimestampMs,
    pub best_bid: Price,
    pub best_ask: Price,
    pub mid: Price,
    pub spread: Price,
    pub spread_bps: f64,
    pub orderbook_imbalance: f64,
    pub microprice: Price,
    pub vwap: Price,
    pub depth_bid: Qty,
    pub depth_ask: Qty,
    pub num_bid_levels: u32,
    pub num_ask_levels: u32,
    pub buy_volume: Qty,
    pub sell_volume: Qty,
    pub trade_intensity: f64,
    pub realized_volatility: f64,
    pub price_impact_estimate: f64,
    pub regime: MarketRegime,
    pub stale: bool,
}

impl MarketState {
    /// True when there is enough two-sided liquidity to quote against.
    pub fn is_quotable(&self) -> bool {
        !self.stale
            && self.regime != MarketRegime::Stale
            && self.regime != MarketRegime::NoLiquidity
            && self.best_bid > Price::ZERO
            && self.best_ask > self.best_bid
    }
}
