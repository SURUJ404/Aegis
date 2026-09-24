//! Sequence / apply / replay benchmarks (p50-style via criterion).

use std::path::Path;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use lq_sequencer::{
    EntryPayload, LedgerState, MarketId, MarketTickCmd, PlaceOrderCmd, Sequencer, SequencerConfig,
    StateMachine,
};
use lq_types::{Exchange, OrderType, Side, Symbol, TimeInForce};
use rust_decimal_macros::dec;
use uuid::Uuid;

fn market() -> MarketId {
    MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into()))
}

fn bench_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequencer/append");
    for n in [100u64, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let dir = tempfile::tempdir().unwrap();
                    let mut cfg = SequencerConfig::new(dir.path());
                    cfg.sync_on_append = false;
                    cfg.snapshot_every = 0;
                    let mut seq = Sequencer::open(cfg, LedgerState::new()).unwrap();
                    let start = std::time::Instant::now();
                    for i in 0..n {
                        seq.append(
                            market(),
                            Some(i),
                            EntryPayload::MarketTick(MarketTickCmd {
                                last: dec!(100),
                                bid: Some(dec!(99)),
                                ask: Some(dec!(101)),
                            }),
                        )
                        .unwrap();
                    }
                    total += start.elapsed();
                }
                total
            })
        });
    }
    group.finish();
}

fn bench_replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequencer/replay");
    for n in [1000u64, 5000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            // Prepare a WAL once per sample size outside the timed loop.
            let dir = tempfile::tempdir().unwrap();
            let mut cfg = SequencerConfig::new(dir.path());
            cfg.sync_on_append = false;
            cfg.snapshot_every = 0;
            let mut seq = Sequencer::open(cfg, LedgerState::new()).unwrap();
            for i in 0..n {
                let id = Uuid::from_u128(i as u128 + 1);
                seq.append(
                    market(),
                    Some(i),
                    EntryPayload::PlaceOrder(PlaceOrderCmd {
                        order_id: id,
                        client_order_id: format!("c{i}"),
                        side: Side::Bid,
                        order_type: OrderType::Limit,
                        price: Some(dec!(100)),
                        quantity: dec!(0.1),
                        time_in_force: TimeInForce::Gtc,
                    }),
                )
                .unwrap();
            }
            let wal = dir.path().join("wal.log");
            b.iter(|| {
                let (sm, _hash) =
                    lq_sequencer::rebuild_empty_log(&wal, LedgerState::new()).unwrap();
                assert_eq!(sm.last_global_seq(), n);
            });
        });
    }
    group.finish();
}

fn bench_apply_in_memory(c: &mut Criterion) {
    c.bench_function("sequencer/apply_only", |b| {
        let mut sm = LedgerState::new();
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let entry = lq_sequencer::LogEntry {
                global_seq: seq,
                market_seq: seq,
                market: market(),
                ts_ms: seq,
                payload: EntryPayload::MarketTick(MarketTickCmd {
                    last: dec!(100),
                    bid: None,
                    ask: None,
                }),
            };
            sm.apply(&entry).unwrap();
            std::hint::black_box(sm.state_hash());
        });
    });
}

fn _unused_path_check(_: &Path) {}

criterion_group!(benches, bench_append, bench_replay, bench_apply_in_memory);
criterion_main!(benches);
