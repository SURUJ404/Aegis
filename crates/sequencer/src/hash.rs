//! Canonical state hashing: stable across platforms and Rust versions.

use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

/// 32-byte SHA-256 of the canonical state encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateHash(pub [u8; 32]);

impl StateHash {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn as_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

impl std::fmt::Display for StateHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hex())
    }
}

/// Hasher used for all state-hash inputs.
pub type StateHasher = Sha256;

pub fn new_hasher() -> StateHasher {
    Sha256::new()
}

pub fn finish(hasher: StateHasher) -> StateHash {
    let out = hasher.finalize();
    let mut h = [0u8; 32];
    h.copy_from_slice(&out);
    StateHash(h)
}

/// Feed a `Decimal` in a canonical form: normalized (no trailing zeros) so
/// `1.10` and `1.1` hash identically. Fixed-point only — never `f64`.
pub fn write_decimal(hasher: &mut StateHasher, d: &Decimal) {
    let n = d.normalize();
    hasher.update(b"d:");
    hasher.update(n.to_string().as_bytes());
    hasher.update([0xff]);
}

pub fn write_u64(hasher: &mut StateHasher, v: u64) {
    hasher.update(b"u:");
    hasher.update(v.to_le_bytes());
    hasher.update([0xff]);
}

pub fn write_bytes(hasher: &mut StateHasher, b: &[u8]) {
    hasher.update(b"b:");
    hasher.update((b.len() as u64).to_le_bytes());
    hasher.update(b);
    hasher.update([0xff]);
}

pub fn write_str(hasher: &mut StateHasher, s: &str) {
    write_bytes(hasher, s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn decimal_scale_does_not_change_hash() {
        let mut a = new_hasher();
        let mut b = new_hasher();
        write_decimal(&mut a, &dec!(1.10));
        write_decimal(&mut b, &dec!(1.1));
        assert_eq!(finish(a), finish(b));
    }

    #[test]
    fn different_values_differ() {
        let mut a = new_hasher();
        let mut b = new_hasher();
        write_decimal(&mut a, &dec!(1.1));
        write_decimal(&mut b, &dec!(1.2));
        assert_ne!(finish(a), finish(b));
    }

    #[test]
    fn hex_is_stable() {
        let mut h = new_hasher();
        write_str(&mut h, "genesis");
        assert_eq!(finish(h).as_hex().len(), 64);
    }
}
