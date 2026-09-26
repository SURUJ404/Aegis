//! State machine trait and the Stage 1 ledger state.
//!
//! Invariants enforced by design:
//! - apply is a pure function of `(state, entry)` (time from entry only)
//! - iteration for hashing uses `BTreeMap` order only
//! - money is `rust_decimal::Decimal` (normalized in the hash); no `f64`
//! - no RNG, no wall-clock reads inside `apply`

use std::collections::BTreeMap;

use lq_types::{OrderStatus, OrderType, Price, Qty, Side, TimeInForce};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entry::{EntryPayload, LogEntry, MarketId};
use crate::hash::{
    finish, new_hasher, write_decimal, write_str, write_u64, StateHash, StateHasher,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApplyError {
    #[error("sequence gap: expected global {expected}, got {got}")]
    GlobalSeqGap { expected: u64, got: u64 },
    #[error("market {market} sequence gap: expected {expected}, got {got}")]
    MarketSeqGap {
        market: String,
        expected: u64,
        got: u64,
    },
    #[error("duplicate order {0}")]
    DuplicateOrder(Uuid),
    #[error("unknown order {0}")]
    UnknownOrder(Uuid),
    #[error("invalid order quantity")]
    InvalidQuantity,
    #[error("invalid limit price")]
    InvalidPrice,
    #[error("order {0} already terminal")]
    OrderTerminal(Uuid),
}

/// Why an order left the book without being filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// Explicit cancel command.
    User,
    /// Cancel/replace superseded the order.
    Replace,
    /// Self-trade prevention cancelled the resting order.
    SelfTradeResting,
    /// Self-trade prevention cancelled the taker remainder.
    SelfTradeTaker,
    /// Self-trade prevention cancelled both sides.
    SelfTradeBoth,
    /// IOC/FOK/market remainder could not rest.
    IocRemainder,
    /// Order was rejected by matching policy (PostOnly cross, FOK, STP).
    Rejected,
}

/// Side effect produced by applying one log entry. Emitted by the state
/// machine for the indexer/gateway (Stage 6); never written back to the WAL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApplyOutput {
    /// A trade between `taker_order_id` and `maker_order_id` at `price`.
    Fill {
        taker_order_id: Uuid,
        maker_order_id: Uuid,
        market: MarketId,
        price: Price,
        quantity: Qty,
        taker_fee: Decimal,
        maker_fee: Decimal,
        ts_ms: u64,
    },
    /// Order accepted (resting or filled).
    Placed {
        order_id: Uuid,
        market: MarketId,
        resting: bool,
        ts_ms: u64,
    },
    /// Order cancelled (see [`CancelReason`]).
    Cancelled {
        order_id: Uuid,
        market: MarketId,
        reason: CancelReason,
        ts_ms: u64,
    },
    /// Order rejected without resting (PostOnly cross, FOK, STP taker, …).
    Rejected {
        order_id: Uuid,
        market: MarketId,
        reason: &'static str,
        ts_ms: u64,
    },
    /// Short-term order expired against the entry's logical time.
    Expired {
        order_id: Uuid,
        market: MarketId,
        ts_ms: u64,
    },
}

/// Deterministic event-sourced state machine.
pub trait StateMachine {
    /// Apply one entry. Returns outputs (fills, lifecycle events) for the
    /// caller (indexer, tests). Same log ⇒ same state hash ⇒ same outputs.
    fn apply(&mut self, entry: &LogEntry) -> Result<Vec<ApplyOutput>, ApplyError>;

    /// Canonical hash of all money-relevant state (sequences included).
    fn state_hash(&self) -> StateHash;

    fn last_global_seq(&self) -> u64;

    fn market_seq(&self, market: &MarketId) -> u64;

    /// All known market sequence watermarks, sorted by market (for snapshots).
    fn market_seqs(&self) -> Vec<(MarketId, u64)>;

    /// Append-only encoding for snapshots (JSON via sequencer snapshot module).
    fn encode_state(&self) -> Result<Vec<u8>, String>;

    fn decode_state(bytes: &[u8]) -> Result<Self, String>
    where
        Self: Sized;
}

/// Order projection stored in the ledger (Stage 1: lifecycle only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerOrder {
    pub order_id: Uuid,
    pub market: MarketId,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Price>,
    pub quantity: Qty,
    pub filled_quantity: Qty,
    pub status: OrderStatus,
    pub time_in_force: TimeInForce,
    pub created_ts_ms: u64,
    pub updated_ts_ms: u64,
}

impl LedgerOrder {
    fn remaining(&self) -> Qty {
        (self.quantity - self.filled_quantity).max(Decimal::ZERO)
    }

    fn write_canonical(&self, h: &mut StateHasher) {
        write_str(h, &self.order_id.to_string());
        write_str(h, self.market.venue.as_str());
        write_str(h, self.market.symbol.as_str());
        write_str(
            h,
            match self.side {
                Side::Bid => "bid",
                Side::Ask => "ask",
            },
        );
        write_str(
            h,
            match self.order_type {
                OrderType::Limit => "limit",
                OrderType::Market => "market",
                OrderType::PostOnly => "post_only",
                OrderType::ImmediateOrCancel => "ioc",
                OrderType::FillOrKill => "fok",
            },
        );
        match &self.price {
            Some(p) => {
                write_str(h, "p:");
                write_decimal(h, p);
            }
            None => write_str(h, "p:none"),
        }
        write_decimal(h, &self.quantity);
        write_decimal(h, &self.filled_quantity);
        write_str(h, self.status.as_str());
        write_u64(h, self.created_ts_ms);
        write_u64(h, self.updated_ts_ms);
    }
}

/// Stage 1 ledger: open orders, fills accounting, last marks, sequences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerState {
    last_global_seq: u64,
    market_seqs: BTreeMap<MarketId, u64>,
    orders: BTreeMap<Uuid, LedgerOrder>,
    /// Net base quantity per market (positive = long).
    net_positions: BTreeMap<MarketId, Qty>,
    realized_fees: BTreeMap<MarketId, Decimal>,
    last_marks: BTreeMap<MarketId, Price>,
    /// Monotonic counters for observability (part of the hash).
    stats_placed: u64,
    stats_cancelled: u64,
    stats_filled: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct LedgerWire {
    last_global_seq: u64,
    market_seqs: Vec<(MarketId, u64)>,
    orders: Vec<LedgerOrder>,
    net_positions: Vec<(MarketId, Qty)>,
    realized_fees: Vec<(MarketId, Decimal)>,
    last_marks: Vec<(MarketId, Price)>,
    stats_placed: u64,
    stats_cancelled: u64,
    stats_filled: u64,
}

impl LedgerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_orders(&self) -> usize {
        self.orders
            .values()
            .filter(|o| !o.status.is_terminal())
            .count()
    }

    pub fn order(&self, id: Uuid) -> Option<&LedgerOrder> {
        self.orders.get(&id)
    }

    pub fn net_position(&self, market: &MarketId) -> Qty {
        self.net_positions
            .get(market)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    pub fn last_mark(&self, market: &MarketId) -> Option<Price> {
        self.last_marks.get(market).copied()
    }

    pub fn stats(&self) -> (u64, u64, u64) {
        (self.stats_placed, self.stats_cancelled, self.stats_filled)
    }

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

    fn apply_place(
        &mut self,
        entry: &LogEntry,
        cmd: &crate::entry::PlaceOrderCmd,
    ) -> Result<(), ApplyError> {
        if cmd.quantity <= Decimal::ZERO {
            return Err(ApplyError::InvalidQuantity);
        }
        if matches!(cmd.order_type, OrderType::Limit | OrderType::PostOnly)
            && cmd.price.map(|p| p <= Decimal::ZERO).unwrap_or(true)
        {
            return Err(ApplyError::InvalidPrice);
        }
        if self.orders.contains_key(&cmd.order_id) {
            return Err(ApplyError::DuplicateOrder(cmd.order_id));
        }
        self.orders.insert(
            cmd.order_id,
            LedgerOrder {
                order_id: cmd.order_id,
                market: entry.market.clone(),
                side: cmd.side,
                order_type: cmd.order_type,
                price: cmd.price,
                quantity: cmd.quantity,
                filled_quantity: Decimal::ZERO,
                status: OrderStatus::Acknowledged,
                time_in_force: cmd.time_in_force,
                created_ts_ms: entry.ts_ms,
                updated_ts_ms: entry.ts_ms,
            },
        );
        self.stats_placed += 1;
        Ok(())
    }

    fn apply_cancel(&mut self, entry: &LogEntry, order_id: Uuid) -> Result<(), ApplyError> {
        let order = self
            .orders
            .get_mut(&order_id)
            .ok_or(ApplyError::UnknownOrder(order_id))?;
        if order.status.is_terminal() {
            return Err(ApplyError::OrderTerminal(order_id));
        }
        order.status = OrderStatus::Cancelled;
        order.updated_ts_ms = entry.ts_ms;
        self.stats_cancelled += 1;
        Ok(())
    }

    fn apply_replace(
        &mut self,
        entry: &LogEntry,
        old_order_id: Uuid,
        new: &crate::entry::PlaceOrderCmd,
    ) -> Result<(), ApplyError> {
        self.apply_cancel(entry, old_order_id)?;
        self.apply_place(entry, new)
    }

    fn apply_fill(
        &mut self,
        entry: &LogEntry,
        fill: &crate::entry::FillCmd,
    ) -> Result<(), ApplyError> {
        if fill.quantity <= Decimal::ZERO {
            return Err(ApplyError::InvalidQuantity);
        }
        let (side, market) = {
            let order = self
                .orders
                .get_mut(&fill.order_id)
                .ok_or(ApplyError::UnknownOrder(fill.order_id))?;
            if order.status.is_terminal() {
                return Err(ApplyError::OrderTerminal(fill.order_id));
            }
            if fill.quantity > order.remaining() {
                return Err(ApplyError::InvalidQuantity);
            }
            order.filled_quantity += fill.quantity;
            order.updated_ts_ms = entry.ts_ms;
            if order.remaining() <= Decimal::ZERO {
                order.status = OrderStatus::Filled;
            } else {
                order.status = OrderStatus::PartiallyFilled;
            }
            (order.side, order.market.clone())
        };

        let signed = match side {
            Side::Bid => fill.quantity,
            Side::Ask => -fill.quantity,
        };
        *self
            .net_positions
            .entry(market.clone())
            .or_insert(Decimal::ZERO) += signed;
        *self.realized_fees.entry(market).or_insert(Decimal::ZERO) += fill.fee;
        self.stats_filled += 1;
        Ok(())
    }

    fn apply_tick(&mut self, market: &MarketId, tick: &crate::entry::MarketTickCmd) {
        self.last_marks.insert(market.clone(), tick.last);
    }
}

impl StateMachine for LedgerState {
    fn apply(&mut self, entry: &LogEntry) -> Result<Vec<ApplyOutput>, ApplyError> {
        self.expect_global(entry)?;
        self.expect_market(entry)?;

        match &entry.payload {
            EntryPayload::PlaceOrder(cmd) => self.apply_place(entry, cmd)?,
            EntryPayload::CancelOrder { order_id } => self.apply_cancel(entry, *order_id)?,
            EntryPayload::ReplaceOrder { old_order_id, new } => {
                self.apply_replace(entry, *old_order_id, new)?
            }
            EntryPayload::Fill(fill) => self.apply_fill(entry, fill)?,
            EntryPayload::MarketTick(tick) => self.apply_tick(&entry.market, tick),
        }

        self.last_global_seq = entry.global_seq;
        self.market_seqs
            .insert(entry.market.clone(), entry.market_seq);
        // Stage 1 ledger consumes fills as inputs; matching outputs belong to
        // the CLOB state machine (`lq-clob`).
        Ok(Vec::new())
    }

    fn state_hash(&self) -> StateHash {
        let mut h = new_hasher();
        write_str(&mut h, "lq-ledger-v1");
        write_u64(&mut h, self.last_global_seq);
        write_u64(&mut h, self.market_seqs.len() as u64);
        for (m, seq) in &self.market_seqs {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_u64(&mut h, *seq);
        }
        write_u64(&mut h, self.orders.len() as u64);
        for (id, order) in &self.orders {
            let _ = id;
            order.write_canonical(&mut h);
        }
        write_u64(&mut h, self.net_positions.len() as u64);
        for (m, qty) in &self.net_positions {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, qty);
        }
        write_u64(&mut h, self.realized_fees.len() as u64);
        for (m, fee) in &self.realized_fees {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, fee);
        }
        write_u64(&mut h, self.last_marks.len() as u64);
        for (m, px) in &self.last_marks {
            write_str(&mut h, m.venue.as_str());
            write_str(&mut h, m.symbol.as_str());
            write_decimal(&mut h, px);
        }
        write_u64(&mut h, self.stats_placed);
        write_u64(&mut h, self.stats_cancelled);
        write_u64(&mut h, self.stats_filled);
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
        let wire = LedgerWire {
            last_global_seq: self.last_global_seq,
            market_seqs: self
                .market_seqs
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            orders: self.orders.values().cloned().collect(),
            net_positions: self
                .net_positions
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            realized_fees: self
                .realized_fees
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_marks: self
                .last_marks
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            stats_placed: self.stats_placed,
            stats_cancelled: self.stats_cancelled,
            stats_filled: self.stats_filled,
        };
        serde_json::to_vec(&wire).map_err(|e| e.to_string())
    }

    fn decode_state(bytes: &[u8]) -> Result<Self, String> {
        let wire: LedgerWire = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let mut market_seqs = BTreeMap::new();
        for (k, v) in wire.market_seqs {
            market_seqs.insert(k, v);
        }
        let mut orders = BTreeMap::new();
        for o in wire.orders {
            orders.insert(o.order_id, o);
        }
        let mut net_positions = BTreeMap::new();
        for (k, v) in wire.net_positions {
            net_positions.insert(k, v);
        }
        let mut realized_fees = BTreeMap::new();
        for (k, v) in wire.realized_fees {
            realized_fees.insert(k, v);
        }
        let mut last_marks = BTreeMap::new();
        for (k, v) in wire.last_marks {
            last_marks.insert(k, v);
        }
        Ok(Self {
            last_global_seq: wire.last_global_seq,
            market_seqs,
            orders,
            net_positions,
            realized_fees,
            last_marks,
            stats_placed: wire.stats_placed,
            stats_cancelled: wire.stats_cancelled,
            stats_filled: wire.stats_filled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{FillCmd, FillLiquidity, MarketTickCmd, PlaceOrderCmd};
    use lq_types::{Exchange, Symbol};
    use rust_decimal_macros::dec;

    fn market() -> MarketId {
        MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into()))
    }

    fn place(seq: u64, market_seq: u64, order_id: Uuid, qty: Qty) -> LogEntry {
        LogEntry {
            global_seq: seq,
            market_seq,
            market: market(),
            ts_ms: 1000 + seq,
            payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id,
                client_order_id: format!("c-{seq}"),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(100)),
                quantity: qty,
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn apply_place_and_hash_stable() {
        let mut sm = LedgerState::new();
        let id = Uuid::nil();
        sm.apply(&place(1, 1, id, dec!(1))).unwrap();
        let h1 = sm.state_hash();
        let mut sm2 = LedgerState::new();
        sm2.apply(&place(1, 1, id, dec!(1))).unwrap();
        assert_eq!(h1, sm2.state_hash());
        assert_eq!(sm.open_orders(), 1);
    }

    #[test]
    fn seq_gap_rejected() {
        let mut sm = LedgerState::new();
        let err = sm.apply(&place(2, 1, Uuid::nil(), dec!(1))).unwrap_err();
        assert!(matches!(
            err,
            ApplyError::GlobalSeqGap {
                expected: 1,
                got: 2
            }
        ));
    }

    #[test]
    fn fill_updates_position_and_net_is_signed() {
        let mut sm = LedgerState::new();
        let bid = Uuid::nil();
        let ask = Uuid::from_u128(1);

        // Long via bid fill
        sm.apply(&LogEntry {
            global_seq: 1,
            market_seq: 1,
            market: market(),
            ts_ms: 1000,
            payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id: bid,
                client_order_id: "b".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(100)),
                quantity: dec!(2),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        })
        .unwrap();
        sm.apply(&LogEntry {
            global_seq: 2,
            market_seq: 2,
            market: market(),
            ts_ms: 1001,
            payload: EntryPayload::Fill(FillCmd {
                order_id: bid,
                price: dec!(100),
                quantity: dec!(1),
                fee: dec!(0.01),
                liquidity: FillLiquidity::Maker,
            }),
        })
        .unwrap();
        assert_eq!(sm.net_position(&market()), dec!(1));

        // Short via ask fill brings net back toward zero / negative
        sm.apply(&LogEntry {
            global_seq: 3,
            market_seq: 3,
            market: market(),
            ts_ms: 1002,
            payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
                order_id: ask,
                client_order_id: "a".into(),
                side: Side::Ask,
                order_type: OrderType::Limit,
                price: Some(dec!(101)),
                quantity: dec!(3),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        })
        .unwrap();
        sm.apply(&LogEntry {
            global_seq: 4,
            market_seq: 4,
            market: market(),
            ts_ms: 1003,
            payload: EntryPayload::Fill(FillCmd {
                order_id: ask,
                price: dec!(101),
                quantity: dec!(2),
                fee: dec!(0.02),
                liquidity: FillLiquidity::Taker,
            }),
        })
        .unwrap();
        assert_eq!(sm.net_position(&market()), dec!(1) - dec!(2));
        assert_eq!(sm.net_position(&market()), dec!(-1));
    }

    #[test]
    fn encode_decode_roundtrip_preserves_hash() {
        let mut sm = LedgerState::new();
        sm.apply(&place(1, 1, Uuid::nil(), dec!(1))).unwrap();
        sm.apply(&LogEntry {
            global_seq: 2,
            market_seq: 2,
            market: market(),
            ts_ms: 5,
            payload: EntryPayload::MarketTick(MarketTickCmd {
                last: dec!(99.5),
                bid: Some(dec!(99.4)),
                ask: Some(dec!(99.6)),
            }),
        })
        .unwrap();
        let bytes = sm.encode_state().unwrap();
        let back = LedgerState::decode_state(&bytes).unwrap();
        assert_eq!(sm.state_hash(), back.state_hash());
        assert_eq!(sm, back);
    }
}
