//! High-resolution monotonic timestamps for latency measurement, plus wall-clock
//! millis for audit purposes.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (wall clock). Used for persistence, logs and
/// cross-venue comparison where a shared clock is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimestampMs(pub u64);

impl TimestampMs {
    pub fn now() -> Self {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(d.as_millis() as u64)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// A stopwatch over a [`std::time::Instant`] producing a duration in nanoseconds.
/// All latency histograms in the engine work in nanoseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct NanoDuration(pub u64);

impl NanoDuration {
    pub fn as_nanos(self) -> u64 {
        self.0
    }

    pub fn as_micros(self) -> f64 {
        self.0 as f64 / 1_000.0
    }

    pub fn as_millis(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_is_roughly_now() {
        let now = TimestampMs::now();
        assert!(now.0 > 1_700_000_000_000);
    }
}