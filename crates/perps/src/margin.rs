//! Pure margin formulas (fixed-point `Decimal` only — no floats anywhere).
//!
//! Kept as free functions so property tests can exercise them without
//! constructing a full [`crate::state::PerpsState`].

use lq_types::{Price, Qty};
use rust_decimal::Decimal;

/// Smallest positive price representable at `QUOTE_DECIMALS` (8 dp). Used to
/// clamp liquidation limit prices away from zero/negative.
pub const MIN_PRICE: Price = Decimal::from_parts(1, 0, 0, false, 8);

/// Canonical scale for machine-computed prices (liquidation floor, ADL).
///
/// Bankruptcy/ADL prices divide equity by quantity, which can emit up to 28
/// significant digits. Moving such values between subaccount balances of
/// different magnitudes then rounds each balance differently and breaks the
/// collateral conservation invariant (found by property testing). Quantizing
/// every computed price to [`PRICE_SCALE`] keeps `balance ± (price · qty)`
/// exact within `Decimal`'s 28-digit budget.
pub const PRICE_SCALE: u32 = 9;

/// Margin requirement for one position: `|qty| · price · ratio`.
pub fn requirement(abs_qty: Qty, price: Price, ratio: Decimal) -> Decimal {
    abs_qty.abs() * price * ratio
}

/// Estimated taker fee on a fill (fee bps on notional, /10 000).
pub fn fee_estimate(notional: Decimal, fee_bps: Decimal) -> Decimal {
    notional * fee_bps / Decimal::from(10_000)
}

/// Liquidation limit price (dYdX bankruptcy price + liquidation fee).
///
/// Closing `close_qty` (signed, same sign as the position) at price `P`
/// changes equity by `close_qty · (P − mark) − fee`, so the price that leaves
/// exactly zero equity after the fee is:
///
/// ```text
/// P = mark + (fee − equity) / close_qty
/// ```
///
/// For a long (`close_qty > 0`) this sits **below** the mark (a floor the
/// book must beat); for a short it sits **above** the mark (a ceiling).
/// `fee = |close_qty| · mark · (taker_bps + liquidation_fee_bps) / 10 000`
/// so paying the real taker fees from the proceeds still lands at ≥ 0.
///
/// Clamped to [`MIN_PRICE`] and quantized to [`PRICE_SCALE`] (see there for
/// why the quantization is required for conservation).
pub fn liquidation_limit_price(
    mark: Price,
    equity: Decimal,
    close_qty: Qty,
    fee: Decimal,
) -> Price {
    debug_assert!(close_qty != Decimal::ZERO);
    let raw = mark + (fee - equity) / close_qty;
    let quantized = raw.round_dp(PRICE_SCALE);
    if quantized <= MIN_PRICE {
        MIN_PRICE
    } else {
        quantized
    }
}

/// True when `equity` has fallen below the maintenance requirement.
pub fn below_maintenance(equity: Decimal, maintenance: Decimal) -> bool {
    equity < maintenance
}
