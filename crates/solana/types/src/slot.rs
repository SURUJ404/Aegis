use std::fmt;

use serde::{Deserialize, Serialize};

/// Solana slot number — monotonically increasing block height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slot(pub u64);

impl Slot {
    pub fn new(slot: u64) -> Self {
        Self(slot)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn isancestor(self, other: Slot) -> bool {
        self.0 < other.0
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for Slot {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

/// Solana transaction signature — 64-byte ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxSignature(String);

impl TxSignature {
    pub fn new(sig: impl Into<String>) -> Self {
        Self(sig.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TxSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for TxSignature {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_ordering() {
        let a = Slot::new(100);
        let b = Slot::new(200);
        assert!(a < b);
        assert!(a.isancestor(b));
        assert!(!b.isancestor(a));
    }

    #[test]
    fn slot_display() {
        let s = Slot::new(42);
        assert_eq!(s.to_string(), "42");
    }

    #[test]
    fn signature_roundtrip() {
        let sig = TxSignature::new("5VERv8NMhJR8DpV1wXqNT3Ej3W5MQ9ZdYm6jMhKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjKjK");
        let sig2 = TxSignature::from(sig.as_str());
        assert_eq!(sig, sig2);
    }
}
