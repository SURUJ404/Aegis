use anyhow::Result;
use lq_core::bus::EventBus;
use lq_types::Symbol;
use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::{
    GeyserConfig, GeyserEventSource, LogsSubscribeClient, RpcConfig, RpcSnapshotClient,
    SolanaMarket, SolanaProtocol, run_solana_feed, SolanaWsConfig,
};

/// Configuration for a Solana market data feed
#[derive(Debug, Clone)]
pub struct SolanaFeedConfig {
    pub market: SolanaMarket,
    pub geyser: Option<GeyserConfig>,
    pub ws: Option<SolanaWsConfig>,
    pub rpc: Option<RpcConfig>,
    pub prefer_geyser: bool,
}

/// Main adapter that manages all Solana data sources for a market
pub struct SolanaMarketDataAdapter {
    config: SolanaFeedConfig,
    handles: Vec<JoinHandle<()>>,
}

impl SolanaMarketDataAdapter {
    pub fn new(config: SolanaFeedConfig) -> Self {
        Self {
            config,
            handles: Vec::new(),
        }
    }

    /// Initialize and start all configured data sources
    pub async fn start(&mut self, bus: Arc<EventBus>) -> Result<()> {
        let market = self.config.market.clone();

        // Fetch initial snapshot via RPC if configured
        if let Some(rpc_config) = &self.config.rpc {
            let rpc = RpcSnapshotClient::new(rpc_config.clone());
            if let Ok(snapshot) = rpc.fetch_snapshot(&market).await {
                let event = snapshot.to_market_event(market.venue);
                let _ = bus.market().try_publish(event);
            }
        }

        // Start Geyser feed if configured (preferred for low latency)
        if self.config.prefer_geyser {
            if let Some(geyser_config) = &self.config.geyser {
                let source = GeyserEventSource::new(geyser_config.clone(), market.clone()).await?;
                let bus_clone = Arc::clone(&bus);
                let handle = tokio::spawn(async move {
                    if let Err(e) = run_solana_feed(source, bus_clone).await {
                        tracing::error!(error = %e, "Geyser feed failed");
                    }
                });
                self.handles.push(handle);
            }
        }

        // Start WebSocket logsSubscribe as fallback or supplement
        if let Some(ws_config) = &self.config.ws {
            let source = LogsSubscribeClient::new(ws_config.clone(), market.clone());
            let bus_clone = Arc::clone(&bus);
            let handle = tokio::spawn(async move {
                if let Err(e) = crate::ws::run_logs_feed(source, bus_clone).await {
                    tracing::error!(error = %e, "LogsSubscribe feed failed");
                }
            });
            self.handles.push(handle);
        }

        // If Geyser not preferred, try it as fallback
        if !self.config.prefer_geyser {
            if let Some(geyser_config) = &self.config.geyser {
                let source = GeyserEventSource::new(geyser_config.clone(), market.clone()).await?;
                let bus_clone = Arc::clone(&bus);
                let handle = tokio::spawn(async move {
                    if let Err(e) = run_solana_feed(source, bus_clone).await {
                        tracing::error!(error = %e, "Geyser feed failed");
                    }
                });
                self.handles.push(handle);
            }
        }

        Ok(())
    }

    /// Wait for all feeds to complete (they run indefinitely)
    pub async fn join_all(mut self) {
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }

    /// Abort all feeds
    pub fn abort_all(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

/// Builder for creating a full Solana market data setup
pub struct SolanaMarketDataBuilder {
    feeds: Vec<SolanaFeedConfig>,
}

impl SolanaMarketDataBuilder {
    pub fn new() -> Self {
        Self { feeds: Vec::new() }
    }

    pub fn add_raydium_clmm(mut self, symbol: Symbol, pool_address: String, geyser: Option<GeyserConfig>, ws: Option<SolanaWsConfig>, rpc: Option<RpcConfig>) -> Self {
        self.feeds.push(SolanaFeedConfig {
            market: SolanaMarket::new(SolanaProtocol::RaydiumClmm, symbol, pool_address),
            geyser,
            ws,
            rpc,
            prefer_geyser: true,
        });
        self
    }

    pub fn add_orca_whirlpool(mut self, symbol: Symbol, pool_address: String, geyser: Option<GeyserConfig>, ws: Option<SolanaWsConfig>, rpc: Option<RpcConfig>) -> Self {
        self.feeds.push(SolanaFeedConfig {
            market: SolanaMarket::new(SolanaProtocol::OrcaWhirlpools, symbol, pool_address),
            geyser,
            ws,
            rpc,
            prefer_geyser: true,
        });
        self
    }

    pub fn add_phoenix(mut self, symbol: Symbol, pool_address: String, geyser: Option<GeyserConfig>, ws: Option<SolanaWsConfig>, rpc: Option<RpcConfig>) -> Self {
        self.feeds.push(SolanaFeedConfig {
            market: SolanaMarket::new(SolanaProtocol::Phoenix, symbol, pool_address),
            geyser,
            ws,
            rpc,
            prefer_geyser: true,
        });
        self
    }

    pub fn add_openbook(mut self, symbol: Symbol, pool_address: String, geyser: Option<GeyserConfig>, ws: Option<SolanaWsConfig>, rpc: Option<RpcConfig>) -> Self {
        self.feeds.push(SolanaFeedConfig {
            market: SolanaMarket::new(SolanaProtocol::OpenBook, symbol, pool_address),
            geyser,
            ws,
            rpc,
            prefer_geyser: true,
        });
        self
    }

    pub async fn build_and_run(self, bus: Arc<EventBus>) -> Result<Vec<SolanaMarketDataAdapter>> {
        let mut adapters = Vec::with_capacity(self.feeds.len());
        for config in self.feeds {
            let mut adapter = SolanaMarketDataAdapter::new(config);
            adapter.start(Arc::clone(&bus)).await?;
            adapters.push(adapter);
        }
        Ok(adapters)
    }
}

impl Default for SolanaMarketDataBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_types::Symbol;

    #[test]
    fn builder_creates_feeds() {
        let builder = SolanaMarketDataBuilder::new()
            .add_raydium_clmm(
                Symbol("SOL-USDC".into()),
                "58oQChx4yWmvKdwLLZzBi4Cho6c2f4d5Pw2E8Z3E8Z3E".into(),
                None,
                None,
                None,
            );
        assert_eq!(builder.feeds.len(), 1);
        assert_eq!(builder.feeds[0].market.protocol, SolanaProtocol::RaydiumClmm);
    }
}