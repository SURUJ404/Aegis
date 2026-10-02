//! Block invariants (dYdX `EndBlocker` analogue), checked after every apply
//! in tests and available to the sequencer/block hook in later stages.
//!
//! 1. **Collateral conserved** — `Σ collateral` over all ledgers (insurance
//!    included) equals `total_deposits − total_withdrawals`. Fills, fees,
//!    funding, liquidation and ADL are all internal transfers.
//! 2. **Σ positions = 0** — per market, over every subaccount including the
//!    insurance fund. Every fill touches exactly two sides.
//! 3. **Margin health visibility** — every non-insurance subaccount that holds
//!    a position and is below maintenance margin appears in the pending
//!    liquidation set (the liquidator daemon consumes it).

use std::fmt;

use lq_sequencer::entry::MarketId;

/// A violated block invariant. Never produced by `apply` itself (replay must
/// stay gap-tolerant); produced by [`crate::state::PerpsState::check_invariants`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvariantViolation {
    /// `Σ collateral` drifted from the deposit/withdrawal boundary.
    CollateralMismatch { expected: String, actual: String },
    /// Positions in a market do not net to zero.
    ZeroSumPositions { market: String, sum: String },
    /// Below-maintenance subaccount missing from the pending liquidation set.
    UnflaggedBelowMaintenance { subaccount: u64 },
    /// Pending set disagrees with a fresh recomputation (stale flag).
    FlagSetMismatch { expected: Vec<u64>, actual: Vec<u64> },
}

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CollateralMismatch { expected, actual } => {
                write!(
                    f,
                    "collateral not conserved: expected {expected}, got {actual}"
                )
            }
            Self::ZeroSumPositions { market, sum } => {
                write!(f, "positions in {market} sum to {sum}, expected 0")
            }
            Self::UnflaggedBelowMaintenance { subaccount } => {
                write!(
                    f,
                    "subaccount {subaccount} below maintenance but not pending liquidation"
                )
            }
            Self::FlagSetMismatch { expected, actual } => {
                write!(f, "pending set {actual:?}, expected {expected:?}")
            }
        }
    }
}

impl std::error::Error for InvariantViolation {}

/// Helper: sum of signed base quantities for a market across ledgers.
pub fn zero_sum_ok(sum: lq_types::Qty) -> bool {
    sum == rust_decimal::Decimal::ZERO
}

/// Convenience for tests: the market label used in violations.
pub fn market_label(market: &MarketId) -> String {
    market.to_string()
}
