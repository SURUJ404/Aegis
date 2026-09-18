use lq_core::event::MarketEvent;
use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Solana-specific protocol identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolanaProtocol {
    RaydiumClmm,
    OrcaWhirlpools,
    Phoenix,
    OpenBook,
    Unknown,
}

impl SolanaProtocol {
    pub fn program_id(&self) -> &'static str {
        match self {
            Self::RaydiumClmm => "CAMMCzo5YL8w4Vfw8KJGkKpUJgZ3eZ3E8Z3E8Z3E8Z3E8",
            Self::OrcaWhirlpools => "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
            Self::Phoenix => "PhoeNiXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8Vj",
            Self::OpenBook => "srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX",
            Self::Unknown => "",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RaydiumClmm => "raydium_clmm",
            Self::OrcaWhirlpools => "orca_whirlpools",
            Self::Phoenix => "phoenix",
            Self::OpenBook => "openbook",
            Self::Unknown => "unknown",
        }
    }
}

/// A Solana market (protocol + pair)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SolanaMarket {
    pub protocol: SolanaProtocol,
    pub symbol: Symbol,
    pub pool_address: String,
    pub venue: Exchange,
}

impl SolanaMarket {
    pub fn new(protocol: SolanaProtocol, symbol: Symbol, pool_address: String) -> Self {
        let venue = match protocol {
            SolanaProtocol::RaydiumClmm => Exchange::Paper,
            SolanaProtocol::OrcaWhirlpools => Exchange::Paper,
            SolanaProtocol::Phoenix => Exchange::Paper,
            SolanaProtocol::OpenBook => Exchange::Paper,
            SolanaProtocol::Unknown => Exchange::Paper,
        };
        Self {
            protocol,
            symbol,
            pool_address,
            venue,
        }
    }
}

impl std::fmt::Display for SolanaMarket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.protocol.as_str(), self.symbol)
    }
}

/// Metadata captured with every Solana event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolanaEventMetadata {
    pub slot: u64,
    pub signature: String,
    pub program_id: String,
    pub receive_ts: TimestampMs,
    pub source_ts: Option<TimestampMs>,
    pub sequence: u64,
}

/// Event types from Solana protocols
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolanaEventType {
    OrderBookSnapshot,
    OrderBookDelta,
    Trade,
    Swap,
    LiquidityChange,
    Tick,
    Status,
}

/// Normalized Solana event that can be converted to MarketEvent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedSolanaEvent {
    pub metadata: SolanaEventMetadata,
    pub market: SolanaMarket,
    pub event_type: SolanaEventType,
    pub payload: SolanaEventPayload,
}

impl NormalizedSolanaEvent {
    pub fn to_market_event(self, venue: Exchange) -> lq_core::event::MarketEvent {
        use lq_core::models::{LevelChange, MarketTick, OrderBookDelta, OrderBookLevel, OrderBookSnapshot, Trade};

        let event_ts = self.metadata.receive_ts;
        let exchange_ts = self.metadata.source_ts.unwrap_or(event_ts);
        let symbol = self.market.symbol.clone();

        match self.payload {
            SolanaEventPayload::OrderBookSnapshot { bids, asks } => {
                MarketEvent::Snapshot(OrderBookSnapshot {
                    venue,
                    symbol,
                    sequence: self.metadata.sequence,
                    event_ts,
                    exchange_ts,
                    bids: bids.into_iter().map(|l| OrderBookLevel::new(l.price, l.qty)).collect(),
                    asks: asks.into_iter().map(|l| OrderBookLevel::new(l.price, l.qty)).collect(),
                })
            }
            SolanaEventPayload::OrderBookDelta { changes } => {
                MarketEvent::Delta(OrderBookDelta {
                    venue,
                    symbol,
                    sequence: self.metadata.sequence,
                    event_ts,
                    exchange_ts,
                    changes: changes.into_iter().map(|c| LevelChange {
                        side: c.side,
                        price: c.price,
                        qty: c.qty,
                    }).collect(),
                    clear: false,
                })
            }
            SolanaEventPayload::Trade { price, qty, side } => {
                MarketEvent::Trade(Trade {
                    venue,
                    symbol,
                    price,
                    qty,
                    aggressor: side,
                    event_ts,
                    exchange_ts,
                })
            }
            SolanaEventPayload::Swap { price, qty, side, .. } => {
                MarketEvent::Trade(Trade {
                    venue,
                    symbol,
                    price,
                    qty,
                    aggressor: side,
                    event_ts,
                    exchange_ts,
                })
            }
            SolanaEventPayload::LiquidityChange { .. } => {
                // Emit as tick for now - liquidity changes affect depth
                MarketEvent::Tick(MarketTick {
                    venue,
                    symbol,
                    last_price: Decimal::ZERO,
                    last_qty: Decimal::ZERO,
                    best_bid: Decimal::ZERO,
                    best_ask: Decimal::ZERO,
                    event_ts,
                })
            }
            SolanaEventPayload::Tick { price, qty, best_bid, best_ask } => {
                MarketEvent::Tick(MarketTick {
                    venue,
                    symbol,
                    last_price: price,
                    last_qty: qty,
                    best_bid,
                    best_ask,
                    event_ts,
                })
            }
            SolanaEventPayload::Status { status } => {
                MarketEvent::Status {
                    venue,
                    symbol,
                    status: status.into(),
                    ts: event_ts,
                }
            }
        }
    }
}

/// Payload variants for different Solana event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SolanaEventPayload {
    OrderBookSnapshot {
        bids: Vec<OrderBookLevelData>,
        asks: Vec<OrderBookLevelData>,
    },
    OrderBookDelta {
        changes: Vec<LevelChangeData>,
    },
    Trade {
        price: Price,
        qty: Qty,
        side: Side,
    },
    Swap {
        price: Price,
        qty: Qty,
        side: Side,
        input_mint: String,
        output_mint: String,
    },
    LiquidityChange {
        liquidity_delta: i128,
        sqrt_price: u128,
        tick: i32,
    },
    Tick {
        price: Price,
        qty: Qty,
        best_bid: Price,
        best_ask: Price,
    },
    Status {
        status: FeedStatusWrapper,
    },
}

/// Wrapper for FeedStatus to enable serialization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatusWrapper {
    Healthy,
    Stale,
    Disconnected,
    Resync,
}

impl From<lq_core::event::FeedStatus> for FeedStatusWrapper {
    fn from(s: lq_core::event::FeedStatus) -> Self {
        match s {
            lq_core::event::FeedStatus::Healthy => FeedStatusWrapper::Healthy,
            lq_core::event::FeedStatus::Stale => FeedStatusWrapper::Stale,
            lq_core::event::FeedStatus::Disconnected => FeedStatusWrapper::Disconnected,
            lq_core::event::FeedStatus::Resync => FeedStatusWrapper::Resync,
        }
    }
}

impl From<FeedStatusWrapper> for lq_core::event::FeedStatus {
    fn from(s: FeedStatusWrapper) -> Self {
        match s {
            FeedStatusWrapper::Healthy => lq_core::event::FeedStatus::Healthy,
            FeedStatusWrapper::Stale => lq_core::event::FeedStatus::Stale,
            FeedStatusWrapper::Disconnected => lq_core::event::FeedStatus::Disconnected,
            FeedStatusWrapper::Resync => lq_core::event::FeedStatus::Resync,
        }
    }
}

/// Simplified level data for serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookLevelData {
    pub price: Price,
    pub qty: Qty,
}

/// Level change data for deltas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelChangeData {
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
}