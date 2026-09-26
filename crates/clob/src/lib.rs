//! `lq-clob`: order-level central limit order book as a pure state machine.
//!
//! Stage 2 of the Aegis exchange core. Matching happens **inside** `apply`
//! of the sequencer log ([`lq_sequencer::state::StateMachine`]):
//!
//! - price-time priority (best price → FIFO within level)
//! - time-in-force: GTC, IOC, FOK, Post-only (+ market orders = IOC, never rest)
//! - self-trade prevention: none / cancel-resting / cancel-taker / cancel-both
//! - cancel/replace as one atomic log entry
//! - short-term orders (logical expiry via entry `ts_ms`) vs stateful orders
//!
//! Determinism: `BTreeMap` iteration only, `Decimal` money, time from the log,
//! no RNG, no wall clock. Same log ⇒ byte-identical [`StateMachine::state_hash`].
//!
//! Command-level failures (duplicate id, PostOnly cross, FOK unfilled, unknown
//! cancel, self-trade) are `Ok` paths with [`ApplyOutput::Rejected`] so the WAL
//! always replays; only sequence gaps return `Err`.

pub mod book;
pub mod matching;
pub mod order;
pub mod state;

pub use book::Book;
pub use matching::{exec_policy, would_cross, ExecFill, MatchOutcome, Policy};
pub use order::ClobOrder;
pub use state::{ClobConfig, ClobState, ClobStats};
