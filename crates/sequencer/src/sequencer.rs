//! The sequencer: sole writer that assigns sequences, WAL-appends, applies.

use std::path::{Path, PathBuf};

use crate::entry::{EntryPayload, LogEntry, MarketId};
use crate::hash::StateHash;
use crate::snapshot::{list_snapshot_files, Snapshot, SnapshotError, SNAPSHOT_VERSION};
use crate::state::StateMachine;
use crate::wal::{Wal, WalError};

#[derive(Debug, thiserror::Error)]
pub enum SequencerError {
    #[error("wal: {0}")]
    Wal(#[from] WalError),
    #[error("snapshot: {0}")]
    Snapshot(#[from] SnapshotError),
    #[error("apply: {0}")]
    Apply(#[from] crate::state::ApplyError),
    #[error("state encode: {0}")]
    StateEncode(String),
    #[error("state decode: {0}")]
    StateDecode(String),
    #[error("snapshot state hash mismatch: recorded {recorded}, actual {actual}")]
    HashMismatch { recorded: String, actual: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct SequencerConfig {
    /// Directory holding `wal.log` and `*.snap`.
    pub data_dir: PathBuf,
    /// Take a snapshot every N successfully applied entries (0 = never).
    pub snapshot_every: u64,
    /// fsync WAL on every append (true for durability; false in tight tests).
    pub sync_on_append: bool,
    /// Logical clock source for entries when the caller does not pass `ts_ms`.
    /// Still never read inside the state machine — only stamped onto the entry.
    pub default_ts_ms: u64,
}

impl SequencerConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            snapshot_every: 1024,
            sync_on_append: true,
            default_ts_ms: 0,
        }
    }

    pub fn wal_path(&self) -> PathBuf {
        self.data_dir.join("wal.log")
    }

    pub fn snapshot_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }
}

/// Single-threaded sequencer over a [`StateMachine`].
pub struct Sequencer<S: StateMachine> {
    cfg: SequencerConfig,
    sm: S,
    wal: Wal,
    entries_since_snapshot: u64,
    applied_total: u64,
}

impl<S: StateMachine> Sequencer<S> {
    /// Open (or create) a sequencer directory: load newest valid snapshot,
    /// recover WAL tail, replay suffix onto the state machine.
    pub fn open(cfg: SequencerConfig, mut sm: S) -> Result<Self, SequencerError> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        std::fs::create_dir_all(cfg.snapshot_dir())?;

        let mut wal = Wal::open(cfg.wal_path())?;
        wal.set_sync_on_append(cfg.sync_on_append);
        let (_kept, _trunc) = wal.recover_tail()?;

        // Load newest snapshot if present.
        let mut snap_base_seq = 0u64;
        let mut best: Option<(u64, std::path::PathBuf)> = None;
        for path in list_snapshot_files(cfg.snapshot_dir())? {
            if let Ok(snap) = Snapshot::load(&path) {
                let prev = best.as_ref().map(|(s, _)| *s).unwrap_or(0);
                if snap.global_seq >= prev {
                    best = Some((snap.global_seq, path));
                }
            }
        }
        if let Some((seq, path)) = best {
            let snap = Snapshot::load(&path)?;
            let decoded = S::decode_state(&snap.state_bytes).map_err(SequencerError::StateDecode)?;
            let actual = decoded.state_hash();
            if !snap.hash_matches(actual) {
                return Err(SequencerError::HashMismatch {
                    recorded: snap.state_hash_hex.clone(),
                    actual: actual.as_hex(),
                });
            }
            sm = decoded;
            snap_base_seq = seq;
            if sm.last_global_seq() != seq {
                return Err(SequencerError::HashMismatch {
                    recorded: format!("snapshot seq {seq}"),
                    actual: format!("state seq {}", sm.last_global_seq()),
                });
            }
        }

        // Replay WAL entries after the snapshot watermark.
        let entries = Wal::read_all(cfg.wal_path())?;
        let mut applied = 0u64;
        for entry in &entries {
            if entry.global_seq <= snap_base_seq {
                continue;
            }
            sm.apply(entry)?;
            applied += 1;
        }

        let snapshot_every = cfg.snapshot_every;
        let entries_since = if snapshot_every > 0 {
            applied % snapshot_every
        } else {
            0
        };

        Ok(Self {
            cfg,
            sm,
            wal,
            entries_since_snapshot: entries_since,
            applied_total: applied,
        })
    }

    pub fn state(&self) -> &S {
        &self.sm
    }

    pub fn state_mut(&mut self) -> &mut S {
        &mut self.sm
    }

    pub fn state_hash(&self) -> StateHash {
        self.sm.state_hash()
    }

    pub fn last_global_seq(&self) -> u64 {
        self.sm.last_global_seq()
    }

    pub fn applied_total(&self) -> u64 {
        self.applied_total
    }

    /// Assign sequences, append to WAL, apply to state. Logical time is
    /// `ts_ms` (or `cfg.default_ts_ms` when `None`).
    pub fn append(
        &mut self,
        market: MarketId,
        ts_ms: Option<u64>,
        payload: EntryPayload,
    ) -> Result<LogEntry, SequencerError> {
        let global_seq = self.sm.last_global_seq() + 1;
        let market_seq = self.sm.market_seq(&market) + 1;
        let entry = LogEntry {
            global_seq,
            market_seq,
            market,
            ts_ms: ts_ms.unwrap_or(self.cfg.default_ts_ms),
            payload,
        };

        // Durability first: WAL, then state.
        self.wal.append(&entry)?;
        self.sm.apply(&entry)?;
        self.applied_total += 1;
        self.entries_since_snapshot += 1;

        if self.cfg.snapshot_every > 0 && self.entries_since_snapshot >= self.cfg.snapshot_every {
            self.write_snapshot()?;
        }
        Ok(entry)
    }

    /// Force a snapshot of the current state.
    pub fn write_snapshot(&mut self) -> Result<PathBuf, SequencerError> {
        let state_bytes = self.sm.encode_state().map_err(SequencerError::StateEncode)?;
        let hash = self.sm.state_hash();
        let market_seqs = self.sm.market_seqs();
        let snap = Snapshot {
            version: SNAPSHOT_VERSION,
            global_seq: self.sm.last_global_seq(),
            market_seqs,
            state_bytes,
            state_hash_hex: hash.as_hex(),
            created_ts_ms: 0, // logical: not read by SM; wall clock only for humans
        };
        let name = format!("snapshot-{:020}.snap", snap.global_seq);
        let path = self.cfg.snapshot_dir().join(name);
        snap.save(&path)?;
        self.entries_since_snapshot = 0;
        Ok(path)
    }

    pub fn config(&self) -> &SequencerConfig {
        &self.cfg
    }

    pub fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }
}
