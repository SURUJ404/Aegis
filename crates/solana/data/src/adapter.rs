use std::collections::BTreeMap;

use lq_core::bus::PublishResult;
use lq_core::EventBus;
use lq_solana_types::{Slot, SolanaLiquidityChange, SolanaMarketId, SolanaProgram};

use tracing::{debug, info, warn};

use crate::normalizer::SolanaNormalizer;
use crate::reconnect::ConnectionState;

/// Events emitted by the Solana adapter, normalized but not yet Aegis events.
#[derive(Debug, Clone)]
pub enum MarketDataEvent {
    PoolState(lq_solana_types::AmmPoolState),
    Swap(lq_solana_types::SolanaTrade),
    LiquidityChange(SolanaLiquidityChange),
    Heartbeat { slot: Slot, timestamp_ms: u64 },
}

/// Configuration for the Solana market data adapter.
#[derive(Debug, Clone)]
pub struct SolanaDataConfig {
    /// gRPC endpoint (e.g., "http://localhost:10000").
    pub grpc_endpoint: String,
    /// gRPC X-Token for authentication.
    pub grpc_token: Option<String>,
    /// Programs to subscribe to.
    pub programs: Vec<SolanaProgram>,
    /// Markets to track (empty = all markets for subscribed programs).
    pub markets: Vec<SolanaMarketId>,
    /// Reconnect base delay in milliseconds.
    pub reconnect_base_ms: u64,
    /// Reconnect max delay in milliseconds.
    pub reconnect_max_ms: u64,
    /// Slot lag before considering stale.
    pub stale_slot_lag: u64,
    /// Slot lag before kill switch.
    pub critical_slot_lag: u64,
}

impl Default for SolanaDataConfig {
    fn default() -> Self {
        Self {
            grpc_endpoint: "http://localhost:10000".to_string(),
            grpc_token: None,
            programs: vec![SolanaProgram::RaydiumAmmV4, SolanaProgram::RaydiumClmm],
            markets: vec![],
            reconnect_base_ms: 500,
            reconnect_max_ms: 30_000,
            stale_slot_lag: 50,
            critical_slot_lag: 200,
        }
    }
}

/// Tracks the last seen slot per market for staleness detection.
struct SlotTracker {
    last_slot: BTreeMap<SolanaMarketId, Slot>,
    latest_global: Slot,
}

impl SlotTracker {
    fn new() -> Self {
        Self {
            last_slot: BTreeMap::new(),
            latest_global: Slot::new(0),
        }
    }

    fn update(&mut self, market: &SolanaMarketId, slot: Slot) -> bool {
        if slot > self.latest_global {
            self.latest_global = slot;
        }
        let prev = self.last_slot.get(market).copied();
        self.last_slot.insert(market.clone(), slot);
        prev.is_none_or(|p| slot > p)
    }

    fn slot_lag(&self, market: &SolanaMarketId) -> u64 {
        self.latest_global
            .as_u64()
            .saturating_sub(self.last_slot.get(market).map_or(0, |s| s.as_u64()))
    }
}

/// The main Solana market data adapter.
///
/// Receives raw Solana events, normalizes them through `SolanaNormalizer`,
/// and publishes `MarketEvent`s to the Aegis event bus.
pub struct SolanaDataAdapter {
    config: SolanaDataConfig,
    normalizer: SolanaNormalizer,
    slot_tracker: SlotTracker,
    connection_state: ConnectionState,
    events_received: u64,
    events_published: u64,
    events_dropped: u64,
}

impl SolanaDataAdapter {
    pub fn new(config: SolanaDataConfig, normalizer: SolanaNormalizer) -> Self {
        let reconnect_base = config.reconnect_base_ms;
        let reconnect_max = config.reconnect_max_ms;
        Self {
            config,
            normalizer,
            slot_tracker: SlotTracker::new(),
            connection_state: ConnectionState::new(reconnect_base, reconnect_max),
            events_received: 0,
            events_published: 0,
            events_dropped: 0,
        }
    }

    /// Process a single raw event and publish to the bus.
    ///
    /// Returns the number of events published.
    pub fn process_event(
        &mut self,
        event: MarketDataEvent,
        bus: &EventBus,
    ) -> usize {
        self.events_received += 1;

        let slot = match &event {
            MarketDataEvent::PoolState(p) => Some(p.slot),
            MarketDataEvent::Swap(t) => Some(t.slot),
            MarketDataEvent::LiquidityChange(l) => Some(l.slot),
            MarketDataEvent::Heartbeat { slot, .. } => Some(*slot),
        };

        if let Some(slot) = slot {
            let market = match &event {
                MarketDataEvent::PoolState(p) => &p.market,
                MarketDataEvent::Swap(t) => &t.market,
                MarketDataEvent::LiquidityChange(l) => &l.market,
                MarketDataEvent::Heartbeat { .. } => return 0,
            };

            let is_new = self.slot_tracker.update(market, slot);
            if !is_new {
                self.events_dropped += 1;
                return 0;
            }

            let lag = self.slot_tracker.slot_lag(market);
            if lag > self.config.critical_slot_lag {
                warn!(
                    slot = slot.as_u64(),
                    lag,
                    market = %market,
                    "critical slot lag — market data may be stale"
                );
            } else if lag > self.config.stale_slot_lag {
                debug!(
                    slot = slot.as_u64(),
                    lag,
                    market = %market,
                    "slot lag detected"
                );
            }
        }

        let aegis_events = self.normalizer.normalize(&event);
        let count = aegis_events.len();

        for aegis_event in aegis_events {
            let result = bus.market().try_publish(aegis_event);
            match result {
                PublishResult::Published => {
                    self.events_published += 1;
                }
                PublishResult::Dropped => {
                    self.events_dropped += 1;
                }
                _ => {}
            }
        }

        count
    }

    pub fn on_disconnect(&mut self) {
        warn!("solana websocket disconnected");
        self.connection_state.on_disconnect();
    }

    pub fn on_reconnect(&mut self) {
        info!("solana websocket reconnected");
        self.connection_state.on_reconnect();
    }

    pub fn is_stale(&self) -> bool {
        self.connection_state.is_disconnected()
    }

    pub fn stats(&self) -> AdapterStats {
        AdapterStats {
            events_received: self.events_received,
            events_published: self.events_published,
            events_dropped: self.events_dropped,
            connected: !self.connection_state.is_disconnected(),
            latest_slot: self.slot_tracker.latest_global.as_u64(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterStats {
    pub events_received: u64,
    pub events_published: u64,
    pub events_dropped: u64,
    pub connected: bool,
    pub latest_slot: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::EventBus;
    use lq_solana_types::{AmmPoolState, SolanaMarketId, SolanaProgram};

    fn make_pool_event(slot: u64) -> MarketDataEvent {
        MarketDataEvent::PoolState(AmmPoolState {
            market: SolanaMarketId::new(SolanaProgram::RaydiumAmmV4, "pool1"),
            slot: Slot::new(slot),
            token_a_reserve: 1_000_000,
            token_b_reserve: 50_000_000,
            sqrt_price: None,
            lp_supply: None,
            fee_numerator: 25,
            fee_denominator: 10000,
            timestamp_ms: 1700000000000,
        })
    }

    #[tokio::test]
    async fn processes_unique_events() {
        let config = SolanaDataConfig::default();
        let normalizer = SolanaNormalizer::new();
        let mut adapter = SolanaDataAdapter::new(config, normalizer);
        let bus = EventBus::new();

        let published = adapter.process_event(make_pool_event(100), &bus);
        assert_eq!(published, 1);

        let published = adapter.process_event(make_pool_event(101), &bus);
        assert_eq!(published, 1);

        let stats = adapter.stats();
        assert_eq!(stats.events_received, 2);
        assert_eq!(stats.events_published, 2);
    }

    #[tokio::test]
    async fn drops_duplicate_slots() {
        let config = SolanaDataConfig::default();
        let normalizer = SolanaNormalizer::new();
        let mut adapter = SolanaDataAdapter::new(config, normalizer);
        let bus = EventBus::new();

        adapter.process_event(make_pool_event(100), &bus);
        let published = adapter.process_event(make_pool_event(100), &bus);
        assert_eq!(published, 0);
        assert_eq!(adapter.stats().events_dropped, 1);
    }

    #[test]
    fn tracks_connection_state() {
        let config = SolanaDataConfig::default();
        let normalizer = SolanaNormalizer::new();
        let mut adapter = SolanaDataAdapter::new(config, normalizer);

        assert!(!adapter.is_stale());
        adapter.on_disconnect();
        assert!(adapter.is_stale());
        adapter.on_reconnect();
        assert!(!adapter.is_stale());
    }
}
