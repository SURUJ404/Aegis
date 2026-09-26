//! Stage 2 behavioral tests: TIF, STP, cancel/replace, ST expiry, determinism.

use lq_clob::state::ClobState;
use lq_sequencer::entry::{EntryPayload, LogEntry, MarketId, PlaceOrderCmd, StpPolicy};
use lq_sequencer::rebuild_empty_log;
use lq_sequencer::state::{ApplyOutput, CancelReason, StateMachine};
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
    m: BTreeMapMarket,
}

// Simple per-market sequence tracker.
struct BTreeMapMarket {
    seqs: std::collections::BTreeMap<String, u64>,
}

impl LogBuilder {
    fn new() -> Self {
        Self {
            g: 0,
            m: BTreeMapMarket {
                seqs: std::collections::BTreeMap::new(),
            },
        }
    }

    fn next(&mut self, market: &MarketId, ts_ms: u64, payload: EntryPayload) -> LogEntry {
        self.g += 1;
        let key = market.to_string();
        let e = self.m.seqs.entry(key).or_insert(0);
        *e += 1;
        LogEntry {
            global_seq: self.g,
            market_seq: *e,
            market: market.clone(),
            ts_ms,
            payload,
        }
    }
}

fn place(
    order_id: Uuid,
    side: Side,
    price: Option<Price>,
    qty: Qty,
    tif: TimeInForce,
    ot: OrderType,
) -> EntryPayload {
    place_owned(
        order_id,
        side,
        price,
        qty,
        tif,
        ot,
        "",
        StpPolicy::None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn place_owned(
    order_id: Uuid,
    side: Side,
    price: Option<Price>,
    qty: Qty,
    tif: TimeInForce,
    ot: OrderType,
    owner: &str,
    stp: StpPolicy,
    expiration_ms: Option<u64>,
) -> EntryPayload {
    EntryPayload::PlaceOrder(PlaceOrderCmd {
        order_id,
        client_order_id: format!("c-{order_id}"),
        side,
        order_type: ot,
        price,
        quantity: qty,
        time_in_force: tif,
        owner: owner.to_string(),
        stp,
        expiration_ms,
    })
}

fn apply(
    sm: &mut ClobState,
    lb: &mut LogBuilder,
    market: &MarketId,
    ts: u64,
    p: EntryPayload,
) -> Vec<ApplyOutput> {
    let e = lb.next(market, ts, p);
    sm.apply(&e).unwrap()
}

// ---- price-time priority -------------------------------------------------

#[test]
fn fills_better_price_first_then_fifo_within_level() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();

    // Asks: two at 100 (A first), one at 101.
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let c = Uuid::from_u128(3);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            a,
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            b,
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        3,
        place(
            c,
            Side::Ask,
            Some(dec!(101)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );

    // Marketable bid for 2 @ <=100: fills A then B, not C.
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        4,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(2),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let fills: Vec<_> = out
        .iter()
        .filter_map(|o| match o {
            ApplyOutput::Fill {
                maker_order_id,
                price,
                quantity,
                ..
            } => Some((*maker_order_id, *price, *quantity)),
            _ => None,
        })
        .collect();
    assert_eq!(
        fills,
        vec![(a, dec!(100), dec!(1)), (b, dec!(100), dec!(1))]
    );
    assert_eq!(sm.order(a).unwrap().status, OrderStatus::Filled);
    assert_eq!(sm.order(b).unwrap().status, OrderStatus::Filled);
    assert_eq!(sm.order(c).unwrap().status, OrderStatus::Acknowledged);
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Filled);
    // taker fully filled; nothing rests from taker
    assert_eq!(sm.resting_count(&m), 1); // only c
}

#[test]
fn better_price_wins_over_earlier_time() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let worse = Uuid::from_u128(1); // placed first, worse price
    let better = Uuid::from_u128(2);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            worse,
            Side::Ask,
            Some(dec!(102)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            better,
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        3,
        place(
            t,
            Side::Bid,
            Some(dec!(102)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    match &out[0] {
        ApplyOutput::Fill {
            maker_order_id,
            price,
            ..
        } => {
            assert_eq!(*maker_order_id, better);
            assert_eq!(*price, dec!(100));
        }
        other => panic!("expected fill, got {other:?}"),
    }
}

// ---- TIF -----------------------------------------------------------------

#[test]
fn gtc_uncrossed_rests() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    assert!(matches!(out[0], ApplyOutput::Placed { resting: true, .. }));
    assert_eq!(sm.resting_count(&m), 1);
}

#[test]
fn ioc_partial_fill_cancels_remainder() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    // IOC bid for 2: fills 1, cancels 1.
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(2),
            TimeInForce::Ioc,
            OrderType::Limit,
        ),
    );
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(sm.order(t).unwrap().filled_quantity, dec!(1));
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Cancelled {
            reason: CancelReason::IocRemainder,
            ..
        }
    )));
    assert_eq!(sm.resting_count(&m), 0);
}

#[test]
fn ioc_no_liquidity_cancels_without_fills() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let t = Uuid::from_u128(9);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Ioc,
            OrderType::Limit,
        ),
    );
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(sm.stats().filled, 0);
}

#[test]
fn fok_rejects_atomically_when_insufficient() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let before = sm.state_hash();
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(2),
            TimeInForce::Fok,
            OrderType::Limit,
        ),
    );
    // Rejected; book untouched.
    assert!(matches!(
        out.iter()
            .find(|o| matches!(o, ApplyOutput::Rejected { .. })),
        Some(ApplyOutput::Rejected {
            reason: "fook_unfilled",
            ..
        })
    ));
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Rejected);
    assert_eq!(sm.stats().filled, 0);
    assert_eq!(sm.resting_count(&m), 1);
    // Maker untouched: filled qty still 0.
    assert_eq!(
        sm.order(Uuid::from_u128(1)).unwrap().filled_quantity,
        dec!(0)
    );
    let _ = before;
}

#[test]
fn fok_fills_entirely_when_liquidity_sufficient() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(2),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let t = Uuid::from_u128(9);
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(2),
            TimeInForce::Fok,
            OrderType::Limit,
        ),
    );
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Filled);
    assert_eq!(
        sm.order(Uuid::from_u128(1)).unwrap().status,
        OrderStatus::Filled
    );
    assert_eq!(sm.resting_count(&m), 0);
}

#[test]
fn post_only_rejects_when_it_would_cross() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            t,
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::PostOnly,
            OrderType::Limit,
        ),
    );
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Rejected {
            reason: "post_only_cross",
            ..
        }
    )));
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Rejected);
    assert_eq!(sm.stats().filled, 0);
    assert_eq!(sm.resting_count(&m), 1);
}

#[test]
fn post_only_rests_when_away_from_touch() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let t = Uuid::from_u128(9);
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            t,
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::PostOnly,
            OrderType::Limit,
        ),
    );
    assert!(matches!(out[0], ApplyOutput::Placed { resting: true, .. }));
    assert_eq!(sm.resting_count(&m), 2);
}

#[test]
fn market_order_sweeps_and_cancels_remainder() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            Uuid::from_u128(2),
            Side::Ask,
            Some(dec!(101)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let t = Uuid::from_u128(9);
    apply(
        &mut sm,
        &mut lb,
        &m,
        3,
        place(
            t,
            Side::Bid,
            None,
            dec!(3),
            TimeInForce::Gtc,
            OrderType::Market,
        ),
    );
    // Filled 2, remainder cancelled; prices were maker prices.
    assert_eq!(sm.order(t).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(sm.order(t).unwrap().filled_quantity, dec!(2));
    assert_eq!(sm.stats().filled, 2);
    // net = taker(bid +2) + makers(ask -1, -1) = 0
    assert_eq!(sm.net_position(&m), Decimal::ZERO);
}

// ---- self-trade prevention -----------------------------------------------

#[test]
fn stp_cancel_resting_cancels_maker_and_taker_continues() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    // Same owner resting ask.
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place_owned(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelResting,
            None,
        ),
    );
    // Second maker, different owner, same price (behind in FIFO).
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place_owned(
            Uuid::from_u128(2),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "other",
            StpPolicy::None,
            None,
        ),
    );
    // Taker from same owner: first maker (self) is cancelled, taker fills "other".
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        3,
        place_owned(
            Uuid::from_u128(9),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelResting,
            None,
        ),
    );
    assert_eq!(
        sm.order(Uuid::from_u128(1)).unwrap().status,
        OrderStatus::Cancelled
    );
    assert_eq!(
        sm.order(Uuid::from_u128(2)).unwrap().status,
        OrderStatus::Filled
    );
    assert_eq!(
        sm.order(Uuid::from_u128(9)).unwrap().status,
        OrderStatus::Filled
    );
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Cancelled { order_id, reason: CancelReason::SelfTradeResting, .. }
            if *order_id == Uuid::from_u128(1)
    )));
}

#[test]
fn stp_cancel_taker_aborts_before_self_fill() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place_owned(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelTaker,
            None,
        ),
    );
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place_owned(
            Uuid::from_u128(9),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelTaker,
            None,
        ),
    );
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Rejected {
            reason: "self_trade",
            ..
        }
    )));
    assert_eq!(
        sm.order(Uuid::from_u128(9)).unwrap().status,
        OrderStatus::Rejected
    );
    assert_eq!(
        sm.order(Uuid::from_u128(1)).unwrap().status,
        OrderStatus::Acknowledged
    );
    assert_eq!(sm.stats().filled, 0);
}

#[test]
fn stp_cancel_both_cancels_resting_and_taker() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place_owned(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelBoth,
            None,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place_owned(
            Uuid::from_u128(9),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::CancelBoth,
            None,
        ),
    );
    assert_eq!(
        sm.order(Uuid::from_u128(1)).unwrap().status,
        OrderStatus::Cancelled
    );
    assert_eq!(
        sm.order(Uuid::from_u128(9)).unwrap().status,
        OrderStatus::Rejected
    );
    assert_eq!(sm.stats().filled, 0);
    assert_eq!(sm.resting_count(&m), 0);
}

#[test]
fn stp_none_allows_self_match() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place_owned(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::None,
            None,
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place_owned(
            Uuid::from_u128(9),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "acct",
            StpPolicy::None,
            None,
        ),
    );
    assert_eq!(
        sm.order(Uuid::from_u128(9)).unwrap().status,
        OrderStatus::Filled
    );
}

// ---- cancel / replace -----------------------------------------------------

#[test]
fn cancel_removes_resting_order() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let id = Uuid::from_u128(1);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            id,
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        EntryPayload::CancelOrder { order_id: id },
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Cancelled {
            reason: CancelReason::User,
            ..
        }
    ));
    assert_eq!(sm.order(id).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(sm.resting_count(&m), 0);
}

#[test]
fn cancel_unknown_order_is_rejected_output_not_error() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        EntryPayload::CancelOrder {
            order_id: Uuid::from_u128(42),
        },
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "unknown_order",
            ..
        }
    ));
    // Seq advanced — replay-safe.
    assert_eq!(sm.last_global_seq(), 1);
}

#[test]
fn replace_cancels_old_and_places_new_atomically() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let old = Uuid::from_u128(1);
    let new = Uuid::from_u128(2);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            old,
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        EntryPayload::ReplaceOrder {
            old_order_id: old,
            new: Box::new(PlaceOrderCmd {
                order_id: new,
                client_order_id: "c-new".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(100)),
                quantity: dec!(2),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        },
    );
    assert_eq!(sm.order(old).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(sm.order(new).unwrap().status, OrderStatus::Acknowledged);
    assert_eq!(sm.resting_count(&m), 1);
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Cancelled {
            reason: CancelReason::Replace,
            ..
        }
    )));
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Placed { order_id, resting: true, .. } if *order_id == new
    )));
}

#[test]
fn replace_with_unknown_old_rejects_new_without_mutation() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        EntryPayload::ReplaceOrder {
            old_order_id: Uuid::from_u128(42),
            new: Box::new(PlaceOrderCmd {
                order_id: Uuid::from_u128(2),
                client_order_id: "c-new".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(100)),
                quantity: dec!(2),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        },
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "replace_old_order_unavailable",
            ..
        }
    ));
    assert_eq!(sm.order(Uuid::from_u128(2)), None);
    assert_eq!(sm.resting_count(&m), 0);
}

// ---- ST expiry vs stateful ------------------------------------------------

#[test]
fn short_term_order_expires_on_logical_time() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let st = Uuid::from_u128(1);
    let stateful = Uuid::from_u128(2);
    // ST expires at ts 1000; stateful never.
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place_owned(
            st,
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "",
            StpPolicy::None,
            Some(1000),
        ),
    );
    apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place_owned(
            stateful,
            Side::Bid,
            Some(dec!(98)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "",
            StpPolicy::None,
            None,
        ),
    );
    assert_eq!(sm.resting_count(&m), 2);

    // Next entry at ts 1000 triggers the sweep (global time).
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        1000,
        EntryPayload::MarketTick(lq_sequencer::entry::MarketTickCmd {
            last: dec!(100),
            bid: None,
            ask: None,
        }),
    );
    assert!(out.iter().any(|o| matches!(
        o,
        ApplyOutput::Expired { order_id, .. } if *order_id == st
    )));
    assert_eq!(sm.order(st).unwrap().status, OrderStatus::Expired);
    assert_eq!(
        sm.order(stateful).unwrap().status,
        OrderStatus::Acknowledged
    );
    assert_eq!(sm.resting_count(&m), 1);
}

#[test]
fn order_arriving_already_expired_is_rejected() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2000,
        place_owned(
            Uuid::from_u128(1),
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "",
            StpPolicy::None,
            Some(1000),
        ),
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "already_expired",
            ..
        }
    ));
    assert_eq!(sm.order(Uuid::from_u128(1)), None);
}

// ---- validation -----------------------------------------------------------

#[test]
fn duplicate_order_id_rejected_without_mutation() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let id = Uuid::from_u128(1);
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            id,
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            id,
            Side::Bid,
            Some(dec!(98)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "duplicate_order",
            ..
        }
    ));
    // Original still resting at 99.
    assert_eq!(sm.order(id).unwrap().price, Some(dec!(99)));
}

#[test]
fn invalid_quantity_and_price_rejected() {
    let mut sm = ClobState::new();
    let mut lb = LogBuilder::new();
    let m = btc();
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Bid,
            Some(dec!(99)),
            Decimal::ZERO,
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "invalid_quantity",
            ..
        }
    ));
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            Uuid::from_u128(2),
            Side::Bid,
            None,
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    assert!(matches!(
        out[0],
        ApplyOutput::Rejected {
            reason: "invalid_price",
            ..
        }
    ));
    assert_eq!(sm.order(Uuid::from_u128(1)), None);
    assert_eq!(sm.order(Uuid::from_u128(2)), None);
}

#[test]
fn seq_gap_is_the_only_error() {
    let mut sm = ClobState::new();
    let e = LogEntry {
        global_seq: 5,
        market_seq: 1,
        market: btc(),
        ts_ms: 0,
        payload: place(
            Uuid::from_u128(1),
            Side::Bid,
            Some(dec!(99)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    };
    assert!(sm.apply(&e).is_err());
}

// ---- determinism / replay -------------------------------------------------

/// The full behavior log used by the determinism tests.
fn behavior_log() -> Vec<LogEntry> {
    let mut lb = LogBuilder::new();
    let m = btc();
    let mut entries: Vec<LogEntry> = Vec::new();

    // Resting liquidity.
    for i in 0..10u128 {
        entries.push(lb.next(
            &m,
            100 + i as u64,
            place(
                Uuid::from_u128(i + 1),
                Side::Ask,
                Some(dec!(100) + Decimal::from(i as i64)),
                dec!(1),
                TimeInForce::Gtc,
                OrderType::Limit,
            ),
        ));
    }
    // Marketable IOC sweeps 3.
    entries.push(lb.next(
        &m,
        200,
        place(
            Uuid::from_u128(100),
            Side::Bid,
            Some(dec!(105)),
            dec!(3),
            TimeInForce::Ioc,
            OrderType::Limit,
        ),
    ));
    // GTC bid rests.
    entries.push(lb.next(
        &m,
        201,
        place(
            Uuid::from_u128(101),
            Side::Bid,
            Some(dec!(99)),
            dec!(2),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    ));
    // Cancel one ask.
    entries.push(lb.next(
        &m,
        202,
        EntryPayload::CancelOrder {
            order_id: Uuid::from_u128(10),
        },
    ));
    // FOK fail.
    entries.push(lb.next(
        &m,
        203,
        place(
            Uuid::from_u128(102),
            Side::Bid,
            Some(dec!(100)),
            dec!(50),
            TimeInForce::Fok,
            OrderType::Limit,
        ),
    ));
    // Replace the GTC bid.
    entries.push(lb.next(
        &m,
        204,
        EntryPayload::ReplaceOrder {
            old_order_id: Uuid::from_u128(101),
            new: Box::new(PlaceOrderCmd {
                order_id: Uuid::from_u128(103),
                client_order_id: "r".into(),
                side: Side::Bid,
                order_type: OrderType::Limit,
                price: Some(dec!(98)),
                quantity: dec!(1),
                time_in_force: TimeInForce::Gtc,
                ..Default::default()
            }),
        },
    ));
    // ST order then expiry.
    entries.push(lb.next(
        &m,
        205,
        place_owned(
            Uuid::from_u128(104),
            Side::Bid,
            Some(dec!(97)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
            "",
            StpPolicy::None,
            Some(500),
        ),
    ));
    entries.push(lb.next(
        &m,
        500,
        EntryPayload::MarketTick(lq_sequencer::entry::MarketTickCmd {
            last: dec!(101),
            bid: None,
            ask: None,
        }),
    ));
    // PostOnly fail + market sweep.
    entries.push(lb.next(
        &m,
        501,
        place(
            Uuid::from_u128(105),
            Side::Bid,
            Some(dec!(200)),
            dec!(1),
            TimeInForce::PostOnly,
            OrderType::Limit,
        ),
    ));
    entries.push(lb.next(
        &m,
        502,
        place(
            Uuid::from_u128(106),
            Side::Bid,
            None,
            dec!(5),
            TimeInForce::Gtc,
            OrderType::Market,
        ),
    ));
    entries
}

fn replay_hash(entries: &[LogEntry]) -> lq_sequencer::hash::StateHash {
    let mut sm = ClobState::new();
    for e in entries {
        sm.apply(e).unwrap();
    }
    sm.state_hash()
}

#[test]
fn same_log_twice_same_hash_and_outputs() {
    let entries = behavior_log();
    let mut sm1 = ClobState::new();
    let mut sm2 = ClobState::new();
    let mut o1 = Vec::new();
    let mut o2 = Vec::new();
    for e in &entries {
        o1.extend(sm1.apply(e).unwrap());
        o2.extend(sm2.apply(e).unwrap());
    }
    assert_eq!(sm1.state_hash(), sm2.state_hash());
    assert_eq!(o1, o2);
    assert!(!o1.is_empty());
}

#[test]
fn empty_log_rebuild_matches_live_hash() {
    let entries = behavior_log();
    let live = replay_hash(&entries);

    // Through a real WAL on disk.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lq_sequencer::SequencerConfig::new(dir.path());
    cfg.sync_on_append = false;
    cfg.snapshot_every = 0;
    let mut seq = lq_sequencer::Sequencer::open(cfg, ClobState::new()).unwrap();
    for e in &entries {
        // Re-append via sequencer (it assigns seqs; ours are already correct
        // and contiguous, so payload/market/ts are what matter).
        seq.append(e.market.clone(), Some(e.ts_ms), e.payload.clone())
            .unwrap();
    }
    let live_seq = seq.state_hash();
    drop(seq);

    let (rebuilt, h) = rebuild_empty_log(dir.path().join("wal.log"), ClobState::new()).unwrap();
    assert_eq!(h, live_seq);
    assert_eq!(rebuilt.state_hash(), live_seq);
    // And direct apply replay agrees too.
    assert_eq!(live, live_seq);
}

#[test]
fn snapshot_midway_replay_matches_clob() {
    let entries = behavior_log();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = lq_sequencer::SequencerConfig::new(dir.path());
    cfg.sync_on_append = false;
    cfg.snapshot_every = 5;
    let mut seq = lq_sequencer::Sequencer::open(cfg, ClobState::new()).unwrap();
    for e in &entries {
        seq.append(e.market.clone(), Some(e.ts_ms), e.payload.clone())
            .unwrap();
    }
    let live = seq.state_hash();
    drop(seq);

    let (from_snap, h1) = lq_sequencer::rebuild(
        dir.path().join("snapshots"),
        dir.path().join("wal.log"),
        ClobState::new(),
    )
    .unwrap();
    let (full, h2) = rebuild_empty_log(dir.path().join("wal.log"), ClobState::new()).unwrap();
    assert_eq!(h1, live);
    assert_eq!(h2, live);
    assert_eq!(from_snap.state_hash(), full.state_hash());
}

#[test]
fn encode_decode_roundtrip_preserves_hash() {
    let entries = behavior_log();
    let mut sm = ClobState::new();
    for e in &entries {
        sm.apply(e).unwrap();
    }
    let bytes = sm.encode_state().unwrap();
    let back = ClobState::decode_state(&bytes).unwrap();
    assert_eq!(sm.state_hash(), back.state_hash());
    assert_eq!(sm, back);
}

// ---- fees / positions -----------------------------------------------------

#[test]
fn fills_charge_configured_fees_and_track_position() {
    let mut sm = ClobState::new(); // taker 5bps, maker 0
    let mut lb = LogBuilder::new();
    let m = btc();
    apply(
        &mut sm,
        &mut lb,
        &m,
        1,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let out = apply(
        &mut sm,
        &mut lb,
        &m,
        2,
        place(
            Uuid::from_u128(9),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            TimeInForce::Gtc,
            OrderType::Limit,
        ),
    );
    let fill = out
        .iter()
        .find_map(|o| match o {
            ApplyOutput::Fill {
                taker_fee,
                maker_fee,
                price,
                quantity,
                ..
            } => Some((*taker_fee, *maker_fee, *price, *quantity)),
            _ => None,
        })
        .expect("fill");
    // notional 100 * 1; taker 5bps = 0.05; maker 0
    assert_eq!(fill.0, dec!(0.05));
    assert_eq!(fill.1, Decimal::ZERO);
    assert_eq!(sm.fees_paid(&m), dec!(0.05));
    // taker bid +1, maker ask -1 => net 0 (single account view)
    assert_eq!(sm.net_position(&m), Decimal::ZERO);
}
