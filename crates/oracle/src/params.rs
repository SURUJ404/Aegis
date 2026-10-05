//! Oracle parameters held **inside** the state machine.
//!
//! Everything in here is part of the state hash: replicas must agree on the
//! circuit-breaker configuration byte-for-byte, because it changes future
//! accept/reject decisions (deterministic replay requires identical config).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Per-market oracle parameters (dYdX `x/prices` exchange params analogue).
///
/// A value of `0` disables the corresponding check (mirroring the `lq-risk`
/// and `lq-perps` limit conventions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleParams {
    /// Max age of an accepted publication (`entry.ts_ms − published_ts_ms`)
    /// before the market counts as stale and new risk is gated. `0` = off.
    ///
    /// This is the log-driven replacement for wall-clock staleness: the same
    /// log always yields the same verdict.
    pub max_staleness_ms: u64,
    /// Max price move between consecutive accepted publications, in basis
    /// points of the previous accepted price. Exceeding it rejects the
    /// publication and **halts** the market until an in-band price or an
    /// explicit `override` publication is accepted. `0` = off.
    pub max_deviation_bps: Decimal,
    /// Minimum venue quorum a publication must claim (`OraclePriceCmd::sources`).
    pub min_sources: u8,
}

impl Default for OracleParams {
    fn default() -> Self {
        Self {
            // 30 s without an accepted publication freezes new risk.
            max_staleness_ms: 30_000,
            // 10 % move between publications trips the deviation breaker.
            max_deviation_bps: Decimal::new(1_000, 0),
            // Single-venue setups work out of the box; raise for production.
            min_sources: 1,
        }
    }
}

/// Monotonic observability counters (part of the state hash).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleStats {
    /// Publications accepted into state.
    pub published: u64,
    /// Publications rejected (validation, staleness, quorum, deviation).
    pub rejected: u64,
    /// Deviation breaker trips (halt transitions, not per rejected price).
    pub halts: u64,
}

/// Result of applying one [`lq_sequencer::entry::OraclePriceCmd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleOutcome {
    /// Price accepted: state updated, halt (if any) cleared.
    Accepted,
    /// Price rejected with a stable machine-readable reason; state keeps the
    /// previous price (and latches a halt for deviation rejections).
    Rejected { reason: &'static str },
}
