//! Stage 4 oracle behavioral tests: log-driven price publication through
//! `PerpsState::apply`, the deviation/staleness circuit breakers gating
//! new-risk entries (place / replace / liquidate / settle-funding), oracle
//! precedence in `price_of`, override re-baselines — plus the determinism
//! and replay contract for oracle-bearing logs.
//!
//! All three block invariants are checked after **every** entry via
//! `Fixture::apply`; time is always the entry's logical `ts_ms`.

use std::collections::BTreeMap;

use lq_oracle::OracleParams;
use lq_perps::PerpsState;
use lq_sequencer::entry::{
    EntryPayload, LogEntry, MarketId, MarketTickCmd, OraclePriceCmd, PlaceOrderCmd, StpPolicy,
};
use lq_sequencer::hash::StateHash;
use lq_sequencer::rebuild_empty_log;
use lq_sequencer::state::{ApplyError, ApplyOutput, StateMachine};
use lq_types::{Exchange, OrderType, Price, Qty, Side, Symbol, TimeInForce};
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

fn publish(price: Price, observation_ts_ms: u64, sources: u8) -> EntryPayload {
    publish_over(price, observation_ts_ms, sources, false)
}

fn publish_over(
    price: Price,
    observation_ts_ms: u64,
    sources: u8,
    override_band: bool,
) -> EntryPayload {
    EntryPayload::OraclePrice(OraclePriceCmd {
        price,
        observation_ts_ms,
        sources,
        override_band,
    })
}

fn place(
    id: Uuid,
    side: Side,
    price: Option<Price>,
    qty: Qty,
    subaccount: Option<u64>,
) -> EntryPayload {
    EntryPayload::PlaceOrder(PlaceOrderCmd {
        order_id: id,
        client_order_id: format!("c-{id}"),
        side,
        order_type: OrderType::Limit,
        price,
        quantity: qty,
        time_in_force: TimeInForce::Gtc,
        owner: String::new(),
        stp: StpPolicy::None,
        expiration_ms: None,
        subaccount,
        reduce_only: false,
    })
}

fn cancel(id: Uuid) -> EntryPayload {
    EntryPayload::CancelOrder { order_id: id }
}

fn liquidate(subaccount: u64) -> EntryPayload {
    EntryPayload::Liquidate {
        subaccount,
        max_qty: None,
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

fn published_price(out: &[ApplyOutput]) -> Option<Price> {
    match out {
        [ApplyOutput::OraclePublished { price, .. }] => Some(*price),
        _ => None,
    }
}

// ---- publication and reference price ----

#[test]
fn publication_emits_oracle_published_and_sets_reference_price() {
    let mut fx = Fixture::new();
    let m = btc();

    let out = fx.apply(&m, 1_000, publish(dec!(100), 990, 3));
    assert!(matches!(
        out.as_slice(),
        [ApplyOutput::OraclePublished { market, price, sources, ts_ms }]
        if market == &m && *price == dec!(100) && *sources == 3 && *ts_ms == 1_000
    ));
    assert_eq!(fx.sm.price_of(&m), Some(dec!(100)));
    assert_eq!(fx.sm.oracle().price(&m), Some(dec!(100)));
    assert_eq!(fx.sm.oracle().stats().published, 1);
    assert_eq!(fx.sm.oracle().stats().rejected, 0);
    assert_eq!(fx.sm.stats().oracle_rejected, 0);
}

#[test]
fn oracle_price_takes_precedence_over_tick_marks() {
    let mut fx = Fixture::new();
    let m = btc();

    fx.apply(&m, 1_000, tick(dec!(200)));
    assert_eq!(fx.sm.price_of(&m), Some(dec!(200)), "legacy tick mark");

    fx.apply(&m, 2_000, publish(dec!(150), 1_990, 1));
    assert_eq!(fx.sm.price_of(&m), Some(dec!(150)), "oracle wins");

    fx.apply(&m, 3_000, tick(dec!(180)));
    assert_eq!(
        fx.sm.price_of(&m),
        Some(dec!(150)),
        "oracle stays authoritative"
    );
}

#[test]
fn equity_marks_positions_at_the_oracle_price() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_010, transfer(1, dec!(1_000)));
    fx.apply(&m, 1_020, transfer(2, dec!(200)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));

    // sub2 rests the ask, sub1's bid crosses ⇒ sub1 long 1 @ 100.
    fx.apply(
        &m,
        1_200,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            Some(2),
        ),
    );
    fx.apply(
        &m,
        1_300,
        place(
            Uuid::from_u128(2),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            Some(1),
        ),
    );
    assert_eq!(fx.sm.position(1, &m), dec!(1));
    assert_eq!(fx.sm.equity_of(1), dec!(999.95)); // 1_000 − 100 − taker fee

    // Publish −10 % (exactly the band edge): equity moves by the delta.
    let out = fx.apply(&m, 2_000, publish(dec!(90), 1_990, 1));
    assert_eq!(published_price(&out), Some(dec!(90)));
    assert_eq!(fx.sm.price_of(&m), Some(dec!(90)));
    assert_eq!(fx.sm.equity_of(1), dec!(989.95)); // 899.95 + 90
    assert_eq!(fx.sm.equity_of(1), dec!(999.95) - dec!(10));
    assert_eq!(fx.sm.equity_of(2), dec!(210)); // 300 − 90
}

// ---- deviation circuit breaker ----

#[test]
fn deviation_rejects_publication_halts_market_and_blocks_places() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));

    // 100 % move vs the 10 % band: rejected, market halted, price frozen.
    let out = fx.apply(&m, 1_200, publish(dec!(200), 1_190, 1));
    assert_eq!(only_reason(&out), Some("oracle_deviation"));
    assert!(fx.sm.oracle().is_halted(&m));
    assert_eq!(fx.sm.oracle().stats().halts, 1);
    assert_eq!(fx.sm.oracle().price(&m), Some(dec!(100)));
    assert_eq!(fx.sm.oracle_gate(&m, 1_200), Some("oracle_halted"));

    // New risk is gated: no order reaches the book.
    let id = Uuid::from_u128(10);
    let out = fx.apply(
        &m,
        1_300,
        place(id, Side::Bid, Some(dec!(100)), dec!(1), Some(0)),
    );
    assert_eq!(only_reason(&out), Some("oracle_halted"));
    assert!(fx.sm.clob().order(id).is_none(), "no book mutation on gate");
    assert_eq!(fx.sm.stats().margin_rejected, 0, "margin check never ran");

    // In-band price clears the halt without an override.
    let out = fx.apply(&m, 1_400, publish(dec!(101), 1_390, 1));
    assert_eq!(published_price(&out), Some(dec!(101)));
    assert!(!fx.sm.oracle().is_halted(&m));
    assert_eq!(fx.sm.oracle_gate(&m, 1_400), None);

    // …and placement works again.
    let id2 = Uuid::from_u128(11);
    fx.apply(
        &m,
        1_500,
        place(id2, Side::Bid, Some(dec!(101)), dec!(1), Some(0)),
    );
    assert!(fx.sm.clob().order(id2).is_some());
}

#[test]
fn halt_latches_once_and_counters_track_rejections() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));

    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));
    assert_eq!(fx.sm.stats().oracle_rejected, 0);

    // Publication rejections count on both counters; halts latch once.
    fx.apply(&m, 1_200, publish(dec!(200), 1_190, 1));
    assert_eq!(fx.sm.oracle().stats().rejected, 1);
    assert_eq!(fx.sm.stats().oracle_rejected, 1);

    fx.apply(&m, 1_300, publish(dec!(300), 1_290, 1));
    assert_eq!(fx.sm.oracle().stats().rejected, 2);
    assert_eq!(fx.sm.oracle().stats().halts, 1, "halt transitions only");
    assert_eq!(fx.sm.stats().oracle_rejected, 2);

    // A gated place adds one more gate rejection.
    fx.apply(
        &m,
        1_400,
        place(
            Uuid::from_u128(1),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            Some(0),
        ),
    );
    assert_eq!(fx.sm.stats().oracle_rejected, 3);

    // Clearing + a successful place add nothing to the rejection counters.
    fx.apply(&m, 1_500, publish(dec!(100.5), 1_490, 1));
    fx.apply(
        &m,
        1_600,
        place(
            Uuid::from_u128(2),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            Some(0),
        ),
    );
    assert_eq!(fx.sm.oracle().stats().published, 2);
    assert_eq!(fx.sm.oracle().stats().rejected, 2);
    assert_eq!(fx.sm.stats().oracle_rejected, 3);
}

#[test]
fn override_publication_rebaselines_halted_market() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));
    fx.apply(&m, 1_200, publish(dec!(200), 1_190, 1)); // halted
    assert!(fx.sm.oracle().is_halted(&m));

    // Operator-acknowledged outlier: accepted, latch cleared.
    let out = fx.apply(&m, 1_300, publish_over(dec!(180), 1_290, 1, true));
    assert_eq!(published_price(&out), Some(dec!(180)));
    assert!(!fx.sm.oracle().is_halted(&m));
    assert_eq!(fx.sm.oracle_gate(&m, 1_300), None);
    assert_eq!(fx.sm.oracle().price(&m), Some(dec!(180)));

    // 180 is the new baseline: the next 11 % move trips the breaker again.
    let out = fx.apply(&m, 1_400, publish(dec!(200), 1_390, 1));
    assert_eq!(only_reason(&out), Some("oracle_deviation"));
    assert!(fx.sm.oracle().is_halted(&m));
    assert_eq!(fx.sm.oracle().stats().halts, 2);
}

#[test]
fn blocked_entries_allowed_while_halted_are_cancel_and_transfer() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_010, transfer(1, dec!(1_000)));
    fx.apply(&m, 1_020, transfer(2, dec!(200)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));

    // Positions and the soon-cancelled ask open while the market is fresh.
    let id = Uuid::from_u128(42);
    fx.apply(
        &m,
        1_200,
        place(id, Side::Ask, Some(dec!(115)), dec!(1), Some(0)),
    );
    fx.apply(
        &m,
        1_300,
        place(
            Uuid::from_u128(43),
            Side::Ask,
            Some(dec!(110)),
            dec!(1),
            Some(2),
        ),
    );
    fx.apply(
        &m,
        1_400,
        place(
            Uuid::from_u128(44),
            Side::Bid,
            Some(dec!(110)),
            dec!(1),
            Some(1),
        ),
    );
    assert_eq!(fx.sm.position(1, &m), dec!(1), "sub1 long via sub2's ask");

    fx.apply(&m, 1_500, publish(dec!(500), 1_490, 1)); // halt
    assert!(fx.sm.oracle().is_halted(&m));

    // Risk-reducing / unrelated entries still pass the gate.
    let out = fx.apply(&m, 1_600, cancel(id));
    assert!(matches!(
        out.as_slice(),
        [ApplyOutput::Cancelled { order_id, .. }] if *order_id == id
    ));
    assert!(
        fx.sm
            .clob()
            .order(id)
            .is_some_and(|o| o.status.is_terminal()),
        "cancel marks the resting ask terminal"
    );

    let out = fx.apply(&m, 1_700, transfer(0, dec!(-1_000)));
    assert!(matches!(out.as_slice(), [ApplyOutput::Transferred { .. }]));

    // Liquidation is new risk on a bad price — still gated.
    let out = fx.apply(&m, 1_800, liquidate(1));
    assert_eq!(only_reason(&out), Some("oracle_halted"));
}

// ---- staleness gate (log-driven) ----

#[test]
fn stale_market_blocks_places_until_a_fresh_publication() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));

    // The publish landed at ts 1_100; 30 s + 1 ms past it: new risk refuses.
    let id = Uuid::from_u128(1);
    let out = fx.apply(
        &m,
        31_201,
        place(id, Side::Bid, Some(dec!(100)), dec!(1), Some(0)),
    );
    assert_eq!(only_reason(&out), Some("oracle_stale"));
    assert!(fx.sm.clob().order(id).is_none());

    // A fresh publication re-baselines the gate clock.
    let out = fx.apply(&m, 31_300, publish(dec!(100), 31_299, 1));
    assert_eq!(published_price(&out), Some(dec!(100)));

    let id2 = Uuid::from_u128(2);
    fx.apply(
        &m,
        31_400,
        place(id2, Side::Bid, Some(dec!(100)), dec!(1), Some(0)),
    );
    assert!(
        fx.sm.clob().order(id2).is_some(),
        "fresh market accepts risk"
    );
}

#[test]
fn liquidation_is_gated_by_staleness_and_halt() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_010, transfer(1, dec!(1_000)));
    fx.apply(&m, 1_020, transfer(2, dec!(200)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));
    fx.apply(
        &m,
        1_200,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            Some(2),
        ),
    );
    fx.apply(
        &m,
        1_300,
        place(
            Uuid::from_u128(2),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            Some(1),
        ),
    );
    assert_eq!(fx.sm.position(1, &m), dec!(1));

    // Stale market (publish at 1_100): gate refuses before health runs.
    let out = fx.apply(&m, 31_201, liquidate(1));
    assert_eq!(only_reason(&out), Some("oracle_stale"));

    // Fresh again: the gate passes and the health check answers instead.
    fx.apply(&m, 31_300, publish(dec!(100), 31_299, 1));
    let out = fx.apply(&m, 31_310, liquidate(1));
    assert_eq!(
        only_reason(&out),
        Some("healthy_subaccount"),
        "fresh gate falls through to the health check"
    );

    // Halted: gate refuses again, before health.
    fx.apply(&m, 31_320, publish(dec!(200), 31_319, 1));
    let out = fx.apply(&m, 31_330, liquidate(1));
    assert_eq!(only_reason(&out), Some("oracle_halted"));

    // In-band clear restores the normal path.
    fx.apply(&m, 31_340, publish(dec!(100.5), 31_339, 1));
    let out = fx.apply(&m, 31_350, liquidate(1));
    assert_eq!(only_reason(&out), Some("healthy_subaccount"));
}

#[test]
fn settle_funding_is_gated_by_staleness() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, transfer(0, dec!(10_000)));
    fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));

    let out = fx.apply(&m, 1_200, settle_funding(dec!(0.001)));
    assert!(
        matches!(out.as_slice(), [ApplyOutput::FundingSettled { .. }]),
        "fresh market settles funding"
    );

    let out = fx.apply(&m, 31_201, settle_funding(dec!(0.001)));
    assert_eq!(only_reason(&out), Some("oracle_stale"));

    fx.apply(&m, 31_300, publish(dec!(100), 31_299, 1));
    let out = fx.apply(&m, 31_310, settle_funding(dec!(0.001)));
    assert!(matches!(
        out.as_slice(),
        [ApplyOutput::FundingSettled { .. }]
    ));
}

// ---- validation rejections through the state machine ----

#[test]
fn insufficient_sources_rejected_at_state_level() {
    let params = OracleParams {
        min_sources: 3,
        ..OracleParams::default()
    };
    let mut fx = Fixture::with_state(PerpsState::new().with_oracle_params(params));
    let m = btc();

    let out = fx.apply(&m, 1_000, publish(dec!(100), 990, 2));
    assert_eq!(only_reason(&out), Some("insufficient_sources"));
    assert!(!fx.sm.oracle().covered(&m), "no price without quorum");

    let out = fx.apply(&m, 1_100, publish(dec!(100), 1_090, 3));
    assert_eq!(published_price(&out), Some(dec!(100)));
}

#[test]
fn stale_observation_rejected_at_state_level() {
    let mut fx = Fixture::new();
    let m = btc();

    let out = fx.apply(&m, 40_000, publish(dec!(100), 0, 1));
    assert_eq!(only_reason(&out), Some("stale_observation"));
    assert!(!fx.sm.oracle().covered(&m));

    let out = fx.apply(&m, 40_100, publish(dec!(100), 40_099, 1));
    assert_eq!(published_price(&out), Some(dec!(100)));
}

#[test]
fn rejected_publication_consumes_sequence_without_mutation() {
    let mut fx = Fixture::new();
    let m = btc();
    fx.apply(&m, 1_000, publish(dec!(100), 990, 1));

    let out = fx.apply(&m, 1_100, publish(dec!(500), 1_090, 1));
    assert_eq!(only_reason(&out), Some("oracle_deviation"));
    assert_eq!(fx.sm.oracle().price(&m), Some(dec!(100)));
    assert_eq!(fx.sm.oracle().stats().published, 1, "no state mutation");
    assert_eq!(fx.sm.oracle().stats().rejected, 1);

    // The sequence advanced: the next entry applies without a gap error.
    let out = fx.apply(&m, 1_200, transfer(0, dec!(50)));
    assert!(matches!(out.as_slice(), [ApplyOutput::Transferred { .. }]));
    assert_eq!(fx.sm.oracle().price(&m), Some(dec!(100)));
}

#[test]
fn sequence_gap_is_still_the_only_hard_error_for_oracle_entries() {
    let mut sm = PerpsState::new();
    let e = LogEntry {
        global_seq: 5, // fresh state expects 1
        market_seq: 1,
        market: btc(),
        ts_ms: 1_000,
        payload: publish(dec!(100), 990, 1),
    };
    match sm.apply(&e) {
        Err(ApplyError::GlobalSeqGap {
            expected: 1,
            got: 5,
        }) => {}
        other => panic!("expected GlobalSeqGap, got {other:?}"),
    }
}

#[test]
fn oracle_gate_is_exported_for_the_gateway() {
    let mut fx = Fixture::new();
    let m = btc();
    assert_eq!(fx.sm.oracle_gate(&m, 1_000), None, "uncovered passes");

    fx.apply(&m, 1_000, publish(dec!(100), 990, 1));
    assert_eq!(fx.sm.oracle_gate(&m, 1_000), None);
    assert_eq!(fx.sm.oracle_gate(&m, 31_001), Some("oracle_stale"));

    fx.apply(&m, 31_100, publish(dec!(500), 31_099, 1)); // deviation → halt
    assert_eq!(fx.sm.oracle_gate(&m, 31_101), Some("oracle_halted"));
}

#[test]
fn state_hash_reflects_oracle_price_and_publication_count() {
    let mk = |extra: Option<Price>| {
        let mut fx = Fixture::new();
        let m = btc();
        fx.apply(&m, 1_000, transfer(0, dec!(1_000)));
        fx.apply(&m, 1_100, publish(dec!(100), 1_090, 1));
        if let Some(p) = extra {
            fx.apply(&m, 1_200, publish(p, 1_190, 1));
        }
        fx.sm.state_hash()
    };

    let base = mk(None);
    assert_eq!(base, mk(None), "identical logs ⇒ identical hash");
    assert_ne!(
        base,
        mk(Some(dec!(101))),
        "an extra publication must differ"
    );
}

// ---- determinism / replay ----

/// Transfers, crossings, a resting order cancelled under a halt, funding,
/// legacy fills, accepted/rejected/overridden publications, a stale place —
/// the basis for the replay contract tests.
fn oracle_log() -> Vec<LogEntry> {
    let m = market("BTC-USDT");
    let mut lb = LogBuilder::new();
    let mut v = Vec::new();
    let mut push = |lb: &mut LogBuilder, ts: u64, payload: EntryPayload| {
        v.push(lb.next(&m, ts, payload));
    };

    push(&mut lb, 1_000, transfer(0, dec!(10_000)));
    push(&mut lb, 1_010, transfer(1, dec!(1_000)));
    push(&mut lb, 1_020, transfer(2, dec!(200)));
    push(&mut lb, 1_100, publish(dec!(100), 1_090, 1));
    // sub2 rests the ask; sub1 crosses ⇒ sub1 long 1 @ 100.
    push(
        &mut lb,
        1_200,
        place(
            Uuid::from_u128(1),
            Side::Ask,
            Some(dec!(100)),
            dec!(1),
            Some(2),
        ),
    );
    push(
        &mut lb,
        1_300,
        place(
            Uuid::from_u128(2),
            Side::Bid,
            Some(dec!(100)),
            dec!(1),
            Some(1),
        ),
    );
    // sub0 rests an ask that will be cancelled while halted.
    push(
        &mut lb,
        1_400,
        place(
            Uuid::from_u128(3),
            Side::Ask,
            Some(dec!(110)),
            dec!(1),
            Some(0),
        ),
    );
    // Funding settles while fresh (350 ms after the accepted publish).
    push(&mut lb, 1_450, settle_funding(dec!(0.001)));
    // Deviation: rejected + halted.
    push(&mut lb, 1_500, publish(dec!(200), 1_490, 1));
    // New risk gated while halted.
    push(
        &mut lb,
        1_600,
        place(
            Uuid::from_u128(4),
            Side::Bid,
            Some(dec!(110)),
            dec!(1),
            Some(0),
        ),
    );
    // Risk-reducing cancel passes the gate.
    push(&mut lb, 1_700, cancel(Uuid::from_u128(3)));
    // Legacy tick mark — oracle still wins for price_of.
    push(&mut lb, 1_900, tick(dec!(105)));
    // In-band clear.
    push(&mut lb, 2_000, publish(dec!(100.5), 1_990, 1));
    // Second deviation (vs 100.5) + explicit override re-baseline.
    push(&mut lb, 2_100, publish(dec!(130), 2_090, 1));
    push(&mut lb, 2_200, publish_over(dec!(120), 2_190, 1, true));
    // Stale place: 30.9 s after the last accepted publish.
    push(
        &mut lb,
        33_000,
        place(
            Uuid::from_u128(5),
            Side::Bid,
            Some(dec!(120)),
            dec!(1),
            Some(0),
        ),
    );
    // Fresh publish unblocks it (120 → 110 is in band).
    push(&mut lb, 33_100, publish(dec!(110), 33_099, 1));
    push(
        &mut lb,
        33_200,
        place(
            Uuid::from_u128(6),
            Side::Bid,
            Some(dec!(110)),
            dec!(1),
            Some(0),
        ),
    );
    // Legacy fill against the resting bid.
    push(
        &mut lb,
        33_300,
        EntryPayload::Fill(lq_sequencer::entry::FillCmd {
            order_id: Uuid::from_u128(6),
            price: dec!(110),
            quantity: dec!(1),
            fee: Decimal::ZERO,
            liquidity: lq_sequencer::entry::FillLiquidity::Maker,
        }),
    );
    push(&mut lb, 33_400, transfer(1, dec!(-100)));
    v
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
fn same_oracle_log_applied_twice_yields_identical_hashes_and_outputs() {
    let entries = oracle_log();
    let (h1, o1) = replay_hash(&entries);
    let (h2, o2) = replay_hash(&entries);
    assert_eq!(h1, h2);
    assert_eq!(o1, o2);

    let flat: Vec<&ApplyOutput> = o1.iter().flatten().collect();
    assert!(flat
        .iter()
        .any(|o| matches!(o, ApplyOutput::OraclePublished { .. })));
    assert!(flat.iter().any(|o| matches!(
        o,
        ApplyOutput::Rejected {
            reason: "oracle_deviation",
            ..
        }
    )));
    assert!(flat.iter().any(|o| matches!(
        o,
        ApplyOutput::Rejected {
            reason: "oracle_halted",
            ..
        }
    )));
    assert!(flat.iter().any(|o| matches!(
        o,
        ApplyOutput::Rejected {
            reason: "oracle_stale",
            ..
        }
    )));
    assert!(flat.iter().any(|o| matches!(o, ApplyOutput::Fill { .. })));
    assert!(flat
        .iter()
        .any(|o| matches!(o, ApplyOutput::FundingSettled { .. })));
    assert!(flat
        .iter()
        .any(|o| matches!(o, ApplyOutput::Cancelled { .. })));
}

#[test]
fn empty_log_rebuild_matches_live_hash_for_oracle_log() {
    let entries = oracle_log();
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
fn snapshot_midway_replay_matches_live_for_oracle_log() {
    let entries = oracle_log();
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
fn encode_decode_roundtrip_preserves_hash_with_oracle_state() {
    let entries = oracle_log();
    let mut sm = PerpsState::new();
    for e in &entries {
        sm.apply(e).unwrap();
    }
    assert!(
        sm.oracle().covered(&btc()),
        "test premise: oracle price set"
    );

    let bytes = sm.encode_state().expect("encode must handle the price map");
    let back = PerpsState::decode_state(&bytes).expect("decode");
    assert_eq!(back.state_hash(), sm.state_hash());
    assert_eq!(back, sm);
    assert_eq!(back.oracle(), sm.oracle());
    assert_eq!(back.oracle().price(&btc()), sm.oracle().price(&btc()));
}

#[test]
fn snapshot_without_oracle_field_decodes_to_empty_book() {
    // Stage 3 snapshots predate the oracle: the wire field is `serde(default)`.
    let mut sm = PerpsState::new();
    let m = btc();
    sm.apply(&LogEntry {
        global_seq: 1,
        market_seq: 1,
        market: m.clone(),
        ts_ms: 1_000,
        payload: transfer(0, dec!(1_000)),
    })
    .unwrap();
    sm.apply(&LogEntry {
        global_seq: 2,
        market_seq: 2,
        market: m.clone(),
        ts_ms: 1_100,
        payload: tick(dec!(100)),
    })
    .unwrap();

    let bytes = sm.encode_state().unwrap();
    let mut wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    wire.as_object_mut().unwrap().remove("oracle");
    let stripped = serde_json::to_vec(&wire).unwrap();

    let back = PerpsState::decode_state(&stripped).expect("old snapshot decodes");
    assert!(!back.oracle().covered(&m));
    assert_eq!(back.oracle().stats().published, 0);
    assert_eq!(
        back.state_hash(),
        sm.state_hash(),
        "defaults hash identically"
    );
}
