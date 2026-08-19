//! Binance WebSocket adapter (`depth` + `trade` streams).
//!
//! Docs: <https://developers.binance.com/docs/binance-spot-api-docs/web-socket-streams>
//!
//! - Endpoint: `wss://stream.binance.com:9443/stream?streams=<s>@depth20@100ms/<s>@trade`
//!   Each message is wrapped as `{"stream":"...","data":{...}}`.
//! - `depth@100ms` pushes the full 20-level book every 100ms → we emit a
//!   snapshot each time (idempotent, no sequence bookkeeping needed).
//! - `trade` carries `m` (is buyer the maker) to derive aggressor side.
//! - App ping: server sends `{"method":"ping"}`; client must reply
//!   `{"method":"pong"}` (see `outbound_reply`). We do not send client pings.

use std::sync::Arc;

use lq_core::bus::{EventBus, Topic};
use lq_core::event::{FeedStatus, MarketEvent};
use lq_core::models::{MarketTick, OrderBookLevel, OrderBookSnapshot, Trade};
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use serde_json::Value;

use crate::adapters::{dec_from_value, pair_at};
use crate::ws_client::FeedDecoder;

pub const BINANCE_PUBLIC_WS: &str = "wss://stream.binance.com:9443";

/// Combined-stream URL for a symbol (depth20@100ms + trades). Pass this as
/// the `WsConfig::url` when constructing the feed.
pub fn stream_url(symbol: &Symbol) -> String {
    let s = symbol.as_str().replace('-', "").to_lowercase();
    format!("{BINANCE_PUBLIC_WS}/stream?streams={s}@depth20@100ms/{s}@trade")
}

pub struct BinanceDecoder {
    bus: Arc<EventBus>,
    symbol: Symbol,
    venue: Exchange,
    seq: u64,
    have_snapshot: bool,
    last_ts: u64,
}

impl BinanceDecoder {
    pub fn new(bus: Arc<EventBus>, symbol: Symbol) -> Self {
        Self {
            bus,
            symbol,
            venue: Exchange::Binance,
            seq: 0,
            have_snapshot: false,
            last_ts: 0,
        }
    }

    fn ts(&mut self, ts: u64) -> TimestampMs {
        self.last_ts = ts.max(self.last_ts);
        TimestampMs(self.last_ts)
    }

    fn handle_depth(&mut self, data: &Value) -> anyhow::Result<()> {
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for row in data.get("bids").and_then(Value::as_array).into_iter().flatten() {
            if let Some((px, qty)) = pair_at(row, 0) {
                bids.push(OrderBookLevel::new(px, qty));
            }
        }
        for row in data.get("asks").and_then(Value::as_array).into_iter().flatten() {
            if let Some((px, qty)) = pair_at(row, 0) {
                asks.push(OrderBookLevel::new(px, qty));
            }
        }
        if bids.is_empty() && asks.is_empty() {
            return Ok(());
        }
        let ts = data
            .get("E")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| TimestampMs::now().as_u64());
        let now = self.ts(ts);
        self.seq += 1;
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
        Ok(())
    }

    fn handle_trade(&mut self, data: &Value) -> anyhow::Result<()> {
        let Some(px) = data.get("p").and_then(dec_from_value) else {
            return Ok(());
        };
        let Some(qty) = data.get("q").and_then(dec_from_value) else {
            return Ok(());
        };
        // `m` = is the buyer the maker. If buyer is maker, the aggressor is
        // the ask side (a sell crossed the book).
        let aggressor = match data.get("m").and_then(Value::as_bool) {
            Some(true) => Side::Ask,
            Some(false) => Side::Bid,
            None => return Ok(()),
        };
        let ts = data
            .get("T")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| TimestampMs::now().as_u64());
        let now = self.ts(ts);
        let trade = Trade {
            venue: self.venue,
            symbol: self.symbol.clone(),
            price: px,
            qty,
            aggressor,
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
        Ok(())
    }
}

impl FeedDecoder for BinanceDecoder {
    fn name(&self) -> &'static str {
        "binance"
    }

    fn subscribe_frames(&mut self) -> Vec<String> {
        self.seq = 0;
        self.have_snapshot = false;
        // Binance subscriptions live in the connection URL; nothing to send.
        Vec::new()
    }

    fn ping_payload(&self) -> Option<String> {
        // Binance does not expect client JSON pings; it pings us and expects a
        // pong reply (see `outbound_reply`).
        None
    }

    fn outbound_reply(&self, text: &str) -> Option<String> {
        let value: Value = serde_json::from_str(text).ok()?;
        if value.get("method").and_then(Value::as_str) == Some("ping") {
            Some(r#"{"method":"pong"}"#.to_string())
        } else {
            None
        }
    }

    fn on_text(&mut self, text: &str, _bus: &EventBus) -> anyhow::Result<bool> {
        let value: Value = serde_json::from_str(text)?;
        // Server ping → reply pong.
        if value.get("method").and_then(Value::as_str) == Some("ping") {
            return Ok(false);
        }
        if value.get("result").is_some() || value.get("id").is_some() {
            return Ok(false);
        }
        // Combined-stream wrapper.
        let data = value.get("data").unwrap_or(&value);
        let stream = value.get("stream").and_then(Value::as_str).unwrap_or("");
        if stream.contains("@trade") || data.get("e").and_then(Value::as_str) == Some("trade") {
            self.handle_trade(data)?;
        } else if stream.contains("@depth") || data.get("e").and_then(Value::as_str) == Some("depthUpdate") {
            self.handle_depth(data)?;
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
    async fn decodes_depth() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = BinanceDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        let msg = r#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":1,"bids":[["100.0","0.8"]],"asks":[["100.1","0.5"]],"E":1720000000000}}"#;
        d.on_text(msg, &bus).unwrap();
        match sub.recv().await.unwrap() {
            MarketEvent::Snapshot(s) => {
                assert_eq!(s.asks[0].price.to_string(), "100.1");
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decodes_trade() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = BinanceDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        let msg = r#"{"stream":"btcusdt@trade","data":{"e":"trade","s":"BTCUSDT","p":"100.05","q":"0.3","m":false,"T":1720000000000}}"#;
        d.on_text(msg, &bus).unwrap();
        match sub.recv().await.unwrap() {
            MarketEvent::Trade(t) => {
                assert_eq!(t.aggressor, Side::Bid);
            }
            other => panic!("expected trade, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn server_ping_gets_no_event() {
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.market().subscribe();
        let mut d = BinanceDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
        assert!(!d.on_text(r#"{"method":"ping"}"#, &bus).unwrap());
        assert!(sub.try_recv().is_err());
    }
}
