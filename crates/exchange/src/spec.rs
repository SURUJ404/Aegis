//! Instrument specifications: tick sizes and lot sizes per venue.

use lq_types::{Price, Qty};
use serde::{Deserialize, Serialize};

/// A price in integer ticks. Internally the order book works on integer tick
/// prices (`PriceTick = u64`); conversion to/from `Decimal` uses the
/// instrument's tick size and precision.
pub type PriceTick = u64;

/// Per-instrument trading rules.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InstrumentSpec {
    /// Smallest price increment, e.g. `0.1` for BTC-USDT on some venues.
    pub tick_size: Price,
    /// Smallest quantity increment, e.g. `0.00001`.
    pub lot_size: Qty,
    pub min_qty: Qty,
    pub max_qty: Option<Qty>,
}

impl InstrumentSpec {
    pub fn new(tick_size: Price, lot_size: Qty) -> Self {
        Self {
            tick_size,
            lot_size,
            min_qty: lot_size,
            max_qty: None,
        }
    }

    /// Number of decimal places in the tick size.
    pub fn tick_precision(&self) -> u32 {
        self.tick_size.scale()
    }

    /// Round a raw price down to a valid tick.
    pub fn floor_tick(&self, price: Price) -> Price {
        price / self.tick_size
    }

    /// Convert a decimal price into integer ticks.
    pub fn to_ticks(&self, price: Price) -> PriceTick {
        // Round to nearest tick, then scale away the decimals.
        let ticks = (price / self.tick_size).round();
        ticks.as_i128().max(0) as PriceTick
    }

    /// Convert integer ticks back into a decimal price.
    pub fn from_ticks(&self, ticks: PriceTick) -> Price {
        self.tick_size * Price::from(ticks)
    }

    /// Round a quantity down to a valid lot.
    pub fn floor_lot(&self, qty: Qty) -> Qty {
        (qty / self.lot_size).floor() * self.lot_size
    }

    pub fn validate_qty(&self, qty: Qty) -> bool {
        qty > Qty::ZERO
            && qty >= self.min_qty
            && self
                .max_qty
                .map(|m| qty <= m)
                .unwrap_or(true)
            && (qty / self.lot_size).fract().is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversion_round_trips() {
        let spec = InstrumentSpec::new(rust_decimal_macros::dec!(0.1), rust_decimal_macros::dec!(0.01));
        let price = rust_decimal_macros::dec!(65432.1);
        let ticks = spec.to_ticks(price);
        assert_eq!(spec.from_ticks(ticks), price);
    }

    #[test]
    fn to_ticks_rounds_to_nearest() {
        let spec = InstrumentSpec::new(rust_decimal_macros::dec!(0.1), rust_decimal_macros::dec!(0.01));
        assert_eq!(spec.to_ticks(rust_decimal_macros::dec!(100.15)), 1002);
        assert_eq!(spec.from_ticks(1002), rust_decimal_macros::dec!(100.2));
        assert_eq!(spec.to_ticks(rust_decimal_macros::dec!(100.12)), 1001);
    }

    #[test]
    fn quantity_validation() {
        let spec = InstrumentSpec::new(rust_decimal_macros::dec!(0.1), rust_decimal_macros::dec!(0.01));
        assert!(spec.validate_qty(rust_decimal_macros::dec!(0.01)));
        assert!(!spec.validate_qty(rust_decimal_macros::dec!(0.005)));
    }
}