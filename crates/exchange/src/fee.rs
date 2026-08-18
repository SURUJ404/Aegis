//! Fee schedules.

use lq_types::Amount;
use serde::{Deserialize, Serialize};

/// A fee tier: taker and maker rates in basis points.
///
/// Fees are expressed in bps of notional. Positive means the venue charges the
/// side; negative means the venue rebates (maker rebate programs).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FeeTier {
    /// Taker fee in bps (charged when we cross the spread).
    pub taker_bps: f64,
    /// Maker fee in bps (charged when we rest on the book).
    pub maker_bps: f64,
}

impl FeeTier {
    pub const ZERO: FeeTier = FeeTier {
        taker_bps: 0.0,
        maker_bps: 0.0,
    };

    pub fn fee_for(&self, taker: bool, notional: Amount) -> Amount {
        let bps = if taker { self.taker_bps } else { self.maker_bps };
        let rate =
            rust_decimal::Decimal::from_f64_retain(bps).unwrap_or_default()
                / rust_decimal::Decimal::from(10_000);
        notional * rate
    }
}

/// Per-venue fee configuration. Real fee schedules vary by tier and volume;
/// the platform reads them from configuration and treats them as assumptions
/// that must be verified against the venue account (see `TRADING.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeeSchedule {
    pub venue: String,
    pub tier: FeeTier,
}

impl FeeSchedule {
    pub fn new(venue: impl Into<String>, tier: FeeTier) -> Self {
        Self {
            venue: venue.into(),
            tier,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taker_fee_on_notional() {
        let tier = FeeTier {
            taker_bps: 10.0,
            maker_bps: 0.0,
        };
        let fee = tier.fee_for(true, rust_decimal_macros::dec!(1000));
        assert_eq!(fee, rust_decimal_macros::dec!(1.0));
    }

    #[test]
    fn maker_rebate_is_negative() {
        let tier = FeeTier {
            taker_bps: 0.0,
            maker_bps: -0.5,
        };
        let fee = tier.fee_for(false, rust_decimal_macros::dec!(1000));
        assert_eq!(fee, rust_decimal_macros::dec!(-0.05));
    }
}