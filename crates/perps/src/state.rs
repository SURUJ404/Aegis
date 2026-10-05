//! The perps margin state machine: implements [`StateMachine`] over the
//! sequencer log, composing [`ClobState`] so a fill and its margin update are
//! **one atomic transition**.
//!
//! Determinism contract (same as `lq-clob`):
//! - only `BTreeMap`/`BTreeSet` iteration, never `HashMap`
//! - time comes exclusively from `entry.ts_ms`
//! - no RNG, no wall clock; `Decimal` fixed-point only, no floats in state
//! - `apply` returns `Err` **only** for sequence gaps. Every command-level
//!   problem (bad margin, reduce-only violation, healthy liquidation target,
//!   insufficient withdrawal collateral, …) is an `Ok` path with an
//!   `ApplyOutput::Rejected` — the WAL always replays cleanly.
//!
//! Money model (cash-basis, see `docs/stages/STAGE_3_PERPS.md`):
//! - `equity(sub) = collateral + Σ qty · price` per position
//! - fills move cash between the two counterparty subaccounts; fees are
//!   credited to the insurance ledger, so `Σ collateral` is conserved up to
//!   the explicit deposit/withdrawal boundary
//! - initial/maintenance requirements are `Σ |qty| · price · ratio` with
//!   per-market ratios held (and hashed) in state

use std::collections::{BTreeMap, BTreeSet};

use lq_clob::state::ClobState;
use lq_oracle::{OracleBook, OracleOutcome, OracleParams};
use lq_sequencer::entry::{EntryPayload, LogEntry, MarketId, PlaceOrderCmd};
use lq_sequencer::hash::{
    finish, new_hasher, write_bytes, write_decimal, write_str, write_u64, StateHash,
};
use lq_sequencer::state::{ApplyError, ApplyOutput, CancelReason, StateMachine};
use lq_types::{Price, Qty, Side};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::check::{PreTradeCode, PreTradeVerdict};
use crate::invariants::InvariantViolation;
use crate::margin::{below_maintenance, fee_estimate, liquidation_limit_price, requirement};
use crate::params::{MarketParams, PerpsConfig, PerpsStats};
use crate::subaccount::{Subaccount, SubaccountId, DEFAULT_SUBACCOUNT, INSURANCE_SUBACCOUNT};

/// High half marker for deterministic liquidation order ids: keeps synthetic
/// ids out of the low `global_seq` range clients use and makes them trivially
/// recognizable in tests/logs.
const LIQUIDATION_UUID_PREFIX: u128 = 0x6C71_6C69_7100_0000 << 64;

/// Event-sourced perpetuals state: subaccounts + margin + liquidation +
/// insurance + ADL + funding, with the CLOB embedded for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerpsState {
    pub(crate) cfg: PerpsConfig,
    pub(crate) clob: ClobState,
    pub(crate) last_global_seq: u64,
    pub(crate) market_seqs: BTreeMap<MarketId, u64>,
    pub(crate) subaccounts: BTreeMap<SubaccountId, Subaccount>,
    pub(crate) market_params: BTreeMap<MarketId, MarketParams>,
    pub(crate) funding_index: BTreeMap<MarketId, Decimal>,
    /// Which subaccount owns each live order (attribution for fills).
    pub(crate) order_owner: BTreeMap<Uuid, SubaccountId>,
    /// Open (non-terminal) order ids per subaccount (`max_open_orders`).
    pub(crate) open_orders: BTreeMap<SubaccountId, BTreeSet<Uuid>>,
    /// Open reduce-only order ids per (subaccount, market).
    pub(crate) reduce_only_open: BTreeMap<(SubaccountId, MarketId), BTreeSet<Uuid>>,
    /// Subaccounts at/below maintenance margin awaiting the liquidator.
    pub(crate) pending_liquidations: BTreeSet<SubaccountId>,
    pub(crate) total_deposits: Decimal,
    pub(crate) total_withdrawals: Decimal,
    /// Marks from `MarketTick` entries (preferred price source).
    pub(crate) tick_marks: BTreeMap<MarketId, Price>,
    /// Last trade price per market (fallback until the first tick).
    pub(crate) last_trade: BTreeMap<MarketId, Price>,
    /// Liquidation notional samples inside the cascade-breaker window.
    pub(crate) liquidation_windows: BTreeMap<MarketId, Vec<(u64, Decimal)>>,
    /// Stage 4: oracle prices + per-market deviation/staleness circuit
    /// breakers (`lq-oracle`). Fed only by `EntryPayload::OraclePrice`
    /// entries; consulted for the reference price and the new-risk gate.
    pub(crate) oracle: OracleBook,
    pub(crate) stats: PerpsStats,
}

#[derive(Debug, Serialize, Deserialize)]
struct PerpsWire {
    cfg: PerpsConfig,
    clob_state: Vec<u8>,
    last_global_seq: u64,
    market_seqs: Vec<(MarketId, u64)>,
    subaccounts: Vec<(SubaccountId, Subaccount)>,
    market_params: Vec<(MarketId, MarketParams)>,
    funding_index: Vec<(MarketId, Decimal)>,
    order_owner: Vec<(Uuid, SubaccountId)>,
    open_orders: Vec<(SubaccountId, Vec<Uuid>)>,
    reduce_only_open: Vec<(SubaccountId, MarketId, Vec<Uuid>)>,
    pending_liquidations: Vec<SubaccountId>,
    total_deposits: Decimal,
    total_withdrawals: Decimal,
    tick_marks: Vec<(MarketId, Price)>,
    last_trade: Vec<(MarketId, Price)>,
    liquidation_windows: Vec<(MarketId, Vec<(u64, Decimal)>)>,
    /// Stage 3 snapshots predate the oracle; absent = empty book (default
    /// params), so old WAL/snapshot pairs still decode.
    #[serde(default)]
    oracle: OracleBook,
    stats: PerpsStats,
}

/// Internal bookkeeping for the second phase of a liquidation (residual
/// close against the insurance ledger, after book fills have settled).
struct LiquidationMeta {
    subaccount: SubaccountId,
    market: MarketId,
    /// Signed quantity to close (same sign as the position).
    close_qty: Qty,
    limit_price: Price,
    order_id: Uuid,
}

/// Parameters for one two-sided fill: both counterparty subaccounts, the
/// match terms, and the taker's side (the maker is always the opposite).
struct FillParams<'a> {
    taker: SubaccountId,
    maker: SubaccountId,
    market: &'a MarketId,
    price: Price,
    quantity: Qty,
    taker_fee: Decimal,
    maker_fee: Decimal,
    taker_side: Side,
}

impl PerpsState {
    pub fn new() -> Self {
        Self::with_config(PerpsConfig::default())
    }

    pub fn with_config(cfg: PerpsConfig) -> Self {
        Self {
            clob: ClobState::new(),
            cfg,
            last_global_seq: 0,
            market_seqs: BTreeMap::new(),
            subaccounts: BTreeMap::new(),
            market_params: BTreeMap::new(),
            funding_index: BTreeMap::new(),
            order_owner: BTreeMap::new(),
            open_orders: BTreeMap::new(),
            reduce_only_open: BTreeMap::new(),
            pending_liquidations: BTreeSet::new(),
            total_deposits: Decimal::ZERO,
            total_withdrawals: Decimal::ZERO,
            tick_marks: BTreeMap::new(),
            last_trade: BTreeMap::new(),
            liquidation_windows: BTreeMap::new(),
            oracle: OracleBook::new(),
            stats: PerpsStats::default(),
        }
    }

    /// Override margin parameters for one market (hashed; replicas must agree).
    pub fn with_market_params(mut self, market: &MarketId, params: MarketParams) -> Self {
        self.market_params.insert(market.clone(), params);
        self
    }

    /// Override oracle circuit-breaker parameters (hashed; replicas must
    /// agree). Call before feeding any `OraclePrice` entries.
    pub fn with_oracle_params(mut self, params: OracleParams) -> Self {
        self.oracle = OracleBook::with_params(params);
        self
    }

    // ---- accessors --------------------------------------------------------

    pub fn clob(&self) -> &ClobState {
        &self.clob
    }

    pub fn config(&self) -> PerpsConfig {
        self.cfg
    }

    pub fn stats(&self) -> PerpsStats {
        self.stats
    }

    pub fn subaccount(&self, id: SubaccountId) -> Option<&Subaccount> {
        self.subaccounts.get(&id)
    }

    pub fn collateral(&self, id: SubaccountId) -> Decimal {
        self.subaccounts
            .get(&id)
            .map(|s| s.collateral)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn position(&self, subaccount: SubaccountId, market: &MarketId) -> Qty {
        self.subaccounts
            .get(&subaccount)
            .map(|s| s.base_qty(market))
            .unwrap_or(Decimal::ZERO)
    }

    /// Subaccounts currently below maintenance margin (the liquidator daemon
    /// consumes this view to submit `Liquidate` entries).
    pub fn pending_liquidations(&self) -> Vec<SubaccountId> {
        self.pending_liquidations.iter().copied().collect()
    }

    pub fn funding_index(&self, market: &MarketId) -> Decimal {
        self.funding_index
            .get(market)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub fn market_params(&self, market: &MarketId) -> MarketParams {
        self.market_params
            .get(market)
            .copied()
            .unwrap_or(self.cfg.default_market_params)
    }

    /// Reference price for margin: oracle price when the market is covered,
    /// else latest tick mark, else latest trade price.
    pub fn price_of(&self, market: &MarketId) -> Option<Price> {
        self.oracle
            .price(market)
            .or_else(|| self.tick_marks.get(market).copied())
            .or_else(|| self.last_trade.get(market).copied())
    }

    /// The oracle book (read access for daemons, APIs and tests).
    pub fn oracle(&self) -> &OracleBook {
        &self.oracle
    }

    /// Circuit-breaker gate for new risk in `market` at logical time
    /// `ts_ms`: `Some("oracle_halted")` / `Some("oracle_stale")` while the
    /// market must refuse places, replaces, liquidations and funding
    /// settlements. Exported for the Stage 5 gateway's CheckTx validation.
    pub fn oracle_gate(&self, market: &MarketId, ts_ms: u64) -> Option<&'static str> {
        self.oracle.gate(market, ts_ms)
    }

    /// Equity = cash + Σ position marked to the reference price.
    pub fn equity_of(&self, subaccount: SubaccountId) -> Decimal {
        let Some(sa) = self.subaccounts.get(&subaccount) else {
            return Decimal::ZERO;
        };
        let mut equity = sa.collateral;
        for (market, pos) in &sa.positions {
            let px = self.price_of(market).unwrap_or(pos.avg_entry);
            equity += pos.base_qty * px;
        }
        equity
    }

    pub fn initial_margin_of(&self, subaccount: SubaccountId) -> Decimal {
        self.requirement_of(subaccount, |p| p.initial_margin_ratio)
    }

    pub fn maintenance_margin_of(&self, subaccount: SubaccountId) -> Decimal {
        self.requirement_of(subaccount, |p| p.maintenance_margin_ratio)
    }

    fn requirement_of(
        &self,
        subaccount: SubaccountId,
        ratio: fn(&MarketParams) -> Decimal,
    ) -> Decimal {
        let Some(sa) = self.subaccounts.get(&subaccount) else {
            return Decimal::ZERO;
        };
        let mut req = Decimal::ZERO;
        for (market, pos) in &sa.positions {
            if pos.base_qty == Decimal::ZERO {
                continue;
            }
            let px = self.price_of(market).unwrap_or(pos.avg_entry);
            req += requirement(pos.base_qty, px, ratio(&self.market_params(market)));
        }
        req
    }

    // ---- block invariants -------------------------------------------------

    /// Per-block invariants (dYdX `EndBlocker` analogue). Called by tests
    /// after **every** entry; available to future block hooks / Raft.
    pub fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // 1. Collateral conserved across all ledgers (insurance included).
        let actual: Decimal = self.subaccounts.values().map(|s| s.collateral).sum();
        let expected = self.total_deposits - self.total_withdrawals;
        if actual != expected {
            return Err(InvariantViolation::CollateralMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }

        // 2. Positions net to zero per market (insurance is a ledger too).
        let mut totals: BTreeMap<&MarketId, Decimal> = BTreeMap::new();
        for sa in self.subaccounts.values() {
            for (market, pos) in &sa.positions {
                *totals.entry(market).or_insert(Decimal::ZERO) += pos.base_qty;
            }
        }
        for (market, sum) in totals {
            if sum != Decimal::ZERO {
                return Err(InvariantViolation::ZeroSumPositions {
                    market: market.to_string(),
                    sum: sum.to_string(),
                });
            }
        }

        // 3. Pending set exactly matches "below maintenance with a position".
        let recomputed = self.compute_flagged();
        let stored: Vec<SubaccountId> = self.pending_liquidations.iter().copied().collect();
        for id in &recomputed {
            if !stored.contains(id) {
                return Err(InvariantViolation::UnflaggedBelowMaintenance { subaccount: *id });
            }
        }
        if recomputed != stored {
            return Err(InvariantViolation::FlagSetMismatch {
                expected: recomputed,
                actual: stored,
            });
        }
        Ok(())
    }

    fn compute_flagged(&self) -> Vec<SubaccountId> {
        let mut flagged = Vec::new();
        for (&id, sa) in &self.subaccounts {
            if id == INSURANCE_SUBACCOUNT || !sa.has_position() {
                continue;
            }
            if below_maintenance(self.equity_of(id), self.maintenance_margin_of(id)) {
                flagged.push(id);
            }
        }
        flagged
    }

    // ---- pre-trade margin / risk check (absorbs `lq-risk`) ----------------

    /// Check an order against margin and operator limits **without** mutating
    /// state. `excluding` skips one open order's reservation (replace flows).
    ///
    /// This is the `checkTx` analogue for the Stage 5 gateway and the gate
    /// enforced inside `apply` before the CLOB ever sees a place/replace.
    pub fn pre_trade_check(
        &self,
        market: &MarketId,
        cmd: &PlaceOrderCmd,
        excluding: Option<Uuid>,
    ) -> PreTradeVerdict {
        let sub = cmd.subaccount.unwrap_or(DEFAULT_SUBACCOUNT);
        if sub == INSURANCE_SUBACCOUNT {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::ReservedSubaccount,
            };
        }
        if cmd.quantity <= Decimal::ZERO {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::InvalidQuantity,
            };
        }
        // Oversized scales break exact cash arithmetic (see `PRICE_SCALE`):
        // reject at the boundary instead of silently rounding client orders.
        if cmd.quantity.normalize().scale() > crate::margin::PRICE_SCALE {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::InvalidQuantity,
            };
        }
        if let Some(p) = cmd.price {
            if p <= Decimal::ZERO {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::InvalidPrice,
                };
            }
            if p.normalize().scale() > crate::margin::PRICE_SCALE {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::InvalidPrice,
                };
            }
        }
        let Some(ref_px) = self.price_of(market).or(cmd.price) else {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::NoMarkPrice,
            };
        };

        // Operator limits absorbed from `lq-risk`.
        if self.cfg.max_order_qty > Decimal::ZERO && cmd.quantity > self.cfg.max_order_qty {
            return PreTradeVerdict::Reduce {
                qty: self.cfg.max_order_qty,
                code: PreTradeCode::MaxOrderQty,
            };
        }
        let fill_px = cmd.price.unwrap_or(ref_px);
        let notional = cmd.quantity * fill_px;
        if self.cfg.max_notional_per_order > Decimal::ZERO
            && notional > self.cfg.max_notional_per_order
        {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::MaxNotional,
            };
        }
        if self.cfg.max_position_qty > Decimal::ZERO {
            let signed = match cmd.side {
                Side::Bid => cmd.quantity,
                Side::Ask => -cmd.quantity,
            };
            let projected = self.position(sub, market) + signed;
            if projected.abs() > self.cfg.max_position_qty {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::MaxPosition,
                };
            }
        }
        if self.cfg.max_open_orders > 0 {
            let count = self
                .open_orders
                .get(&sub)
                .map(|set| set.iter().filter(|id| excluding != Some(**id)).count())
                .unwrap_or(0);
            if count >= self.cfg.max_open_orders as usize {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::MaxOpenOrders,
                };
            }
        }

        // Reduce-only: may only shrink the existing position.
        if cmd.reduce_only {
            let pos = self.position(sub, market);
            let reduces_long = matches!(cmd.side, Side::Ask);
            let wrong_direction = pos == Decimal::ZERO
                || (reduces_long && pos < Decimal::ZERO)
                || (!reduces_long && pos > Decimal::ZERO);
            if wrong_direction {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::ReduceOnlyExceedsPosition,
                };
            }
            let open_ro = self.open_reduce_only_qty(sub, market, excluding);
            if open_ro + cmd.quantity > pos.abs() {
                return PreTradeVerdict::Reject {
                    code: PreTradeCode::ReduceOnlyExceedsPosition,
                };
            }
        }

        // Margin: projected equity vs initial (incl. open-order reservation)
        // and maintenance requirements after the hypothetical fill.
        let params = self.market_params(market);
        let pos = self.position(sub, market);
        let signed = match cmd.side {
            Side::Bid => cmd.quantity,
            Side::Ask => -cmd.quantity,
        };
        let taker_fee = fee_estimate(notional, self.clob.config().fee_taker_bps);
        let equity_after = self.equity_of(sub) + signed * (ref_px - fill_px) - taker_fee;
        let im_after = self.requirement_with_projection(sub, market, pos + signed, ref_px, |p| {
            p.initial_margin_ratio
        });
        let mm_after = self.requirement_with_projection(sub, market, pos + signed, ref_px, |p| {
            p.maintenance_margin_ratio
        });
        let reserve = self.open_order_margin_reserve(sub, excluding);
        let margin_required = im_after + reserve;

        if equity_after < margin_required {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::InsufficientMargin,
            };
        }
        if equity_after < mm_after {
            return PreTradeVerdict::Reject {
                code: PreTradeCode::BelowMaintenance,
            };
        }
        let _ = params;
        PreTradeVerdict::Allow { margin_required }
    }

    fn open_reduce_only_qty(
        &self,
        sub: SubaccountId,
        market: &MarketId,
        excluding: Option<Uuid>,
    ) -> Qty {
        let Some(set) = self.reduce_only_open.get(&(sub, market.clone())) else {
            return Decimal::ZERO;
        };
        let mut total = Decimal::ZERO;
        for id in set {
            if excluding == Some(*id) {
                continue;
            }
            if let Some(o) = self.clob.order(*id) {
                if !o.status.is_terminal() {
                    total += o.remaining();
                }
            }
        }
        total
    }

    /// Requirement for `subaccount` where the position in `market` is
    /// projected to `projected_qty` priced at `projected_px`.
    fn requirement_with_projection(
        &self,
        subaccount: SubaccountId,
        market: &MarketId,
        projected_qty: Qty,
        projected_px: Price,
        ratio: fn(&MarketParams) -> Decimal,
    ) -> Decimal {
        let mut req = Decimal::ZERO;
        let mut covered = false;
        if let Some(sa) = self.subaccounts.get(&subaccount) {
            for (m, pos) in &sa.positions {
                if m == market {
                    covered = true;
                    req += requirement(projected_qty, projected_px, ratio(&self.market_params(m)));
                } else if pos.base_qty != Decimal::ZERO {
                    let px = self.price_of(m).unwrap_or(pos.avg_entry);
                    req += requirement(pos.base_qty, px, ratio(&self.market_params(m)));
                }
            }
        }
        if !covered && projected_qty != Decimal::ZERO {
            req += requirement(
                projected_qty,
                projected_px,
                ratio(&self.market_params(market)),
            );
        }
        req
    }

    /// Marginal initial-margin increase if every other open order of the
    /// subaccount filled at its limit (worst-case, direction aware: orders
    /// that *reduce* an existing position reserve nothing).
    fn open_order_margin_reserve(
        &self,
        subaccount: SubaccountId,
        excluding: Option<Uuid>,
    ) -> Decimal {
        let Some(ids) = self.open_orders.get(&subaccount) else {
            return Decimal::ZERO;
        };
        let mut reserve = Decimal::ZERO;
        for id in ids {
            if excluding == Some(*id) {
                continue;
            }
            let Some(o) = self.clob.order(*id) else {
                continue;
            };
            if o.status.is_terminal() || o.remaining() == Decimal::ZERO {
                continue;
            }
            let Some(px) = self.price_of(&o.market).or(o.price) else {
                continue;
            };
            let pos = self.position(subaccount, &o.market);
            let delta = match o.side {
                Side::Bid => o.remaining(),
                Side::Ask => -o.remaining(),
            };
            let increase = (pos + delta).abs() - pos.abs();
            if increase > Decimal::ZERO {
                reserve += requirement(
                    increase,
                    px,
                    self.market_params(&o.market).initial_margin_ratio,
                );
            }
        }
        reserve
    }

    // ---- bookkeeping helpers ---------------------------------------------

    fn expect_global(&self, entry: &LogEntry) -> Result<(), ApplyError> {
        let expected = self.last_global_seq + 1;
        if entry.global_seq != expected {
            return Err(ApplyError::GlobalSeqGap {
                expected,
                got: entry.global_seq,
            });
        }
        Ok(())
    }

    fn expect_market(&self, entry: &LogEntry) -> Result<(), ApplyError> {
        let expected = self.market_seq_of(&entry.market) + 1;
        if entry.market_seq != expected {
            return Err(ApplyError::MarketSeqGap {
                market: entry.market.to_string(),
                expected,
                got: entry.market_seq,
            });
        }
        Ok(())
    }

    fn market_seq_of(&self, market: &MarketId) -> u64 {
        self.market_seqs.get(market).copied().unwrap_or(0)
    }

    /// Owner string used for STP: subaccount-scoped when the command names
    /// one, verbatim otherwise (legacy behaviour preserved).
    fn normalize_owner(cmd: &PlaceOrderCmd, sub: SubaccountId) -> PlaceOrderCmd {
        let mut out = cmd.clone();
        if cmd.subaccount.is_some() {
            out.owner = format!("sub:{sub}");
        }
        out
    }

    fn track_place(
        &mut self,
        sub: SubaccountId,
        order_id: Uuid,
        market: &MarketId,
        reduce_only: bool,
    ) {
        self.order_owner.insert(order_id, sub);
        self.open_orders.entry(sub).or_default().insert(order_id);
        if reduce_only {
            self.reduce_only_open
                .entry((sub, market.clone()))
                .or_default()
                .insert(order_id);
        }
    }

    fn close_bookkeeping(&mut self, order_id: Uuid) {
        let Some(sub) = self.order_owner.remove(&order_id) else {
            return;
        };
        if let Some(set) = self.open_orders.get_mut(&sub) {
            set.remove(&order_id);
            if set.is_empty() {
                self.open_orders.remove(&sub);
            }
        }
        let market = self.clob.order(order_id).map(|o| o.market.clone());
        if let Some(market) = market {
            let key = (sub, market);
            if let Some(set) = self.reduce_only_open.get_mut(&key) {
                set.remove(&order_id);
                if set.is_empty() {
                    self.reduce_only_open.remove(&key);
                }
            }
        }
    }

    /// Undo `track_place` when the CLOB refused to rest the order.
    fn untrack(&mut self, sub: SubaccountId, market: &MarketId, order_id: Uuid) {
        self.order_owner.remove(&order_id);
        if let Some(set) = self.open_orders.get_mut(&sub) {
            set.remove(&order_id);
            if set.is_empty() {
                self.open_orders.remove(&sub);
            }
        }
        let key = (sub, market.clone());
        if let Some(set) = self.reduce_only_open.get_mut(&key) {
            set.remove(&order_id);
            if set.is_empty() {
                self.reduce_only_open.remove(&key);
            }
        }
    }

    /// Cash + position legs for one match. Both counterparty subaccounts are
    /// updated in the same `apply` call (atomic fill + margin transition).
    fn apply_fill(&mut self, f: FillParams<'_>) {
        let FillParams {
            taker,
            maker,
            market,
            price,
            quantity,
            taker_fee,
            maker_fee,
            taker_side,
        } = f;
        let taker_signed = match taker_side {
            Side::Bid => quantity,
            Side::Ask => -quantity,
        };
        let maker_signed = -taker_signed;
        let notional = price * quantity;
        let funding_idx = self.funding_index(market);

        for (sub, signed, fee) in [
            (taker, taker_signed, taker_fee),
            (maker, maker_signed, maker_fee),
        ] {
            let entry = self.subaccounts.entry(sub).or_default();
            if signed > Decimal::ZERO {
                entry.collateral -= notional + fee;
            } else {
                entry.collateral += notional - fee;
            }
            entry.apply_fill(market, signed, price, funding_idx);
        }
        if taker_fee + maker_fee != Decimal::ZERO {
            self.subaccounts
                .entry(INSURANCE_SUBACCOUNT)
                .or_default()
                .collateral += taker_fee + maker_fee;
        }
        self.last_trade.insert(market.clone(), price);
    }

    /// Reconcile subaccount-side bookkeeping against CLOB outputs, then
    /// revalidate reduce-only orders whose position shrank.
    fn process_clob_outputs(
        &mut self,
        clob_out: &[ApplyOutput],
        touched: &mut BTreeSet<(SubaccountId, MarketId)>,
        out: &mut Vec<ApplyOutput>,
        ts_ms: u64,
    ) {
        for output in clob_out {
            match output {
                ApplyOutput::Fill {
                    taker_order_id,
                    maker_order_id,
                    market,
                    price,
                    quantity,
                    taker_fee,
                    maker_fee,
                    ..
                } => {
                    let taker_side = match self.clob.order(*taker_order_id) {
                        Some(o) => o.side,
                        None => continue,
                    };
                    let (Some(&taker_sub), Some(&maker_sub)) = (
                        self.order_owner.get(taker_order_id),
                        self.order_owner.get(maker_order_id),
                    ) else {
                        continue;
                    };
                    self.apply_fill(FillParams {
                        taker: taker_sub,
                        maker: maker_sub,
                        market,
                        price: *price,
                        quantity: *quantity,
                        taker_fee: *taker_fee,
                        maker_fee: *maker_fee,
                        taker_side,
                    });
                    touched.insert((taker_sub, market.clone()));
                    touched.insert((maker_sub, market.clone()));
                    for id in [*taker_order_id, *maker_order_id] {
                        let terminal = self
                            .clob
                            .order(id)
                            .map(|o| o.status.is_terminal())
                            .unwrap_or(false);
                        if terminal {
                            self.close_bookkeeping(id);
                        }
                    }
                }
                ApplyOutput::Placed {
                    order_id, resting, ..
                } => {
                    if !resting {
                        self.close_bookkeeping(*order_id);
                    }
                }
                ApplyOutput::Cancelled { order_id, .. }
                | ApplyOutput::Rejected { order_id, .. }
                | ApplyOutput::Expired { order_id, .. } => {
                    self.close_bookkeeping(*order_id);
                }
                // Perps-level outputs never come from the CLOB.
                ApplyOutput::Transferred { .. }
                | ApplyOutput::Liquidated { .. }
                | ApplyOutput::FundingSettled { .. }
                | ApplyOutput::Adl { .. }
                | ApplyOutput::MarginFlagged { .. }
                | ApplyOutput::OraclePublished { .. } => {}
            }
        }
        self.revalidate_reduce_only(touched, ts_ms, out);
    }

    /// Cancel reduce-only orders whose combined remaining size exceeds the
    /// owner's position (deterministic: `BTreeSet` uuid order).
    fn revalidate_reduce_only(
        &mut self,
        touched: &BTreeSet<(SubaccountId, MarketId)>,
        ts_ms: u64,
        out: &mut Vec<ApplyOutput>,
    ) {
        for (sub, market) in touched {
            let Some(set) = self.reduce_only_open.get(&(*sub, market.clone())) else {
                continue;
            };
            if set.is_empty() {
                continue;
            }
            let ids: Vec<Uuid> = set.iter().copied().collect();
            let pos_abs = self.position(*sub, market).abs();
            let mut open_total = Decimal::ZERO;
            for id in &ids {
                if let Some(o) = self.clob.order(*id) {
                    if !o.status.is_terminal() {
                        open_total += o.remaining();
                    }
                }
            }
            if open_total <= pos_abs {
                continue;
            }
            let mut excess = open_total - pos_abs;
            for id in ids {
                if excess <= Decimal::ZERO {
                    break;
                }
                let Some(o) = self.clob.order(id) else {
                    continue;
                };
                if o.status.is_terminal() {
                    continue;
                }
                let remaining = o.remaining();
                if self
                    .clob
                    .force_cancel(id, CancelReason::ReduceOnly, ts_ms, out)
                {
                    self.close_bookkeeping(id);
                    excess -= remaining;
                }
            }
        }
    }

    // ---- handlers ---------------------------------------------------------

    fn reject_output(
        order_id: Uuid,
        market: &MarketId,
        reason: &'static str,
        ts_ms: u64,
    ) -> ApplyOutput {
        ApplyOutput::Rejected {
            order_id,
            market: market.clone(),
            reason,
            ts_ms,
        }
    }

    fn apply_transfer(
        &mut self,
        entry: &LogEntry,
        sub: SubaccountId,
        amount: Decimal,
        out: &mut Vec<ApplyOutput>,
    ) {
        // Boundary crossing: quantize once so `total_*` and the balance see
        // the identical (exact within `PRICE_SCALE`) amount.
        let amount = amount.round_dp(crate::margin::PRICE_SCALE);
        if sub == INSURANCE_SUBACCOUNT {
            out.push(Self::reject_output(
                Uuid::nil(),
                &entry.market,
                "reserved_subaccount",
                entry.ts_ms,
            ));
            return;
        }
        if amount == Decimal::ZERO {
            out.push(Self::reject_output(
                Uuid::nil(),
                &entry.market,
                "invalid_amount",
                entry.ts_ms,
            ));
            return;
        }
        if amount < Decimal::ZERO {
            let after = self.collateral(sub) + amount;
            if after < Decimal::ZERO {
                out.push(Self::reject_output(
                    Uuid::nil(),
                    &entry.market,
                    "insufficient_collateral",
                    entry.ts_ms,
                ));
                return;
            }
        }
        if amount > Decimal::ZERO {
            self.total_deposits += amount;
        } else {
            self.total_withdrawals += -amount;
        }
        let sa = self.subaccounts.entry(sub).or_default();
        sa.collateral += amount;
        let after = sa.collateral;
        self.stats.transfers += 1;
        out.push(ApplyOutput::Transferred {
            subaccount: sub,
            amount,
            collateral_after: after,
            ts_ms: entry.ts_ms,
        });
    }

    fn apply_funding(&mut self, entry: &LogEntry, rate: Decimal, out: &mut Vec<ApplyOutput>) {
        let market = &entry.market;
        // Stage 4: settling funding at a halted/stale reference price would
        // move collateral on numbers the oracle no longer vouches for.
        if let Some(reason) = self.oracle.gate(market, entry.ts_ms) {
            out.push(Self::reject_output(
                Uuid::nil(),
                market,
                reason,
                entry.ts_ms,
            ));
            return;
        }
        let Some(mark) = self.price_of(market) else {
            out.push(Self::reject_output(
                Uuid::nil(),
                market,
                "no_mark_price",
                entry.ts_ms,
            ));
            return;
        };
        let delta = (rate * mark).round_dp(crate::margin::PRICE_SCALE);
        let idx_new = self.funding_index(market) + delta;
        self.funding_index.insert(market.clone(), idx_new);

        // Collect holders first (owned data) so the mutation loop borrows
        // only `subaccounts`. All positions share the same previous index by
        // construction, so `Σ q · delta = delta · Σ q = 0`: zero-sum funding.
        let holders: Vec<(SubaccountId, Qty)> = self
            .subaccounts
            .iter()
            .filter_map(|(id, sa)| {
                sa.positions
                    .get(market)
                    .map(|p| (*id, p.base_qty))
                    .filter(|(_, q)| *q != Decimal::ZERO)
            })
            .collect();

        let mut payments = Vec::new();
        for (id, qty) in holders {
            let payment = qty * delta;
            if payment == Decimal::ZERO {
                continue;
            }
            let sa = self.subaccounts.get_mut(&id).expect("holder exists");
            sa.collateral -= payment;
            if let Some(pos) = sa.positions.get_mut(market) {
                pos.last_funding_index = idx_new;
            }
            payments.push((id, payment));
        }
        self.stats.funding_settlements += 1;
        out.push(ApplyOutput::FundingSettled {
            market: market.clone(),
            rate,
            payments,
            ts_ms: entry.ts_ms,
        });
    }

    /// Phase 1 of a liquidation: validate, build the synthetic marketable
    /// limit order at the bankruptcy price and match it against the book.
    /// The residual (if any) closes against the insurance ledger in
    /// [`Self::finish_liquidation`].
    fn start_liquidation(
        &mut self,
        entry: &LogEntry,
        sub: SubaccountId,
        max_qty: Option<Qty>,
        clob_out: &mut Vec<ApplyOutput>,
        out: &mut Vec<ApplyOutput>,
    ) -> Result<Option<LiquidationMeta>, ApplyError> {
        let market = &entry.market;

        let mut reject = |reason: &'static str| {
            out.push(Self::reject_output(
                Uuid::nil(),
                market,
                reason,
                entry.ts_ms,
            ));
        };

        if sub == INSURANCE_SUBACCOUNT {
            reject("reserved_subaccount");
            return Ok(None);
        }
        let position_qty = self.position(sub, market);
        if position_qty == Decimal::ZERO {
            reject("no_position");
            return Ok(None);
        }
        let close_abs = max_qty
            .map(|m| m.abs().min(position_qty.abs()))
            .unwrap_or(position_qty.abs());
        if close_abs <= Decimal::ZERO {
            reject("invalid_quantity");
            return Ok(None);
        }
        let Some(mark) = self.price_of(market) else {
            reject("no_mark_price");
            return Ok(None);
        };

        // Stage 4: liquidations need a trustworthy mark — refuse while the
        // market's oracle is halted or stale (bankruptcy math on a known-bad
        // price would liquidate at the wrong level).
        if let Some(reason) = self.oracle.gate(market, entry.ts_ms) {
            reject(reason);
            return Ok(None);
        }

        // Only unhealthy subaccounts may be liquidated.
        let equity = self.equity_of(sub);
        let maintenance = self.maintenance_margin_of(sub);
        if !below_maintenance(equity, maintenance) {
            reject("healthy_subaccount");
            return Ok(None);
        }

        // Per-market liquidation cascade breaker (logical time window).
        if self.cfg.max_liquidation_notional_per_window > Decimal::ZERO {
            let window = self.cfg.liquidation_window_ms;
            let now = entry.ts_ms;
            let samples = self.liquidation_windows.entry(market.clone()).or_default();
            samples.retain(|(ts, _)| now.saturating_sub(*ts) <= window);
            let total: Decimal = samples.iter().map(|(_, n)| *n).sum();
            if total >= self.cfg.max_liquidation_notional_per_window {
                reject("liquidation_cascade");
                return Ok(None);
            }
        }

        let close_qty = if position_qty > Decimal::ZERO {
            close_abs
        } else {
            -close_abs
        };

        // Deterministic synthetic order id derived from the entry sequence.
        let order_id = Uuid::from_u128(LIQUIDATION_UUID_PREFIX | entry.global_seq as u128);
        if self.clob.order(order_id).is_some() {
            reject("liquidation_id_collision");
            return Ok(None);
        }

        // Liquidation limit = bankruptcy price adjusted for fees.
        let taker_bps = self.clob.config().fee_taker_bps;
        let liq_bps = self.market_params(market).liquidation_fee_bps;
        let fee = fee_estimate(close_abs * mark, taker_bps + liq_bps);
        let limit_price = liquidation_limit_price(mark, equity, close_qty, fee);

        let side = if close_qty > Decimal::ZERO {
            Side::Ask
        } else {
            Side::Bid
        };
        let cmd = PlaceOrderCmd {
            order_id,
            client_order_id: format!("liq-{}", entry.global_seq),
            side,
            order_type: lq_types::OrderType::ImmediateOrCancel,
            price: Some(limit_price),
            quantity: close_abs,
            time_in_force: lq_types::TimeInForce::Ioc,
            owner: format!("sub:{sub}"),
            stp: lq_sequencer::entry::StpPolicy::None,
            expiration_ms: None,
            subaccount: Some(sub),
            reduce_only: false,
        };
        self.track_place(sub, order_id, market, false);

        let synthetic = LogEntry {
            global_seq: entry.global_seq,
            market_seq: entry.market_seq,
            market: entry.market.clone(),
            ts_ms: entry.ts_ms,
            payload: EntryPayload::PlaceOrder(cmd),
        };
        *clob_out = self.clob.apply(&synthetic)?;

        // Cascade window: record only after a successful execution path.
        if self.cfg.max_liquidation_notional_per_window > Decimal::ZERO {
            self.liquidation_windows
                .entry(market.clone())
                .or_default()
                .push((entry.ts_ms, close_abs * mark));
        }

        Ok(Some(LiquidationMeta {
            subaccount: sub,
            market: market.clone(),
            close_qty,
            limit_price,
            order_id,
        }))
    }

    /// Phase 2: close whatever the book did not fill against the insurance
    /// ledger at exactly the limit price, then emit the liquidation record.
    fn finish_liquidation(
        &mut self,
        meta: &LiquidationMeta,
        ts_ms: u64,
        out: &mut Vec<ApplyOutput>,
    ) {
        let total = meta.close_qty.abs();
        let book_filled = self
            .clob
            .order(meta.order_id)
            .map(|o| o.filled_quantity)
            .unwrap_or(Decimal::ZERO)
            .min(total);
        let residual = total - book_filled;
        let funding_idx = self.funding_index(&meta.market);

        if residual > Decimal::ZERO {
            let sign = if meta.close_qty > Decimal::ZERO {
                Decimal::ONE
            } else {
                -Decimal::ONE
            };
            let cash = meta.limit_price * residual;

            // Liquidated side closes `residual` at the limit price.
            let sa = self.subaccounts.entry(meta.subaccount).or_default();
            if sign > Decimal::ZERO {
                sa.collateral += cash;
            } else {
                sa.collateral -= cash;
            }
            sa.apply_fill(
                &meta.market,
                -sign * residual,
                meta.limit_price,
                funding_idx,
            );

            // Insurance takes the opposite side (it profits when marked:
            // it bought below / sold above the reference price).
            let ins = self.subaccounts.entry(INSURANCE_SUBACCOUNT).or_default();
            if sign > Decimal::ZERO {
                ins.collateral -= cash;
            } else {
                ins.collateral += cash;
            }
            ins.apply_fill(&meta.market, sign * residual, meta.limit_price, funding_idx);
        }

        self.stats.liquidations += 1;
        out.push(ApplyOutput::Liquidated {
            subaccount: meta.subaccount,
            market: meta.market.clone(),
            quantity: total,
            book_filled,
            insurance_filled: residual,
            limit_price: meta.limit_price,
            ts_ms,
        });

        // Position shrank: reduce-only orders may now be oversized.
        let mut touched = BTreeSet::new();
        touched.insert((meta.subaccount, meta.market.clone()));
        self.revalidate_reduce_only(&touched, ts_ms, out);
    }

    // ---- end-of-apply pipeline --------------------------------------------

    /// Flat subaccounts with negative cash (funding/fee dust) hand the debt to
    /// the insurance ledger so no subaccount is stranded insolvent-and-flat.
    fn sweep_flat_negatives(&mut self) {
        let mut sweeps: Vec<(SubaccountId, Decimal)> = Vec::new();
        for (&id, sa) in &self.subaccounts {
            if id == INSURANCE_SUBACCOUNT {
                continue;
            }
            if sa.positions.is_empty() && sa.collateral < Decimal::ZERO {
                sweeps.push((id, sa.collateral));
            }
        }
        for (id, balance) in sweeps {
            self.subaccounts
                .get_mut(&id)
                .expect("id from scan")
                .collateral = Decimal::ZERO;
            self.subaccounts
                .entry(INSURANCE_SUBACCOUNT)
                .or_default()
                .collateral += balance;
        }
    }

    /// Auto-deleveraging: when the insurance ledger's equity is negative,
    /// close its positions against the largest opposite positions (deterministic
    /// order: `|qty|` desc, then subaccount id asc) at the price that restores
    /// insurance equity to zero. Each iteration removes one position entry, so
    /// the loop terminates.
    fn run_adl(&mut self, ts_ms: u64, out: &mut Vec<ApplyOutput>) {
        loop {
            let ins_equity = self.equity_of(INSURANCE_SUBACCOUNT);
            if ins_equity >= Decimal::ZERO {
                break;
            }
            let Some((market, ins_qty, mark)) = self.largest_insurance_position() else {
                break; // Flat insurance debt cannot be ADL'd (documented).
            };
            let gain_needed = -ins_equity;

            // Opposite-side holders in this market, largest first.
            let mut candidates: Vec<(SubaccountId, Qty)> = self
                .subaccounts
                .iter()
                .filter(|(id, sa)| {
                    let q = sa.base_qty(&market);
                    **id != INSURANCE_SUBACCOUNT
                        && q != Decimal::ZERO
                        && (q > Decimal::ZERO) != (ins_qty > Decimal::ZERO)
                })
                .map(|(id, sa)| (*id, sa.base_qty(&market)))
                .collect();
            if candidates.is_empty() {
                break;
            }
            candidates.sort_by(|a, b| b.1.abs().cmp(&a.1.abs()).then(a.0.cmp(&b.0)));

            let (counterparty, counterparty_qty) = candidates[0];
            let close = ins_qty.abs().min(counterparty_qty.abs());
            if close == Decimal::ZERO {
                break;
            }
            // Insurance closes `close` of its position at `price`:
            // equity change = close · (price − mark) for a long (negated for
            // a short) ⇒ price = mark ± gain/close. Quantized to
            // `PRICE_SCALE` so the cash legs stay exactly symmetric.
            let price = if ins_qty > Decimal::ZERO {
                (mark + gain_needed / close).round_dp(crate::margin::PRICE_SCALE)
            } else {
                let p = (mark - gain_needed / close).round_dp(crate::margin::PRICE_SCALE);
                if p <= crate::margin::MIN_PRICE {
                    crate::margin::MIN_PRICE
                } else {
                    p
                }
            };

            let funding_idx = self.funding_index(&market);
            let ins_signed = if ins_qty > Decimal::ZERO {
                -close
            } else {
                close
            };
            let cp_signed = -ins_signed;

            {
                let ins = self.subaccounts.entry(INSURANCE_SUBACCOUNT).or_default();
                if ins_signed < Decimal::ZERO {
                    // Insurance sold `close` (long): receives cash.
                    ins.collateral += price * close;
                } else {
                    ins.collateral -= price * close;
                }
                ins.apply_fill(&market, ins_signed, price, funding_idx);
            }
            {
                let cp = self.subaccounts.entry(counterparty).or_default();
                if cp_signed < Decimal::ZERO {
                    cp.collateral += price * close;
                } else {
                    cp.collateral -= price * close;
                }
                cp.apply_fill(&market, cp_signed, price, funding_idx);
            }

            self.stats.adl_events += 1;
            out.push(ApplyOutput::Adl {
                subaccount: counterparty,
                market: market.clone(),
                quantity: close,
                price,
                ts_ms,
            });
        }
    }

    fn largest_insurance_position(&self) -> Option<(MarketId, Qty, Price)> {
        let ins = self.subaccounts.get(&INSURANCE_SUBACCOUNT)?;
        let mut best: Option<(MarketId, Qty, Price)> = None;
        for (market, pos) in &ins.positions {
            if pos.base_qty == Decimal::ZERO {
                continue;
            }
            let Some(px) = self.price_of(market) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((_, q, _)) => pos.base_qty.abs() > q.abs(),
            };
            if better {
                best = Some((market.clone(), pos.base_qty, px));
            }
        }
        best
    }

    /// Rebuild the pending-liquidation set (deterministic scan) and emit
    /// `MarginFlagged` outputs for subaccounts that are newly queued.
    fn rebuild_flags(&mut self, ts_ms: u64, out: &mut Vec<ApplyOutput>) {
        let previous = std::mem::take(&mut self.pending_liquidations);
        let mut fresh = BTreeSet::new();
        for (&id, sa) in &self.subaccounts {
            if id == INSURANCE_SUBACCOUNT || !sa.has_position() {
                continue;
            }
            if below_maintenance(self.equity_of(id), self.maintenance_margin_of(id)) {
                fresh.insert(id);
            }
        }
        for &id in &fresh {
            if !previous.contains(&id) {
                self.stats.flagged += 1;
                out.push(ApplyOutput::MarginFlagged {
                    subaccount: id,
                    ts_ms,
                });
            }
        }
        self.pending_liquidations = fresh;
    }
}

impl Default for PerpsState {
    fn default() -> Self {
        Self::new()
    }
}

impl StateMachine for PerpsState {
    fn apply(&mut self, entry: &LogEntry) -> Result<Vec<ApplyOutput>, ApplyError> {
        self.expect_global(entry)?;
        self.expect_market(entry)?;

        // Outputs produced before the CLOB leg runs (pre-trade rejections,
        // transfers, funding, validation failures).
        let mut out: Vec<ApplyOutput> = Vec::new();
        let clob_out: Vec<ApplyOutput>;
        let mut liquidation: Option<LiquidationMeta> = None;

        match &entry.payload {
            EntryPayload::PlaceOrder(cmd) => {
                let sub = cmd.subaccount.unwrap_or(DEFAULT_SUBACCOUNT);
                // Stage 4: deviation/staleness breaker refuses new risk
                // before the margin check runs; the entry is still consumed.
                let placed: Option<PlaceOrderCmd> =
                    match self.oracle.gate(&entry.market, entry.ts_ms) {
                        Some(reason) => {
                            self.stats.oracle_rejected += 1;
                            out.push(Self::reject_output(
                                cmd.order_id,
                                &entry.market,
                                reason,
                                entry.ts_ms,
                            ));
                            None
                        }
                        None => match self.pre_trade_check(&entry.market, cmd, None) {
                            PreTradeVerdict::Allow { .. } => Some(cmd.clone()),
                            PreTradeVerdict::Reduce { qty, .. } => {
                                // `lq-risk` parity: oversized orders rest at the cap
                                // instead of being rejected.
                                let mut smaller = cmd.clone();
                                smaller.quantity = qty;
                                Some(smaller)
                            }
                            verdict => {
                                self.stats.margin_rejected += 1;
                                out.push(Self::reject_output(
                                    cmd.order_id,
                                    &entry.market,
                                    verdict.as_str(),
                                    entry.ts_ms,
                                ));
                                None
                            }
                        },
                    };
                if let Some(placed) = placed {
                    let normalized = Self::normalize_owner(&placed, sub);
                    let is_new = self.clob.order(placed.order_id).is_none();
                    if is_new {
                        self.track_place(sub, placed.order_id, &entry.market, placed.reduce_only);
                    }
                    let deleg = LogEntry {
                        global_seq: entry.global_seq,
                        market_seq: entry.market_seq,
                        market: entry.market.clone(),
                        ts_ms: entry.ts_ms,
                        payload: EntryPayload::PlaceOrder(normalized),
                    };
                    clob_out = self.clob.apply(&deleg)?;
                    if is_new && self.clob.order(placed.order_id).is_none() {
                        // CLOB refused the resting order (STP self-match …):
                        // drop the bookkeeping we optimistically added.
                        self.untrack(sub, &entry.market, placed.order_id);
                    }
                } else {
                    clob_out = self.clob.apply_noop(entry)?;
                }
            }
            EntryPayload::ReplaceOrder { old_order_id, new } => {
                let sub = new.subaccount.unwrap_or(DEFAULT_SUBACCOUNT);
                let placed: Option<PlaceOrderCmd> =
                    match self.oracle.gate(&entry.market, entry.ts_ms) {
                        Some(reason) => {
                            self.stats.oracle_rejected += 1;
                            out.push(Self::reject_output(
                                new.order_id,
                                &entry.market,
                                reason,
                                entry.ts_ms,
                            ));
                            None
                        }
                        None => match self.pre_trade_check(&entry.market, new, Some(*old_order_id))
                        {
                            PreTradeVerdict::Allow { .. } => Some(new.as_ref().clone()),
                            PreTradeVerdict::Reduce { qty, .. } => {
                                let mut smaller = new.as_ref().clone();
                                smaller.quantity = qty;
                                Some(smaller)
                            }
                            verdict => {
                                self.stats.margin_rejected += 1;
                                out.push(Self::reject_output(
                                    new.order_id,
                                    &entry.market,
                                    verdict.as_str(),
                                    entry.ts_ms,
                                ));
                                None
                            }
                        },
                    };
                if let Some(placed) = placed {
                    let normalized = Self::normalize_owner(&placed, sub);
                    let is_new = self.clob.order(placed.order_id).is_none();
                    if is_new {
                        self.track_place(sub, placed.order_id, &entry.market, placed.reduce_only);
                    }
                    let deleg = LogEntry {
                        global_seq: entry.global_seq,
                        market_seq: entry.market_seq,
                        market: entry.market.clone(),
                        ts_ms: entry.ts_ms,
                        payload: EntryPayload::ReplaceOrder {
                            old_order_id: *old_order_id,
                            new: Box::new(normalized),
                        },
                    };
                    clob_out = self.clob.apply(&deleg)?;
                    if is_new && self.clob.order(placed.order_id).is_none() {
                        self.untrack(sub, &entry.market, placed.order_id);
                    }
                } else {
                    clob_out = self.clob.apply_noop(entry)?;
                }
            }
            EntryPayload::CancelOrder { .. } | EntryPayload::MarketTick(_) => {
                if let EntryPayload::MarketTick(tick) = &entry.payload {
                    self.tick_marks.insert(entry.market.clone(), tick.last);
                }
                clob_out = self.clob.apply(entry)?;
            }
            EntryPayload::Fill(fill) => {
                // Legacy Stage-1 entry: advance the book/order, track the last
                // trade — subaccount positions are **not** moved (the
                // counterparty of an external fill is unknown, and inventing
                // one would break `Σ positions = 0`). Stage-3 logs use
                // CLOB-generated fills exclusively.
                self.last_trade.insert(entry.market.clone(), fill.price);
                clob_out = self.clob.apply(entry)?;
                let terminal = self
                    .clob
                    .order(fill.order_id)
                    .map(|o| o.status.is_terminal())
                    .unwrap_or(false);
                if terminal {
                    self.close_bookkeeping(fill.order_id);
                }
            }
            EntryPayload::Transfer { subaccount, amount } => {
                clob_out = self.clob.apply_noop(entry)?;
                self.apply_transfer(entry, *subaccount, *amount, &mut out);
            }
            EntryPayload::Liquidate {
                subaccount,
                max_qty,
            } => {
                let mut sub_clob_out = Vec::new();
                liquidation = self.start_liquidation(
                    entry,
                    *subaccount,
                    *max_qty,
                    &mut sub_clob_out,
                    &mut out,
                )?;
                if liquidation.is_none() {
                    // Validation failed: still consume the entry's sequence.
                    sub_clob_out = self.clob.apply(entry)?;
                }
                clob_out = sub_clob_out;
            }
            EntryPayload::SettleFunding { rate } => {
                clob_out = self.clob.apply_noop(entry)?;
                self.apply_funding(entry, *rate, &mut out);
            }
            EntryPayload::OraclePrice(cmd) => {
                // Stage 4: the oracle layer validates quorum, observation
                // freshness and the deviation band, then (on acceptance)
                // publishes the price. The CLOB consumes the sequence; the
                // common tail below re-runs the margin pipeline because a
                // price change moves equity (ADL, flag rebuild).
                match self.oracle.apply_price(&entry.market, cmd, entry.ts_ms) {
                    OracleOutcome::Accepted => {
                        out.push(ApplyOutput::OraclePublished {
                            market: entry.market.clone(),
                            price: cmd.price,
                            sources: cmd.sources,
                            ts_ms: entry.ts_ms,
                        });
                    }
                    OracleOutcome::Rejected { reason } => {
                        self.stats.oracle_rejected += 1;
                        out.push(Self::reject_output(
                            Uuid::nil(),
                            &entry.market,
                            reason,
                            entry.ts_ms,
                        ));
                    }
                }
                clob_out = self.clob.apply(entry)?;
            }
        }

        // CLOB outputs → subaccount cash/positions (atomic with the match).
        out.extend_from_slice(&clob_out);
        let mut touched: BTreeSet<(SubaccountId, MarketId)> = BTreeSet::new();
        self.process_clob_outputs(&clob_out, &mut touched, &mut out, entry.ts_ms);

        // Liquidation residual + record (after book fills have settled).
        if let Some(meta) = &liquidation {
            self.finish_liquidation(meta, entry.ts_ms, &mut out);
        }

        // End-of-entry pipeline: insurance recap, ADL, margin flagging.
        self.sweep_flat_negatives();
        let mut adl_touched: BTreeSet<(SubaccountId, MarketId)> = BTreeSet::new();
        let adl_before = out.len();
        self.run_adl(entry.ts_ms, &mut out);
        for output in &out[adl_before..] {
            if let ApplyOutput::Adl {
                subaccount, market, ..
            } = output
            {
                adl_touched.insert((*subaccount, market.clone()));
            }
        }
        if !adl_touched.is_empty() {
            self.revalidate_reduce_only(&adl_touched, entry.ts_ms, &mut out);
        }
        self.rebuild_flags(entry.ts_ms, &mut out);

        self.last_global_seq = entry.global_seq;
        self.market_seqs
            .insert(entry.market.clone(), entry.market_seq);
        Ok(out)
    }

    fn state_hash(&self) -> StateHash {
        let mut h = new_hasher();
        write_str(&mut h, "lq-perps-v1");
        write_decimal(&mut h, &self.cfg.default_market_params.initial_margin_ratio);
        write_decimal(
            &mut h,
            &self.cfg.default_market_params.maintenance_margin_ratio,
        );
        write_decimal(&mut h, &self.cfg.default_market_params.liquidation_fee_bps);
        write_decimal(&mut h, &self.cfg.max_order_qty);
        write_decimal(&mut h, &self.cfg.max_position_qty);
        write_decimal(&mut h, &self.cfg.max_notional_per_order);
        write_u64(&mut h, self.cfg.max_open_orders as u64);
        write_u64(&mut h, self.cfg.liquidation_window_ms);
        write_decimal(&mut h, &self.cfg.max_liquidation_notional_per_window);

        // The embedded CLOB contributes its entire canonical hash.
        let clob_hash = self.clob.state_hash();
        write_bytes(&mut h, &clob_hash.0);

        // The oracle book contributes its own canonical hash (params,
        // accepted prices, breaker latches, counters).
        self.oracle.write_hash(&mut h);

        write_u64(&mut h, self.last_global_seq);
        write_u64(&mut h, self.market_seqs.len() as u64);
        for (m, seq) in &self.market_seqs {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, *seq);
        }

        write_u64(&mut h, self.subaccounts.len() as u64);
        for (id, sa) in &self.subaccounts {
            write_u64(&mut h, *id);
            write_decimal(&mut h, &sa.collateral);
            write_decimal(&mut h, &sa.realized_pnl);
            write_u64(&mut h, sa.positions.len() as u64);
            for (m, pos) in &sa.positions {
                write_str(&mut h, m.venue.as_str());
                write_str(&mut h, m.symbol.as_str());
                write_decimal(&mut h, &pos.base_qty);
                write_decimal(&mut h, &pos.avg_entry);
                write_decimal(&mut h, &pos.last_funding_index);
            }
        }

        write_u64(&mut h, self.market_params.len() as u64);
        for (m, p) in &self.market_params {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, &p.initial_margin_ratio);
            write_decimal(&mut h, &p.maintenance_margin_ratio);
            write_decimal(&mut h, &p.liquidation_fee_bps);
        }

        write_u64(&mut h, self.funding_index.len() as u64);
        for (m, idx) in &self.funding_index {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, idx);
        }

        write_u64(&mut h, self.order_owner.len() as u64);
        for (id, sub) in &self.order_owner {
            write_str(&mut h, &id.to_string());
            write_u64(&mut h, *sub);
        }
        write_u64(&mut h, self.open_orders.len() as u64);
        for (sub, ids) in &self.open_orders {
            write_u64(&mut h, *sub);
            write_u64(&mut h, ids.len() as u64);
            for id in ids {
                write_str(&mut h, &id.to_string());
            }
        }
        write_u64(&mut h, self.reduce_only_open.len() as u64);
        for ((sub, m), ids) in &self.reduce_only_open {
            write_u64(&mut h, *sub);
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, ids.len() as u64);
            for id in ids {
                write_str(&mut h, &id.to_string());
            }
        }
        write_u64(&mut h, self.pending_liquidations.len() as u64);
        for id in &self.pending_liquidations {
            write_u64(&mut h, *id);
        }

        write_decimal(&mut h, &self.total_deposits);
        write_decimal(&mut h, &self.total_withdrawals);

        write_u64(&mut h, self.tick_marks.len() as u64);
        for (m, px) in &self.tick_marks {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, px);
        }
        write_u64(&mut h, self.last_trade.len() as u64);
        for (m, px) in &self.last_trade {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, px);
        }
        write_u64(&mut h, self.liquidation_windows.len() as u64);
        for (m, samples) in &self.liquidation_windows {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, samples.len() as u64);
            for (ts, notional) in samples {
                write_u64(&mut h, *ts);
                write_decimal(&mut h, notional);
            }
        }

        write_u64(&mut h, self.stats.transfers);
        write_u64(&mut h, self.stats.liquidations);
        write_u64(&mut h, self.stats.adl_events);
        write_u64(&mut h, self.stats.funding_settlements);
        write_u64(&mut h, self.stats.margin_rejected);
        write_u64(&mut h, self.stats.flagged);
        write_u64(&mut h, self.stats.oracle_rejected);
        finish(h)
    }

    fn last_global_seq(&self) -> u64 {
        self.last_global_seq
    }

    fn market_seq(&self, market: &MarketId) -> u64 {
        self.market_seq_of(market)
    }

    fn market_seqs(&self) -> Vec<(MarketId, u64)> {
        self.market_seqs
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    fn encode_state(&self) -> Result<Vec<u8>, String> {
        let wire = PerpsWire {
            cfg: self.cfg,
            clob_state: self.clob.encode_state()?,
            last_global_seq: self.last_global_seq,
            market_seqs: self
                .market_seqs
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            subaccounts: self
                .subaccounts
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            market_params: self
                .market_params
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            funding_index: self
                .funding_index
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            order_owner: self.order_owner.iter().map(|(k, v)| (*k, *v)).collect(),
            open_orders: self
                .open_orders
                .iter()
                .map(|(k, v)| (*k, v.iter().copied().collect()))
                .collect(),
            reduce_only_open: self
                .reduce_only_open
                .iter()
                .map(|((sub, m), v)| (*sub, m.clone(), v.iter().copied().collect()))
                .collect(),
            pending_liquidations: self.pending_liquidations.iter().copied().collect(),
            total_deposits: self.total_deposits,
            total_withdrawals: self.total_withdrawals,
            tick_marks: self
                .tick_marks
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_trade: self
                .last_trade
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            liquidation_windows: self
                .liquidation_windows
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            oracle: self.oracle.clone(),
            stats: self.stats,
        };
        serde_json::to_vec(&wire).map_err(|e| e.to_string())
    }

    fn decode_state(bytes: &[u8]) -> Result<Self, String> {
        let wire: PerpsWire = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let clob = ClobState::decode_state(&wire.clob_state)?;
        let mut state = Self {
            cfg: wire.cfg,
            clob,
            last_global_seq: wire.last_global_seq,
            market_seqs: wire.market_seqs.into_iter().collect(),
            subaccounts: wire.subaccounts.into_iter().collect(),
            market_params: wire.market_params.into_iter().collect(),
            funding_index: wire.funding_index.into_iter().collect(),
            order_owner: wire.order_owner.into_iter().collect(),
            open_orders: wire
                .open_orders
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
            reduce_only_open: wire
                .reduce_only_open
                .into_iter()
                .map(|(s, m, v)| ((s, m), v.into_iter().collect()))
                .collect(),
            pending_liquidations: wire.pending_liquidations.into_iter().collect(),
            total_deposits: wire.total_deposits,
            total_withdrawals: wire.total_withdrawals,
            tick_marks: wire.tick_marks.into_iter().collect(),
            last_trade: wire.last_trade.into_iter().collect(),
            liquidation_windows: wire.liquidation_windows.into_iter().collect(),
            oracle: wire.oracle,
            stats: wire.stats,
        };
        // Rebuild the pending set defensively (it is derivable state).
        state.pending_liquidations = state.compute_flagged().into_iter().collect();
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::margin::{fee_estimate, liquidation_limit_price, requirement};
    use rust_decimal_macros::dec;

    #[test]
    fn requirement_is_abs_qty_times_price_times_ratio() {
        assert_eq!(requirement(dec!(2), dec!(100), dec!(0.10)), dec!(20));
        assert_eq!(requirement(dec!(-2), dec!(100), dec!(0.05)), dec!(10));
    }

    #[test]
    fn fee_estimate_is_bps_on_notional() {
        assert_eq!(fee_estimate(dec!(1000), dec!(5)), dec!(0.5));
    }

    #[test]
    fn liquidation_limit_long_is_below_mark() {
        // Long 1 @ mark 100, equity 5 ⇒ bankruptcy floor at 95.
        let p = liquidation_limit_price(dec!(100), dec!(5), dec!(1), Decimal::ZERO);
        assert_eq!(p, dec!(95));
        // With a fee the floor rises (proceeds must cover the fee).
        let p = liquidation_limit_price(dec!(100), dec!(5), dec!(1), dec!(1));
        assert_eq!(p, dec!(96));
    }

    #[test]
    fn liquidation_limit_short_is_above_mark() {
        // Short -1 @ mark 100, equity 5 ⇒ cover ceiling at 105.
        let p = liquidation_limit_price(dec!(100), dec!(5), dec!(-1), Decimal::ZERO);
        assert_eq!(p, dec!(105));
    }

    #[test]
    fn liquidation_limit_clamped_to_min_price() {
        let p = liquidation_limit_price(dec!(100), dec!(1000), dec!(1), Decimal::ZERO);
        assert_eq!(p, crate::margin::MIN_PRICE);
    }

    #[test]
    fn position_avg_and_realized_math() {
        let mut sa = Subaccount::default();
        let m = MarketId::new(
            lq_types::Exchange::Paper,
            lq_types::Symbol("BTC-USD".to_string()),
        );
        // Open long 2 @ 100.
        sa.apply_fill(&m, dec!(2), dec!(100), Decimal::ZERO);
        assert_eq!(sa.base_qty(&m), dec!(2));
        // Add 2 @ 110 ⇒ avg 105.
        sa.apply_fill(&m, dec!(2), dec!(110), Decimal::ZERO);
        assert_eq!(sa.positions[&m].avg_entry, dec!(105));
        // Close 1 @ 120 ⇒ realized (120−105)·1 = 15.
        sa.apply_fill(&m, dec!(-1), dec!(120), Decimal::ZERO);
        assert_eq!(sa.realized_pnl, dec!(15));
        assert_eq!(sa.base_qty(&m), dec!(3));
        // Flat: entry removed.
        sa.apply_fill(&m, dec!(-3), dec!(100), Decimal::ZERO);
        assert!(sa.positions.is_empty());
    }
}
