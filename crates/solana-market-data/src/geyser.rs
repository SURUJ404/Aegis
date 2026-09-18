use anyhow::Result;
use async_trait::async_trait;
use lq_core::event::FeedStatus;
use lq_types::{Exchange, TimestampMs};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::models::{NormalizedSolanaEvent, SolanaMarket};

/// Geyser gRPC client configuration
#[derive(Debug, Clone)]
pub struct GeyserConfig {
    pub endpoint: String,
    pub x_token: Option<String>,
    pub commitment: String,
    pub ping_interval_secs: u64,
}

impl Default for GeyserConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://geyser.mainnet.solana.com".to_string(),
            x_token: None,
            commitment: "processed".to_string(),
            ping_interval_secs: 30,
        }
    }
}

/// Geyser gRPC client for real-time account updates
/// Note: Full implementation requires Geyser protobuf definitions.
/// This is a compile-time placeholder that shows the intended interface.
pub struct GeyserClient {
    config: GeyserConfig,
    accounts_tx: mpsc::UnboundedSender<GeyserAccountUpdate>,
    accounts_rx: Option<mpsc::UnboundedReceiver<GeyserAccountUpdate>>,
    subscribed_accounts: Vec<Pubkey>,
}

#[derive(Debug, Clone)]
pub struct GeyserAccountUpdate {
    pub slot: u64,
    pub signature: String,
    pub account: Pubkey,
    pub data: Vec<u8>,
    pub owner: Pubkey,
    pub executable: bool,
    pub rent_epoch: u64,
    pub lamports: u64,
}

impl GeyserClient {
    pub fn new(config: GeyserConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            config,
            accounts_tx: tx,
            accounts_rx: Some(rx),
            subscribed_accounts: Vec::new(),
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        // TODO: Implement actual gRPC connection with tonic
        // For now, just validate config
        if self.config.endpoint.is_empty() {
            return Err(anyhow::anyhow!("Empty Geyser endpoint"));
        }
        info!("Geyser client configured for {}", self.config.endpoint);
        Ok(())
    }

    pub async fn subscribe_accounts(&mut self, accounts: Vec<Pubkey>) -> Result<()> {
        self.subscribed_accounts = accounts.clone();
        info!(count = accounts.len(), "Subscribed to accounts via Geyser (placeholder)");
        Ok(())
    }

    pub fn take_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<GeyserAccountUpdate>> {
        self.accounts_rx.take()
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<GeyserAccountUpdate> {
        self.accounts_tx.clone()
    }
}

/// A Geyser-based event source for a specific market
pub struct GeyserEventSource {
    client: GeyserClient,
    decoder: crate::decoder::SolanaEventDecoder,
    market: SolanaMarket,
    receiver: Option<mpsc::UnboundedReceiver<GeyserAccountUpdate>>,
}

impl GeyserEventSource {
    pub async fn new(config: GeyserConfig, market: SolanaMarket) -> Result<Self> {
        let mut client = GeyserClient::new(config);
        client.connect().await?;

        let pool_pubkey: Pubkey = market.pool_address.parse()?;
        client.subscribe_accounts(vec![pool_pubkey]).await?;

        let receiver = client.take_receiver();
        let decoder = crate::decoder::SolanaEventDecoder::new(market.clone());

        Ok(Self {
            client,
            decoder,
            market,
            receiver,
        })
    }
}

#[async_trait]
impl crate::SolanaEventSource for GeyserEventSource {
    async fn connect(&mut self) -> Result<()> {
        self.client.connect().await?;
        let pool_pubkey: Pubkey = self.market.pool_address.parse()?;
        self.client.subscribe_accounts(vec![pool_pubkey]).await?;
        self.receiver = self.client.take_receiver();
        Ok(())
    }

    async fn next_event(&mut self) -> Result<NormalizedSolanaEvent> {
        let mut rx = self.receiver.take().expect("Receiver not initialized");

        while let Some(update) = rx.recv().await {
            if let Some(event) = self.decoder.decode_account_update(
                update.slot,
                &update.signature,
                &update.data,
                &update.owner,
            )? {
                self.receiver = Some(rx);
                return Ok(event);
            }
        }

        self.receiver = Some(rx);
        Err(anyhow::anyhow!("Channel closed"))
    }

    fn status(&self) -> FeedStatus {
        FeedStatus::Healthy
    }

    fn market(&self) -> SolanaMarket {
        self.market.clone()
    }

    fn reconnect_base_ms(&self) -> u64 {
        self.client.config.ping_interval_secs * 1000
    }
}

/// Run a Geyser feed with reconnection logic
pub async fn run_geyser_feed<S: crate::SolanaEventSource>(
    mut source: S,
    bus: Arc<lq_core::bus::EventBus>,
) -> Result<()> {
    let market = source.market();
    let venue: Exchange = market.venue;
    let symbol = market.symbol.clone();

    loop {
        if let Err(e) = source.connect().await {
            tracing::error!(error = %e, "Geyser connect failed");
            tokio::time::sleep(tokio::time::Duration::from_millis(source.reconnect_base_ms())).await;
            continue;
        }

        let _ = bus.market().try_publish(lq_core::event::MarketEvent::Status {
            venue,
            symbol: symbol.clone(),
            status: FeedStatus::Healthy,
            ts: TimestampMs::now(),
        });

        while let Ok(event) = source.next_event().await {
            let normalized = event.to_market_event(venue);
            let _ = bus.market().try_publish(normalized);
        }

        warn!("Geyser disconnected, reconnecting...");
        let _ = bus.market().try_publish(lq_core::event::MarketEvent::Status {
            venue,
            symbol: symbol.clone(),
            status: FeedStatus::Disconnected,
            ts: TimestampMs::now(),
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(source.reconnect_base_ms())).await;
    }
}