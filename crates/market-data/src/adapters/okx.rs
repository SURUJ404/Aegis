//! OKX WebSocket adapter (`books5` + `trades` channels).
//!
//! Docs: <https://www.okx.com/docs-v5/en/#overview-websocket-overview>
//!
//! - Endpoint: `wss://ws.okx.com:8443/ws/v5/public`
//! - Subscribe: `{"op":"subscribe","args":[{"channel":"books5","instId":"BTC-USDT"},{"channel":"trades","instId":"BTC-USDT"}]}`
//! - Book push has an `action` of `snapshot` (full 5 levels) or `update`
//!   (the current top-5); we treat each as a full top-of-book replacement.
//! - App ping: `{"op":"ping"}` → server replies `{"op":"pong"}`.

use std::sync::Arc;

use lq_core::bus::{EventBus, Topic};
use lq_core::event::{FeedStatus, MarketEvent};
use lq_core::models::{LevelChange, MarketTick, OrderBookLevel, OrderBookSnapshot, OrderBookDelta, Trade};
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use serde_json::Value;

use crate::adapters::{dec_from_value, pair_at};
use crate::ws_client::FeedDecoder;

pub const OKX_PUBLIC_WS: &str = "wss://ws.okx.com:8443/ws/v5/public";

pub struct OkxDecoder {
    bus: Arc<EventBus>,
    symbol: Symbol,
    venue: Exchange,
    seq: u64,
    have_snapshot: bool,
    last_ts: u64,
}

impl OkxDecoder {
    pub fn new(bus: Arc<EventBus>, symbol: Symbol) -> Self {
        Self {
            bus,
            symbol,
            venue: Exchange::Okx,
            seq: 0,
            have_snapshot: false,
            last_ts: 0,
        }
    }

    fn symbol_str(&self) -> String {
        self.symbol.as_str().to_string()
    }

    fn ts(&mut self, ts: u64) -> TimestampMs {
        self.last_ts = ts.max(self.last_ts);
        TimestampMs(self.last_ts)
    }

    fn handle_books(&mut self, value: &Value, data: &Value) -> anyhow::Result<()> {
        let Some(first) = data.as_array().and_then(|a| a.first()) else {
            return Ok(());
        };
        let ts = first.get("ts").and_then(Value::as_str).and_then(|s| s.parse::<u64>().ok()).unwrap_or_else(|| TimestampMs::now().as_u64());
        let now = self.ts(ts);

        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for row in first.get("bids").and_then(Value::as_array).into_iter().flatten() {
            if let Some((px, qty)) = pair_at(row, 0) {
                bids.push(OrderBookLevel::new(px, qty));
            }
        }
        for row in first.get("asks").and_then(Value::as_array).into_iter().flatten() {
            if let Some((px, qty)) = pair_at(row, 0) {
                asks.push(OrderBookLevel::new(px, qty));
            }
        }
        if bids.is_empty() && asks.is_empty() {
            return Ok(());
        }

        let action = value
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("update");
        self.seq += 1;

        if action == "snapshot" || !self.have_snapshot {
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

    fn handle_trades(&mut self, data: &Value) -> anyhow::Result<()> {
        let Some(rows) = data.as_array() else {
            return Ok(());
        };
        for row in rows {
            let Some(px) = row.get("px").and_then(dec_from_value) else {
                continue;
            };
            let Some(qty) = row.get("sz").and_then(dec_from_value) else {
                continue;
            };
            let side = match row.get("side").and_then(Value::as_str) {
                Some("buy") => Side::Bid,
                Some("sell") => Side::Ask,
                _ => continue,
            };
            let ts = row.get("ts").and_then(Value::as_str).and_then(|s| s.parse::<u64>().ok()).unwrap_or_else(|| TimestampMs::now().as_u64());
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

impl FeedDecoder for OkxDecoder {
    fn name(&self) -> &'static str {
        "okx"
    }

    fn subscribe_frames(&mut self) -> Vec<String> {
        let sub = format!(
            r#"{{"op":"subscribe","args":[{{"channel":"books5","instId":"{}"}},{{"channel":"trades","instId":"{}"}}]}}"#,
            self.symbol_str(),
            self.symbol_str()
        );
        self.have_snapshot = false;
        self.seq = 0;
        vec![sub]
    }

    fn ping_payload(&self) -> Option<String> {
        Some(r#"{"op":"ping"}"#.to_string())
    }

    fn on_text(&mut self, text: &str, bus: &EventBus) -> anyhow::Result<bool> {
        let value: Value = serde_json::from_str(text)?;
        if let Some(op) = value.get("op").and_then(Value::as_str) {
            match op {
                "pong" => return Ok(false),
                "subscribe" => {
                    tracing::debug!(feed = "okx", "subscribed");
                    return Ok(false);
                }
                "error" => {
                    tracing::error!(feed = "okx", text = %text, "protocol error");
                    return Ok(false);
                }
                _ => {}
            }
        }
        let Some(arg) = value.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str) else {
            return Ok(false);
        };
        let Some(data) = value.get("data") else {
            return Ok(false);
        };
        let _ = bus;
        match arg {
            "books5" => self.handle_books(&value, data)?,
            "trades" => self.handle_trades(data)?,
            _ => {}
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
        let mut d = OkxDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        let msg = r#"{"arg":{"channel":"books5","instId":"BTC-USDT"},"action":"snapshot","data":[{"asks":[["100.1","0.5","0","1"],["100.2","1.0","0","2"]],"bids":[["100.0","0.8","0","1"],["99.9","1.5","0","2"]],"ts":"1720000000000"}]}"#;
        d.on_text(msg, &bus).unwrap();
        match sub.recv().await.unwrap() {
            MarketEvent::Snapshot(s) => {
                assert_eq!(s.bids[0].price.to_string(), "100.0");
                assert_eq!(s.asks[0].qty.to_string(), "0.5");
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decodes_trade() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = OkxDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        let msg = r#"{"arg":{"channel":"trades","instId":"BTC-USDT"},"data":[{"px":"100.05","sz":"0.3","side":"buy","ts":"1720000000000","tradeId":"1"}]}"#;
        d.on_text(msg, &bus).unwrap();
        assert!(matches!(sub.recv().await.unwrap(), MarketEvent::Trade(_)));
        assert!(matches!(sub.recv().await.unwrap(), MarketEvent::Tick(_)));
    }

    #[tokio::test]
    async fn pong_is_not_liveness() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = OkxDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        assert!(!d.on_text(r#"{"op":"pong"}"#, &bus).unwrap());
        assert!(sub.try_recv().is_err());
    }
}
