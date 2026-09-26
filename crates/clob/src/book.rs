//! Per-market order-level book with price-time priority.
//!
//! - Bids: highest price first (iterate the `BTreeMap` in reverse).
//! - Asks: lowest price first.
//! - Within a price level: FIFO (`VecDeque`, back = newest, front = oldest).
//!
//! Iteration order is fully deterministic (`BTreeMap`/`VecDeque`); no hashing
//! containers anywhere on the money path.

use std::collections::{BTreeMap, VecDeque};

use lq_types::{Price, Side};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One side's price levels. Derived helpers pick the side by [`Side`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub bids: BTreeMap<Price, VecDeque<Uuid>>,
    pub asks: BTreeMap<Price, VecDeque<Uuid>>,
}

impl Book {
    pub fn new() -> Self {
        Self::default()
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Price, VecDeque<Uuid>> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    fn side(&self, side: Side) -> &BTreeMap<Price, VecDeque<Uuid>> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    /// Insert `id` at the back (newest) of `price`'s queue on `side`.
    pub fn rest(&mut self, side: Side, price: Price, id: Uuid) {
        self.side_mut(side).entry(price).or_default().push_back(id);
    }

    /// Best price on `side` (max for bids, min for asks).
    pub fn best_price(&self, side: Side) -> Option<Price> {
        match side {
            Side::Bid => self.bids.keys().next_back().copied(),
            Side::Ask => self.asks.keys().next().copied(),
        }
    }

    /// Front (oldest) order at `price` on `side`.
    pub fn front(&self, side: Side, price: Price) -> Option<Uuid> {
        self.side(side).get(&price).and_then(|q| q.front()).copied()
    }

    /// Best opposite order visible to a taker on `taker_side`
    /// → `(level_price, order_id)`.
    pub fn best_opposite(&self, taker_side: Side) -> Option<(Price, Uuid)> {
        let opp = taker_side.opposite();
        let price = self.best_price(opp)?;
        let id = self.front(opp, price)?;
        Some((price, id))
    }

    /// Pop the front of `price`'s queue on `side` (maker fully filled).
    pub fn pop_front(&mut self, side: Side, price: Price) -> Option<Uuid> {
        let q = self.side_mut(side).get_mut(&price)?;
        let id = q.pop_front();
        if q.is_empty() {
            self.side_mut(side).remove(&price);
        }
        id
    }

    /// Remove a specific order (cancel/expiry) from wherever it sits.
    pub fn remove(&mut self, side: Side, price: Price, id: Uuid) -> bool {
        let map = self.side_mut(side);
        let Some(q) = map.get_mut(&price) else {
            return false;
        };
        let pos = q.iter().position(|x| *x == id);
        let removed = pos.map(|i| q.remove(i).is_some()).unwrap_or(false);
        if q.is_empty() {
            map.remove(&price);
        }
        removed
    }

    /// Total resting orders on `side` (invariant helper for tests).
    pub fn depth(&self, side: Side) -> usize {
        self.side(side).values().map(|q| q.len()).sum()
    }

    /// Price levels strictly ordered: true by construction (`BTreeMap`), but
    /// also used by tests to assert no empty levels exist.
    pub fn no_empty_levels(&self) -> bool {
        self.bids.values().all(|q| !q.is_empty()) && self.asks.values().all(|q| !q.is_empty())
    }
}
