//! Stage 3 behavioral tests: transfers, margin pre-trade checks, atomic
//! fills, reduce-only, funding, liquidation (book + insurance paths), ADL,
//! cascade breaker, flags — plus the determinism/replay contract (same log ⇒
//! same hash, WAL rebuild, snapshot recovery, encode/decode roundtrip).
//!
//! All three block invariants are checked after **every** entry via
//! `Fixture::apply`.

use std::collections::BTreeMap;

use lq_perps::{PerpsConfig, PerpsState, DEFAULT_SUBACCOUNT, INSURANCE_SUBACCOUNT};
use lq_sequencer::entry::{
    EntryPayload, FillCmd, FillLiquidity, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd,
    StpPolicy,
};
use lq_sequencer::hash::StateHash;
use lq_sequencer::rebuild_empty_log;
use lq_sequencer::state::{ApplyError, ApplyOutput, CancelReason, StateMachine};
use lq_types::{Exchange, OrderStatus, OrderType, Price, Qty, Side, Symbol, TimeInForce};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

fn market(sym: &str) -> MarketId {
    MarketId::new(Exchange::Paper, Symbol(sym.to_string()))
}

fn btc() -> MarketId {
    market("BTC-USDT")
}

struct LogBuilder {
    g: u64,
    seqs: BTreeMap<String, u64>,
}

impl LogBuilder {
    fn new() -> Self {
        Self {
            g: 0,
            seqs: BTreeMap::new(),
        }
    }

    fn next(&mut self, market: &MarketId, ts_ms: u64, payload: EntryPayload) -> LogEntry {
        self.g += 1;
        let key = market.to_string();
        let seq = self.seqs.entry(key).or_insert(0);
        *seq += 1;
        LogEntry {
            global_seq: self.g,
            market_seq: *seq,
            market: market.clone(),
            ts_ms,
            payload,
        }
    }
}

/// Applies entries and asserts the block invariants after each one.
struct Fixture {
    sm: PerpsState,
    lb: LogBuilder,
}

impl Fixture {
    fn new() -> Self {
        Self::with_state(PerpsState::new())
    }

    fn with_state(sm: PerpsState) -> Self {
        Self {
            sm,
            lb: LogBuilder::new(),
        }
    }

    fn apply(&mut self, market: &MarketId, ts: u64, payload: EntryPayload) -> Vec<ApplyOutput> {
        let e = self.lb.next(market, ts, payload);
        let out = self
            .sm
            .apply(&e)
            .expect("command problems are Ok paths with Rejected outputs");
        self.sm
            .check_invariants()
            .expect("invariants hold after every entry");
        out
    }
}

fn transfer(subaccount: u64, amount: Decimal) -> EntryPayload {
    EntryPayload::Transfer { subaccount, amount }
}

fn tick(last: Price) -> EntryPayload {
    EntryPayload::MarketTick(MarketTickCmd {
        last,
        bid: None,
        ask: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn place(
    id: Uuid,
    side: Side,
    price: Option<Price>,
    qty: Qty,
    subaccount: Option<u64>,
    reduce_only: bool,
) -> EntryPayload {
    place_tif(id, side, price, qty, subaccount, reduce_only, TimeInForce::Gtc)
}

#[allow(clippy::too_many_arguments)]
fn place_tif(
    id: Uuid,
    side: Side,
    price: Option<Price>,
    qty: Qty,
    subaccount: Option<u64>,
    reduce_only: bool,
    tif: TimeInForce,
) -> EntryPayload {
    EntryPayload::PlaceOrder(PlaceOrderCmd {
        order_id: id,
        client_order_id: format!("c-{id}"),
        side,
        order_type: OrderType::Limit,
        price,
        quantity: qty,
        time_in_force: tif,
        owner: String::new(),
        stp: StpPolicy::None,
        expiration_ms: None,
        subaccount,
        reduce_only,
    })
}

fn liquidate(subaccount: u64, max_qty: Option<Qty>) -> EntryPayload {
    EntryPayload::Liquidate {
        subaccount,
        max_qty,
    }
}

fn settle_funding(rate: Decimal) -> EntryPayload {
    EntryPayload::SettleFunding { rate }
}

/// One rejected command, if that is the only output.
fn only_reason(out: &[ApplyOutput]) -> Option<&'static str> {
    match out {
        [ApplyOutput::Rejected { reason, .. }] => Some(reason),
        _ => None,
    }
}

fn find_liquidated(out: &[ApplyOutput]) -> Option<(Qty, Qty, Qty, Price)> {
    out.iter().find_map(|o| match o {
        ApplyOutput::Liquidated {
            quantity,
            book_filled,
            insurance_filled,
            limit_price,
            ..
        } => Some((*quantity, *book_filled, *insurance_filled, *limit_price)),
        _ => None,
    })
}

fn find_adl(out: &[ApplyOutput]) -> Option<(u64, Qty, Price)> {
    out.iter().find_map(|o| match o {
        ApplyOutput::Adl {
            subaccount,
            quantity,
            price,
            ..
        } => Some((*subaccount, *quantity, *price)),
        _ => None,
    })
}

fn has_margin_flagged(out: &[ApplyOutput], sub: u64) -> bool {
    out.iter().any(|o| matches!(
        o,
        ApplyOutput::MarginFlagged { subaccount, .. } if *subaccount == sub
    ))
}

fn has_reduce_only_cancel(out: &[ApplyOutput], id: Uuid) -> bool {
    out.iter().any(|o| matches!(
        o,
        ApplyOutput::Cancelled { order_id, reason: CancelReason::ReduceOnly, .. }
            if *order_id == id
    ))
}

// ---- transfers ------------------------------------------------------------

#[test]
fn deposits_and_withdrawals_cross_the_collateral_boundary() {
    let mut fx = Fixture::new();
    let m = btc();

    let out = fx.apply(&m, 1, transfer(0, dec!(10_000)));
    assert!(matches!(
        out.as_slice(),
        [ApplyOutput::Transferred {
            collateral_after,
            ..
        }] if *collateral_after == dec!(10_000)
    ));
    fx.apply(&m, 2, transfer(1, dec!(500)));
    let out = fx.apply(&m, 3, transfer(1, dec!(-200)));
    assert!(matches!(
        out.as_slice(),
        [ApplyOutput::Transferred {
            collateral_after,
            ..
        }] if *collateral_after == dec!(300)
    ));

    // Overdraft: rejected, balance untouched.
    let out = fx.apply(&m, 4, transfer(1, dec!(-400)));
    assert_eq!(only_reason(&out), Some("insufficient_collateral"));
    assert_eq!(fx.sm.collateral(1), dec!(300));

    // Zero and insurance deposits are rejected too.
    assert_eq!(
        only_reason(&fx.apply(&m, 5, transfer(1, Decimal::ZERO))),
        Some("invalid_amount")
    );
    assert_eq!(
        only_reason(&fx.apply(&m, 6, transfer(INSURANCE_SUBACCOUNT, dec!(1)))),
        Some("reserved_subaccount")
    );
    assert_eq!(fx.sm.collateral(INSURANCE_SUBACCOUNT), Decimal::ZERO);

    assert_eq!(fx.sm.stats().transfers, 3);
    assert_eq!(fx.sm.collateral(0), dec!(10_000));
}

// ---- fills ----------------------------------------------------------------

#[test]
fn fills_update_both_subaccounts_atomically_and_credit_fees() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(500)));
    fx.apply(&m, 3, tick(dec!(100)));

    // Maker ask rests, taker bid crosses it — one entry, both sides move.
    fx.apply(
        &m,
        4,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(1), Some(0), false),
    );
    let out = fx.apply(
        &m,
        5,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    assert!(out.iter().any(|o| matches!(o, ApplyOutput::Fill { .. })));

    assert_eq!(fx.sm.position(0, &m), dec!(-1));
    assert_eq!(fx.sm.position(1, &m), dec!(1));
    // Maker (sub0): +100 cash, no maker fee. Taker (sub1): −100 − 0.05.
    assert_eq!(fx.sm.collateral(0), dec!(10_100));
    assert_eq!(fx.sm.collateral(1), dec!(399.95));
    // Insurance received the 5 bps taker fee.
    assert_eq!(fx.sm.collateral(INSURANCE_SUBACCOUNT), dec!(0.05));
    assert_eq!(fx.sm.equity_of(0), dec!(10_000));
    assert_eq!(fx.sm.equity_of(1), dec!(499.95));
    assert!(fx.sm.pending_liquidations().is_empty());
    assert_eq!(fx.sm.stats().liquidations, 0);
}

// ---- pre-trade checks -----------------------------------------------------

#[test]
fn insufficient_margin_rejects_before_reaching_the_book() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(1, dec!(1)));
    fx.apply(&m, 2, tick(dec!(100)));

    let id = Uuid::from_u128(9);
    let out = fx.apply(
        &m,
        3,
        place(id, Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    assert_eq!(only_reason(&out), Some("insufficient_margin"));
    assert!(fx.sm.clob().order(id).is_none(), "order never reached the book");
    assert_eq!(fx.sm.position(1, &m), Decimal::ZERO);
    assert_eq!(fx.sm.collateral(1), dec!(1));
    assert_eq!(fx.sm.stats().margin_rejected, 1);
    assert!(fx.sm.pending_liquidations().is_empty());
}

#[test]
fn max_order_qty_reduces_the_order_instead_of_rejecting() {
    let cfg = PerpsConfig {
        max_order_qty: dec!(1),
        ..PerpsConfig::default()
    };
    let mut fx = Fixture::with_state(PerpsState::with_config(cfg));
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));

    let id = Uuid::from_u128(10);
    let out = fx.apply(
        &m,
        3,
        place(id, Side::Bid, Some(dec!(99)), dec!(2), Some(0), false),
    );
    assert!(out.iter().any(|o| matches!(o, ApplyOutput::Placed { .. })));
    assert!(!out.iter().any(|o| matches!(o, ApplyOutput::Rejected { .. })));
    let order = fx.sm.clob().order(id).expect("reduced order rests");
    assert_eq!(order.quantity, dec!(1));
    assert_eq!(fx.sm.stats().margin_rejected, 0);
}

#[test]
fn max_notional_per_order_is_rejected() {
    let cfg = PerpsConfig {
        max_notional_per_order: dec!(500),
        ..PerpsConfig::default()
    };
    let mut fx = Fixture::with_state(PerpsState::with_config(cfg));
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));

    let id = Uuid::from_u128(11);
    let out = fx.apply(
        &m,
        3,
        place(id, Side::Bid, Some(dec!(100)), dec!(6), Some(0), false),
    );
    assert_eq!(only_reason(&out), Some("max_notional"));
    assert!(fx.sm.clob().order(id).is_none());
}

#[test]
fn max_position_qty_is_rejected() {
    let cfg = PerpsConfig {
        max_position_qty: dec!(0.5),
        ..PerpsConfig::default()
    };
    let mut fx = Fixture::with_state(PerpsState::with_config(cfg));
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));

    let id = Uuid::from_u128(12);
    let out = fx.apply(
        &m,
        3,
        place(id, Side::Bid, Some(dec!(100)), dec!(1), Some(0), false),
    );
    assert_eq!(only_reason(&out), Some("max_position"));
    assert!(fx.sm.clob().order(id).is_none());
}

#[test]
fn max_open_orders_caps_resting_orders_per_subaccount() {
    let cfg = PerpsConfig {
        max_open_orders: 1,
        ..PerpsConfig::default()
    };
    let mut fx = Fixture::with_state(PerpsState::with_config(cfg));
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));

    fx.apply(
        &m,
        3,
        place(Uuid::from_u128(20), Side::Bid, Some(dec!(99)), dec!(1), Some(0), false),
    );
    let id = Uuid::from_u128(21);
    let out = fx.apply(
        &m,
        4,
        place(id, Side::Bid, Some(dec!(98)), dec!(1), Some(0), false),
    );
    assert_eq!(only_reason(&out), Some("max_open_orders"));
    assert!(fx.sm.clob().order(id).is_none());
}

#[test]
fn pre_trade_check_is_pure_and_reasoned() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(1, dec!(1)));
    fx.apply(&m, 2, tick(dec!(100)));

    let before = fx.sm.state_hash();
    let cmd = PlaceOrderCmd {
        order_id: Uuid::from_u128(30),
        side: Side::Bid,
        price: Some(dec!(100)),
        quantity: dec!(1),
        subaccount: Some(1),
        ..PlaceOrderCmd::default()
    };
    let v1 = fx.sm.pre_trade_check(&m, &cmd, None);
    let v2 = fx.sm.pre_trade_check(&m, &cmd, None);
    assert_eq!(v1, v2);
    assert_eq!(v1.as_str(), "insufficient_margin");
    assert_eq!(fx.sm.state_hash(), before, "pre_trade_check must not mutate");
}

// ---- reduce-only ----------------------------------------------------------

#[test]
fn reduce_only_orders_cannot_exceed_the_position_at_place_time() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(500)));
    fx.apply(&m, 3, tick(dec!(100)));
    // Establish sub1 long 1 (maker ask from sub0, taker bid from sub1).
    fx.apply(
        &m,
        4,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(1), Some(0), false),
    );
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    assert_eq!(fx.sm.position(1, &m), dec!(1));

    // Reduce-only ask of 2 against a position of 1: rejected.
    let id = Uuid::from_u128(3);
    let out = fx.apply(
        &m,
        6,
        place(id, Side::Ask, Some(dec!(110)), dec!(2), Some(1), true),
    );
    assert_eq!(only_reason(&out), Some("reduce_only_exceeds_position"));
    assert!(fx.sm.clob().order(id).is_none());

    // Exactly 1 is fine and rests.
    let ok = Uuid::from_u128(4);
    let out = fx.apply(
        &m,
        7,
        place(ok, Side::Ask, Some(dec!(110)), dec!(1), Some(1), true),
    );
    assert!(out.iter().any(|o| matches!(o, ApplyOutput::Placed { .. })));
    assert!(fx.sm.clob().order(ok).is_some());
}

#[test]
fn reduce_only_orders_auto_cancel_when_the_position_shrinks() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(20)));
    fx.apply(&m, 3, tick(dec!(100)));

    // sub0 maker ask, sub1 taker bid ⇒ sub1 long 1, sub0 short 1.
    fx.apply(
        &m,
        4,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(1), Some(0), false),
    );
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    // sub0 rests a bid so sub1 can close its long later.
    fx.apply(
        &m,
        6,
        place(Uuid::from_u128(3), Side::Bid, Some(dec!(95)), dec!(1), Some(0), false),
    );
    // Reduce-only ask (reduce the long) rests above the market.
    let ro = Uuid::from_u128(4);
    fx.apply(
        &m,
        7,
        place(ro, Side::Ask, Some(dec!(110)), dec!(1), Some(1), true),
    );
    assert!(!fx.sm.clob().order(ro).unwrap().status.is_terminal());

    // sub1 closes its long by selling into sub0's bid ⇒ position goes flat.
    let out = fx.apply(
        &m,
        8,
        place(Uuid::from_u128(5), Side::Ask, Some(dec!(95)), dec!(1), Some(1), false),
    );
    assert_eq!(fx.sm.position(1, &m), Decimal::ZERO);
    assert!(
        has_reduce_only_cancel(&out, ro),
        "oversized reduce-only order must be cancelled, got {out:?}"
    );
    assert!(fx.sm.clob().order(ro).unwrap().status.is_terminal());
}

// ---- funding --------------------------------------------------------------

#[test]
fn funding_settlement_is_zero_sum_and_conserves_collateral() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(500)));
    fx.apply(&m, 3, tick(dec!(100)));
    fx.apply(
        &m,
        4,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(1), Some(0), false),
    );
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );

    let out = fx.apply(&m, 6, settle_funding(dec!(0.01)));
    let payments = match out.as_slice() {
        [ApplyOutput::FundingSettled { payments, rate, .. }] => {
            assert_eq!(*rate, dec!(0.01));
            payments.clone()
        }
        other => panic!("expected FundingSettled, got {other:?}"),
    };
    // Long pays 1, short receives 1 (rate 0.01 × mark 100 = 1).
    assert_eq!(payments, vec![(0, dec!(-1)), (1, dec!(1))]);
    assert_eq!(payments.iter().map(|(_, p)| *p).sum::<Decimal>(), Decimal::ZERO);
    assert_eq!(fx.sm.collateral(0), dec!(10_101));
    assert_eq!(fx.sm.collateral(1), dec!(398.95));
    assert_eq!(fx.sm.funding_index(&m), dec!(1));
    assert_eq!(
        fx.sm.subaccount(1).unwrap().positions[&m].last_funding_index,
        dec!(1)
    );
    assert_eq!(fx.sm.stats().funding_settlements, 1);

    // A second settlement charges the holders again; still zero-sum.
    fx.apply(&m, 7, settle_funding(dec!(0.01)));
    assert_eq!(fx.sm.collateral(0), dec!(10_102));
    assert_eq!(fx.sm.collateral(1), dec!(397.95));
    assert_eq!(fx.sm.funding_index(&m), dec!(2));
}

#[test]
fn funding_without_a_mark_price_is_rejected() {
    let mut fx = Fixture::new();
    let m = market("ETH-USDT");
    let out = fx.apply(&m, 1, settle_funding(dec!(0.01)));
    assert_eq!(only_reason(&out), Some("no_mark_price"));
    assert_eq!(fx.sm.funding_index(&m), Decimal::ZERO);
    assert_eq!(fx.sm.stats().funding_settlements, 0);
}

// ---- liquidation ----------------------------------------------------------

/// Liquidation executed against the resting book (no insurance residual).
#[test]
fn liquidation_fills_against_the_book_at_the_bankruptcy_floor() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(20)));
    fx.apply(&m, 3, transfer(2, dec!(200)));
    fx.apply(&m, 4, tick(dec!(100)));

    // sub1 bids 1 @ 100; sub2's ask crosses it ⇒ sub1 long, sub2 short.
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(1), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    fx.apply(
        &m,
        6,
        place(Uuid::from_u128(2), Side::Ask, Some(dec!(100)), dec!(1), Some(2), false),
    );
    // sub0 rests a bid at 94 (deep below mark but above the bankruptcy floor).
    fx.apply(
        &m,
        7,
        place(Uuid::from_u128(3), Side::Bid, Some(dec!(94)), dec!(1), Some(0), false),
    );

    // Crash: sub1 equity = −80 + 84 = 4 < mm 4.2 ⇒ flagged.
    let out = fx.apply(&m, 8, tick(dec!(84)));
    assert!(has_margin_flagged(&out, 1));
    assert_eq!(fx.sm.pending_liquidations(), vec![1]);

    let out = fx.apply(&m, 9, liquidate(1, None));
    let (quantity, book_filled, insurance_filled, limit) =
        find_liquidated(&out).expect("Liquidated output");
    assert_eq!(quantity, dec!(1));
    assert_eq!(book_filled, dec!(1), "closed entirely against the book");
    assert_eq!(insurance_filled, Decimal::ZERO);
    // Bankruptcy floor: mark + (fee − equity)/qty = 84 + (0.042 − 4) = 80.042.
    assert_eq!(limit, dec!(80.042));

    assert_eq!(fx.sm.position(1, &m), Decimal::ZERO, "position fully closed");
    assert_eq!(fx.sm.collateral(1), dec!(13.953)); // −80 + 94 − 0.047
    assert_eq!(fx.sm.position(0, &m), dec!(1));
    assert_eq!(fx.sm.collateral(0), dec!(9_906));
    // Insurance: taker fee from the original fill (0.05) + liquidation fill
    // taker fee (0.047), no residual position.
    assert_eq!(fx.sm.collateral(INSURANCE_SUBACCOUNT), dec!(0.097));
    assert_eq!(fx.sm.equity_of(INSURANCE_SUBACCOUNT), dec!(0.097));
    assert!(fx.sm.pending_liquidations().is_empty());
    assert_eq!(fx.sm.stats().liquidations, 1);
    assert_eq!(fx.sm.stats().adl_events, 0);
}

/// No resting bids: the insurance fund takes the residual at the bankruptcy
/// price, immediately drops below zero equity, and ADL restores it.
#[test]
fn insurance_residual_triggers_adl_that_restores_the_fund() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(1, dec!(20)));
    fx.apply(&m, 2, transfer(2, dec!(200)));
    fx.apply(&m, 3, tick(dec!(100)));

    // sub1 bids, sub2's ask crosses ⇒ sub1 long 1 @ 100, sub2 short 1.
    fx.apply(
        &m,
        4,
        place(Uuid::from_u128(1), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(2), Side::Ask, Some(dec!(100)), dec!(1), Some(2), false),
    );

    // Crash to 40: sub1 equity = −80.05 + 40 = −40.05 ⇒ flagged.
    let out = fx.apply(&m, 6, tick(dec!(40)));
    assert!(has_margin_flagged(&out, 1));

    let out = fx.apply(&m, 7, liquidate(1, None));
    let (quantity, book_filled, insurance_filled, limit) =
        find_liquidated(&out).expect("Liquidated output");
    assert_eq!(quantity, dec!(1));
    assert_eq!(book_filled, Decimal::ZERO, "empty book ⇒ all to insurance");
    assert_eq!(insurance_filled, dec!(1));
    // sub1 was maker on its fill (cash −80); equity −80 + 40 = −40, fee 0.02
    // ⇒ floor 40 + (0.02 − (−40)) = 80.02.
    assert_eq!(limit, dec!(80.02));

    // The insurance ledger is insolvent after absorbing the residual ⇒ ADL
    // closes its long against sub2 (the largest short) at 79.97.
    let (adl_sub, adl_qty, adl_price) = find_adl(&out).expect("Adl output");
    assert_eq!(adl_sub, 2);
    assert_eq!(adl_qty, dec!(1));
    assert_eq!(adl_price, dec!(79.97));

    assert_eq!(fx.sm.position(1, &m), Decimal::ZERO);
    assert_eq!(fx.sm.collateral(1), dec!(0.02)); // −80.05 + 80.07
    assert_eq!(fx.sm.position(2, &m), Decimal::ZERO);
    assert_eq!(fx.sm.collateral(2), dec!(219.98)); // 299.95 − 79.97
    assert_eq!(fx.sm.collateral(INSURANCE_SUBACCOUNT), Decimal::ZERO);
    assert!(fx.sm.equity_of(INSURANCE_SUBACCOUNT) >= Decimal::ZERO);
    assert!(fx.sm.pending_liquidations().is_empty());
    assert_eq!(fx.sm.stats().liquidations, 1);
    assert_eq!(fx.sm.stats().adl_events, 1);
}

#[test]
fn liquidating_a_flat_or_healthy_subaccount_is_rejected() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(500)));
    fx.apply(&m, 3, tick(dec!(100)));

    // No position.
    let out = fx.apply(&m, 4, liquidate(1, None));
    assert_eq!(only_reason(&out), Some("no_position"));

    // Healthy position (plenty of margin).
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(1), Some(0), false),
    );
    fx.apply(
        &m,
        6,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    let out = fx.apply(&m, 7, liquidate(1, None));
    assert_eq!(only_reason(&out), Some("healthy_subaccount"));
    assert_eq!(fx.sm.position(1, &m), dec!(1));
    assert_eq!(fx.sm.stats().liquidations, 0);
}

#[test]
fn liquidation_cascade_breaker_rejects_within_the_window() {
    let cfg = PerpsConfig {
        liquidation_window_ms: 60_000,
        max_liquidation_notional_per_window: dec!(40),
        ..PerpsConfig::default()
    };
    let mut fx = Fixture::with_state(PerpsState::with_config(cfg));
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, transfer(1, dec!(20)));
    fx.apply(&m, 3, transfer(3, dec!(20)));
    fx.apply(&m, 4, tick(dec!(100)));

    // sub0 asks 2 @ 100; sub1 and sub3 buy one each.
    fx.apply(
        &m,
        5,
        place(Uuid::from_u128(1), Side::Ask, Some(dec!(100)), dec!(2), Some(0), false),
    );
    fx.apply(
        &m,
        6,
        place(Uuid::from_u128(2), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
    );
    fx.apply(
        &m,
        7,
        place(Uuid::from_u128(3), Side::Bid, Some(dec!(100)), dec!(1), Some(3), false),
    );
    // Crash: both buyers are underwater (equity −40.05 < mm 2).
    let out = fx.apply(&m, 8, tick(dec!(40)));
    assert!(has_margin_flagged(&out, 1));
    assert!(has_margin_flagged(&out, 3));
    let mut pending = fx.sm.pending_liquidations();
    pending.sort_unstable();
    assert_eq!(pending, vec![1, 3]);

    // First liquidation: allowed, records 1 × 40 = 40 notional in the window.
    let out = fx.apply(&m, 9, liquidate(1, None));
    assert!(find_liquidated(&out).is_some());
    assert_eq!(fx.sm.stats().liquidations, 1);

    // Second within the window: breaker rejects it.
    let out = fx.apply(&m, 10, liquidate(3, None));
    assert_eq!(only_reason(&out), Some("liquidation_cascade"));
    assert_eq!(fx.sm.stats().liquidations, 1, "second liquidation did not run");
    assert_eq!(fx.sm.position(3, &m), dec!(1), "position untouched");
    assert_eq!(fx.sm.pending_liquidations(), vec![3], "still queued");
}

// ---- legacy entries & sequences ------------------------------------------

#[test]
fn legacy_fill_entries_advance_the_book_but_not_subaccounts() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(0, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));
    let id = Uuid::from_u128(40);
    fx.apply(
        &m,
        3,
        place(id, Side::Bid, Some(dec!(99)), dec!(1), Some(0), false),
    );

    let out = fx.apply(
        &m,
        4,
        EntryPayload::Fill(FillCmd {
            order_id: id,
            price: dec!(99),
            quantity: dec!(1),
            fee: Decimal::ZERO,
            liquidity: FillLiquidity::Maker,
        }),
    );
    // The CLOB consumes the fill; subaccount cash/positions do not move
    // (the external counterparty is unknown — documented Stage-1 legacy path).
    assert_eq!(
        fx.sm.clob().order(id).unwrap().status,
        OrderStatus::Filled
    );
    assert_eq!(fx.sm.position(0, &m), Decimal::ZERO);
    assert_eq!(fx.sm.collateral(0), dec!(10_000));
    assert!(!out.iter().any(|o| matches!(o, ApplyOutput::Fill { .. })));
}

#[test]
fn sequence_gaps_are_errors_while_command_rejections_are_ok() {
    let mut sm = PerpsState::new();
    let m = btc();

    // Global gap.
    let gap = LogEntry {
        global_seq: 2,
        market_seq: 1,
        market: m.clone(),
        ts_ms: 1,
        payload: transfer(0, dec!(100)),
    };
    assert!(matches!(
        sm.apply(&gap),
        Err(ApplyError::GlobalSeqGap { expected: 1, got: 2 })
    ));

    // Market gap (global ok).
    let mgap = LogEntry {
        global_seq: 1,
        market_seq: 7,
        market: m.clone(),
        ts_ms: 1,
        payload: transfer(0, dec!(100)),
    };
    assert!(matches!(
        sm.apply(&mgap),
        Err(ApplyError::MarketSeqGap { .. })
    ));

    // A well-sequenced but rejected command is an Ok path.
    let ok = LogEntry {
        global_seq: 1,
        market_seq: 1,
        market: m.clone(),
        ts_ms: 1,
        payload: transfer(0, dec!(100)),
    };
    let out = sm.apply(&ok).expect("rejections replay as Ok");
    assert_eq!(only_reason(&out), None); // successful transfer
    let bad = LogEntry {
        global_seq: 2,
        market_seq: 2,
        market: m.clone(),
        ts_ms: 2,
        payload: transfer(0, dec!(-1_000)),
    };
    let out = sm.apply(&bad).expect("overdraft is an Ok rejection");
    assert_eq!(only_reason(&out), Some("insufficient_collateral"));
    assert_eq!(sm.last_global_seq(), 2);
}

// ---- determinism / replay -------------------------------------------------

/// A log exercising transfers, fills, reduce-only, funding, liquidation + ADL,
/// withdrawals and legacy fills — the basis for the replay contract tests.
fn behavior_log() -> Vec<LogEntry> {
    let mut lb = LogBuilder::new();
    let m = btc();
    vec![
        lb.next(&m, 1, transfer(0, dec!(10_000))),
        lb.next(&m, 2, transfer(1, dec!(20))),
        lb.next(&m, 3, transfer(2, dec!(200))),
        lb.next(&m, 4, tick(dec!(100))),
        // sub1 long 1 vs sub2 short 1.
        lb.next(
            &m,
            5,
            place(Uuid::from_u128(1), Side::Bid, Some(dec!(100)), dec!(1), Some(1), false),
        ),
        lb.next(
            &m,
            6,
            place(Uuid::from_u128(2), Side::Ask, Some(dec!(100)), dec!(1), Some(2), false),
        ),
        // Reduce-only ask resting above market for sub1.
        lb.next(
            &m,
            7,
            place(Uuid::from_u128(3), Side::Ask, Some(dec!(110)), dec!(1), Some(1), true),
        ),
        // Crash ⇒ flag, liquidate, insurance residual, ADL, RO auto-cancel.
        lb.next(&m, 8, tick(dec!(40))),
        lb.next(&m, 9, liquidate(1, None)),
        // Funding with everyone flat (still a valid settlement).
        lb.next(&m, 10, settle_funding(dec!(0.01))),
        // Valid withdrawal from sub2's post-ADL balance.
        lb.next(&m, 11, transfer(2, dec!(-100))),
        // A resting order that a legacy fill completes.
        lb.next(
            &m,
            12,
            place(Uuid::from_u128(4), Side::Bid, Some(dec!(39)), dec!(1), Some(0), false),
        ),
        lb.next(
            &m,
            13,
            EntryPayload::Fill(FillCmd {
                order_id: Uuid::from_u128(4),
                price: dec!(39),
                quantity: dec!(1),
                fee: Decimal::ZERO,
                liquidity: FillLiquidity::Maker,
            }),
        ),
        // Place + cancel roundtrip.
        lb.next(
            &m,
            14,
            place(Uuid::from_u128(5), Side::Bid, Some(dec!(30)), dec!(2), Some(0), false),
        ),
        lb.next(
            &m,
            15,
            EntryPayload::CancelOrder {
                order_id: Uuid::from_u128(5),
            },
        ),
    ]
}

fn replay_hash(entries: &[LogEntry]) -> (StateHash, Vec<Vec<ApplyOutput>>) {
    let mut sm = PerpsState::new();
    let mut all = Vec::new();
    for e in entries {
        all.push(sm.apply(e).unwrap());
        sm.check_invariants().unwrap();
    }
    (sm.state_hash(), all)
}

#[test]
fn same_log_applied_twice_yields_identical_hashes_and_outputs() {
    let entries = behavior_log();
    let (h1, o1) = replay_hash(&entries);
    let (h2, o2) = replay_hash(&entries);
    assert_eq!(h1, h2);
    assert_eq!(o1, o2);
    assert!(!o1.is_empty());

    // Sanity: the log actually exercised the interesting machinery.
    let flat: Vec<&ApplyOutput> = o1.iter().flatten().collect();
    assert!(flat.iter().any(|o| matches!(o, ApplyOutput::Fill { .. })));
    assert!(flat.iter().any(|o| matches!(o, ApplyOutput::Liquidated { .. })));
    assert!(flat.iter().any(|o| matches!(o, ApplyOutput::Adl { .. })));
    assert!(flat.iter().any(|o| matches!(
        o,
        ApplyOutput::Cancelled {
            reason: CancelReason::ReduceOnly,
            ..
        }
    )));
}

#[test]
fn empty_log_rebuild_matches_live_hash() {
    let entries = behavior_log();
    let (direct, _) = replay_hash(&entries);

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lq_sequencer::SequencerConfig::new(dir.path());
    cfg.sync_on_append = false;
    cfg.snapshot_every = 0;
    let mut seq = lq_sequencer::Sequencer::open(cfg, PerpsState::new()).unwrap();
    for e in &entries {
        seq.append(e.market.clone(), Some(e.ts_ms), e.payload.clone())
            .unwrap();
    }
    let live = seq.state_hash();
    drop(seq);

    let (rebuilt, h) = rebuild_empty_log(dir.path().join("wal.log"), PerpsState::new()).unwrap();
    assert_eq!(h, live);
    assert_eq!(rebuilt.state_hash(), live);
    assert_eq!(direct, live);
}

#[test]
fn snapshot_midway_replay_matches_live() {
    let entries = behavior_log();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lq_sequencer::SequencerConfig::new(dir.path());
    cfg.sync_on_append = false;
    cfg.snapshot_every = 5;
    let mut seq = lq_sequencer::Sequencer::open(cfg, PerpsState::new()).unwrap();
    for e in &entries {
        seq.append(e.market.clone(), Some(e.ts_ms), e.payload.clone())
            .unwrap();
    }
    let live = seq.state_hash();
    drop(seq);

    let (from_snap, h1) = lq_sequencer::rebuild(
        dir.path().join("snapshots"),
        dir.path().join("wal.log"),
        PerpsState::new(),
    )
    .unwrap();
    let (full, h2) = rebuild_empty_log(dir.path().join("wal.log"), PerpsState::new()).unwrap();
    assert_eq!(h1, live);
    assert_eq!(h2, live);
    assert_eq!(from_snap.state_hash(), full.state_hash());
}

#[test]
fn encode_decode_roundtrip_preserves_hash() {
    let entries = behavior_log();
    let mut sm = PerpsState::new();
    for e in &entries {
        sm.apply(e).unwrap();
    }
    let bytes = sm.encode_state().unwrap();
    let back = PerpsState::decode_state(&bytes).unwrap();
    assert_eq!(sm.state_hash(), back.state_hash());
    assert_eq!(sm, back);
}

#[test]
fn default_subaccount_is_used_when_none_is_named() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1, transfer(DEFAULT_SUBACCOUNT, dec!(10_000)));
    fx.apply(&m, 2, tick(dec!(100)));
    fx.apply(
        &m,
        3,
        place(Uuid::from_u128(1), Side::Bid, Some(dec!(99)), dec!(1), None, false),
    );
    // The resting order belongs to subaccount 0.
    assert_eq!(fx.sm.position(DEFAULT_SUBACCOUNT, &m), Decimal::ZERO);
    assert!(fx.sm.clob().order(Uuid::from_u128(1)).is_some());
}
