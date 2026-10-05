//! Subaccounts and positions: the `x/subaccounts` analogue.
//!
//! Collateral is cash-basis: every fill moves cash between subaccounts, so
//! `Σ collateral` across all ledgers (insurance included) is conserved up to
//! the explicit deposit/withdrawal boundary. Unrealized PnL lives in
//! `equity = collateral + Σ qty · mark`, never in the cash balance.

use std::collections::BTreeMap;

use lq_sequencer::entry::MarketId;
use lq_types::{Price, Qty};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Subaccount number (dYdX uses `0..N` per address; here a bare `u64` until
/// accounts land in Stage 5 gateway auth).
pub type SubaccountId = u64;

/// Default subaccount for orders that do not name one.
pub const DEFAULT_SUBACCOUNT: SubaccountId = 0;

/// Reserved ledger for the insurance fund (`x/insurance` analogue). Not
/// externally fundable; receives liquidation fees, liquidation residuals and
/// absorbs shortfalls; ADL restores it when its equity goes negative.
pub const INSURANCE_SUBACCOUNT: SubaccountId = u64::MAX;

/// A per-market position. Cash-basis: `avg_entry` is display/accounting only
/// (realized PnL accumulates on the subaccount at close time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Signed base quantity (positive = long).
    pub base_qty: Qty,
    /// Volume-weighted average entry price of the *current* direction.
    pub avg_entry: Price,
    /// Funding index when this position last settled funding. Invariant:
    /// equals the market's current index for every position at rest, so the
    /// next settlement is zero-sum across holders.
    pub last_funding_index: Decimal,
}

impl Position {
    pub fn new(base_qty: Qty, avg_entry: Price, last_funding_index: Decimal) -> Self {
        Self {
            base_qty,
            avg_entry,
            last_funding_index,
        }
    }
}

/// One subaccount: cash balance + positions + accumulated realized PnL.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Subaccount {
    pub collateral: Decimal,
    /// Serialized as a sequence of `(market, position)` pairs: `serde_json`
    /// (the snapshot/wire format) rejects struct-typed map keys.
    #[serde(
        serialize_with = "ser_positions",
        deserialize_with = "de_positions",
        default
    )]
    pub positions: BTreeMap<MarketId, Position>,
    /// Realized PnL (fees excluded) accumulated over closed trade volumes.
    pub realized_pnl: Decimal,
}

fn ser_positions<S>(
    positions: &BTreeMap<MarketId, Position>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(positions.len()))?;
    for (market, position) in positions {
        seq.serialize_element(&(market, position))?;
    }
    seq.end()
}

fn de_positions<'de, D>(deserializer: D) -> Result<BTreeMap<MarketId, Position>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Vec::<(MarketId, Position)>::deserialize(deserializer)?
        .into_iter()
        .collect())
}

impl Subaccount {
    pub fn base_qty(&self, market: &MarketId) -> Qty {
        self.positions
            .get(market)
            .map(|p| p.base_qty)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn has_position(&self) -> bool {
        self.positions.values().any(|p| p.base_qty != Decimal::ZERO)
    }

    /// Apply a signed fill: cash flows elsewhere (see `state::apply_fill`);
    /// this mutates only the position leg and realized-PnL accounting.
    ///
    /// Deterministic and exact (`Decimal`); position entries are removed at
    /// flat so `Σ positions` scans never see zero dust.
    pub fn apply_fill(
        &mut self,
        market: &MarketId,
        signed_qty: Qty,
        price: Price,
        funding_index: Decimal,
    ) {
        if signed_qty == Decimal::ZERO {
            return;
        }
        let old = self.base_qty(market);
        let new = old + signed_qty;

        if old == Decimal::ZERO {
            // Open.
            self.positions.insert(
                market.clone(),
                Position::new(signed_qty, price, funding_index),
            );
            return;
        }

        let pos = self.positions.get_mut(market).expect("old != 0");
        let same_direction = (old.is_sign_negative() && signed_qty.is_sign_negative())
            || (old.is_sign_positive() && signed_qty.is_sign_positive());

        if same_direction {
            // Increase: volume-weighted average entry.
            let abs_old = old.abs();
            let abs_add = signed_qty.abs();
            let total = abs_old + abs_add;
            if total != Decimal::ZERO {
                pos.avg_entry = (abs_old * pos.avg_entry + abs_add * price) / total;
            }
            pos.base_qty = new;
        } else {
            // Reduce or flip: realize on the closed volume.
            let closing = old.abs().min(signed_qty.abs());
            let direction = if old.is_sign_positive() {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            self.realized_pnl += (price - pos.avg_entry) * closing * direction;
            if new == Decimal::ZERO {
                self.positions.remove(market);
            } else if (new.is_sign_negative() && old.is_sign_positive())
                || (new.is_sign_positive() && old.is_sign_negative())
            {
                // Flipped: remainder opens at the fill price.
                pos.base_qty = new;
                pos.avg_entry = price;
            } else {
                pos.base_qty = new;
            }
        }
    }
}
