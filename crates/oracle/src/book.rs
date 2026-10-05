//! `OracleBook`: the log-driven oracle price store with per-market circuit
//! breakers — the `x/prices` state analogue embedded by `lq-perps`.
//!
//! Determinism contract (same as every money-path component):
//! - `BTreeMap` iteration only, `Decimal` fixed-point prices
//! - time comes exclusively from `entry.ts_ms` / the command's observation
//!   timestamp — never from a clock
//! - no RNG, no live feed reads: prices arrive **only** as log entries
//!
//! Circuit breakers:
//! - **deviation** — a publication moving more than
//!   [`OracleParams::max_deviation_bps`] from the previous accepted price is
//!   rejected and latches `halted` for that market;
//! - **staleness** — [`OracleBook::gate`] reports `oracle_stale` once
//!   `entry.ts_ms − published_ts_ms` exceeds
//!   [`OracleParams::max_staleness_ms`];
//! - a market is halted (`oracle_halted`) until an in-band or `override`
//!   publication is accepted.

use std::collections::BTreeMap;

use lq_sequencer::entry::{MarketId, OraclePriceCmd};
use lq_sequencer::hash::{write_decimal, write_str, write_u64, StateHasher};
use lq_types::Price;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::params::{OracleOutcome, OracleParams, OracleStats};

/// One market's accepted oracle price and its audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleEntry {
    /// Accepted aggregated price (becomes the market's reference price).
    pub price: Price,
    /// `entry.ts_ms` of the accepted publication — the staleness baseline.
    pub published_ts_ms: u64,
    /// Observation time the producer stamped on that publication (audit).
    pub observation_ts_ms: u64,
    /// Venue quorum claimed at acceptance.
    pub sources: u8,
    /// Deviation breaker latched: new risk is gated until an accepted
    /// publication clears it.
    pub halted: bool,
}

/// JSON (and every other self-describing format) cannot use a struct as a
/// map key, and `MarketId` is a struct — so the price map serializes as the
/// same `Vec<(K, V)>` convention `PerpsWire` uses for its maps.
mod prices_serde {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K, V, S>(map: &BTreeMap<K, V>, ser: S) -> Result<S::Ok, S::Error>
    where
        K: Ord + Serialize,
        V: Serialize,
        S: Serializer,
    {
        map.iter().collect::<Vec<_>>().serialize(ser)
    }

    pub fn deserialize<'de, K, V, D>(de: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Ord + Deserialize<'de>,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let pairs = Vec::<(K, V)>::deserialize(de)?;
        Ok(pairs.into_iter().collect())
    }
}

/// Oracle price store: parameters + one [`OracleEntry`] per covered market.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OracleBook {
    params: OracleParams,
    #[serde(with = "prices_serde")]
    prices: BTreeMap<MarketId, OracleEntry>,
    stats: OracleStats,
}

impl OracleBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_params(params: OracleParams) -> Self {
        Self {
            params,
            prices: BTreeMap::new(),
            stats: OracleStats::default(),
        }
    }

    pub fn params(&self) -> OracleParams {
        self.params
    }

    pub fn stats(&self) -> OracleStats {
        self.stats
    }

    /// Accepted oracle price for a covered market (the reference price the
    /// margin system prefers over trade/tick marks).
    pub fn price(&self, market: &MarketId) -> Option<Price> {
        self.prices.get(market).map(|e| e.price)
    }

    pub fn entry(&self, market: &MarketId) -> Option<&OracleEntry> {
        self.prices.get(market)
    }

    /// Markets with at least one accepted publication, sorted by market.
    pub fn markets(&self) -> Vec<&MarketId> {
        self.prices.keys().collect()
    }

    pub fn covered(&self, market: &MarketId) -> bool {
        self.prices.contains_key(market)
    }

    pub fn is_halted(&self, market: &MarketId) -> bool {
        self.prices.get(market).map(|e| e.halted).unwrap_or(false)
    }

    /// Circuit-breaker gate consulted by the margin machine **before** any
    /// new-risk entry (place / replace / liquidate / settle-funding) is
    /// dispatched. `None` = the market may proceed.
    ///
    /// Returns `"oracle_halted"` while the deviation breaker is latched and
    /// `"oracle_stale"` when the last accepted publication is older (in log
    /// time) than `max_staleness_ms`. Markets without an accepted oracle
    /// price are uncovered and pass through (legacy tick/trade marks apply).
    pub fn gate(&self, market: &MarketId, ts_ms: u64) -> Option<&'static str> {
        let e = self.prices.get(market)?;
        if e.halted {
            return Some("oracle_halted");
        }
        if self.params.max_staleness_ms > 0
            && ts_ms.saturating_sub(e.published_ts_ms) > self.params.max_staleness_ms
        {
            return Some("oracle_stale");
        }
        None
    }

    /// Apply one oracle publication. Pure: only `market`, `cmd` and the
    /// entry's logical time matter.
    ///
    /// Validation order (first failure wins, every failure increments
    /// `stats.rejected`):
    /// 1. positive price
    /// 2. observation not in the future of the entry
    /// 3. observation not older than `max_staleness_ms` (`0` = off)
    /// 4. quorum `sources >= min_sources`
    /// 5. deviation band (unless `cmd.override_band`)
    pub fn apply_price(
        &mut self,
        market: &MarketId,
        cmd: &OraclePriceCmd,
        entry_ts_ms: u64,
    ) -> OracleOutcome {
        let reject = |book: &mut Self, reason: &'static str| {
            book.stats.rejected += 1;
            OracleOutcome::Rejected { reason }
        };

        if cmd.price <= Decimal::ZERO {
            return reject(self, "invalid_price");
        }
        if cmd.observation_ts_ms > entry_ts_ms {
            return reject(self, "observation_in_future");
        }
        if self.params.max_staleness_ms > 0
            && entry_ts_ms - cmd.observation_ts_ms > self.params.max_staleness_ms
        {
            return reject(self, "stale_observation");
        }
        if cmd.sources < self.params.min_sources {
            return reject(self, "insufficient_sources");
        }

        // Deviation circuit breaker against the previously accepted price.
        if let Some(prev) = self.prices.get(market) {
            if !cmd.override_band && self.params.max_deviation_bps > Decimal::ZERO {
                let band = prev.price * self.params.max_deviation_bps;
                let moved = (cmd.price - prev.price).abs() * Decimal::from(10_000);
                if moved > band {
                    let e = self.prices.get_mut(market).expect("entry checked above");
                    if !e.halted {
                        e.halted = true;
                        self.stats.halts += 1;
                    }
                    return reject(self, "oracle_deviation");
                }
            }
        }

        // Accept: publish (first time or update) and clear any latch.
        let entry = OracleEntry {
            price: cmd.price,
            published_ts_ms: entry_ts_ms,
            observation_ts_ms: cmd.observation_ts_ms,
            sources: cmd.sources,
            halted: false,
        };
        self.prices.insert(market.clone(), entry);
        self.stats.published += 1;
        OracleOutcome::Accepted
    }

    /// Canonical hash contribution (called by the embedding machine).
    pub fn write_hash(&self, h: &mut StateHasher) {
        write_str(h, "lq-oracle-v1");
        write_u64(h, self.params.max_staleness_ms);
        write_decimal(h, &self.params.max_deviation_bps);
        write_u64(h, self.params.min_sources as u64);
        write_u64(h, self.stats.published);
        write_u64(h, self.stats.rejected);
        write_u64(h, self.stats.halts);
        write_u64(h, self.prices.len() as u64);
        for (m, e) in &self.prices {
            write_str(h, m.venue.as_str());
            write_str(h, m.symbol.as_str());
            write_decimal(h, &e.price);
            write_u64(h, e.published_ts_ms);
            write_u64(h, e.observation_ts_ms);
            write_u64(h, e.sources as u64);
            write_str(h, if e.halted { "halted" } else { "ok" });
        }
    }
}
