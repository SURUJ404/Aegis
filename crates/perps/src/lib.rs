//! `lq-perps`: the perpetuals margin state machine — subaccounts, initial and
//! maintenance margin, liquidation at the bankruptcy price, insurance fund,
//! ADL and funding — composed **over** [`lq_clob`] so that a fill and its
//! margin update happen in one atomic [`lq_sequencer::StateMachine::apply`].
//!
//! Architecture (Stage 3 of the dYdX v4-style rebuild, see
//! `docs/stages/STAGE_3_PERPS.md`):
//!
//! - [`PerpsState`] embeds [`lq_clob::ClobState`] and is the machine the
//!   [`lq_sequencer::Sequencer`] drives: pre-trade margin check → matching →
//!   cash/position/margin update, all inside one log entry.
//! - New log entries: `Transfer` (deposit/withdraw), `Liquidate`
//!   (bankruptcy-price execution against the book with the insurance fund as
//!   backstop) and `SettleFunding` (zero-sum funding payments).
//! - Risk limits from `lq-risk` are absorbed as pre-trade checks
//!   ([`PerpsState::pre_trade_check`]); the legacy `RiskEngine` remains only
//!   for the old in-process engine path until Stage 8.
//! - Block invariants ([`PerpsState::check_invariants`]): collateral
//!   conserved, `Σ positions = 0` per market, below-maintenance subaccounts
//!   always pending liquidation.
//!
//! Determinism: `BTreeMap`/`BTreeSet` only, `Decimal` fixed-point money, no
//! wall clock (time from `entry.ts_ms`), no RNG, no floats in state. Same log
//! ⇒ byte-identical [`lq_sequencer::hash::StateHash`].

pub mod check;
pub mod invariants;
pub mod margin;
pub mod params;
pub mod state;
pub mod subaccount;

pub use check::{PreTradeCode, PreTradeVerdict};
pub use invariants::InvariantViolation;
pub use margin::{fee_estimate, liquidation_limit_price, requirement, MIN_PRICE};
pub use params::{MarketParams, PerpsConfig, PerpsStats};
pub use state::PerpsState;
pub use subaccount::{
    Position, Subaccount, SubaccountId, DEFAULT_SUBACCOUNT, INSURANCE_SUBACCOUNT,
};
