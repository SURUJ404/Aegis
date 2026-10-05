//! `lq-oracle`: the oracle layer — multi-venue price aggregation on the read
//! side, log-driven oracle prices with circuit breakers on the write side.
//!
//! Stage 4 of the Aegis dYdX v4-style exchange core (see
//! `docs/stages/STAGE_4_ORACLE.md`). Two halves:
//!
//! **Write side (money path).** [`OracleBook`] stores one accepted price per
//! market plus the deviation/staleness circuit breakers. It is embedded by
//! `lq-perps` and fed exclusively by `EntryPayload::OraclePrice` log entries —
//! the state machine never reads a live feed. [`OracleBook::gate`] tells the
//! margin machine when new risk must be refused (`"oracle_halted"`,
//! `"oracle_stale"`).
//!
//! **Read side (disposable daemon).** [`ObservationBook`] ingests the
//! normalized `MarketEvent` stream produced by the existing `lq-market-data`
//! adapters, and [`aggregate`] computes a deterministic median with freshness
//! filtering and outlier rejection. The daemon publishes the result into the
//! log as an `OraclePriceCmd`; if it lags or crashes, trading continues on
//! the last accepted price until the staleness gate trips.
//!
//! Determinism: `BTreeMap` only, `Decimal` fixed-point, time is always passed
//! in by the caller (`entry.ts_ms` / daemon clock) — nothing here reads a
//! clock, spawns tasks or performs I/O.

pub mod aggregate;
pub mod book;
pub mod observe;
pub mod params;

pub use aggregate::{aggregate, AggregateConfig, Aggregation, Observation};
pub use book::{OracleBook, OracleEntry};
pub use observe::ObservationBook;
pub use params::{OracleOutcome, OracleParams, OracleStats};

/// Build the log payload for one aggregated price (the daemon's publish
/// step). Sequencing, WAL durability and validation all happen downstream in
/// `lq-sequencer` / the state machine.
pub fn price_payload(
    price: lq_types::Price,
    observation_ts_ms: u64,
    sources: u8,
    override_band: bool,
) -> lq_sequencer::entry::EntryPayload {
    lq_sequencer::entry::EntryPayload::OraclePrice(lq_sequencer::entry::OraclePriceCmd {
        price,
        observation_ts_ms,
        sources,
        override_band,
    })
}

/// Turn an [`Aggregation`] into the payload to publish, or `None` when the
/// daemon must not publish (insufficient sources / lost consensus).
pub fn payload_from_aggregation(
    agg: &Aggregation,
    observation_ts_ms: u64,
    override_band: bool,
) -> Option<lq_sequencer::entry::EntryPayload> {
    let price = agg.price()?;
    let sources = match agg {
        Aggregation::Fresh { sources, .. } => *sources,
        _ => return None,
    };
    Some(price_payload(price, observation_ts_ms, sources, override_band))
}
