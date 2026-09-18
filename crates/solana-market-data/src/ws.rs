use anyhow::Result;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use lq_core::event::FeedStatus;
use lq_types::{Exchange, TimestampMs};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

use crate::models::{NormalizedSolanaEvent, SolanaMarket};

/// WebSocket configuration for Solana RPC
#[derive(Debug, Clone)]
pub struct SolanaWsConfig {
    pub endpoint: String,
    pub ping_interval_secs: u64,
    pub reconnect_base_ms: u64,
    pub reconnect_max_ms: u64,
}

impl Default for SolanaWsConfig {
    fn default() -> Self {
        Self {
            endpoint: "wss://api.mainnet-beta.solana.com".to_string(),
            ping_interval_secs: 20,
            reconnect_base_ms: 1000,
            reconnect_max_ms: 60000,
        }
    }
}

/// LogsSubscribe client for transaction logs
pub struct LogsSubscribeClient {
    config: SolanaWsConfig,
    market: SolanaMarket,
    decoder: crate::decoder::SolanaEventDecoder,
    ws_stream: Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
    subscription_id: Option<u64>,
}

impl LogsSubscribeClient {
    pub fn new(config: SolanaWsConfig, market: SolanaMarket) -> Self {
        let decoder = crate::decoder::SolanaEventDecoder::new(market.clone());
        Self {
            config,
            market,
            decoder,
            ws_stream: None,
            subscription_id: None,
        }
    }

    async fn connect(&mut self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.config.endpoint).await?;
        self.ws_stream = Some(ws_stream);

        // Subscribe to logs for the program
        let program_id = self.market.protocol.program_id();
        let sub_msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                { "mentions": [program_id] },
                { "commitment": "processed" }
            ]
        });

        self.ws_stream.as_mut().unwrap().send(Message::Text(sub_msg.to_string())).await?;
        info!(program = %program_id, "Subscribed to logs");

        // Wait for subscription confirmation
        while let Some(msg) = self.ws_stream.as_mut().unwrap().next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&text) {
                    if resp.get("result").is_some() {
                        self.subscription_id = resp["result"].as_u64();
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl crate::SolanaEventSource for LogsSubscribeClient {
    async fn connect(&mut self) -> Result<()> {
        self.connect().await
    }

    async fn next_event(&mut self) -> Result<NormalizedSolanaEvent> {
        loop {
            let msg = {
                let ws = self.ws_stream.as_mut().ok_or_else(|| anyhow::anyhow!("Not connected"))?;
                match ws.next().await {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => return Err(anyhow::anyhow!("WebSocket error: {}", e)),
                    None => return Err(anyhow::anyhow!("WebSocket stream ended")),
                }
            };

            let text = match msg {
                Message::Text(text) => text,
                Message::Ping(data) => {
                    if let Some(ws) = self.ws_stream.as_mut() {
                        ws.send(Message::Pong(data)).await?;
                    }
                    continue;
                }
                Message::Close(_) => {
                    warn!("WebSocket closed");
                    return Err(anyhow::anyhow!("WebSocket closed"));
                }
                _ => continue,
            };

            if let Some(event) = self.parse_log_notification(&text)? {
                return Ok(event);
            }
        }
    }

    fn status(&self) -> FeedStatus {
        if self.ws_stream.is_some() {
            FeedStatus::Healthy
        } else {
            FeedStatus::Disconnected
        }
    }

    fn market(&self) -> SolanaMarket {
        self.market.clone()
    }

    fn reconnect_base_ms(&self) -> u64 {
        self.config.reconnect_base_ms
    }
}

impl LogsSubscribeClient {
    fn parse_log_notification(&self, text: &str) -> Result<Option<NormalizedSolanaEvent>> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        let params = value.get("params").and_then(|p| p.get("result")).and_then(|r| r.get("value"));

        let Some(logs) = params.and_then(|v| v.get("logs")).and_then(|l| l.as_array()) else {
            return Ok(None);
        };

        let signature = params
            .and_then(|v| v.get("signature"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        let slot = params
            .and_then(|v| v.get("slot"))
            .and_then(|s| s.as_u64())
            .unwrap_or(0);

        let log_strings: Vec<String> = logs.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();

        if let Some(event) = self.decoder.decode_transaction_log(slot, &signature, &log_strings)? {
            return Ok(Some(event));
        }

        Ok(None)
    }
}

/// Run a logsSubscribe feed with reconnection
pub async fn run_logs_feed<S: crate::SolanaEventSource>(
    mut client: S,
    bus: std::sync::Arc<lq_core::bus::EventBus>,
) -> Result<()> {
    let market = client.market();
    let venue: Exchange = market.venue;
    let symbol = market.symbol.clone();

    loop {
        if let Err(e) = client.connect().await {
            error!(error = %e, "LogsSubscribe connect failed");
            tokio::time::sleep(tokio::time::Duration::from_millis(client.reconnect_base_ms())).await;
            continue;
        }

        let _ = bus.market().try_publish(lq_core::event::MarketEvent::Status {
            venue,
            symbol: symbol.clone(),
            status: FeedStatus::Healthy,
            ts: TimestampMs::now(),
        });

        while let Ok(event) = client.next_event().await {
            let normalized = event.to_market_event(venue);
            let _ = bus.market().try_publish(normalized);
        }

        warn!("LogsSubscribe disconnected, reconnecting...");
        let _ = bus.market().try_publish(lq_core::event::MarketEvent::Status {
            venue,
            symbol: symbol.clone(),
            status: FeedStatus::Disconnected,
            ts: TimestampMs::now(),
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(client.reconnect_base_ms())).await;
    }
}