use std::hint::black_box;
use std::time::Instant;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, BenchmarkGroup};
use lq_core::models::{LevelChange, OrderBookDelta, OrderBookLevel, OrderBookSnapshot};
use lq_exchange::spec::InstrumentSpec;
use lq_orderbook::book_impls::{ArrayBackedBook, BTreeMapBook, HashMapSortedVecBook, OrderBookImpl};
use lq_orderbook::book::OrderBook;
use lq_orderbook::analytics::{AnalyticsConfig, MarketStateEngine};
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const VENUE: Exchange = Exchange::Paper;

fn symbol() -> Symbol {
    Symbol("BTC-USDT".into())
}

fn spec() -> InstrumentSpec {
    InstrumentSpec::new(dec!(0.1), dec!(0.01))
}

fn snapshot(levels: usize) -> OrderBookSnapshot {
    let make_levels = |base: f64| {
        (0..levels)
            .map(|i| {
                OrderBookLevel::new(
                    Decimal::from_f64_retain(base + i as f64 * 0.1).unwrap(),
                    Decimal::from_f64_retain(10.0 - (i % 5) as f64 * 0.5).unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    OrderBookSnapshot {
        venue: VENUE,
        symbol: symbol(),
        sequence: 1,
        event_ts: TimestampMs(1),
        exchange_ts: TimestampMs(1),
        bids: make_levels(100.0),
        asks: make_levels(100.0),
    }
}

fn delta(seq: u64, num_changes: usize) -> OrderBookDelta {
    let mut changes = Vec::with_capacity(num_changes);
    for i in 0..num_changes {
        changes.push(LevelChange {
            side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
            price: dec!(100.0) + Decimal::from(i % 20) * dec!(0.1),
            qty: if i % 7 == 0 {
                Decimal::ZERO
            } else {
                Decimal::from_f64_retain(5.0 + (i % 10) as f64 * 0.5).unwrap()
            },
        });
    }
    OrderBookDelta {
        venue: VENUE,
        symbol: symbol(),
        sequence: seq,
        event_ts: TimestampMs(seq),
        exchange_ts: TimestampMs(seq),
        changes,
        clear: false,
    }
}

fn bench_snapshot_apply<B: OrderBookImpl>(group: &mut BenchmarkGroup<criterion::measurement::WallTime>, name: &str, levels: usize) {
    let snap = snapshot(levels);
    group.bench_with_input(BenchmarkId::new(name, levels), &snap, |b, snap| {
        b.iter(|| {
            let mut book = B::new(VENUE, symbol(), spec());
            book.apply_snapshot(snap);
            black_box(book);
        });
    });
}

fn bench_delta_apply<B: OrderBookImpl>(group: &mut BenchmarkGroup<criterion::measurement::WallTime>, name: &str, levels: usize, changes: usize) {
    let snap = snapshot(levels);
    let mut book = B::new(VENUE, symbol(), spec());
    book.apply_snapshot(&snap);
    let d = delta(2, changes);
    group.bench_with_input(
        BenchmarkId::new(format!("{}/{}_changes", name, changes), levels),
        &d,
        |b, d| {
            b.iter(|| {
                black_box(book.apply_delta(d));
            });
        },
    );
}

fn bench_full_cycle<B: OrderBookImpl>(group: &mut BenchmarkGroup<criterion::measurement::WallTime>, name: &str, levels: usize, num_deltas: usize) {
    let snap = snapshot(levels);
    let deltas: Vec<OrderBookDelta> = (2..=num_deltas as u64 + 1).map(|i| delta(i, 4)).collect();

    group.bench_with_input(
        BenchmarkId::new(format!("{}/full_cycle", name), levels),
        &(&snap, &deltas),
        |b, (snap, deltas)| {
            b.iter(|| {
                let mut book = B::new(VENUE, symbol(), spec());
                book.apply_snapshot(snap);
                for d in deltas.iter() {
                    book.apply_delta(d);
                }
                black_box(book);
            });
        },
    );
}

fn bench_analytics_compute(group: &mut BenchmarkGroup<criterion::measurement::WallTime>, name: &str, levels: usize) {
    let snap = snapshot(levels);
    let mut book = OrderBook::new(VENUE, symbol(), spec());
    book.apply_snapshot(&snap);
    let engine = MarketStateEngine::new(VENUE, symbol(), spec(), AnalyticsConfig::default());

    group.bench_with_input(
        BenchmarkId::new(format!("{}/analytics", name), levels),
        &book,
        |b, book| {
            b.iter(|| {
                black_box(engine.compute(book, TimestampMs(3)));
            });
        },
    );
}

fn bench_throughput<B: OrderBookImpl>(group: &mut BenchmarkGroup<criterion::measurement::WallTime>, name: &str) {
    let levels = 100;
    let snap = snapshot(levels);
    let num_ops = 1_000_000;
    let deltas: Vec<OrderBookDelta> = (2..=num_ops + 1).map(|i| delta(i, 2)).collect();

    let bench_name = format!("{}/throughput_1m_ops", name);
    group.bench_function(&bench_name, |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _ in 0..iters {
                let mut book = B::new(VENUE, symbol(), spec());
                book.apply_snapshot(&snap);
                for d in &deltas {
                    book.apply_delta(d);
                }
                black_box(book);
            }
            start.elapsed()
        });
    });
}

pub fn bench_all_implementations(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_comparison");

    for levels in [10, 50, 100, 200, 500, 1000] {
        bench_snapshot_apply::<BTreeMapBook>(&mut group, "BTreeMapBook", levels);
        bench_snapshot_apply::<HashMapSortedVecBook>(&mut group, "HashMapSortedVecBook", levels);
        bench_snapshot_apply::<ArrayBackedBook>(&mut group, "ArrayBackedBook", levels);

        bench_delta_apply::<BTreeMapBook>(&mut group, "BTreeMapBook", levels, 4);
        bench_delta_apply::<HashMapSortedVecBook>(&mut group, "HashMapSortedVecBook", levels, 4);
        bench_delta_apply::<ArrayBackedBook>(&mut group, "ArrayBackedBook", levels, 4);

        bench_full_cycle::<BTreeMapBook>(&mut group, "BTreeMapBook", levels, 100);
        bench_full_cycle::<HashMapSortedVecBook>(&mut group, "HashMapSortedVecBook", levels, 100);
        bench_full_cycle::<ArrayBackedBook>(&mut group, "ArrayBackedBook", levels, 100);

        bench_analytics_compute(&mut group, "OrderBook", levels);
    }

    for levels in [10, 50, 100, 200] {
        for changes in [1, 10, 50] {
            bench_delta_apply::<BTreeMapBook>(&mut group, "BTreeMapBook", levels, changes);
            bench_delta_apply::<HashMapSortedVecBook>(&mut group, "HashMapSortedVecBook", levels, changes);
            bench_delta_apply::<ArrayBackedBook>(&mut group, "ArrayBackedBook", levels, changes);
        }
    }

    bench_throughput::<BTreeMapBook>(&mut group, "BTreeMapBook");
    bench_throughput::<HashMapSortedVecBook>(&mut group, "HashMapSortedVecBook");
    bench_throughput::<ArrayBackedBook>(&mut group, "ArrayBackedBook");

    group.finish();
}

criterion_group!(benches, bench_all_implementations);
criterion_main!(benches);