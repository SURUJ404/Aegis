//! Book store: sequence management, gap detection and market-event ingestion.

use dashmap::DashMap;
use lq_core::event::MarketEvent;
use lq_core::models::OrderBookSnapshot;
use lq_exchange::spec::InstrumentSpec;
use lq_types::{Exchange, Symbol};

use crate::book::{DeltaOutcome, OrderBook};

/// Outcome of ingesting a market event into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Event applied to a book (snapshot or in-sequence delta).
    Applied,
    /// Snapshot (re)established a book.
    Resync,
    /// Delta arrived with no book yet; a snapshot must precede deltas.
    NoBook,
    /// Sequence gap detected; the book is stale until resync.
    Gap { expected: u64, got: u64 },
    /// Event type does not touch the book (trade/tick/status).
    NoChange,
}

/// The default spec used when an instrument has not been registered with a
/// venue-specific spec. Overridden via [`BookStore::register`].
fn default_spec() -> InstrumentSpec {
    InstrumentSpec::new(
        rust_decimal_macros::dec!(0.1),
        rust_decimal_macros::dec!(0.01),
    )
}

/// Manages one [`OrderBook`] per `(Exchange, Symbol)`.
#[derive(Debug, Default)]
pub struct BookStore {
    books: DashMap<(Exchange, Symbol), OrderBook>,
    specs: DashMap<(Exchange, Symbol), InstrumentSpec>,
    /// Number of sequence gaps detected per book (recovered via resync).
    gaps: DashMap<(Exchange, Symbol), u64>,
}

impl BookStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a venue-specific instrument spec.
    pub fn register(&self, venue: Exchange, symbol: Symbol, spec: InstrumentSpec) {
        self.specs.insert((venue, symbol), spec);
    }

    fn spec_for(&self, venue: Exchange, symbol: &Symbol) -> InstrumentSpec {
        self.specs
            .get(&(venue, symbol.clone()))
            .map(|s| *s)
            .unwrap_or_else(default_spec)
    }

    /// Clone of a book (for queries, snapshots, analytics). `None` if no book
    /// exists yet.
    pub fn book(&self, venue: Exchange, symbol: &Symbol) -> Option<OrderBook> {
        self.books.get(&(venue, symbol.clone())).map(|b| b.clone())
    }

    /// Mutable access for backtests / direct manipulation.
    pub fn book_mut(&self, venue: Exchange, symbol: &Symbol) -> Option<impl std::ops::DerefMut<Target = OrderBook> + '_> {
        self.books.get_mut(&(venue, symbol.clone()))
    }

    /// Apply a snapshot directly to a book, bypassing the event bus.
    pub fn apply_snapshot(&self, snap: &OrderBookSnapshot) -> IngestOutcome {
        let key = (snap.venue, snap.symbol.clone());
        match self.books.get_mut(&key) {
            Some(mut book) => {
                book.apply_snapshot(snap);
                IngestOutcome::Resync
            }
            None => {
                let spec = self.spec_for(snap.venue, &snap.symbol);
                let mut book = OrderBook::new(snap.venue, snap.symbol.clone(), spec);
                book.apply_snapshot(snap);
                self.books.insert(key, book);
                IngestOutcome::Resync
            }
        }
    }

    /// Ingest a [`MarketEvent`] into the store, maintaining sequence invariants.
    pub fn ingest(&self, event: &MarketEvent) -> IngestOutcome {
        match event {
            MarketEvent::Snapshot(snap) => self.apply_snapshot(snap),
            MarketEvent::Delta(delta) => {
                let key = (delta.venue, delta.symbol.clone());
                let Some(mut book) = self.books.get_mut(&key) else {
                    return IngestOutcome::NoBook;
                };
                match book.apply_delta(delta) {
                    DeltaOutcome::Applied => IngestOutcome::Applied,
                    DeltaOutcome::Duplicate => IngestOutcome::Applied,
                    DeltaOutcome::NoBook => IngestOutcome::NoBook,
                    DeltaOutcome::Gap { expected, got } => {
                        let mut gaps = self.gaps.entry(key).or_insert(0);
                        *gaps += 1;
                        IngestOutcome::Gap { expected, got }
                    }
                }
            }
            MarketEvent::Trade(_) | MarketEvent::Tick(_) | MarketEvent::Status { .. } => {
                IngestOutcome::NoChange
            }
        }
    }

    /// Number of gaps detected for a book since startup.
    pub fn gap_count(&self, venue: Exchange, symbol: &Symbol) -> u64 {
        self.gaps.get(&(venue, symbol.clone())).map(|g| *g).unwrap_or(0)
    }

    /// Total books tracked.
    pub fn len(&self) -> usize {
        self.books.len()
    }

    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{LevelChange, OrderBookDelta, OrderBookLevel};
    use lq_types::{Side, TimestampMs};

    fn snap(seq: u64) -> MarketEvent {
        MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: seq,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![OrderBookLevel {
                price: rust_decimal_macros::dec!(100.0),
                qty: rust_decimal_macros::dec!(1.0),
            }],
            asks: vec![OrderBookLevel {
                price: rust_decimal_macros::dec!(100.1),
                qty: rust_decimal_macros::dec!(1.0),
            }],
        })
    }

    fn delta(seq: u64) -> MarketEvent {
        MarketEvent::Delta(OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: seq,
            event_ts: TimestampMs(2),
            exchange_ts: TimestampMs(2),
            changes: vec![LevelChange {
                side: Side::Bid,
                price: rust_decimal_macros::dec!(100.0),
                qty: rust_decimal_macros::dec!(0.0),
            }],
            clear: false,
        })
    }

    #[test]
    fn snapshot_then_in_sequence_deltas() {
        let store = BookStore::new();
        assert_eq!(store.ingest(&snap(5)), IngestOutcome::Resync);
        assert_eq!(store.ingest(&delta(6)), IngestOutcome::Applied);
        assert_eq!(store.ingest(&delta(7)), IngestOutcome::Applied);
        let b = store.book(Exchange::Paper, &Symbol("BTC-USDT".into())).unwrap();
        assert_eq!(b.sequence(), 7);
        assert!(b.best_bid().is_none());
    }

    #[test]
    fn gap_is_counted_and_book_kept() {
        let store = BookStore::new();
        store.ingest(&snap(5));
        let out = store.ingest(&delta(9));
        match out {
            IngestOutcome::Gap { expected, got } => {
                assert_eq!(expected, 6);
                assert_eq!(got, 9);
            }
            _ => panic!("expected gap"),
        }
        assert_eq!(store.gap_count(Exchange::Paper, &Symbol("BTC-USDT".into())), 1);
        // After resync the book is usable again.
        store.ingest(&snap(9));
        assert_eq!(store.ingest(&delta(10)), IngestOutcome::Applied);
    }

    #[test]
    fn delta_before_snapshot_is_no_book() {
        let store = BookStore::new();
        assert_eq!(store.ingest(&delta(1)), IngestOutcome::NoBook);
    }

    #[test]
    fn duplicate_delta_is_harmless() {
        let store = BookStore::new();
        store.ingest(&snap(5));
        store.ingest(&delta(6));
        assert_eq!(store.ingest(&delta(6)), IngestOutcome::Applied);
        assert_eq!(store.gap_count(Exchange::Paper, &Symbol("BTC-USDT".into())), 0);
    }
}