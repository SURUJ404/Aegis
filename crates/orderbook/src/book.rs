//! High-performance local order book.
//!
//! ## Design
//!
//! The book works on **integer tick prices** (`u64`) and **integer scaled
//! quantities** (`u64`, base units × 10^8). This avoids 128-bit decimal
//! arithmetic on the hot path; `Decimal` conversions happen only at the
//! boundary (snapshot/delta ingestion and query results).
//!
//! Levels are held in `BTreeMap<PriceTick, u64>`:
//! - deterministic ordering (stable iteration for depth/VWAP),
//! - `O(log n)` insert/remove,
//! - no allocation per update after warm-up.
//!
//! The design is deliberately simple: no self-balancing custom trees, no
//! arena. Measured against `1M updates/s` in `benches/orderbook.rs` this is
//! comfortably sufficient, and the constant factors are easy to reason about.

use std::collections::BTreeMap;

use lq_core::models::{OrderBookLevel, OrderBookSnapshot};
use lq_exchange::spec::{InstrumentSpec, PriceTick};
use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;

/// Quantity scale: quantities are stored as base units × 10^8.
pub const QTY_SCALE: u64 = 100_000_000;
const QTY_SCALE_DEC: Decimal = rust_decimal::Decimal::from_parts(100_000_000, 0, 0, false, 0);

#[inline]
fn qty_to_u64(qty: Qty) -> u64 {
    (qty * QTY_SCALE_DEC).as_i128().max(0) as u64
}

#[inline]
fn qty_from_u64(qty: u64) -> Qty {
    Decimal::from(qty) / QTY_SCALE_DEC
}

/// A single venue's local order book for one symbol.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub venue: Exchange,
    pub symbol: Symbol,
    spec: InstrumentSpec,
    /// Bids ascending by price (best bid is the last key).
    bids: BTreeMap<PriceTick, u64>,
    /// Asks ascending by price (best ask is the first key).
    asks: BTreeMap<PriceTick, u64>,
    /// Last applied sequence number.
    sequence: u64,
    last_event_ts: TimestampMs,
}

/// Outcome of applying an incremental update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaOutcome {
    Applied,
    /// Sequence gap: the delta was newer than expected. Book is now stale.
    Gap { expected: u64, got: u64 },
    /// Duplicate or out-of-order sequence; safely ignored.
    Duplicate,
    /// No book yet (snapshot not applied).
    NoBook,
}

impl OrderBook {
    pub fn new(venue: Exchange, symbol: Symbol, spec: InstrumentSpec) -> Self {
        Self {
            venue,
            symbol,
            spec,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            sequence: 0,
            last_event_ts: TimestampMs::now(),
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Wall-clock of the last applied event.
    pub fn last_update_ms(&self) -> TimestampMs {
        self.last_event_ts
    }

    pub fn is_empty(&self) -> bool {
        self.bids.is_empty() || self.asks.is_empty()
    }

    /// Replace the entire book with a snapshot.
    pub fn apply_snapshot(&mut self, snap: &OrderBookSnapshot) {
        self.bids.clear();
        self.asks.clear();
        for level in &snap.bids {
            self.bids.insert(self.to_ticks(level.price), qty_to_u64(level.qty));
        }
        for level in &snap.asks {
            self.asks.insert(self.to_ticks(level.price), qty_to_u64(level.qty));
        }
        self.sequence = snap.sequence;
        self.last_event_ts = snap.event_ts;
    }

    /// Apply an incremental delta with sequence checking.
    pub fn apply_delta(&mut self, delta: &lq_core::models::OrderBookDelta) -> DeltaOutcome {
        if delta.clear {
            self.apply_snapshot(&OrderBookSnapshot {
                venue: delta.venue,
                symbol: delta.symbol.clone(),
                sequence: delta.sequence,
                event_ts: delta.event_ts,
                exchange_ts: delta.exchange_ts,
                bids: delta
                    .changes
                    .iter()
                    .filter(|c| c.side == Side::Bid)
                    .map(|c| OrderBookLevel {
                        price: c.price,
                        qty: c.qty,
                    })
                    .collect(),
                asks: delta
                    .changes
                    .iter()
                    .filter(|c| c.side == Side::Ask)
                    .map(|c| OrderBookLevel {
                        price: c.price,
                        qty: c.qty,
                    })
                    .collect(),
            });
            return DeltaOutcome::Applied;
        }

        if self.sequence == 0 {
            return DeltaOutcome::NoBook;
        }

        if delta.sequence <= self.sequence {
            return DeltaOutcome::Duplicate;
        }
        if delta.sequence != self.sequence + 1 {
            return DeltaOutcome::Gap {
                expected: self.sequence + 1,
                got: delta.sequence,
            };
        }

        for change in &delta.changes {
            self.apply_level_change(change.side, change.price, change.qty);
        }
        self.sequence = delta.sequence;
        self.last_event_ts = delta.event_ts;
        DeltaOutcome::Applied
    }

    /// Apply a raw level change (no sequence bookkeeping). Quantity 0 removes.
    #[inline]
    pub fn apply_level_change(&mut self, side: Side, price: Price, qty: Qty) {
        let tick = self.to_ticks(price);
        let side_map = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        if qty.is_zero() {
            side_map.remove(&tick);
        } else {
            side_map.insert(tick, qty_to_u64(qty));
        }
    }

    // -- best / mid / spread -------------------------------------------------

    #[inline]
    pub fn best_bid_tick(&self) -> Option<PriceTick> {
        self.bids.keys().next_back().copied()
    }

    #[inline]
    pub fn best_ask_tick(&self) -> Option<PriceTick> {
        self.asks.keys().next().copied()
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.best_bid_tick().map(|t| self.from_ticks(t))
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.best_ask_tick().map(|t| self.from_ticks(t))
    }

    pub fn mid_price(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::TWO)
    }

    pub fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    /// Spread in basis points of the mid price.
    pub fn spread_bps(&self) -> Option<f64> {
        let spread = self.spread()?;
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        let bps = spread / mid * Decimal::from(10_000);
        Some(bps.as_f64())
    }

    /// Top-of-book level depth for a side.
    pub fn top_level(&self, side: Side) -> Option<(Price, Qty)> {
        let (tick, qty) = match side {
            Side::Bid => self.bids.iter().next_back(),
            Side::Ask => self.asks.iter().next(),
        }?;
        Some((self.from_ticks(*tick), qty_from_u64(*qty)))
    }

    /// Aggregated quantity within `levels` of the touch.
    pub fn depth(&self, side: Side, levels: usize) -> Qty {
        let iter: Box<dyn Iterator<Item = &u64>> = match side {
            Side::Bid => Box::new(self.bids.values().rev().take(levels)),
            Side::Ask => Box::new(self.asks.values().take(levels)),
        };
        iter.map(|q| qty_from_u64(*q)).sum()
    }

    /// Order-book imbalance in `[-1, 1]` over the top `levels` on each side.
    /// `+1` = all bid, `-1` = all ask, `0` = balanced.
    pub fn imbalance(&self, levels: usize) -> f64 {
        let bid = self.depth(Side::Bid, levels);
        let ask = self.depth(Side::Ask, levels);
        let total = bid + ask;
        if total.is_zero() {
            return 0.0;
        }
        let diff = bid - ask;
        diff.as_f64() / total.as_f64().max(1e-9)
    }

    /// Volume-weighted average price to fill `notional` quote currency,
    /// walking levels outward from the touch. Returns `None` if the book does
    /// not cover the notional.
    pub fn vwap(&self, side: Side, notional: Price) -> Option<Price> {
        let iter: Box<dyn Iterator<Item = (&PriceTick, &u64)>> = match side {
            Side::Bid => Box::new(self.bids.iter().rev()),
            Side::Ask => Box::new(self.asks.iter()),
        };
        let mut remaining = notional;
        let mut cost = Decimal::ZERO;
        let mut qty = Decimal::ZERO;
        for (tick, level_qty) in iter {
            let price = self.from_ticks(*tick);
            let q = qty_from_u64(*level_qty);
            let level_notional = price * q;
            if level_notional >= remaining {
                let take = remaining / price;
                cost += take * price;
                qty += take;
                remaining = Decimal::ZERO;
                break;
            }
            cost += level_notional;
            qty += q;
            remaining -= level_notional;
        }
        if remaining > Decimal::ZERO {
            return None;
        }
        Some(if qty.is_zero() { Decimal::ZERO } else { cost / qty })
    }

    /// Snapshot for observability/resync.
    pub fn snapshot(&self, max_levels: usize) -> OrderBookSnapshot {
        OrderBookSnapshot {
            venue: self.venue,
            symbol: self.symbol.clone(),
            sequence: self.sequence,
            event_ts: self.last_event_ts,
            exchange_ts: self.last_event_ts,
            bids: self
                .bids
                .iter()
                .rev()
                .take(max_levels)
                .map(|(t, q)| OrderBookLevel {
                    price: self.from_ticks(*t),
                    qty: qty_from_u64(*q),
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .take(max_levels)
                .map(|(t, q)| OrderBookLevel {
                    price: self.from_ticks(*t),
                    qty: qty_from_u64(*q),
                })
                .collect(),
        }
    }

    /// Number of price levels currently maintained.
    pub fn num_levels(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    // -- conversions ---------------------------------------------------------

    #[inline]
    pub fn to_ticks(&self, price: Price) -> PriceTick {
        self.spec.to_ticks(price)
    }

    #[inline]
    pub fn from_ticks(&self, ticks: PriceTick) -> Price {
        self.spec.from_ticks(ticks)
    }

    pub fn spec(&self) -> &InstrumentSpec {
        &self.spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{LevelChange, OrderBookDelta};
    use rust_decimal_macros::dec;

    fn book() -> OrderBook {
        OrderBook::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
        )
    }

    fn snapshot() -> OrderBookSnapshot {
        OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 100,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![
                OrderBookLevel {
                    price: dec!(100.0),
                    qty: dec!(1.0),
                },
                OrderBookLevel {
                    price: dec!(99.9),
                    qty: dec!(2.0),
                },
            ],
            asks: vec![
                OrderBookLevel {
                    price: dec!(100.1),
                    qty: dec!(1.5),
                },
                OrderBookLevel {
                    price: dec!(100.2),
                    qty: dec!(0.5),
                },
            ],
        }
    }

    fn delta(seq: u64, changes: Vec<LevelChange>) -> OrderBookDelta {
        OrderBookDelta {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: seq,
            event_ts: TimestampMs(2),
            exchange_ts: TimestampMs(2),
            changes,
            clear: false,
        }
    }

    #[test]
    fn snapshot_initializes_book() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        assert_eq!(b.best_bid(), Some(dec!(100.0)));
        assert_eq!(b.best_ask(), Some(dec!(100.1)));
        assert_eq!(b.mid_price(), Some(dec!(100.05)));
        assert_eq!(b.spread(), Some(dec!(0.1)));
        let bps = b.spread_bps().unwrap();
        assert!((bps - 9.995002498750625).abs() < 1e-6);
    }

    #[test]
    fn delta_applies_in_sequence() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        let out = b.apply_delta(&delta(
            101,
            vec![LevelChange {
                side: Side::Bid,
                price: dec!(100.0),
                qty: dec!(0.0),
            }],
        ));
        assert_eq!(out, DeltaOutcome::Applied);
        assert_eq!(b.best_bid(), Some(dec!(99.9)));
    }

    #[test]
    fn gap_and_duplicate_detection() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        assert_eq!(
            b.apply_delta(&delta(200, vec![])),
            DeltaOutcome::Gap {
                expected: 101,
                got: 200
            }
        );
        // Gap left sequence unchanged; a duplicate is still detected relative
        // to the pre-gap sequence.
        assert_eq!(
            b.apply_delta(&delta(100, vec![])),
            DeltaOutcome::Duplicate
        );
    }

    #[test]
    fn delta_before_snapshot_is_no_book() {
        let mut b = book();
        assert_eq!(b.apply_delta(&delta(1, vec![])), DeltaOutcome::NoBook);
    }

    #[test]
    fn depth_and_imbalance() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        assert_eq!(b.depth(Side::Bid, 2), dec!(3.0));
        assert_eq!(b.depth(Side::Ask, 2), dec!(2.0));
        let imb = b.imbalance(2);
        assert!(imb > 0.0 && imb < 1.0);
    }

    #[test]
    fn vwap_covers_notional() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        // First ask level notional = 100.1 * 1.5 = 150.15. Buying 160 USDT
        // crosses into the second level at 100.2.
        let v = b.vwap(Side::Ask, dec!(160.0)).unwrap();
        assert!(v > dec!(100.1) && v < dec!(100.2));
        // Exact: 160 / (1.5 + (160 - 150.15) / 100.2)
        let expected = dec!(160.0)
            / (dec!(1.5) + (dec!(160.0) - dec!(150.15)) / dec!(100.2));
        assert_eq!(v, expected);
    }

    #[test]
    fn vwap_not_covered() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        assert!(b.vwap(Side::Ask, dec!(10_000.0)).is_none());
    }

    #[test]
    fn quantity_zero_removes_level() {
        let mut b = book();
        b.apply_snapshot(&snapshot());
        b.apply_level_change(Side::Ask, dec!(100.1), dec!(0.0));
        assert_eq!(b.best_ask(), Some(dec!(100.2)));
    }
}