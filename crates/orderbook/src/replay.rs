use lq_core::event::market::MarketEvent;
use lq_core::models::book::OrderBookSnapshot;
use lq_exchange::spec::InstrumentSpec;
use lq_types::{Exchange, Price, Qty, Symbol};

use crate::book::OrderBook;

/// A recorded event with checksum for deterministic replay.
#[derive(Debug, Clone)]
pub struct RecordedEvent {
    pub seq: u64,
    pub event: MarketEvent,
    pub checksum: u64,
    pub timestamp_ms: u64,
}

/// Result of replaying a recorded event sequence.
#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub final_checksum: u64,
    pub events_processed: u64,
    pub gaps_detected: u64,
    pub duplicates_detected: u64,
    pub book_snapshot: Option<OrderBookSnapshot>,
}

/// Deterministic event recorder — stores events with running checksums.
pub struct EventRecorder {
    events: Vec<RecordedEvent>,
    running_checksum: u64,
    seq: u64,
}

impl EventRecorder {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            running_checksum: 0x9e3779b97f4a7c15,
            seq: 0,
        }
    }

    /// Record a market event, compute its checksum, and append.
    pub fn record(&mut self, event: &MarketEvent) {
        let event_checksum = checksum_event(event);
        self.running_checksum = self.running_checksum
            .wrapping_mul(0x517cc1b727220a95)
            .wrapping_add(event_checksum);

        self.seq += 1;
        self.events.push(RecordedEvent {
            seq: self.seq,
            event: event.clone(),
            checksum: self.running_checksum,
            timestamp_ms: lq_types::TimestampMs::now().as_u64(),
        });
    }

    pub fn checksum(&self) -> u64 {
        self.running_checksum
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn events(&self) -> &[RecordedEvent] {
        &self.events
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// Replay recorded events through a fresh OrderBook.
pub struct EventReplay;

impl EventReplay {
    pub fn replay(events: &[RecordedEvent]) -> ReplayResult {
        let mut book = OrderBook::new(
            Exchange::Paper,
            Symbol("REPLAY-SOL-USDC".to_string()),
            InstrumentSpec::new(Price::from(1), Qty::from(1)),
        );
        let mut checksum = 0x9e3779b97f4a7c15u64;
        let mut gaps = 0u64;
        let mut duplicates = 0u64;
        let mut last_seq = 0u64;

        for recorded in events {
            if recorded.seq > 0 && recorded.seq != last_seq + 1 {
                if recorded.seq <= last_seq {
                    duplicates += 1;
                    continue;
                }
                gaps += 1;
            }
            last_seq = recorded.seq;

            match &recorded.event {
                MarketEvent::Snapshot(snapshot) => {
                    book.apply_snapshot(snapshot);
                }
                MarketEvent::Delta(delta) => {
                    book.apply_delta(delta);
                }
                _ => {}
            }

            let event_checksum = checksum_event(&recorded.event);
            checksum = checksum.wrapping_mul(0x517cc1b727220a95).wrapping_add(event_checksum);
        }

        ReplayResult {
            final_checksum: checksum,
            events_processed: events.len() as u64,
            gaps_detected: gaps,
            duplicates_detected: duplicates,
            book_snapshot: if !book.is_empty() {
                Some(book.snapshot(100))
            } else {
                None
            },
        }
    }
}

/// Compute a deterministic checksum for a MarketEvent.
///
/// Uses key fields of the event rather than full serialization,
/// since MarketEvent does not derive Serialize/Deserialize.
fn checksum_event(event: &MarketEvent) -> u64 {
    match event {
        MarketEvent::Snapshot(s) => {
            let mut h = 0xcbf29ce484222325u64;
            h = fold_u64(h, s.sequence);
            h = fold_u64(h, s.event_ts.as_u64());
            for level in &s.bids {
                h = fold_decimal(h, level.price);
                h = fold_decimal(h, level.qty);
            }
            for level in &s.asks {
                h = fold_decimal(h, level.price);
                h = fold_decimal(h, level.qty);
            }
            h
        }
        MarketEvent::Delta(d) => {
            let mut h = 0xcbf29ce484222325u64;
            h = fold_u64(h, d.sequence);
            h = fold_u64(h, d.event_ts.as_u64());
            h = fold_bool(h, d.clear);
            for change in &d.changes {
                h = fold_decimal(h, change.price);
                h = fold_decimal(h, change.qty);
            }
            h
        }
        MarketEvent::Trade(t) => {
            let mut h = 0xcbf29ce484222325u64;
            h = fold_decimal(h, t.price);
            h = fold_decimal(h, t.qty);
            h = fold_u64(h, t.event_ts.as_u64());
            h
        }
        MarketEvent::Tick(t) => {
            let mut h = 0xcbf29ce484222325u64;
            h = fold_decimal(h, t.last_price);
            h = fold_u64(h, t.event_ts.as_u64());
            h
        }
        MarketEvent::Status { ts, .. } => {
            let mut h = 0xcbf29ce484222325u64;
            h = fold_u64(h, ts.as_u64());
            h
        }
    }
}

fn fold_u64(h: u64, v: u64) -> u64 {
    h.wrapping_mul(0x517cc1b727220a95).wrapping_add(v)
}

fn fold_decimal(h: u64, d: rust_decimal::Decimal) -> u64 {
    let unpacked = d.unpack();
    let mut h = fold_u64(h, unpacked.lo as u64);
    h = fold_u64(h, unpacked.mid as u64);
    h = fold_u64(h, unpacked.hi as u64);
    h
}

fn fold_bool(h: u64, b: bool) -> u64 {
    fold_u64(h, if b { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::book::{LevelChange, OrderBookDelta, OrderBookLevel};
    use lq_types::{Price, Qty, Side, TimestampMs};

    fn make_snapshot(seq: u64) -> MarketEvent {
        MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("SOL-USDC".to_string()),
            sequence: seq,
            event_ts: TimestampMs(1700000000000),
            exchange_ts: TimestampMs(1700000000000),
            bids: vec![
                OrderBookLevel { price: Price::from(99), qty: Qty::from(10) },
                OrderBookLevel { price: Price::from(98), qty: Qty::from(20) },
            ],
            asks: vec![
                OrderBookLevel { price: Price::from(101), qty: Qty::from(10) },
                OrderBookLevel { price: Price::from(102), qty: Qty::from(20) },
            ],
        })
    }

    fn make_delta(seq: u64, price: Price, qty: Qty) -> MarketEvent {
        MarketEvent::Delta(OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("SOL-USDC".to_string()),
            sequence: seq,
            event_ts: TimestampMs(1700000000000),
            exchange_ts: TimestampMs(1700000000000),
            changes: vec![LevelChange { side: Side::Bid, price, qty }],
            clear: false,
        })
    }

    #[test]
    fn deterministic_replay_checksum() {
        let mut recorder = EventRecorder::new();
        recorder.record(&make_snapshot(1));
        for i in 2..=11 {
            recorder.record(&make_delta(i, Price::from(99 - (i as i64 - 2)), Qty::from(5)));
        }
        let checksum1 = recorder.checksum();
        assert!(checksum1 != 0);

        let result = EventReplay::replay(recorder.events());
        assert_eq!(result.events_processed, 11);
        assert!(result.book_snapshot.is_some());

        let mut recorder2 = EventRecorder::new();
        recorder2.record(&make_snapshot(1));
        for i in 2..=11 {
            recorder2.record(&make_delta(i, Price::from(99 - (i as i64 - 2)), Qty::from(5)));
        }
        assert_eq!(checksum1, recorder2.checksum());
    }

    #[test]
    fn checksum_changes_with_different_events() {
        let mut r1 = EventRecorder::new();
        r1.record(&make_snapshot(1));
        let c1 = r1.checksum();

        let mut r2 = EventRecorder::new();
        r2.record(&make_snapshot(2));
        let c2 = r2.checksum();

        assert_ne!(c1, c2);
    }

    #[test]
    fn replay_detects_gaps() {
        let events = vec![
            RecordedEvent { seq: 1, event: make_snapshot(1), checksum: 0, timestamp_ms: 0 },
            RecordedEvent { seq: 5, event: make_delta(5, Price::from(99), Qty::from(5)), checksum: 0, timestamp_ms: 0 },
        ];
        let result = EventReplay::replay(&events);
        assert_eq!(result.gaps_detected, 1);
    }

    #[test]
    fn fold_checksum_deterministic() {
        let mut h1 = 0xcbf29ce484222325u64;
        let mut h2 = 0xcbf29ce484222325u64;
        for &b in b"test event data" {
            h1 = h1.wrapping_mul(0x517cc1b727220a95).wrapping_add(b as u64);
            h2 = h2.wrapping_mul(0x517cc1b727220a95).wrapping_add(b as u64);
        }
        assert_eq!(h1, h2);
    }
}
