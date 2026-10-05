//! Pure multi-venue price aggregation: freshness filter, median, outlier
//! rejection — a deterministic function of `(observations, config, now)`.
//!
//! This is the off-chain half of the oracle (dYdX `pricefeed_server` /
//! `x/prices` vote analogue): it runs in the oracle **daemon**, outside the
//! state machine. Its output is published into the log as an
//! [`lq_sequencer::entry::OraclePriceCmd`]; the state machine never calls
//! anything in this module.
//!
//! Determinism: the result is a pure function of the observation *set* — input
//! order never matters (observations are sorted by `(price, venue)` and
//! de-duplicated per venue keeping the newest). Money is `Decimal` only.

use lq_types::{Exchange, Price, Symbol};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// One venue's price observation for a market.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub price: Price,
    /// Venue/arrival time of the observation (millisecond epoch).
    pub ts_ms: u64,
}

/// Aggregation configuration (daemon side; not part of the state hash — the
/// state machine only ever sees the resulting `OraclePriceCmd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateConfig {
    /// Minimum number of fresh, post-outlier venues required for `Fresh`.
    pub min_sources: u8,
    /// Max observation age vs `now_ms`. `0` = disabled.
    pub max_observation_age_ms: u64,
    /// Max distance from the median, in bps of the median, before an
    /// observation is rejected as an outlier. `0` = disabled.
    pub outlier_band_bps: Decimal,
    /// Rounding scale for the median price.
    pub price_scale: u32,
}

impl Default for AggregateConfig {
    fn default() -> Self {
        Self {
            min_sources: 1,
            max_observation_age_ms: 5_000,
            outlier_band_bps: Decimal::new(100, 0), // 1 %
            price_scale: 8,
        }
    }
}

/// Result of one aggregation round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aggregation {
    /// Enough fresh, mutually consistent sources; `price` is their median.
    Fresh {
        price: Price,
        sources: u8,
        /// Venues that contributed, sorted by venue.
        used: Vec<Exchange>,
    },
    /// Fewer than `min_sources` fresh observations (stale/future filtered out).
    InsufficientSources { fresh: u8, total: u8 },
    /// A fresh quorum existed but outlier rejection left fewer than
    /// `min_sources` mutually consistent venues — do not publish.
    ConsensusLost { fresh: u8 },
}

impl Aggregation {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh { .. })
    }

    /// Price to publish, if any.
    pub fn price(&self) -> Option<Price> {
        match self {
            Self::Fresh { price, .. } => Some(*price),
            _ => None,
        }
    }
}

/// Median of a **sorted non-empty** price slice (fixed-point).
///
/// Odd counts pick the middle element; even counts average the two middle
/// elements and round to `price_scale` (dividing by 2 always terminates in
/// `Decimal`, so no repeating fractions can appear).
fn median_sorted(prices: &[Price], price_scale: u32) -> Price {
    debug_assert!(!prices.is_empty());
    let n = prices.len();
    if n % 2 == 1 {
        prices[n / 2]
    } else {
        let sum = prices[n / 2 - 1] + prices[n / 2];
        (sum / Decimal::TWO).round_dp(price_scale)
    }
}

/// Aggregate observations for one market at logical time `now_ms`.
///
/// Steps:
/// 1. drop observations in the future of `now_ms` or older than
///    `max_observation_age_ms` (`0` disables the age filter);
/// 2. reject early when fewer than `min_sources` fresh observations remain;
/// 3. keep the newest observation per venue (deterministic sort first);
/// 4. compute the median;
/// 5. reject outliers farther than `outlier_band_bps` from the median and
///    recompute; if fewer than `min_sources` venues survive, report
///    [`Aggregation::ConsensusLost`].
pub fn aggregate(obs: &[Observation], cfg: &AggregateConfig, now_ms: u64) -> Aggregation {
    let total = u8::try_from(obs.len()).unwrap_or(u8::MAX);
    let mut fresh: Vec<&Observation> = obs
        .iter()
        .filter(|o| {
            if o.price <= Decimal::ZERO || o.ts_ms > now_ms {
                return false;
            }
            cfg.max_observation_age_ms == 0
                || now_ms.saturating_sub(o.ts_ms) <= cfg.max_observation_age_ms
        })
        .collect();

    if fresh.len() < cfg.min_sources as usize || fresh.is_empty() {
        // The `is_empty` guard covers `min_sources == 0` with everything
        // filtered out (all future / non-positive) — the median of nothing
        // is undefined, so report insufficient sources instead.
        return Aggregation::InsufficientSources {
            fresh: u8::try_from(fresh.len()).unwrap_or(u8::MAX),
            total,
        };
    }

    // Deterministic ordering: newest first per venue, then venue identity.
    fresh.sort_by(|a, b| {
        b.ts_ms
            .cmp(&a.ts_ms)
            .then_with(|| a.venue.cmp(&b.venue))
            .then_with(|| a.price.cmp(&b.price))
    });
    let mut seen: Vec<Exchange> = Vec::new();
    fresh.retain(|o| {
        if seen.contains(&o.venue) {
            false
        } else {
            seen.push(o.venue);
            true
        }
    });

    let mut prices: Vec<Price> = fresh.iter().map(|o| o.price).collect();
    prices.sort();

    // Outlier rejection relative to the first median.
    if cfg.outlier_band_bps > Decimal::ZERO && !prices.is_empty() {
        let med = median_sorted(&prices, cfg.price_scale);
        if med > Decimal::ZERO {
            let band = med * cfg.outlier_band_bps;
            let inliers: Vec<&Observation> = fresh
                .iter()
                .copied()
                .filter(|o| (o.price - med).abs() * Decimal::from(10_000) <= band)
                .collect();
            if inliers.is_empty() || inliers.len() < cfg.min_sources as usize {
                // Every source was an outlier (possible with min_sources = 0
                // and a scattered set): there is no defensible consensus.
                return Aggregation::ConsensusLost {
                    fresh: u8::try_from(fresh.len()).unwrap_or(u8::MAX),
                };
            }
            let mut inlier_prices: Vec<Price> = inliers.iter().map(|o| o.price).collect();
            inlier_prices.sort();
            let mut used: Vec<Exchange> = inliers.iter().map(|o| o.venue).collect();
            used.sort();
            used.dedup();
            return Aggregation::Fresh {
                price: median_sorted(&inlier_prices, cfg.price_scale),
                sources: u8::try_from(inliers.len()).unwrap_or(u8::MAX),
                used,
            };
        }
    }

    let mut used: Vec<Exchange> = fresh.iter().map(|o| o.venue).collect();
    used.sort();
    Aggregation::Fresh {
        price: median_sorted(&prices, cfg.price_scale),
        sources: u8::try_from(fresh.len()).unwrap_or(u8::MAX),
        used,
    }
}
