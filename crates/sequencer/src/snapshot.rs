//! Snapshot format: state bytes + sequence watermarks + state hash.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::codec::{from_json, to_json};
use crate::entry::MarketId;
use crate::hash::StateHash;

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec: {0}")]
    Codec(String),
    #[error("unsupported snapshot version {0}")]
    Version(u32),
    #[error("state decode failed: {0}")]
    State(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub global_seq: u64,
    pub market_seqs: Vec<(MarketId, u64)>,
    /// Serialized [`crate::state::LedgerState`] (or other SM) bytes.
    pub state_bytes: Vec<u8>,
    /// Hex SHA-256 of the state at snapshot time (verified on load).
    pub state_hash_hex: String,
    pub created_ts_ms: u64,
}

impl Snapshot {
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        to_json(self).map_err(|e| SnapshotError::Codec(e.to_string()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let snap: Snapshot = from_json(bytes).map_err(|e| SnapshotError::Codec(e.to_string()))?;
        if snap.version != SNAPSHOT_VERSION {
            return Err(SnapshotError::Version(snap.version));
        }
        Ok(snap)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, SnapshotError> {
        let bytes = std::fs::read(path.as_ref())?;
        Self::decode(&bytes)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), SnapshotError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path.as_ref(), self.encode()?)?;
        Ok(())
    }

    pub fn hash_matches(&self, actual: StateHash) -> bool {
        self.state_hash_hex == actual.as_hex()
    }
}

/// Candidate snapshot file names under a data directory (newest wins by
/// embedded global_seq after listing).
pub fn list_snapshot_files(
    dir: impl AsRef<Path>,
) -> Result<Vec<std::path::PathBuf>, SnapshotError> {
    let dir = dir.as_ref();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) == Some("snap") {
            out.push(path);
        }
    }
    Ok(out)
}
