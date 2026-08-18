//! Bybit V5 WebSocket adapter (`orderbook.5` + `publicTrade` channels).
//!
//! Docs: <https://bybit-exchange.github.io/docs/v5/ws/connect>
//!
//! - Endpoint: `wss://stream.bybit.com/v5/public/spot`
//! - Subscribe: `{"op":"subscribe","args":["orderbook.5.BTCUSDT","publicTrade.BTCUSDT"]}`
//! - Order-book pushes have `type` `snapshot` (full) or `delta` (change list);
//!   the delta `data` holds `b`/`a` arrays of `[price, qty]`.
//! - App ping: `{"op":"ping"}` → `{"op":"pong"}`.

use std::sync::Arc;

use lq_core::bus::{EventBus, Topic};
use lq_core::event::{FeedStatus, MarketEvent};
use lq_core::models::{LevelChange, MarketTick, OrderBookLevel, OrderBookDelta, OrderBookSnapshot, Trade};
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use serde_json::Value;

use crate::adapters::{dec_from_value, pair_at};
use crate::ws_client::FeedDecoder;

pub const BYBIT_PUBLIC_WS: &str = "wss://stream.bybit.com/v5/public/spot";

pub struct BybitDecoder {
    bus: Arc<EventBus>,
    symbol: Symbol,
    venue: Exchange,
    seq: u64,
    have_snapshot: bool,
    last_ts: u64,
}

impl BybitDecoder {
    pub fn new(bus: Arc<EventBus>, symbol: Symbol) -> Self {
        Self {
            bus,
            symbol,
            venue: Exchange::Bybit,
            seq: 0,
            have_snapshot: false,
            last_ts: 0,
        }
    }

    fn symbol_str(&self) -> String {
        self.symbol.as_str().replace('-', "").to_uppercase()
    }

    fn ts(&mut self, ts: u64) -> TimestampMs {
        self.last_ts = ts.max(self.last_ts);
        TimestampMs(self.last_ts)
    }

    fn handle_orderbook(&mut self, value: &Value) -> anyhow::Result<()> {
        let Some(data) = value.get("data") else {
            return Ok(());
        };
        let typ = value.get("type").and_then(Value::as_str).unwrap_or("snapshot");
        let ts = value
            .get("ts")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| TimestampMs::now().as_u64());
        let now = self.ts(ts);

        let level = |v: &Value| -> Vec<OrderBookLevel> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| pair_at(row, 0).map(|(px, qty)| OrderBookLevel::new(px, qty)))
                .collect()
        };

        let bids = level(&data["b"]);
        let asks = level(&data["a"]);
        if bids.is_empty() && asks.is_empty() {
            return Ok(());
        }

        self.seq += 1;

        if typ == "snapshot" || !self.have_snapshot {
            self.have_snapshot = true;
            let _ = self.bus.market().try_publish(MarketEvent::Snapshot(OrderBookSnapshot {
                venue: self.venue,
                symbol: self.symbol.clone(),
                sequence: self.seq,
                event_ts: now,
                exchange_ts: now,
                bids,
                asks,
            }));
        } else {
            let mut changes = Vec::with_capacity(bids.len() + asks.len());
            for l in &bids {
                changes.push(LevelChange { side: Side::Bid, price: l.price, qty: l.qty });
            }
            for l in &asks {
                changes.push(LevelChange { side: Side::Ask, price: l.price, qty: l.qty });
            }
            let _ = self.bus.market().try_publish(MarketEvent::Delta(OrderBookDelta {
                venue: self.venue,
                symbol: self.symbol.clone(),
                sequence: self.seq,
                event_ts: now,
                exchange_ts: now,
                changes,
                clear: false,
            }));
        }
        Ok(())
    }

    fn handle_trade(&mut self, value: &Value) -> anyhow::Result<()> {
        let Some(rows) = value.get("data").and_then(Value::as_array) else {
            return Ok(());
        };
        for row in rows {
            let Some(px) = row.get("p").and_then(dec_from_value) else {
                continue;
            };
            let Some(qty) = row.get("v").and_then(dec_from_value) else {
                continue;
            };
            let side = match row.get("S").and_then(Value::as_str) {
                Some("Buy") => Side::Bid,
                Some("Sell") => Side::Ask,
                _ => continue,
            };
            let ts = row.get("T").and_then(Value::as_u64).unwrap_or_else(|| TimestampMs::now().as_u64());
            let now = self.ts(ts);
            let trade = Trade {
                venue: self.venue,
                symbol: self.symbol.clone(),
                price: px,
                qty,
                aggressor: side,
                event_ts: now,
                exchange_ts: now,
            };
            let _ = self.bus.market().try_publish(MarketEvent::Trade(trade.clone()));
            let _ = self.bus.market().try_publish(MarketEvent::Tick(MarketTick {
                venue: self.venue,
                symbol: self.symbol.clone(),
                last_price: px,
                last_qty: qty,
                best_bid: px,
                best_ask: px,
                event_ts: now,
            }));
        }
        Ok(())
    }
}

impl FeedDecoder for BybitDecoder {
    fn name(&self) -> &'static str {
        "bybit"
    }

    fn subscribe_frames(&mut self) -> Vec<String> {
        let s = self.symbol_str();
        self.have_snapshot = false;
        self.seq = 0;
        vec![format!(
            r#"{{"op":"subscribe","args":["orderbook.5.{s}","publicTrade.{s}"]}}"#
        )]
    }

    fn ping_payload(&self) -> Option<String> {
        Some(r#"{"op":"ping"}"#.to_string())
    }

    fn on_text(&mut self, text: &str, _bus: &EventBus) -> anyhow::Result<bool> {
        let value: Value = serde_json::from_str(text)?;
        if value.get("op").and_then(Value::as_str) == Some("pong") {
            return Ok(false);
        }
        let Some(topic) = value.get("topic").and_then(Value::as_str) else {
            return Ok(false);
        };
        if topic.starts_with("orderbook.") {
            self.handle_orderbook(&value)?;
        } else if topic.starts_with("publicTrade.") {
            self.handle_trade(&value)?;
        }
        Ok(true)
    }

    fn status_event(&self, status: FeedStatus) -> MarketEvent {
        MarketEvent::Status {
            venue: self.venue,
            symbol: self.symbol.clone(),
            status,
            ts: TimestampMs::now(),
        }
    }

    fn market_topic(&self) -> &Topic<MarketEvent> {
        self.bus.market()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decodes_snapshot() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = BybitDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        let msg = r#"{"topic":"orderbook.5.BTCUSDT","type":"snapshot","ts":1720000000000,"data":{"s":"BTCUSDT","b":[["100.0","0.8"]],"a":[["100.1","0.5"]]}}"#;
        d.on_text(msg, &bus).unwrap();
        match sub.recv().await.unwrap() {
            MarketEvent::Snapshot(s) => {
                assert_eq!(s.bids[0].qty.to_string(), "0.8");
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decodes_delta() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = BybitDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        d.on_text(
            r#"{"topic":"orderbook.5.BTCUSDT","type":"snapshot","ts":1720000000000,"data":{"s":"BTCUSDT","b":[["100.0","0.8"]],"a":[["100.1","0.5"]]}}"#,
            &bus,
        )
        .unwrap();
        let _ = sub.recv().await.unwrap();
        d.on_text(
            r#"{"topic":"orderbook.5.BTCUSDT","type":"delta","ts":1720000001000,"data":{"s":"BTCUSDT","b":[["100.0","0.7"]],"a":[["100.1","0.4"]]}}"#,
            &bus,
        )
        .unwrap();
        match sub.recv().await.unwrap() {
            MarketEvent::Delta(delta) => {
                assert!(delta.changes.iter().any(|c| c.side == Side::Bid && c.qty.to_string() == "0.7"));
            }
            other => panic!("expected delta, got {other:?}"),
        }
    }
}
