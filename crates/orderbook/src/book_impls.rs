use std::collections::{BTreeMap, HashMap};

use crate::book::DeltaOutcome;
use lq_core::models::{OrderBookDelta, OrderBookLevel, OrderBookSnapshot};
use lq_exchange::spec::{InstrumentSpec, PriceTick};
use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;

const QTY_SCALE_DEC: Decimal = rust_decimal::Decimal::from_parts(100_000_000, 0, 0, false, 0);

#[inline]
fn qty_to_u64(qty: Qty) -> u64 {
    (qty * QTY_SCALE_DEC).as_i128().max(0) as u64
}

#[inline]
fn qty_from_u64(qty: u64) -> Qty {
    Decimal::from(qty) / QTY_SCALE_DEC
}

pub trait OrderBookImpl: Send + Sync + Clone {
    fn new(venue: Exchange, symbol: Symbol, spec: InstrumentSpec) -> Self;
    fn apply_snapshot(&mut self, snap: &OrderBookSnapshot);
    fn apply_delta(&mut self, delta: &OrderBookDelta) -> DeltaOutcome;
    fn best_bid(&self) -> Option<Price>;
    fn best_ask(&self) -> Option<Price>;
    fn mid_price(&self) -> Option<Price>;
    fn spread(&self) -> Option<Price>;
    fn spread_bps(&self) -> Option<f64>;
    fn depth(&self, side: Side, levels: usize) -> Qty;
    fn imbalance(&self, levels: usize) -> f64;
    fn sequence(&self) -> u64;
    fn last_update_ms(&self) -> TimestampMs;
    fn num_levels(&self, side: Side) -> usize;
    fn is_empty(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct BTreeMapBook {
    venue: Exchange,
    symbol: Symbol,
    spec: InstrumentSpec,
    bids: BTreeMap<PriceTick, u64>,
    asks: BTreeMap<PriceTick, u64>,
    sequence: u64,
    last_event_ts: TimestampMs,
}

impl OrderBookImpl for BTreeMapBook {
    fn new(venue: Exchange, symbol: Symbol, spec: InstrumentSpec) -> Self {
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

    fn apply_snapshot(&mut self, snap: &OrderBookSnapshot) {
        self.bids.clear();
        self.asks.clear();
        for level in &snap.bids {
            self.bids.insert(self.spec.to_ticks(level.price), qty_to_u64(level.qty));
        }
        for level in &snap.asks {
            self.asks.insert(self.spec.to_ticks(level.price), qty_to_u64(level.qty));
        }
        self.sequence = snap.sequence;
        self.last_event_ts = snap.event_ts;
    }

    fn apply_delta(&mut self, delta: &OrderBookDelta) -> DeltaOutcome {
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
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
                    .collect(),
                asks: delta
                    .changes
                    .iter()
                    .filter(|c| c.side == Side::Ask)
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
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
            let tick = self.spec.to_ticks(change.price);
            let side_map = match change.side {
                Side::Bid => &mut self.bids,
                Side::Ask => &mut self.asks,
            };
            if change.qty.is_zero() {
                side_map.remove(&tick);
            } else {
                side_map.insert(tick, qty_to_u64(change.qty));
            }
        }
        self.sequence = delta.sequence;
        self.last_event_ts = delta.event_ts;
        DeltaOutcome::Applied
    }

    fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().map(|t| self.spec.from_ticks(*t))
    }

    fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().map(|t| self.spec.from_ticks(*t))
    }

    fn mid_price(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::TWO)
    }

    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    fn spread_bps(&self) -> Option<f64> {
        let spread = self.spread()?;
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        Some((spread / mid * Decimal::from(10_000)).as_f64())
    }

    fn depth(&self, side: Side, levels: usize) -> Qty {
        let iter: Box<dyn Iterator<Item = &u64>> = match side {
            Side::Bid => Box::new(self.bids.values().rev().take(levels)),
            Side::Ask => Box::new(self.asks.values().take(levels)),
        };
        iter.map(|q| qty_from_u64(*q)).sum()
    }

    fn imbalance(&self, levels: usize) -> f64 {
        let bid = self.depth(Side::Bid, levels);
        let ask = self.depth(Side::Ask, levels);
        let total = bid + ask;
        if total.is_zero() {
            return 0.0;
        }
        let diff = bid - ask;
        diff.as_f64() / total.as_f64().max(1e-9)
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn last_update_ms(&self) -> TimestampMs {
        self.last_event_ts
    }

    fn num_levels(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.bids.is_empty() || self.asks.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct HashMapSortedVecBook {
    venue: Exchange,
    symbol: Symbol,
    spec: InstrumentSpec,
    bids: HashMap<PriceTick, u64>,
    asks: HashMap<PriceTick, u64>,
    bid_keys: Vec<PriceTick>,
    ask_keys: Vec<PriceTick>,
    keys_dirty: bool,
    sequence: u64,
    last_event_ts: TimestampMs,
}

impl HashMapSortedVecBook {
    fn rebuild_keys(&mut self) {
        if self.keys_dirty {
            self.bid_keys.clear();
            self.bid_keys.extend(self.bids.keys().copied());
            self.bid_keys.sort_unstable();
            self.ask_keys.clear();
            self.ask_keys.extend(self.asks.keys().copied());
            self.ask_keys.sort_unstable();
            self.keys_dirty = false;
        }
    }

    fn ensure_keys_sorted(&mut self) {
        if self.keys_dirty {
            self.rebuild_keys();
        }
    }
}

impl OrderBookImpl for HashMapSortedVecBook {
    fn new(venue: Exchange, symbol: Symbol, spec: InstrumentSpec) -> Self {
        Self {
            venue,
            symbol,
            spec,
            bids: HashMap::with_capacity(1024),
            asks: HashMap::with_capacity(1024),
            bid_keys: Vec::with_capacity(1024),
            ask_keys: Vec::with_capacity(1024),
            keys_dirty: false,
            sequence: 0,
            last_event_ts: TimestampMs::now(),
        }
    }

    fn apply_snapshot(&mut self, snap: &OrderBookSnapshot) {
        self.bids.clear();
        self.asks.clear();
        self.bid_keys.clear();
        self.ask_keys.clear();
        for level in &snap.bids {
            let tick = self.spec.to_ticks(level.price);
            self.bids.insert(tick, qty_to_u64(level.qty));
            self.bid_keys.push(tick);
        }
        for level in &snap.asks {
            let tick = self.spec.to_ticks(level.price);
            self.asks.insert(tick, qty_to_u64(level.qty));
            self.ask_keys.push(tick);
        }
        self.bid_keys.sort_unstable();
        self.ask_keys.sort_unstable();
        self.keys_dirty = false;
        self.sequence = snap.sequence;
        self.last_event_ts = snap.event_ts;
    }

    fn apply_delta(&mut self, delta: &OrderBookDelta) -> DeltaOutcome {
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
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
                    .collect(),
                asks: delta
                    .changes
                    .iter()
                    .filter(|c| c.side == Side::Ask)
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
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
            let tick = self.spec.to_ticks(change.price);
            let side_map = match change.side {
                Side::Bid => &mut self.bids,
                Side::Ask => &mut self.asks,
            };
            if change.qty.is_zero() {
                side_map.remove(&tick);
            } else {
                side_map.insert(tick, qty_to_u64(change.qty));
            }
        }
        self.keys_dirty = true;
        self.sequence = delta.sequence;
        self.last_event_ts = delta.event_ts;
        DeltaOutcome::Applied
    }

    fn best_bid(&self) -> Option<Price> {
        let mut this = self.clone();
        this.ensure_keys_sorted();
        this.bid_keys.last().map(|t| self.spec.from_ticks(*t))
    }

    fn best_ask(&self) -> Option<Price> {
        let mut this = self.clone();
        this.ensure_keys_sorted();
        this.ask_keys.first().map(|t| self.spec.from_ticks(*t))
    }

    fn mid_price(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::TWO)
    }

    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    fn spread_bps(&self) -> Option<f64> {
        let spread = self.spread()?;
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        Some((spread / mid * Decimal::from(10_000)).as_f64())
    }

    fn depth(&self, side: Side, levels: usize) -> Qty {
        let mut this = self.clone();
        this.ensure_keys_sorted();
        let iter: Box<dyn Iterator<Item = &u64>> = match side {
            Side::Bid => Box::new(
                this.bid_keys
                    .iter()
                    .rev()
                    .take(levels)
                    .filter_map(|t| this.bids.get(t)),
            ),
            Side::Ask => Box::new(
                this.ask_keys
                    .iter()
                    .take(levels)
                    .filter_map(|t| this.asks.get(t)),
            ),
        };
        iter.map(|q| qty_from_u64(*q)).sum()
    }

    fn imbalance(&self, levels: usize) -> f64 {
        let bid = self.depth(Side::Bid, levels);
        let ask = self.depth(Side::Ask, levels);
        let total = bid + ask;
        if total.is_zero() {
            return 0.0;
        }
        let diff = bid - ask;
        diff.as_f64() / total.as_f64().max(1e-9)
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn last_update_ms(&self) -> TimestampMs {
        self.last_event_ts
    }

    fn num_levels(&self, side: Side) -> usize {
        let mut this = self.clone();
        this.ensure_keys_sorted();
        match side {
            Side::Bid => this.bid_keys.len(),
            Side::Ask => this.ask_keys.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.bids.is_empty() || self.asks.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct ArrayBackedBook {
    venue: Exchange,
    symbol: Symbol,
    spec: InstrumentSpec,
    tick_size: PriceTick,
    base_price_tick: PriceTick,
    bid_array: Vec<u64>,
    ask_array: Vec<u64>,
    bid_min_idx: usize,
    bid_max_idx: usize,
    ask_min_idx: usize,
    ask_max_idx: usize,
    sequence: u64,
    last_event_ts: TimestampMs,
    initialized: bool,
}

impl ArrayBackedBook {
    fn price_to_index(&self, price_tick: PriceTick) -> isize {
        (price_tick as isize) - (self.base_price_tick as isize)
    }

    fn ensure_capacity(&mut self, idx: isize) {
        if idx < 0 {
            let shift = (-idx) as usize;
            let new_len = self.bid_array.len() + shift;
            let mut new_bids = vec![0u64; new_len];
            let mut new_asks = vec![0u64; new_len];
            new_bids[shift..].copy_from_slice(&self.bid_array);
            new_asks[shift..].copy_from_slice(&self.ask_array);
            self.bid_array = new_bids;
            self.ask_array = new_asks;
            self.base_price_tick = self.base_price_tick.saturating_sub(shift as PriceTick);
            self.bid_min_idx += shift;
            self.bid_max_idx += shift;
            self.ask_min_idx += shift;
            self.ask_max_idx += shift;
        } else if idx >= self.bid_array.len() as isize {
            let new_len = (idx as usize) + 1;
            self.bid_array.resize(new_len, 0);
            self.ask_array.resize(new_len, 0);
        }
    }
}

impl OrderBookImpl for ArrayBackedBook {
    fn new(venue: Exchange, symbol: Symbol, spec: InstrumentSpec) -> Self {
        Self {
            venue,
            symbol,
            spec,
            tick_size: spec.tick_size.scale() as u64,
            base_price_tick: 0,
            bid_array: Vec::with_capacity(4096),
            ask_array: Vec::with_capacity(4096),
            bid_min_idx: usize::MAX,
            bid_max_idx: 0,
            ask_min_idx: usize::MAX,
            ask_max_idx: 0,
            sequence: 0,
            last_event_ts: TimestampMs::now(),
            initialized: false,
        }
    }

    fn apply_snapshot(&mut self, snap: &OrderBookSnapshot) {
        self.bid_array.fill(0);
        self.ask_array.fill(0);
        self.bid_min_idx = usize::MAX;
        self.bid_max_idx = 0;
        self.ask_min_idx = usize::MAX;
        self.ask_max_idx = 0;
        self.initialized = true;

        for level in &snap.bids {
            let tick = self.spec.to_ticks(level.price);
            let idx = self.price_to_index(tick);
            self.ensure_capacity(idx);
            let idx = idx as usize;
            self.bid_array[idx] = qty_to_u64(level.qty);
            self.bid_min_idx = self.bid_min_idx.min(idx);
            self.bid_max_idx = self.bid_max_idx.max(idx);
        }
        for level in &snap.asks {
            let tick = self.spec.to_ticks(level.price);
            let idx = self.price_to_index(tick);
            self.ensure_capacity(idx);
            let idx = idx as usize;
            self.ask_array[idx] = qty_to_u64(level.qty);
            self.ask_min_idx = self.ask_min_idx.min(idx);
            self.ask_max_idx = self.ask_max_idx.max(idx);
        }
        self.sequence = snap.sequence;
        self.last_event_ts = snap.event_ts;
    }

    fn apply_delta(&mut self, delta: &OrderBookDelta) -> DeltaOutcome {
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
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
                    .collect(),
                asks: delta
                    .changes
                    .iter()
                    .filter(|c| c.side == Side::Ask)
                    .map(|c| OrderBookLevel { price: c.price, qty: c.qty })
                    .collect(),
            });
            return DeltaOutcome::Applied;
        }

        if self.sequence == 0 || !self.initialized {
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
            let tick = self.spec.to_ticks(change.price);
            let idx = self.price_to_index(tick);
            self.ensure_capacity(idx);
            let idx = idx as usize;

            match change.side {
                Side::Bid => {
                    if change.qty.is_zero() {
                        self.bid_array[idx] = 0;
                    } else {
                        self.bid_array[idx] = qty_to_u64(change.qty);
                    }
                    if self.bid_array[idx] > 0 {
                        self.bid_min_idx = self.bid_min_idx.min(idx);
                        self.bid_max_idx = self.bid_max_idx.max(idx);
                    }
                }
                Side::Ask => {
                    if change.qty.is_zero() {
                        self.ask_array[idx] = 0;
                    } else {
                        self.ask_array[idx] = qty_to_u64(change.qty);
                    }
                    if self.ask_array[idx] > 0 {
                        self.ask_min_idx = self.ask_min_idx.min(idx);
                        self.ask_max_idx = self.ask_max_idx.max(idx);
                    }
                }
            }
        }
        self.sequence = delta.sequence;
        self.last_event_ts = delta.event_ts;
        DeltaOutcome::Applied
    }

    fn best_bid(&self) -> Option<Price> {
        if !self.initialized || self.bid_max_idx == 0 && self.bid_array[self.bid_max_idx] == 0 {
            for i in (self.bid_min_idx..=self.bid_max_idx).rev() {
                if self.bid_array[i] > 0 {
                    return Some(self.spec.from_ticks((self.base_price_tick + i as u64) as PriceTick));
                }
            }
            return None;
        }
        for i in (self.bid_min_idx..=self.bid_max_idx).rev() {
            if self.bid_array[i] > 0 {
                return Some(self.spec.from_ticks((self.base_price_tick + i as u64) as PriceTick));
            }
        }
        None
    }

    fn best_ask(&self) -> Option<Price> {
        if !self.initialized || self.ask_min_idx == usize::MAX {
            return None;
        }
        for i in self.ask_min_idx..=self.ask_max_idx {
            if self.ask_array[i] > 0 {
                return Some(self.spec.from_ticks((self.base_price_tick + i as u64) as PriceTick));
            }
        }
        None
    }

    fn mid_price(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some((bid + ask) / Decimal::TWO)
    }

    fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        Some(ask - bid)
    }

    fn spread_bps(&self) -> Option<f64> {
        let spread = self.spread()?;
        let mid = self.mid_price()?;
        if mid.is_zero() {
            return None;
        }
        Some((spread / mid * Decimal::from(10_000)).as_f64())
    }

    fn depth(&self, side: Side, levels: usize) -> Qty {
        if !self.initialized {
            return Qty::ZERO;
        }
        let mut sum = Decimal::ZERO;
        let mut count = 0;
        match side {
            Side::Bid => {
                for i in (self.bid_min_idx..=self.bid_max_idx).rev() {
                    if self.bid_array[i] > 0 {
                        sum += qty_from_u64(self.bid_array[i]);
                        count += 1;
                        if count >= levels {
                            break;
                        }
                    }
                }
            }
            Side::Ask => {
                for i in self.ask_min_idx..=self.ask_max_idx {
                    if self.ask_array[i] > 0 {
                        sum += qty_from_u64(self.ask_array[i]);
                        count += 1;
                        if count >= levels {
                            break;
                        }
                    }
                }
            }
        }
        sum
    }

    fn imbalance(&self, levels: usize) -> f64 {
        let bid = self.depth(Side::Bid, levels);
        let ask = self.depth(Side::Ask, levels);
        let total = bid + ask;
        if total.is_zero() {
            return 0.0;
        }
        let diff = bid - ask;
        diff.as_f64() / total.as_f64().max(1e-9)
    }

    fn sequence(&self) -> u64 {
        self.sequence
    }

    fn last_update_ms(&self) -> TimestampMs {
        self.last_event_ts
    }

    fn num_levels(&self, side: Side) -> usize {
        if !self.initialized {
            return 0;
        }
        let mut count = 0;
        match side {
            Side::Bid => {
                for i in self.bid_min_idx..=self.bid_max_idx {
                    if self.bid_array[i] > 0 {
                        count += 1;
                    }
                }
            }
            Side::Ask => {
                for i in self.ask_min_idx..=self.ask_max_idx {
                    if self.ask_array[i] > 0 {
                        count += 1;
                    }
                }
            }
        }
        count
    }

    fn is_empty(&self) -> bool {
        !self.initialized || self.bid_min_idx > self.bid_max_idx || self.ask_min_idx > self.ask_max_idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{LevelChange, OrderBookDelta, OrderBookLevel};
    use rust_decimal_macros::dec;

    fn test_spec() -> InstrumentSpec {
        InstrumentSpec::new(dec!(0.1), dec!(0.01))
    }

    fn snapshot() -> OrderBookSnapshot {
        OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 100,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![
                OrderBookLevel { price: dec!(100.0), qty: dec!(1.0) },
                OrderBookLevel { price: dec!(99.9), qty: dec!(2.0) },
            ],
            asks: vec![
                OrderBookLevel { price: dec!(100.1), qty: dec!(1.5) },
                OrderBookLevel { price: dec!(100.2), qty: dec!(0.5) },
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

    fn test_book<B: OrderBookImpl>() {
        let mut b = B::new(Exchange::Paper, Symbol("BTC-USDT".into()), test_spec());
        b.apply_snapshot(&snapshot());
        assert_eq!(b.best_bid(), Some(dec!(100.0)));
        assert_eq!(b.best_ask(), Some(dec!(100.1)));
        assert_eq!(b.mid_price(), Some(dec!(100.05)));

        let out = b.apply_delta(&delta(
            101,
            vec![LevelChange { side: Side::Bid, price: dec!(100.0), qty: dec!(0.0) }],
        ));
        assert_eq!(out, DeltaOutcome::Applied);
        assert_eq!(b.best_bid(), Some(dec!(99.9)));

        assert_eq!(b.depth(Side::Bid, 2), dec!(2.0));
        assert_eq!(b.depth(Side::Ask, 2), dec!(2.0));
    }

    #[test]
    fn btreemap_book_works() {
        test_book::<BTreeMapBook>();
    }

    #[test]
    fn hashmap_sortedvec_book_works() {
        test_book::<HashMapSortedVecBook>();
    }

    #[test]
    fn array_backed_book_works() {
        test_book::<ArrayBackedBook>();
    }

    #[test]
    fn all_books_match_on_random_sequence() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};

        let mut rng = StdRng::seed_from_u64(42);
        let mut btree = BTreeMapBook::new(Exchange::Paper, Symbol("BTC-USDT".into()), test_spec());
        let mut hashvec = HashMapSortedVecBook::new(Exchange::Paper, Symbol("BTC-USDT".into()), test_spec());
        let mut array = ArrayBackedBook::new(Exchange::Paper, Symbol("BTC-USDT".into()), test_spec());

        let snap = snapshot();
        btree.apply_snapshot(&snap);
        hashvec.apply_snapshot(&snap);
        array.apply_snapshot(&snap);

        for i in 101..200 {
            let changes = vec![
                LevelChange {
                    side: if rng.gen() { Side::Bid } else { Side::Ask },
                    price: dec!(100.0) + Decimal::from(rng.gen_range(0..20)) * dec!(0.1),
                    qty: if rng.gen() { Decimal::ZERO } else { Decimal::from(rng.gen_range(1..100)) / Decimal::from(100) },
                };
                5
            ];
            let d = delta(i, changes);
            btree.apply_delta(&d);
            hashvec.apply_delta(&d);
            array.apply_delta(&d);
        }

        assert_eq!(btree.best_bid(), hashvec.best_bid());
        assert_eq!(btree.best_bid(), array.best_bid());
        assert_eq!(btree.best_ask(), hashvec.best_ask());
        assert_eq!(btree.best_ask(), array.best_ask());
        assert_eq!(btree.mid_price(), hashvec.mid_price());
        assert_eq!(btree.mid_price(), array.mid_price());
    }
}