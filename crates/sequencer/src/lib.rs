//! Deterministic sequencing and event-sourced state foundation.
//!
//! `lq-sequencer` is the write path for everything that touches money:
//!
//! - [`LogEntry`] — one command with a **global sequence** (total order) and a
//!   **per-market sequence** (book-local order).
//! - [`Sequencer`] — assigns sequences, appends to a CRC-framed WAL, applies to
//!   a [`StateMachine`], and snapshots.
//! - [`StateMachine`] — pure apply + canonical state hash. Same log ⇒
//!   byte-identical hash. No wall-clock, no RNG, no `HashMap` iteration order.
//! - [`rebuild`] — load newest snapshot, replay WAL suffix, return the state.
//!
//! Paper mode remains the only supported trading mode; this crate does not
//! route live orders.

pub mod codec;
pub mod entry;
pub mod hash;
pub mod replay;
pub mod sequencer;
pub mod snapshot;
pub mod state;
pub mod wal;

pub use codec::{decode_entry, encode_entry, CodecError};
pub use entry::{
    EntryPayload, FillCmd, FillLiquidity, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd,
    StpPolicy,
};
pub use hash::{write_decimal, StateHash};
pub use replay::{rebuild, rebuild_empty_log, ReplayError};
pub use sequencer::{Sequencer, SequencerConfig, SequencerError};
pub use snapshot::{Snapshot, SnapshotError};
pub use state::{ApplyError, ApplyOutput, CancelReason, LedgerOrder, LedgerState, StateMachine};
pub use wal::{Wal, WalError};
