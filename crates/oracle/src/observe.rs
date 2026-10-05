//! Observation book: last price per venue for each target market, fed by the
//! normalized [`MarketEvent`](lq_core::MarketEvent) stream that the existing
//! `lq-market-data` adapters (OKX / Binance / Bybit) already produce.
//!
//! This is the "reuse `lq-market-data`" seam: adapters decode venue payloads
//! into `MarketEvent`s, [`ObservationBook::on_market_event`] turns them into
//! venue observations, and [`ObservationBook::aggregate`] produces the median
//! the oracle daemon publishes into the log.
//!
//! Determinism: `BTreeMap` keys, newest-wins per venue, no clocks — `now_ms`
//! is always supplied by the caller (the daemon's own clock, outside the
//! state machine).

use std::collections::BTreeMap;

use lq_core::MarketEvent;
use lq_sequencer::entry::MarketId;
use lq_types::{Exchange, Price, Symbol};
use rust_decimal::Decimal;

use crate::aggregate::{aggregate, AggregateConfig, Aggregation, Observation};

/// Latest observation per venue for one market, keyed by venue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservationBook {
    books: BTreeMap<MarketId, BTreeMap<Exchange, Observation>>,
}

impl ObservationBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an observation for `market`. Out-of-order updates are ignored:
    /// a venue's observation timestamp never moves backwards.
    pub fn record(&mut self, market: &MarketId, obs: Observation) -> bool {
        let venue_book = self.books.entry(market.clone()).or_default();
        let stale = venue_book
            .get(&obs.venue)
            .map(|prev| obs.ts_ms < prev.ts_ms)
            .unwrap_or(false);
        if stale {
            return false;
        }
        venue_book.insert(obs.venue, obs);
        true
    }

    /// Observations currently held for `market`, sorted by venue.
    pub fn observations(&self, market: &MarketId) -> Vec<Observation> {
        self.books
            .get(market)
            .map(|b| b.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn venue_count(&self, market: &MarketId) -> usize {
        self.books.get(market).map(|b| b.len()).unwrap_or(0)
    }

    /// Aggregate the current observations for `market` at `now_ms`.
    pub fn aggregate_for(
        &self,
        market: &MarketId,
        cfg: &AggregateConfig,
        now_ms: u64,
    ) -> Aggregation {
        let obs = self.observations(market);
        aggregate(&obs, cfg, now_ms)
    }

    /// Extract `(venue, symbol, price, ts)` from a normalized feed event.
    ///
    /// - `Tick` → last price
    /// - `Trade` → trade price
    /// - `Snapshot` → touch mid (best bid + best ask) / 2
    /// - `Delta` / `Status` → `None` (a delta has no price without a rebuild)
    pub fn extract(event: &MarketEvent) -> Option<(Exchange, Symbol, Price, u64)> {
        match event {
            MarketEvent::Tick(t) => Some((t.venue, t.symbol.clone(), t.last_price, t.event_ts.0)),
            MarketEvent::Trade(t) => Some((t.venue, t.symbol.clone(), t.price, t.event_ts.0)),
            MarketEvent::Snapshot(s) => {
                let bid = s.bids.first()?.price;
                let ask = s.asks.first()?.price;
                let mid = ((bid + ask) / Decimal::TWO).round_dp(8);
                Some((s.venue, s.symbol.clone(), mid, s.event_ts.0))
            }
            MarketEvent::Delta(_) | MarketEvent::Status { .. } => None,
        }
    }

    /// Extract and record in one step: feed events arrive keyed by the
    /// *venue's* symbol; the caller maps them to the internal `MarketId`
    /// (symbol mapping is daemon configuration, Stage 5 wiring).
    pub fn on_market_event(&mut self, market: &MarketId, event: &MarketEvent) -> bool {
        let Some((venue, symbol, price, ts_ms)) = Self::extract(event) else {
            return false;
        };
        self.record(
            market,
            Observation {
                venue,
                symbol,
                price,
                ts_ms,
            },
        )
    }
}
