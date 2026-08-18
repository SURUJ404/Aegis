//! Bus-to-store bridge.
//!
//! [`PersistenceSink`] subscribes to the market and execution topics and
//! forwards every event to a store. Producers are never blocked: like any
//! subscriber, this sink can fall behind under extreme load and have events
//! dropped and counted by the topic (a documented engine trade-off). It is
//! intended for *collection* workloads (the market-data service). For
//! lossless bookkeeping the engine should write fills/positions directly
//! through the store, not via the bus.

use std::sync::Arc;

use lq_core::bus::EventBus;

use crate::store::{ExecutionStore, MarketDataStore};

/// Background subscribers forwarding bus events to a store.
pub struct PersistenceSink {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl PersistenceSink {
    /// Spawn market + execution forwarding workers for `store`.
    pub fn spawn<S>(bus: Arc<EventBus>, store: Arc<S>) -> Self
    where
        S: MarketDataStore + ExecutionStore + Send + Sync + 'static,
    {
        let market_store = Arc::clone(&store);
        let execution_bus = Arc::clone(&bus);
        let market_handle = tokio::spawn(async move {
            let mut sub = bus.market().subscribe();
            while let Some(event) = sub.recv().await {
                if let Err(e) = market_store.save_market_event(&event).await {
                    tracing::warn!(err = %e, kind = ?event.kind(), "market event persist failed");
                }
            }
        });

        let execution_handle = tokio::spawn(async move {
            let mut sub = execution_bus.execution().subscribe();
            while let Some(event) = sub.recv().await {
                if let Err(e) = store.save_execution_event(&event).await {
                    tracing::warn!(err = %e, "execution event persist failed");
                }
            }
        });

        Self {
            handles: vec![market_handle, execution_handle],
        }
    }

    /// Abort the forwarding workers. Drop does this too.
    pub fn shutdown(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

impl Drop for PersistenceSink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreError;
    use lq_core::event::{ExecutionEvent, MarketEvent};
    use lq_core::models::{LevelChange, OrderBookDelta, OrderBookLevel, OrderBookSnapshot};
    use lq_types::{Exchange, Side, Symbol, TimestampMs};
    use rust_decimal_macros::dec;
    use std::sync::Mutex;
    use uuid::Uuid;

    struct MemStore {
        market: Mutex<Vec<String>>,
        execution: Mutex<Vec<String>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                market: Mutex::new(Vec::new()),
                execution: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl MarketDataStore for MemStore {
        async fn save_market_event(&self, event: &MarketEvent) -> Result<(), StoreError> {
            self.market
                .lock()
                .unwrap()
                .push(format!("{:?}", event.kind()));
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ExecutionStore for MemStore {
        async fn save_execution_event(&self, event: &ExecutionEvent) -> Result<(), StoreError> {
            let kind = match event {
                ExecutionEvent::Fill(_) => "fill".to_string(),
                _ => format!("{:?}", event.order_id()),
            };
            self.execution.lock().unwrap().push(kind);
            Ok(())
        }
    }

    #[tokio::test]
    async fn sink_forwards_bus_events() {
        let bus = Arc::new(EventBus::new());
        let store = Arc::new(MemStore::new());
        let _sink = PersistenceSink::spawn(Arc::clone(&bus), Arc::clone(&store));

        // Let the workers register their subscriptions before publishing.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        for seq in 1..=3 {
            let _ = bus.market().try_publish(MarketEvent::Delta(OrderBookDelta {
                venue: Exchange::Paper,
                symbol: Symbol("BTC-USDT".into()),
                sequence: seq,
                event_ts: TimestampMs(seq),
                exchange_ts: TimestampMs(seq),
                changes: vec![LevelChange {
                    side: Side::Bid,
                    price: dec!(100),
                    qty: dec!(1),
                }],
                clear: false,
            }));
        }
        let _ = bus.market().try_publish(MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 10,
            event_ts: TimestampMs(10),
            exchange_ts: TimestampMs(10),
            bids: vec![OrderBookLevel::new(dec!(99.8), dec!(10.0))],
            asks: vec![OrderBookLevel::new(dec!(100.2), dec!(10.0))],
        }));

        let fill = ExecutionEvent::Fill(lq_core::models::FillEvent {
            execution_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            client_order_id: "c1".into(),
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            side: Side::Bid,
            price: dec!(100),
            qty: dec!(0.1),
            fee: dec!(0.0025),
            fee_currency: "USDT".into(),
            exchange_ts: TimestampMs(1),
            event_ts: TimestampMs(1),
        });
        let _ = bus.execution().try_publish(fill);

        // Let the workers drain (with a bounded wait for determinism).
        for _ in 0..50 {
            if store.market.lock().unwrap().len() == 4
                && store.execution.lock().unwrap().len() == 1
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        let market = store.market.lock().unwrap();
        assert_eq!(market.len(), 4);
        assert!(market.contains(&"Delta".to_string()));
        assert!(market.contains(&"Snapshot".to_string()));

        let execution = store.execution.lock().unwrap();
        assert_eq!(execution.len(), 1);
        assert_eq!(execution[0], "fill");
    }
}