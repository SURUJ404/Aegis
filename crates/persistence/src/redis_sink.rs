//! Bus-to-Redis hot-state bridge.
//!
//! [`RedisHotStateSink`] mirrors cheap scalars (last mid price, kill-switch
//! state, working-order counts) from the engine's event topics into Redis so
//! the control API and dashboards can read them without touching shared engine
//! state. Like any subscriber it can fall behind under extreme load; hot-state
//! writes are best-effort and never block producers.

use std::sync::Arc;

use lq_core::bus::EventBus;
use lq_core::event::{ControlEvent, ExecutionEvent, MarketEvent};

use crate::redis::RedisHotState;
use crate::store::StoreError;

/// Background subscriber mirroring engine state into Redis.
pub struct RedisHotStateSink {
    handle: tokio::task::JoinHandle<()>,
}

impl RedisHotStateSink {
    /// Connect and spawn market + execution + control mirroring workers.
    pub async fn spawn(bus: Arc<EventBus>, url: &str) -> Result<Self, StoreError> {
        let mut state = RedisHotState::connect(url).await?;
        let market_bus = Arc::clone(&bus);
        let execution_bus = Arc::clone(&bus);
        let control_bus = Arc::clone(&bus);

        let handle = tokio::spawn(async move {
            let mut market_sub = market_bus.market().subscribe();
            let mut execution_sub = execution_bus.execution().subscribe();
            let mut control_sub = control_bus.control().subscribe();

            loop {
                tokio::select! {
                    Some(event) = market_sub.recv() => {
                        let Some((symbol, mid)) = mid_of(&event) else { continue };
                        if let Err(e) = state.set_last_price(&symbol, mid).await {
                            tracing::debug!(err = %e, "redis last-price write failed");
                        }
                    }
                    Some(event) = execution_sub.recv() => {
                        let Some((venue, delta)) = open_order_delta(&event) else { continue };
                        if let Err(e) = state.adjust_open_orders(venue.as_str(), delta).await {
                            tracing::debug!(err = %e, "redis open-order write failed");
                        }
                    }
                    Some(event) = control_sub.recv() => {
                        let halted = matches!(event, ControlEvent::KillSwitch { .. });
                        if let Err(e) = state.set_halted(halted).await {
                            tracing::debug!(err = %e, "redis halt write failed");
                        }
                    }
                    else => break,
                }
            }
        });

        Ok(Self { handle })
    }

    /// Abort the mirroring worker. Drop does this too.
    pub fn shutdown(&mut self) {
        self.handle.abort();
    }
}

impl Drop for RedisHotStateSink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn mid_of(event: &MarketEvent) -> Option<(lq_types::Symbol, f64)> {
    match event {
        MarketEvent::Snapshot(s) => {
            let bid = s.bids.first()?.price;
            let ask = s.asks.first()?.price;
            let mid = (bid.as_f64() + ask.as_f64()) / 2.0;
            Some((s.symbol.clone(), mid))
        }
        MarketEvent::Trade(t) => Some((t.symbol.clone(), t.price.as_f64())),
        _ => None,
    }
}

fn open_order_delta(event: &ExecutionEvent) -> Option<(lq_types::Exchange, i64)> {
    match event {
        ExecutionEvent::New { venue, .. } | ExecutionEvent::Acknowledged { venue, .. } => {
            Some((*venue, 1))
        }
        ExecutionEvent::Cancelled { venue, .. } => Some((*venue, -1)),
        ExecutionEvent::Fill(f) => Some((f.venue, -1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{OrderBookLevel, OrderBookSnapshot};
    use lq_types::{Exchange, Symbol, TimestampMs};
    use rust_decimal_macros::dec;

    fn snapshot() -> OrderBookSnapshot {
        OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![OrderBookLevel::new(dec!(99.0), dec!(1.0))],
            asks: vec![OrderBookLevel::new(dec!(101.0), dec!(1.0))],
        }
    }

    #[test]
    fn mid_is_average_of_touch() {
        let (_, mid) = mid_of(&MarketEvent::Snapshot(snapshot())).unwrap();
        assert!((mid - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn open_order_deltas_are_balanced() {
        use lq_types::Exchange;
        let ack = ExecutionEvent::Acknowledged {
            order_id: uuid::Uuid::new_v4(),
            venue: Exchange::Paper,
            ts: TimestampMs(1),
        };
        let cancel = ExecutionEvent::Cancelled {
            order_id: uuid::Uuid::new_v4(),
            venue: Exchange::Paper,
            ts: TimestampMs(2),
        };
        assert_eq!(open_order_delta(&ack), Some((Exchange::Paper, 1)));
        assert_eq!(open_order_delta(&cancel), Some((Exchange::Paper, -1)));
    }
}