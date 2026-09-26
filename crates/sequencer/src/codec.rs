//! Encode/decode log entries for the WAL.
//!
//! Framing (outside this module, see [`crate::wal`]):
//! `u32 le payload_len | u32 le crc32(payload) | payload`.
//!
//! Payload is JSON so the decoder is easy to fuzz: arbitrary bytes must return
//! `Err`, never panic.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::entry::LogEntry;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("serialize failed: {0}")]
    Serialize(String),
    #[error("deserialize failed: {0}")]
    Deserialize(String),
    #[error("utf8 error")]
    Utf8,
}

pub fn encode_entry(entry: &LogEntry) -> Result<Vec<u8>, CodecError> {
    serde_json::to_vec(entry).map_err(|e| CodecError::Serialize(e.to_string()))
}

pub fn decode_entry(bytes: &[u8]) -> Result<LogEntry, CodecError> {
    serde_json::from_slice(bytes).map_err(|e| CodecError::Deserialize(e.to_string()))
}

/// Generic JSON helpers used by snapshot encoding as well.
pub fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    serde_json::to_vec(value).map_err(|e| CodecError::Serialize(e.to_string()))
}

pub fn from_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    serde_json::from_slice(bytes).map_err(|e| CodecError::Deserialize(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EntryPayload, MarketId, MarketTickCmd, PlaceOrderCmd};
    use lq_types::{Exchange, OrderType, Side, Symbol, TimeInForce};
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn sample() -> LogEntry {
        LogEntry {
            global_seq: 1,
            market_seq: 1,
            market: MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into())),
            ts_ms: 1_700_000_000_000,
            payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id: Uuid::nil(),
                client_order_id: "c1".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(100.5)),
                quantity: dec!(0.25),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn roundtrip_place_order() {
        let e = sample();
        let bytes = encode_entry(&e).unwrap();
        let back = decode_entry(&bytes).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn roundtrip_market_tick() {
        let e = LogEntry {
            global_seq: 2,
            market_seq: 1,
            market: MarketId::new(Exchange::Paper, Symbol("ETH-USDT".into())),
            ts_ms: 42,
            payload: EntryPayload::MarketTick(MarketTickCmd {
                last: dec!(1),
                bid: Some(dec!(0.9)),
                ask: Some(dec!(1.1)),
            }),
        };
        let back = decode_entry(&encode_entry(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn garbage_bytes_err_not_panic() {
        for bytes in [
            &[][..],
            b"{",
            b"null",
            b"[]",
            b"\"hi\"",
            b"\xff\xfe\xfd",
            b"{\"global_seq\":",
        ] {
            assert!(decode_entry(bytes).is_err(), "expected err for {bytes:?}");
        }
    }
}
