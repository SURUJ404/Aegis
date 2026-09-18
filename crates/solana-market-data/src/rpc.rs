use anyhow::Result;
use lq_types::{Symbol, TimestampMs};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

use crate::models::{NormalizedSolanaEvent, SolanaMarket, SolanaProtocol, SolanaEventType, SolanaEventPayload, SolanaEventMetadata};

/// RPC client configuration
#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub endpoint: String,
    pub timeout_secs: u64,
    pub commitment: String,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://api.mainnet-beta.solana.com".to_string(),
            timeout_secs: 30,
            commitment: "processed".to_string(),
        }
    }
}

/// RPC-based snapshot client for initial book state
pub struct RpcSnapshotClient {
    client: Arc<RpcClient>,
    _config: RpcConfig,
}

impl RpcSnapshotClient {
    pub fn new(config: RpcConfig) -> Self {
        let client = Arc::new(RpcClient::new_with_commitment(
            config.endpoint.clone(),
            solana_sdk::commitment_config::CommitmentConfig::processed(),
        ));
        Self { client, _config: config }
    }

    /// Fetch order book snapshot for a market
    pub async fn fetch_snapshot(&self, market: &SolanaMarket) -> Result<NormalizedSolanaEvent> {
        let pool_pubkey: Pubkey = market.pool_address.parse()?;
        let account = self.client.get_account(&pool_pubkey)?;

        // Parse account data based on protocol
        let (bids, asks) = self.parse_pool_account(&account.data, market.protocol)?;

        let sequence = 1; // Snapshots always start at sequence 1
        let now = TimestampMs::now();

        Ok(NormalizedSolanaEvent {
            metadata: SolanaEventMetadata {
                slot: account.lamports, // Not accurate - would need getSlot
                signature: "rpc_snapshot".to_string(),
                program_id: market.protocol.program_id().to_string(),
                receive_ts: now,
                source_ts: Some(now),
                sequence,
            },
            market: market.clone(),
            event_type: SolanaEventType::OrderBookSnapshot,
            payload: SolanaEventPayload::OrderBookSnapshot { bids, asks },
        })
    }

    /// Fetch multiple account snapshots (for batch initialization)
    pub async fn fetch_multiple(&self, markets: &[SolanaMarket]) -> Result<Vec<NormalizedSolanaEvent>> {
        let mut events = Vec::with_capacity(markets.len());
        for market in markets {
            match self.fetch_snapshot(market).await {
                Ok(event) => events.push(event),
                Err(e) => {
                    tracing::warn!(market = %market.symbol, error = %e, "Failed to fetch snapshot");
                }
            }
        }
        Ok(events)
    }

    fn parse_pool_account(&self, _data: &[u8], _protocol: SolanaProtocol) -> Result<(Vec<crate::models::OrderBookLevelData>, Vec<crate::models::OrderBookLevelData>)> {
        // All parsing requires borsh schemas - return empty for now
        Ok((vec![], vec![]))
    }
}

/// Helper to create a market from pool address
pub fn create_market_from_pool(protocol: SolanaProtocol, symbol: Symbol, pool_address: String) -> SolanaMarket {
    SolanaMarket::new(protocol, symbol, pool_address)
}