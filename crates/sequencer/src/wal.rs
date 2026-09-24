//! Write-ahead log: length-prefixed, CRC32-protected JSON records.
//!
//! Append is durable before the sequencer applies the entry to state. A
//! truncated or corrupt **tail** is tolerated on recovery (stop at last good
//! record); corruption in the middle surfaces as [`WalError::CorruptRecord`].

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::codec::{decode_entry, encode_entry};
use crate::entry::LogEntry;

const HEADER_LEN: usize = 8;
const MAX_RECORD_LEN: u32 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WalError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("record length {0} exceeds maximum")]
    RecordTooLarge(u32),
    #[error("crc mismatch at offset {offset}")]
    CrcMismatch { offset: u64 },
    #[error("decode failed at offset {offset}: {detail}")]
    Decode { offset: u64, detail: String },
    #[error("corrupt record at offset {offset}")]
    CorruptRecord { offset: u64 },
}

/// Append-only WAL at a single file path.
pub struct Wal {
    path: PathBuf,
    file: File,
    /// Byte offset of the next append.
    len: u64,
    /// Fsync after every append (default true for money-path durability).
    sync_on_append: bool,
}

impl Wal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            len,
            sync_on_append: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn set_sync_on_append(&mut self, on: bool) {
        self.sync_on_append = on;
    }

    pub fn append(&mut self, entry: &LogEntry) -> Result<(), WalError> {
        let payload = encode_entry(entry).map_err(|e| WalError::Decode {
            offset: self.len,
            detail: e.to_string(),
        })?;
        if payload.len() as u32 > MAX_RECORD_LEN {
            return Err(WalError::RecordTooLarge(payload.len() as u32));
        }
        let len = payload.len() as u32;
        let crc = crc32fast::hash(&payload);
        let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&payload);
        self.file.write_all(&buf)?;
        if self.sync_on_append {
            self.file.sync_data()?;
        }
        self.len += buf.len() as u64;
        Ok(())
    }

    /// Read every intact record from the start of the file.
    ///
    /// Stops at a truncated tail (partial header/payload). Returns an error on
    /// CRC/decode failure of a **complete** record so mid-file corruption is not
    /// silently skipped.
    pub fn read_all(path: impl AsRef<Path>) -> Result<Vec<LogEntry>, WalError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = File::open(path)?;
        let mut out = Vec::new();
        let mut offset: u64 = 0;
        let file_len = file.metadata()?.len();

        loop {
            if offset + HEADER_LEN as u64 > file_len {
                break; // truncated header → clean stop
            }
            let mut header = [0u8; HEADER_LEN];
            file.read_exact(&mut header)?;
            let len = u32::from_le_bytes(header[0..4].try_into().unwrap());
            let crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if len > MAX_RECORD_LEN {
                return Err(WalError::RecordTooLarge(len));
            }
            let payload_start = offset + HEADER_LEN as u64;
            if payload_start + len as u64 > file_len {
                break; // truncated payload → clean stop
            }
            let mut payload = vec![0u8; len as usize];
            file.read_exact(&mut payload)?;
            let actual = crc32fast::hash(&payload);
            if actual != crc {
                return Err(WalError::CrcMismatch { offset });
            }
            let entry = decode_entry(&payload).map_err(|e| WalError::Decode {
                offset,
                detail: e.to_string(),
            })?;
            out.push(entry);
            offset = payload_start + len as u64;
            file.seek(SeekFrom::Start(offset))?;
        }
        Ok(out)
    }

    /// Truncate the file to `len` bytes (used when discarding a corrupt tail
    /// during recovery). Returns the previous length.
    pub fn truncate(&mut self, len: u64) -> Result<u64, WalError> {
        let prev = self.len;
        self.file.set_len(len)?;
        self.file.sync_data()?;
        self.file.seek(SeekFrom::End(0))?;
        self.len = len;
        Ok(prev)
    }

    /// Scan for the end of the last intact record; truncate any corrupt tail.
    /// Returns the number of records kept and bytes truncated.
    pub fn recover_tail(&mut self) -> Result<(usize, u64), WalError> {
        let mut good_end = 0u64;
        let mut count = 0usize;
        let mut file = File::open(&self.path)?;
        let file_len = file.metadata()?.len();
        let mut offset = 0u64;

        while offset + HEADER_LEN as u64 <= file_len {
            let mut header = [0u8; HEADER_LEN];
            if file.read_exact(&mut header).is_err() {
                break;
            }
            let len = u32::from_le_bytes(header[0..4].try_into().unwrap());
            let crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if len > MAX_RECORD_LEN {
                break;
            }
            let payload_start = offset + HEADER_LEN as u64;
            if payload_start + len as u64 > file_len {
                break;
            }
            let mut payload = vec![0u8; len as usize];
            if file.read_exact(&mut payload).is_err() {
                break;
            }
            if crc32fast::hash(&payload) != crc {
                break;
            }
            if decode_entry(&payload).is_err() {
                break;
            }
            count += 1;
            good_end = payload_start + len as u64;
            offset = good_end;
            let _ = file.seek(SeekFrom::Start(offset));
        }

        let truncated = file_len.saturating_sub(good_end);
        if truncated > 0 {
            self.truncate(good_end)?;
        } else {
            self.len = file_len;
        }
        Ok((count, truncated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EntryPayload, MarketId, MarketTickCmd};
    use lq_types::{Exchange, Symbol};
    use rust_decimal_macros::dec;

    fn entry(seq: u64) -> LogEntry {
        LogEntry {
            global_seq: seq,
            market_seq: seq,
            market: MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into())),
            ts_ms: seq * 10,
            payload: EntryPayload::MarketTick(MarketTickCmd {
                last: dec!(100) + rust_decimal::Decimal::from(seq),
                bid: None,
                ask: None,
            }),
        }
    }

    #[test]
    fn append_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        for i in 1..=5 {
            wal.append(&entry(i)).unwrap();
        }
        let all = Wal::read_all(&path).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].global_seq, 1);
        assert_eq!(all[4].global_seq, 5);
    }

    #[test]
    fn truncated_tail_is_skipped_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        wal.append(&entry(1)).unwrap();
        wal.append(&entry(2)).unwrap();
        let full = std::fs::read(&path).unwrap();
        // Chop the last record mid-payload.
        std::fs::write(&path, &full[..full.len() - 3]).unwrap();

        let all = Wal::read_all(&path).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].global_seq, 1);
    }

    #[test]
    fn recover_tail_truncates_corrupt_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let mut wal = Wal::open(&path).unwrap();
        wal.append(&entry(1)).unwrap();
        wal.append(&entry(2)).unwrap();
        drop(wal);

        // Corrupt the second record's payload (after its header).
        let mut bytes = std::fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();

        let mut wal = Wal::open(&path).unwrap();
        let (kept, truncated) = wal.recover_tail().unwrap();
        assert_eq!(kept, 1);
        assert!(truncated > 0);
        let all = Wal::read_all(&path).unwrap();
        assert_eq!(all.len(), 1);
    }
}
