pub mod adapter;
pub mod decoder;
pub mod geyser;
pub mod models;
pub mod rpc;
pub mod ws;

pub use adapter::{SolanaFeedConfig, SolanaMarketDataAdapter};
pub use decoder::SolanaEventDecoder;
pub use geyser::{GeyserClient, GeyserConfig, GeyserEventSource, run_geyser_feed};
pub use models::{NormalizedSolanaEvent, SolanaEventMetadata, SolanaMarket, SolanaProtocol, SolanaEventPayload, SolanaEventType};
pub use rpc::{RpcSnapshotClient, RpcConfig};
pub use ws::{LogsSubscribeClient, SolanaWsConfig, run_logs_feed};

use lq_core::event::{FeedStatus, MarketEvent};
use lq_types::{Exchange, TimestampMs};

/// Solana-specific exchange variants for the engine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SolanaExchange {
    RaydiumClmm,
    OrcaWhirlpools,
    Phoenix,
    OpenBook,
}

impl SolanaExchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RaydiumClmm => "raydium_clmm",
            Self::OrcaWhirlpools => "orca_whirlpools",
            Self::Phoenix => "phoenix",
            Self::OpenBook => "openbook",
        }
    }

    pub fn program_id(&self) -> &'static str {
        match self {
            Self::RaydiumClmm => "CAMMCzo5YL8w4Vfw8KJGkKpUJgZ3eZ3E8Z3E8Z3E8Z3E8",
            Self::OrcaWhirlpools => "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
            Self::Phoenix => "PhoeNiXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8VjXZ8Vj",
            Self::OpenBook => "srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX",
        }
    }
}

/// Convert Solana exchange to core Exchange for integration
impl From<SolanaExchange> for Exchange {
    fn from(_val: SolanaExchange) -> Self {
        // For now map to a custom exchange variant
        // In production, you'd add these to the Exchange enum
        Exchange::Paper // Placeholder - would need to extend Exchange enum
    }
}

/// Trait for Solana event sources
#[async_trait::async_trait]
pub trait SolanaEventSource: Send + Sync {
    async fn connect(&mut self) -> anyhow::Result<()>;
    async fn next_event(&mut self) -> anyhow::Result<NormalizedSolanaEvent>;
    fn status(&self) -> FeedStatus;
    fn market(&self) -> SolanaMarket;
    fn reconnect_base_ms(&self) -> u64 {
        1000
    }
}

/// Run a Solana market data feed
pub async fn run_solana_feed<F: SolanaEventSource + 'static>(
    mut source: F,
    bus: std::sync::Arc<lq_core::bus::EventBus>,
) -> anyhow::Result<()> {
    source.connect().await?;
    let market = source.market();
    let venue: Exchange = market.venue;
    let symbol = market.symbol.clone();

    loop {
        match source.next_event().await {
            Ok(event) => {
                let normalized = event.to_market_event(venue);
                let _ = bus.market().try_publish(normalized);
            }
            Err(e) => {
                tracing::error!(market = %market.symbol, error = %e, "Solana feed error");
                let _ = bus.market().try_publish(MarketEvent::Status {
                    venue,
                    symbol: symbol.clone(),
                    status: FeedStatus::Disconnected,
                    ts: TimestampMs::now(),
                });
                // Reconnect logic
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                if let Err(e) = source.connect().await {
                    tracing::error!(error = %e, "Reconnect failed");
                    continue;
                }
                let _ = bus.market().try_publish(MarketEvent::Status {
                    venue,
                    symbol: symbol.clone(),
                    status: FeedStatus::Resync,
                    ts: TimestampMs::now(),
                });
            }
        }
    }
}