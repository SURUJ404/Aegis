//! Criterion benches for CLOB matching: p50/p99 of the hot paths.

use std::collections::BTreeMap;

use criterion::{black_box, BatchSize, Criterion};
use lq_clob::state::ClobState;
use lq_sequencer::entry::{EntryPayload, LogEntry, MarketId, PlaceOrderCmd};
use lq_sequencer::state::StateMachine;
use lq_types::{Exchange, OrderType, Side, Symbol, TimeInForce};
use rust_decimal::Decimal;
use uuid::Uuid;

fn market() -> MarketId {
    MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into()))
}

fn place_entry(seq: u64, order_id: Uuid, side: Side, price: Decimal, qty: Decimal) -> LogEntry {
    LogEntry {
        global_seq: seq,
        market_seq: seq,
        market: market(),
        ts_ms: seq,
        payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
            order_id,
            client_order_id: format!("c-{seq}"),
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: qty,
            time_in_force: TimeInForce::Gtc,
            ..Default::default()
        }),
    }
}

fn market_entry(seq: u64, order_id: Uuid, side: Side, qty: Decimal) -> LogEntry {
    LogEntry {
        global_seq: seq,
        market_seq: seq,
        market: market(),
        ts_ms: seq,
        payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
            order_id,
            client_order_id: format!("m-{seq}"),
            side,
            order_type: OrderType::Market,
            price: None,
            quantity: qty,
            time_in_force: TimeInForce::Gtc,
            ..Default::default()
        }),
    }
}

/// Build a state with `n` resting asks at ascending prices.
fn setup_asks(n: u64) -> ClobState {
    let mut sm = ClobState::new();
    for i in 0..n {
        let e = place_entry(
            i + 1,
            Uuid::from_u128(i as u128 + 1),
            Side::Ask,
            Decimal::from(100) + Decimal::from(i),
            Decimal::ONE,
        );
        sm.apply(&e).unwrap();
    }
    sm
}

pub fn bench_clob(c: &mut Criterion) {
    let mut group = c.benchmark_group("clob_match");

    group.bench_function("rest_1000_orders", |b| {
        b.iter_batched(
            || 0u64,
            |_| {
                let mut sm = ClobState::new();
                let mut seq = 0u64;
                for i in 0..1000u64 {
                    seq += 1;
                    let e = place_entry(
                        seq,
                        Uuid::from_u128(i as u128 + 1),
                        if i % 2 == 0 { Side::Bid } else { Side::Ask },
                        Decimal::from(100) + Decimal::from((i % 50) as i64),
                        Decimal::ONE,
                    );
                    sm.apply(&e).unwrap();
                }
                black_box(sm.state_hash())
            },
            BatchSize::SmallInput,
        );
    });

    // Sweep: one market order eats 100 resting asks.
    group.bench_function("sweep_100_asks_market", |b| {
        b.iter_batched(
            || (setup_asks(100), 101u64),
            |(mut sm, seq)| {
                let e = market_entry(seq, Uuid::from_u128(999_999), Side::Bid, dec_100());
                sm.apply(&e).unwrap();
                black_box(sm.stats().filled)
            },
            BatchSize::SmallInput,
        );
    });

    // Incremental: single passive fill (rest + one crossing limit).
    group.bench_function("single_passive_fill", |b| {
        b.iter_batched(
            || {
                let mut sm = ClobState::new();
                let maker = place_entry(1, Uuid::from_u128(1), Side::Ask, dec_100(), Decimal::ONE);
                sm.apply(&maker).unwrap();
                (sm, 2u64)
            },
            |(mut sm, seq)| {
                let e = place_entry(seq, Uuid::from_u128(2), Side::Bid, dec_100(), Decimal::ONE);
                sm.apply(&e).unwrap();
                black_box(sm.stats().filled)
            },
            BatchSize::SmallInput,
        );
    });

    // State hash cost over a fat book.
    group.bench_function("state_hash_2000_orders", |b| {
        let mut sm = ClobState::new();
        let mut seq = 0u64;
        for i in 0..2000u64 {
            seq += 1;
            let e = place_entry(
                seq,
                Uuid::from_u128(i as u128 + 1),
                if i % 2 == 0 { Side::Bid } else { Side::Ask },
                Decimal::from(100) + Decimal::from((i % 100) as i64),
                Decimal::ONE,
            );
            sm.apply(&e).unwrap();
        }
        b.iter(|| black_box(sm.state_hash()));
    });

    group.finish();
    let _ = BTreeMap::<u64, u64>::new();
}

fn dec_100() -> Decimal {
    Decimal::from(100)
}

criterion::criterion_group!(benches, bench_clob);
criterion::criterion_main!(benches);
