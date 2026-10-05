//! The CLOB state machine: implements [`StateMachine`] over the sequencer log.
//!
//! Determinism contract:
//! - only `BTreeMap` iteration, never `HashMap`
//! - time comes exclusively from `entry.ts_ms`
//! - no RNG anywhere; matching is a pure function of (state, entry)
//! - `apply` returns `Err` **only** for sequence gaps (structural). Every
//!   command-level problem (duplicate id, invalid qty, PostOnly cross, FOK
//!   unfilled, unknown cancel, self-trade) is an `Ok` path that records a
//!   rejection output — so the WAL always replays cleanly through `Sequencer::open`.

use std::collections::BTreeMap;

use lq_sequencer::entry::{
    EntryPayload, FillCmd, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd,
};
use lq_sequencer::hash::{finish, new_hasher, write_decimal, write_str, write_u64, StateHash};
use lq_sequencer::state::{ApplyError, ApplyOutput, CancelReason, StateMachine};
use lq_types::{OrderStatus, OrderType, Price, Qty, Side, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::book::Book;
use crate::matching::{self, exec_policy, would_cross, MatchOutcome, Policy};
use crate::order::ClobOrder;

/// Fee configuration held in state (part of the hash — changing fees changes
/// future fills, so replicas must agree). Decimal basis points, never `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClobConfig {
    pub fee_taker_bps: Decimal,
    pub fee_maker_bps: Decimal,
}

impl Default for ClobConfig {
    fn default() -> Self {
        Self {
            fee_taker_bps: Decimal::from(5),
            fee_maker_bps: Decimal::ZERO,
        }
    }
}

/// Monotonic observability counters (part of the state hash).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClobStats {
    pub placed: u64,
    pub cancelled: u64,
    pub filled: u64,
    pub rejected: u64,
    pub expired: u64,
}

/// Event-sourced CLOB: order-level books per market, matching inside `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClobState {
    cfg: ClobConfig,
    last_global_seq: u64,
    market_seqs: BTreeMap<MarketId, u64>,
    orders: BTreeMap<Uuid, ClobOrder>,
    books: BTreeMap<MarketId, Book>,
    /// Net base quantity per market (positive = long), sum over all fills.
    net_positions: BTreeMap<MarketId, Qty>,
    /// Cumulative fees charged (taker + maker) per market; rebates negative.
    fees_paid: BTreeMap<MarketId, Decimal>,
    last_marks: BTreeMap<MarketId, Price>,
    stats: ClobStats,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClobWire {
    cfg: ClobConfig,
    last_global_seq: u64,
    market_seqs: Vec<(MarketId, u64)>,
    orders: Vec<ClobOrder>,
    books: Vec<(MarketId, Book)>,
    net_positions: Vec<(MarketId, Qty)>,
    fees_paid: Vec<(MarketId, Decimal)>,
    last_marks: Vec<(MarketId, Price)>,
    stats: ClobStats,
}

impl ClobState {
    pub fn new() -> Self {
        Self::with_config(ClobConfig::default())
    }

    pub fn with_config(cfg: ClobConfig) -> Self {
        Self {
            cfg,
            last_global_seq: 0,
            market_seqs: BTreeMap::new(),
            orders: BTreeMap::new(),
            books: BTreeMap::new(),
            net_positions: BTreeMap::new(),
            fees_paid: BTreeMap::new(),
            last_marks: BTreeMap::new(),
            stats: ClobStats::default(),
        }
    }

    pub fn config(&self) -> ClobConfig {
        self.cfg
    }

    pub fn order(&self, id: Uuid) -> Option<&ClobOrder> {
        self.orders.get(&id)
    }

    pub fn book(&self, market: &MarketId) -> Option<&Book> {
        self.books.get(market)
    }

    pub fn net_position(&self, market: &MarketId) -> Qty {
        self.net_positions
            .get(market)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub fn fees_paid(&self, market: &MarketId) -> Decimal {
        self.fees_paid.get(market).copied().unwrap_or(Decimal::ZERO)
    }

    pub fn last_mark(&self, market: &MarketId) -> Option<Price> {
        self.last_marks.get(market).copied()
    }

    pub fn stats(&self) -> ClobStats {
        self.stats
    }

    pub fn open_order_count(&self) -> usize {
        self.orders
            .values()
            .filter(|o| !o.status.is_terminal())
            .count()
    }

    pub fn resting_count(&self, market: &MarketId) -> usize {
        self.books
            .get(market)
            .map(|b| b.depth(Side::Bid) + b.depth(Side::Ask))
            .unwrap_or(0)
    }

    // ---- apply helpers -------------------------------------------------

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
        let expected = self.market_seq(&entry.market) + 1;
        if entry.market_seq != expected {
            return Err(ApplyError::MarketSeqGap {
                market: entry.market.to_string(),
                expected,
                got: entry.market_seq,
            });
        }
        Ok(())
    }

    /// Expire every non-terminal short-term order whose logical deadline has
    /// passed. Deterministic (orders iterated in `BTreeMap`/uuid order).
    fn sweep_expired(&mut self, ts_ms: u64, out: &mut Vec<ApplyOutput>) {
        let expired: Vec<Uuid> = self
            .orders
            .values()
            .filter(|o| {
                !o.status.is_terminal() && o.expiration_ms.map(|exp| ts_ms >= exp).unwrap_or(false)
            })
            .map(|o| o.order_id)
            .collect();
        for id in expired {
            let market = self.orders[&id].market.clone();
            if let Some(price) = self.orders[&id].book_price {
                let side = self.orders[&id].side;
                if let Some(book) = self.books.get_mut(&market) {
                    book.remove(side, price, id);
                }
            }
            let o = self.orders.get_mut(&id).expect("id from scan");
            o.status = OrderStatus::Expired;
            o.book_price = None;
            o.updated_ts_ms = ts_ms;
            self.stats.expired += 1;
            out.push(ApplyOutput::Expired {
                order_id: id,
                market,
                ts_ms,
            });
        }
    }

    fn apply_place(&mut self, entry: &LogEntry, cmd: &PlaceOrderCmd, out: &mut Vec<ApplyOutput>) {
        // Fundamental validation: Ok path, nothing recorded, nothing mutated.
        if cmd.quantity <= Decimal::ZERO {
            self.stats.rejected += 1;
            out.push(ApplyOutput::Rejected {
                order_id: cmd.order_id,
                market: entry.market.clone(),
                reason: "invalid_quantity",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        let is_market = matches!(cmd.order_type, OrderType::Market);
        if !is_market && cmd.price.map(|p| p <= Decimal::ZERO).unwrap_or(true) {
            self.stats.rejected += 1;
            out.push(ApplyOutput::Rejected {
                order_id: cmd.order_id,
                market: entry.market.clone(),
                reason: "invalid_price",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        if self.orders.contains_key(&cmd.order_id) {
            self.stats.rejected += 1;
            out.push(ApplyOutput::Rejected {
                order_id: cmd.order_id,
                market: entry.market.clone(),
                reason: "duplicate_order",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        if cmd
            .expiration_ms
            .map(|exp| entry.ts_ms >= exp)
            .unwrap_or(false)
        {
            self.stats.rejected += 1;
            out.push(ApplyOutput::Rejected {
                order_id: cmd.order_id,
                market: entry.market.clone(),
                reason: "already_expired",
                ts_ms: entry.ts_ms,
            });
            return;
        }

        let market = entry.market.clone();
        self.books.entry(market.clone()).or_default();

        let order = ClobOrder {
            order_id: cmd.order_id,
            client_order_id: cmd.client_order_id.clone(),
            market: market.clone(),
            side: cmd.side,
            order_type: cmd.order_type,
            price: cmd.price,
            quantity: cmd.quantity,
            filled_quantity: Decimal::ZERO,
            status: OrderStatus::Acknowledged,
            time_in_force: cmd.time_in_force,
            owner: cmd.owner.clone(),
            stp: cmd.stp,
            expiration_ms: cmd.expiration_ms,
            place_global_seq: entry.global_seq,
            created_ts_ms: entry.ts_ms,
            updated_ts_ms: entry.ts_ms,
            book_price: None,
        };
        self.orders.insert(cmd.order_id, order);

        let limit = if is_market { None } else { cmd.price };
        let policy = exec_policy(cmd.order_type, cmd.time_in_force);

        match policy {
            Policy::PostOnly => {
                let px = cmd.price.expect("validated above");
                if would_cross(&self.books[&market], cmd.side, px) {
                    self.reject_in_place(
                        cmd.order_id,
                        &market,
                        "post_only_cross",
                        entry.ts_ms,
                        out,
                    );
                    return;
                }
                self.rest_order(cmd.order_id, entry.ts_ms);
                self.stats.placed += 1;
                out.push(ApplyOutput::Placed {
                    order_id: cmd.order_id,
                    market,
                    resting: true,
                    ts_ms: entry.ts_ms,
                });
            }
            Policy::Fok => {
                let (sim_remaining, sim_abort) = matching::simulate(
                    &self.books[&market],
                    &self.orders,
                    cmd.side,
                    limit,
                    &cmd.owner,
                    cmd.stp,
                    cmd.quantity,
                );
                if sim_remaining > Decimal::ZERO || sim_abort {
                    self.reject_in_place(
                        cmd.order_id,
                        &market,
                        if sim_abort {
                            "self_trade"
                        } else {
                            "fook_unfilled"
                        },
                        entry.ts_ms,
                        out,
                    );
                    return;
                }
                let outcome = self.run_match(cmd.order_id, cmd.side, limit, entry.ts_ms);
                debug_assert_eq!(outcome.remaining, Decimal::ZERO);
                debug_assert!(outcome.abort.is_none());
                self.settle_taker(cmd.order_id, &outcome, entry.ts_ms, policy, out);
            }
            Policy::Ioc | Policy::Gtc => {
                let outcome = self.run_match(cmd.order_id, cmd.side, limit, entry.ts_ms);
                self.settle_taker(cmd.order_id, &outcome, entry.ts_ms, policy, out);
            }
        }
    }

    fn apply_cancel(
        &mut self,
        entry: &LogEntry,
        order_id: Uuid,
        reason: CancelReason,
        out: &mut Vec<ApplyOutput>,
    ) {
        let Some(order) = self.orders.get(&order_id) else {
            out.push(ApplyOutput::Rejected {
                order_id,
                market: entry.market.clone(),
                reason: "unknown_order",
                ts_ms: entry.ts_ms,
            });
            return;
        };
        if order.status.is_terminal() {
            out.push(ApplyOutput::Rejected {
                order_id,
                market: order.market.clone(),
                reason: "already_terminal",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        let market = order.market.clone();
        let side = order.side;
        if let Some(price) = order.book_price {
            if let Some(book) = self.books.get_mut(&market) {
                book.remove(side, price, order_id);
            }
        }
        let o = self.orders.get_mut(&order_id).expect("checked above");
        o.status = OrderStatus::Cancelled;
        o.book_price = None;
        o.updated_ts_ms = entry.ts_ms;
        self.stats.cancelled += 1;
        out.push(ApplyOutput::Cancelled {
            order_id,
            market,
            reason,
            ts_ms: entry.ts_ms,
        });
    }

    /// Cancel `old`, then place `new`. If the old order cannot be cancelled,
    /// the whole replace is rejected with no mutation.
    fn apply_replace(
        &mut self,
        entry: &LogEntry,
        old_order_id: Uuid,
        new: &PlaceOrderCmd,
        out: &mut Vec<ApplyOutput>,
    ) {
        let cancellable = self
            .orders
            .get(&old_order_id)
            .map(|o| !o.status.is_terminal())
            .unwrap_or(false);
        if !cancellable {
            self.stats.rejected += 1;
            out.push(ApplyOutput::Rejected {
                order_id: new.order_id,
                market: entry.market.clone(),
                reason: "replace_old_order_unavailable",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        self.apply_cancel(entry, old_order_id, CancelReason::Replace, out);
        self.apply_place(entry, new, out);
    }

    /// Legacy Stage-1 fill entry: authoritative external fill. Updates order,
    /// book and accounting; no matching outputs (fills come from the log).
    fn apply_external_fill(
        &mut self,
        entry: &LogEntry,
        fill: &FillCmd,
        out: &mut Vec<ApplyOutput>,
    ) {
        if fill.quantity <= Decimal::ZERO {
            out.push(ApplyOutput::Rejected {
                order_id: fill.order_id,
                market: entry.market.clone(),
                reason: "invalid_quantity",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        let Some(order) = self.orders.get(&fill.order_id) else {
            out.push(ApplyOutput::Rejected {
                order_id: fill.order_id,
                market: entry.market.clone(),
                reason: "unknown_order",
                ts_ms: entry.ts_ms,
            });
            return;
        };
        if order.status.is_terminal() || fill.quantity > order.remaining() {
            out.push(ApplyOutput::Rejected {
                order_id: fill.order_id,
                market: order.market.clone(),
                reason: "invalid_fill",
                ts_ms: entry.ts_ms,
            });
            return;
        }
        let (side, market) = {
            let o = self.orders.get_mut(&fill.order_id).expect("checked");
            o.filled_quantity += fill.quantity;
            o.updated_ts_ms = entry.ts_ms;
            let full = o.remaining() <= Decimal::ZERO;
            o.status = if full {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            (o.side, o.market.clone())
        };
        if self.orders[&fill.order_id].remaining() <= Decimal::ZERO {
            if let Some(price) = self.orders[&fill.order_id].book_price {
                if let Some(book) = self.books.get_mut(&market) {
                    book.remove(side, price, fill.order_id);
                }
                self.orders
                    .get_mut(&fill.order_id)
                    .expect("exists")
                    .book_price = None;
            }
        }
        let signed = match side {
            Side::Bid => fill.quantity,
            Side::Ask => -fill.quantity,
        };
        *self
            .net_positions
            .entry(market.clone())
            .or_insert(Decimal::ZERO) += signed;
        *self.fees_paid.entry(market).or_insert(Decimal::ZERO) += fill.fee;
        self.stats.filled += 1;
    }

    // ---- matching plumbing ---------------------------------------------

    fn run_match(
        &mut self,
        taker_id: Uuid,
        side: Side,
        limit: Option<Price>,
        ts_ms: u64,
    ) -> MatchOutcome {
        // Split the field borrows explicitly.
        let (books, orders) = (
            self.books
                .get_mut(&self.orders[&taker_id].market.clone())
                .expect("book exists"),
            &mut self.orders,
        );
        matching::match_taker(books, orders, taker_id, side, limit, ts_ms)
    }

    /// Apply the outcome of `run_match` to the taker order and emit outputs:
    /// fills (with fees/positions) plus placement/cancellation/rejection.
    fn settle_taker(
        &mut self,
        taker_id: Uuid,
        outcome: &MatchOutcome,
        ts_ms: u64,
        policy: Policy,
        out: &mut Vec<ApplyOutput>,
    ) {
        let (market, side, _price) = {
            let t = &self.orders[&taker_id];
            (t.market.clone(), t.side, t.price)
        };

        // STP-cancelled resting makers (encountered during the match loop).
        let stp_reason = match outcome.abort {
            Some(matching::StpAbort::Both) => CancelReason::SelfTradeBoth,
            _ => CancelReason::SelfTradeResting,
        };
        for id in &outcome.stp_cancelled {
            self.stats.cancelled += 1;
            out.push(ApplyOutput::Cancelled {
                order_id: *id,
                market: market.clone(),
                reason: stp_reason,
                ts_ms,
            });
        }

        // Record fills: fees, positions, stats, outputs.
        for f in &outcome.fills {
            let notional = f.price * f.quantity;
            let taker_fee = notional * self.cfg.fee_taker_bps / Decimal::from(10_000);
            let maker_fee = notional * self.cfg.fee_maker_bps / Decimal::from(10_000);

            let taker_signed = match side {
                Side::Bid => f.quantity,
                Side::Ask => -f.quantity,
            };
            let maker_signed = -taker_signed;
            let pos = self
                .net_positions
                .entry(market.clone())
                .or_insert(Decimal::ZERO);
            *pos += taker_signed + maker_signed;
            let fees = self
                .fees_paid
                .entry(market.clone())
                .or_insert(Decimal::ZERO);
            *fees += taker_fee + maker_fee;

            self.stats.filled += 1;
            out.push(ApplyOutput::Fill {
                taker_order_id: taker_id,
                maker_order_id: f.maker_order_id,
                market: market.clone(),
                price: f.price,
                quantity: f.quantity,
                taker_fee,
                maker_fee,
                ts_ms,
            });
        }

        let remaining = outcome.remaining;
        let filled_any = self.orders[&taker_id].filled_quantity > Decimal::ZERO;

        // Status / book disposition by policy + STP abort.
        if let Some(stp_abort) = outcome.abort {
            let reason = match stp_abort {
                matching::StpAbort::Taker => CancelReason::SelfTradeTaker,
                matching::StpAbort::Both => CancelReason::SelfTradeBoth,
            };
            let market = {
                let t = self.orders.get_mut(&taker_id).expect("taker exists");
                t.status = if filled_any {
                    OrderStatus::Cancelled
                } else {
                    OrderStatus::Rejected
                };
                t.book_price = None;
                t.updated_ts_ms = ts_ms;
                t.market.clone()
            };
            if filled_any {
                self.stats.cancelled += 1;
                out.push(ApplyOutput::Cancelled {
                    order_id: taker_id,
                    market,
                    reason,
                    ts_ms,
                });
            } else {
                self.stats.rejected += 1;
                out.push(ApplyOutput::Rejected {
                    order_id: taker_id,
                    market,
                    reason: "self_trade",
                    ts_ms,
                });
            }
            return;
        }

        if remaining <= Decimal::ZERO {
            // Fully filled (match loop already set status).
            self.stats.placed += 1;
            out.push(ApplyOutput::Placed {
                order_id: taker_id,
                market,
                resting: false,
                ts_ms,
            });
            return;
        }

        match policy {
            Policy::Gtc | Policy::PostOnly => {
                // Rest the remainder (PostOnly never crosses — pre-checked).
                self.rest_order(taker_id, ts_ms);
                self.stats.placed += 1;
                out.push(ApplyOutput::Placed {
                    order_id: taker_id,
                    market,
                    resting: true,
                    ts_ms,
                });
            }
            Policy::Ioc | Policy::Fok => {
                // Unfilled remainder cannot rest.
                let status_market = {
                    let t = self.orders.get_mut(&taker_id).expect("taker exists");
                    t.status = OrderStatus::Cancelled;
                    t.book_price = None;
                    t.updated_ts_ms = ts_ms;
                    t.market.clone()
                };
                self.stats.cancelled += 1;
                out.push(ApplyOutput::Cancelled {
                    order_id: taker_id,
                    market: status_market,
                    reason: CancelReason::IocRemainder,
                    ts_ms,
                });
            }
        }
    }

    fn rest_order(&mut self, order_id: Uuid, ts_ms: u64) {
        let (market, side, price) = {
            let o = self.orders.get_mut(&order_id).expect("order exists");
            o.updated_ts_ms = ts_ms;
            let price = o.price.expect("resting orders are limit-priced");
            (o.market.clone(), o.side, price)
        };
        let book = self.books.get_mut(&market).expect("book exists");
        book.rest(side, price, order_id);
        let o = self.orders.get_mut(&order_id).expect("order exists");
        o.book_price = Some(price);
        if o.filled_quantity > Decimal::ZERO {
            o.status = OrderStatus::PartiallyFilled;
        } else {
            o.status = OrderStatus::Acknowledged;
        }
    }

    fn reject_in_place(
        &mut self,
        order_id: Uuid,
        market: &MarketId,
        reason: &'static str,
        ts_ms: u64,
        out: &mut Vec<ApplyOutput>,
    ) {
        let o = self.orders.get_mut(&order_id).expect("order exists");
        o.status = OrderStatus::Rejected;
        o.updated_ts_ms = ts_ms;
        self.stats.rejected += 1;
        out.push(ApplyOutput::Rejected {
            order_id,
            market: market.clone(),
            reason,
            ts_ms,
        });
    }
}

impl Default for ClobState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClobState {
    /// Apply only the structural shell of an entry: sequence checks, the
    /// expiry sweep and the sequence advance — **without** dispatching the
    /// payload. Used by `lq-perps` when a place/replace is rejected by the
    /// pre-trade margin check: the order must never reach the book, but the
    /// log entry still has to be consumed so replay stays gap-free.
    pub fn apply_noop(&mut self, entry: &LogEntry) -> Result<Vec<ApplyOutput>, ApplyError> {
        self.expect_global(entry)?;
        self.expect_market(entry)?;

        let mut out = Vec::new();
        self.sweep_expired(entry.ts_ms, &mut out);

        self.last_global_seq = entry.global_seq;
        self.market_seqs
            .insert(entry.market.clone(), entry.market_seq);
        Ok(out)
    }

    /// Cancel an order outside of a client command (reduce-only violations
    /// driven by the margin state machine). No-op if the order is unknown or
    /// already terminal; returns `true` when the book was mutated.
    pub fn force_cancel(
        &mut self,
        order_id: Uuid,
        reason: CancelReason,
        ts_ms: u64,
        out: &mut Vec<ApplyOutput>,
    ) -> bool {
        let Some(order) = self.orders.get(&order_id) else {
            return false;
        };
        if order.status.is_terminal() {
            return false;
        }
        let market = order.market.clone();
        let side = order.side;
        if let Some(price) = order.book_price {
            if let Some(book) = self.books.get_mut(&market) {
                book.remove(side, price, order_id);
            }
        }
        let o = self.orders.get_mut(&order_id).expect("checked above");
        o.status = OrderStatus::Cancelled;
        o.book_price = None;
        o.updated_ts_ms = ts_ms;
        self.stats.cancelled += 1;
        out.push(ApplyOutput::Cancelled {
            order_id,
            market,
            reason,
            ts_ms,
        });
        true
    }
}

impl StateMachine for ClobState {
    fn apply(&mut self, entry: &LogEntry) -> Result<Vec<ApplyOutput>, ApplyError> {
        self.expect_global(entry)?;
        self.expect_market(entry)?;

        let mut out = Vec::new();
        self.sweep_expired(entry.ts_ms, &mut out);

        match &entry.payload {
            EntryPayload::PlaceOrder(cmd) => self.apply_place(entry, cmd, &mut out),
            EntryPayload::CancelOrder { order_id } => {
                self.apply_cancel(entry, *order_id, CancelReason::User, &mut out)
            }
            EntryPayload::ReplaceOrder { old_order_id, new } => {
                self.apply_replace(entry, *old_order_id, new, &mut out)
            }
            EntryPayload::Fill(fill) => self.apply_external_fill(entry, fill, &mut out),
            EntryPayload::MarketTick(tick) => {
                self.apply_tick(&entry.market, tick);
            }
            // Stage 3 margin entries belong to `lq-perps`; the CLOB consumes
            // their sequence (advance below) but mutates nothing. Stage 4
            // oracle entries belong to the oracle + margin layer above.
            EntryPayload::Transfer { .. }
            | EntryPayload::Liquidate { .. }
            | EntryPayload::SettleFunding { .. }
            | EntryPayload::OraclePrice(_) => {}
        }

        self.last_global_seq = entry.global_seq;
        self.market_seqs
            .insert(entry.market.clone(), entry.market_seq);
        Ok(out)
    }

    fn state_hash(&self) -> StateHash {
        let mut h = new_hasher();
        write_str(&mut h, "lq-clob-v1");
        write_decimal(&mut h, &self.cfg.fee_taker_bps);
        write_decimal(&mut h, &self.cfg.fee_maker_bps);
        write_u64(&mut h, self.last_global_seq);

        write_u64(&mut h, self.market_seqs.len() as u64);
        for (m, seq) in &self.market_seqs {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, *seq);
        }

        write_u64(&mut h, self.orders.len() as u64);
        for o in self.orders.values() {
            write_str(&mut h, &o.order_id.to_string());
            write_str(&mut h, o.market.venue.as_str());
            write_str(&mut h, o.market.symbol.as_str());
            write_str(
                &mut h,
                match o.side {
                    Side::Bid => "bid",
                    Side::Ask => "ask",
                },
            );
            write_str(
                &mut h,
                match o.order_type {
                    OrderType::Limit => "limit",
                    OrderType::Market => "market",
                    OrderType::PostOnly => "post_only",
                    OrderType::ImmediateOrCancel => "ioc",
                    OrderType::FillOrKill => "fok",
                },
            );
            match &o.price {
                Some(p) => {
                    write_str(&mut h, "p:");
                    write_decimal(&mut h, p);
                }
                None => write_str(&mut h, "p:none"),
            }
            write_decimal(&mut h, &o.quantity);
            write_decimal(&mut h, &o.filled_quantity);
            write_str(&mut h, o.status.as_str());
            write_str(
                &mut h,
                match o.time_in_force {
                    TimeInForce::Gtc => "gtc",
                    TimeInForce::Ioc => "ioc",
                    TimeInForce::Fok => "fok",
                    TimeInForce::PostOnly => "post_only",
                },
            );
            write_str(&mut h, &o.owner);
            write_str(
                &mut h,
                match o.stp {
                    lq_sequencer::entry::StpPolicy::None => "stp:none",
                    lq_sequencer::entry::StpPolicy::CancelResting => "stp:cancel_resting",
                    lq_sequencer::entry::StpPolicy::CancelTaker => "stp:cancel_taker",
                    lq_sequencer::entry::StpPolicy::CancelBoth => "stp:cancel_both",
                },
            );
            match o.expiration_ms {
                Some(e) => {
                    write_str(&mut h, "exp:");
                    write_u64(&mut h, e);
                }
                None => write_str(&mut h, "exp:none"),
            }
            write_u64(&mut h, o.place_global_seq);
            write_u64(&mut h, o.created_ts_ms);
            write_u64(&mut h, o.updated_ts_ms);
            match &o.book_price {
                Some(p) => {
                    write_str(&mut h, "bp:");
                    write_decimal(&mut h, p);
                }
                None => write_str(&mut h, "bp:none"),
            }
        }

        write_u64(&mut h, self.books.len() as u64);
        for (m, book) in &self.books {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, book.bids.len() as u64);
            for (px, q) in &book.bids {
                write_decimal(&mut h, px);
                write_u64(&mut h, q.len() as u64);
                for id in q {
                    write_str(&mut h, &id.to_string());
                }
            }
            write_u64(&mut h, book.asks.len() as u64);
            for (px, q) in &book.asks {
                write_decimal(&mut h, px);
                write_u64(&mut h, q.len() as u64);
                for id in q {
                    write_str(&mut h, &id.to_string());
                }
            }
        }

        write_u64(&mut h, self.net_positions.len() as u64);
        for (m, q) in &self.net_positions {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, q);
        }
        write_u64(&mut h, self.fees_paid.len() as u64);
        for (m, f) in &self.fees_paid {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, f);
        }
        write_u64(&mut h, self.last_marks.len() as u64);
        for (m, p) in &self.last_marks {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, p);
        }
        write_u64(&mut h, self.stats.placed);
        write_u64(&mut h, self.stats.cancelled);
        write_u64(&mut h, self.stats.filled);
        write_u64(&mut h, self.stats.rejected);
        write_u64(&mut h, self.stats.expired);
        finish(h)
    }

    fn last_global_seq(&self) -> u64 {
        self.last_global_seq
    }

    fn market_seq(&self, market: &MarketId) -> u64 {
        self.market_seqs.get(market).copied().unwrap_or(0)
    }

    fn market_seqs(&self) -> Vec<(MarketId, u64)> {
        self.market_seqs
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    fn encode_state(&self) -> Result<Vec<u8>, String> {
        let wire = ClobWire {
            cfg: self.cfg,
            last_global_seq: self.last_global_seq,
            market_seqs: self
                .market_seqs
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            orders: self.orders.values().cloned().collect(),
            books: self
                .books
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            net_positions: self
                .net_positions
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            fees_paid: self
                .fees_paid
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_marks: self
                .last_marks
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            stats: self.stats,
        };
        serde_json::to_vec(&wire).map_err(|e| e.to_string())
    }

    fn decode_state(bytes: &[u8]) -> Result<Self, String> {
        let wire: ClobWire = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let mut market_seqs = BTreeMap::new();
        for (k, v) in wire.market_seqs {
            market_seqs.insert(k, v);
        }
        let mut orders = BTreeMap::new();
        for o in wire.orders {
            orders.insert(o.order_id, o);
        }
        let mut books = BTreeMap::new();
        for (k, v) in wire.books {
            books.insert(k, v);
        }
        let mut net_positions = BTreeMap::new();
        for (k, v) in wire.net_positions {
            net_positions.insert(k, v);
        }
        let mut fees_paid = BTreeMap::new();
        for (k, v) in wire.fees_paid {
            fees_paid.insert(k, v);
        }
        let mut last_marks = BTreeMap::new();
        for (k, v) in wire.last_marks {
            last_marks.insert(k, v);
        }
        Ok(Self {
            cfg: wire.cfg,
            last_global_seq: wire.last_global_seq,
            market_seqs,
            orders,
            books,
            net_positions,
            fees_paid,
            last_marks,
            stats: wire.stats,
        })
    }
}

impl ClobState {
    fn apply_tick(&mut self, market: &MarketId, tick: &MarketTickCmd) {
        self.last_marks.insert(market.clone(), tick.last);
    }
}
