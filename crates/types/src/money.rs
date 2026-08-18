//! Money and quantity primitives.
//!
//! Prices and quantities are `rust_decimal::Decimal` — arbitrary precision fixed
//! point. Quantities are restricted to a sane number of decimal places
//! ([`QUOTE_DECIMALS`]) by [`Qty`] constructors to avoid unrealistic dust.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Maximum number of fractional digits kept on quantities.
pub const QUOTE_DECIMALS: u32 = 8;

/// A price in quote currency (e.g. USDT per BTC).
pub type Price = Decimal;

/// A base-asset quantity (e.g. BTC).
pub type Qty = Decimal;

/// A notional / monetary amount in quote currency.
pub type Amount = Decimal;

/// Newtype that normalizes quantities and rejects invalid values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Money(pub Decimal);

impl Money {
    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    pub fn from_qty(qty: Decimal) -> Result<Self, MoneyError> {
        if qty.is_sign_negative() {
            return Err(MoneyError::Negative);
        }
        Ok(Self(qty.round_dp(QUOTE_DECIMALS)))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MoneyError {
    #[error("quantity is negative")]
    Negative,
    #[error("invalid decimal literal: {0}")]
    Parse(String),
}

impl FromStr for Money {
    type Err = MoneyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let d = Decimal::from_str(s).map_err(|e| MoneyError::Parse(e.to_string()))?;
        Self::from_qty(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_quote_decimals() {
        let m = Money::from_qty(Decimal::new(1234567890, 10)).unwrap();
        assert_eq!(m.0, Decimal::new(12345679, 8));
    }

    #[test]
    fn rejects_negative() {
        assert!(Money::from_qty(Decimal::new(-1, 0)).is_err());
    }

    #[test]
    fn parses() {
        let m: Money = "0.001".parse().unwrap();
        assert_eq!(m.0, Decimal::new(1, 3));
    }
}