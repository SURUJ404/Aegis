//! Rebuild state from an empty log or from snapshot + WAL.

use std::path::Path;

use crate::snapshot::{list_snapshot_files, Snapshot};
use crate::state::StateMachine;
use crate::wal::{Wal, WalError};

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("wal: {0}")]
    Wal(#[from] WalError),
    #[error("snapshot io: {0}")]
    SnapshotIo(#[from] std::io::Error),
    #[error("snapshot: {0}")]
    SnapshotErr(#[from] crate::snapshot::SnapshotError),
    #[error("snapshot decode: {0}")]
    Snapshot(String),
    #[error("state decode: {0}")]
    State(String),
    #[error("apply: {0}")]
    Apply(#[from] crate::state::ApplyError),
    #[error("hash mismatch after replay: recorded {recorded}, actual {actual}")]
    HashMismatch { recorded: String, actual: String },
}

/// Rebuild purely from WAL (empty start). Verifies entries apply cleanly.
pub fn rebuild_empty_log<S: StateMachine>(
    wal_path: impl AsRef<Path>,
    mut sm: S,
) -> Result<(S, crate::hash::StateHash), ReplayError> {
    let entries = Wal::read_all(wal_path)?;
    for entry in &entries {
        let _ = sm.apply(entry)?;
    }
    let hash = sm.state_hash();
    Ok((sm, hash))
}

/// Load newest snapshot under `snapshot_dir` (if any), then replay WAL suffix.
/// Returns the rebuilt state and its hash. Does not require a snapshot to exist.
pub fn rebuild<S: StateMachine>(
    snapshot_dir: impl AsRef<Path>,
    wal_path: impl AsRef<Path>,
    mut sm: S,
) -> Result<(S, crate::hash::StateHash), ReplayError> {
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    for path in list_snapshot_files(snapshot_dir)? {
        if let Ok(snap) = Snapshot::load(&path) {
            let prev = best.as_ref().map(|(s, _)| *s).unwrap_or(0);
            if snap.global_seq >= prev {
                best = Some((snap.global_seq, path));
            }
        }
    }

    let mut base_seq = 0u64;
    if let Some((_seq, path)) = best {
        let snap = Snapshot::load(&path)?;
        sm = S::decode_state(&snap.state_bytes).map_err(ReplayError::State)?;
        let actual = sm.state_hash();
        if !snap.hash_matches(actual) {
            return Err(ReplayError::HashMismatch {
                recorded: snap.state_hash_hex,
                actual: actual.as_hex(),
            });
        }
        base_seq = snap.global_seq;
    }

    let entries = Wal::read_all(wal_path)?;
    for entry in &entries {
        if entry.global_seq <= base_seq {
            continue;
        }
        let _ = sm.apply(entry)?;
    }

    let hash = sm.state_hash();
    Ok((sm, hash))
}
