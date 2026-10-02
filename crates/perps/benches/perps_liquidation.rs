//! Criterion benchmarks for the Stage 3 margin/liquidation paths.
//!
//! Budgets (informational, updated in `docs/stages/STAGE_3_PERPS.md`):
//! - `apply_place_with_margin_check` — pre-trade + match overhead per entry
//! - `liquidate_underwater_position` — full liquidation path (bankruptcy
//!   price → synthetic match → residual vs insurance → flag rebuild)
//! - `state_hash_2000_subaccounts` — canonical hash scaling

use criterion::{criterion_group, criterion_main, Criterion};
use lq_perps::{MarketParams, PerpsState};
use lq_sequencer::entry::{
    EntryPayload, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd,
};
use lq_sequencer::state::StateMachine;
use lq_types::{Exchange, OrderType, Price, Qty, Side, Symbol, TimeInForce};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

fn market() -> MarketId {
    MarketId::new(Exchange::Paper, Symbol("BTC-USD".to_string()))
}

fn entry(seq: u64, market: &MarketId, payload: EntryPayload) -> LogEntry {
    LogEntry {
        global_seq: seq,
        market_seq: seq,
        market: market.clone(),
        ts_ms: seq * 1_000,
        payload,
    }
}

fn place_cmd(side: Side, price: Price, qty: Qty, sub: Option<u64>) -> PlaceOrderCmd {
    PlaceOrderCmd {
        order_id: Uuid::from_u128(Uuid::new_v4().as_u128()),
        client_order_id: String::new(),
        side,
        order_type: OrderType::Limit,
        price: Some(price),
        quantity: qty,
        time_in_force: TimeInForce::Gtc,
        owner: String::new(),
        stp: Default::default(),
        expiration_ms: None,
        subaccount: sub,
        reduce_only: false,
    }
}

/// State with one market, marks, and a funded subaccount `0`.
fn funded_state(collateral: Decimal) -> (PerpsState, MarketId, u64) {
    let m = market();
    let mut sm = PerpsState::new().with_market_params(
        &m,
        MarketParams {
            initial_margin_ratio: dec!(0.10),
            maintenance_margin_ratio: dec!(0.05),
            liquidation_fee_bps: Decimal::ZERO,
        },
    );
    let mut seq = 0;
    seq += 1;
    sm.apply(&entry(
        seq,
        &m,
        EntryPayload::Transfer {
            subaccount: 0,
            amount: collateral,
        },
    ))
    .expect("deposit");
    seq += 1;
    sm.apply(&entry(
        seq,
        &m,
        EntryPayload::MarketTick(MarketTickCmd {
            last: dec!(100),
            bid: None,
            ask: None,
        }),
    ))
    .expect("tick");
    (sm, m, seq)
}

fn bench_place(c: &mut Criterion) {
    c.bench_function("apply_place_with_margin_check", |b| {
        b.iter_batched(
            || {
                let (mut sm, m, mut seq) = funded_state(dec!(10_000));
                // Rest a passive bid so later places both check margin and
                // walk a non-empty book.
                seq += 1;
                let bid = place_cmd(Side::Bid, dec!(99), dec!(1), Some(0));
                sm.apply(&entry(
                    seq,
                    &m,
                    EntryPayload::PlaceOrder(bid.clone()),
                ))
                .expect("rest bid");
                (sm, m, seq)
            },
            |(mut sm, m, mut seq)| {
                seq += 1;
                let ask = place_cmd(Side::Ask, dec!(101), dec!(1), Some(0));
                let out = sm
                    .apply(&entry(seq, &m, EntryPayload::PlaceOrder(ask)))
                    .expect("apply");
                (sm, seq, out)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_liquidation(c: &mut Criterion) {
    c.bench_function("liquidate_underwater_position", |b| {
        b.iter_batched(
            || {
                // Sub 1: long 1 @ 100 with thin collateral; mark crashes to 40
                // so equity < maintenance ⇒ liquidatable.
                let (mut sm, m, mut seq) = funded_state(dec!(10_000));
                seq += 1;
                sm.apply(&entry(
                    seq,
                    &m,
                    EntryPayload::Transfer {
                        subaccount: 1,
                        amount: dec!(6),
                    },
                ))
                .expect("deposit sub1");
                seq += 1;
                let buy = place_cmd(Side::Bid, dec!(100), dec!(1), Some(1));
                sm.apply(&entry(seq, &m, EntryPayload::PlaceOrder(buy)))
                    .expect("buy");
                // Counterparty liquidity on the bid side for the liquidation.
                seq += 1;
                let rest_bid = place_cmd(Side::Bid, dec!(45), dec!(1), Some(0));
                sm.apply(&entry(seq, &m, EntryPayload::PlaceOrder(rest_bid)))
                    .expect("rest bid");
                seq += 1;
                sm.apply(&entry(
                    seq,
                    &m,
                    EntryPayload::MarketTick(MarketTickCmd {
                        last: dec!(40),
                        bid: None,
                        ask: None,
                    }),
                ))
                .expect("crash tick");
                (sm, m, seq)
            },
            |(mut sm, m, mut seq)| {
                seq += 1;
                let out = sm
                    .apply(&entry(
                        seq,
                        &m,
                        EntryPayload::Liquidate {
                            subaccount: 1,
                            max_qty: None,
                        },
                    ))
                    .expect("liquidate");
                (sm, seq, out)
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_state_hash(c: &mut Criterion) {
    c.bench_function("state_hash_2000_subaccounts", |b| {
        let mut sm = PerpsState::new();
        let m = market();
        for i in 0..2_000u64 {
            let e = LogEntry {
                global_seq: i + 1,
                market_seq: i + 1,
                market: m.clone(),
                ts_ms: i,
                payload: EntryPayload::Transfer {
                    subaccount: i,
                    amount: Decimal::ONE,
                },
            };
            sm.apply(&e).expect("deposit");
        }
        b.iter(|| sm.state_hash());
    });
}

criterion_group!(benches, bench_place, bench_liquidation, bench_state_hash);
criterion_main!(benches);
