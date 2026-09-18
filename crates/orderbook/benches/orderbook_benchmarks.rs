use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lq_core::event::MarketEvent;
use lq_core::models::{
    LevelChange, OrderBookDelta, OrderBookLevel, OrderBookSnapshot, Trade,
};
use lq_core::EventBus;
use lq_exchange::spec::InstrumentSpec;
use lq_orderbook::analytics::{AnalyticsConfig, MarketStateEngine};
use lq_orderbook::book::OrderBook;
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const VENUE: Exchange = Exchange::Paper;
const SYMBOL: &str = "BTC-USDT";

fn symbol() -> Symbol {
    Symbol(SYMBOL.into())
}

fn spec() -> InstrumentSpec {
    InstrumentSpec::new(dec!(0.1), dec!(0.01))
}

// ---------------------------------------------------------------------------
// Helpers: build snapshots / deltas of varying depth
// ---------------------------------------------------------------------------

/// Generate a symmetric snapshot with `n` levels per side.
/// Bids: 100.0, 99.9, 99.8, ... descending. Asks: 100.1, 100.2, ... ascending.
fn make_snapshot(n: usize, seq: u64) -> OrderBookSnapshot {
    let bids: Vec<OrderBookLevel> = (0..n)
        .map(|i| {
            OrderBookLevel::new(
                dec!(100.0) - Decimal::from(i as u64) * dec!(0.1),
                dec!(10.0) - Decimal::from((i % 5) as u64),
            )
        })
        .collect();
    let asks: Vec<OrderBookLevel> = (0..n)
        .map(|i| {
            OrderBookLevel::new(
                dec!(100.1) + Decimal::from(i as u64) * dec!(0.1),
                dec!(10.0) - Decimal::from((i % 5) as u64),
            )
        })
        .collect();
    OrderBookSnapshot {
        venue: VENUE,
        symbol: symbol(),
        sequence: seq,
        event_ts: TimestampMs(1_000),
        exchange_ts: TimestampMs(1_000),
        bids,
        asks,
    }
}

/// Build a populated book with `n` levels per side.
fn make_book(n: usize) -> OrderBook {
    let mut book = OrderBook::new(VENUE, symbol(), spec());
    book.apply_snapshot(&make_snapshot(n, 1));
    book
}

/// Create a delta that updates an existing bid level (qty 5.0 -> qty 3.0).
fn make_update_delta(seq: u64) -> OrderBookDelta {
    OrderBookDelta {
        venue: VENUE,
        symbol: symbol(),
        sequence: seq,
        event_ts: TimestampMs(1_001),
        exchange_ts: TimestampMs(1_001),
        changes: vec![LevelChange {
            side: Side::Bid,
            price: dec!(100.0),
            qty: dec!(3.0),
        }],
        clear: false,
    }
}

/// Generate N sequential deltas for incremental apply benchmarks.
fn make_delta_chain(start_seq: u64, count: usize) -> Vec<OrderBookDelta> {
    (0..count)
        .map(|i| {
            OrderBookDelta {
                venue: VENUE,
                symbol: symbol(),
                sequence: start_seq + i as u64,
                event_ts: TimestampMs(1_001 + i as u64),
                exchange_ts: TimestampMs(1_001 + i as u64),
                changes: vec![LevelChange {
                    side: if i % 2 == 0 { Side::Bid } else { Side::Ask },
                    price: if i % 2 == 0 {
                        dec!(100.0) - Decimal::from((i / 2) as u64) * dec!(0.1)
                    } else {
                        dec!(100.1) + Decimal::from((i / 2) as u64) * dec!(0.1)
                    },
                    qty: dec!(1.0) + Decimal::from((i % 10) as u64),
                }],
                clear: false,
            }
        })
        .collect()
}

/// Generate synthetic trades for the analytics engine.
fn make_trades(n: usize) -> Vec<Trade> {
    (0..n)
        .map(|i| Trade {
            venue: VENUE,
            symbol: symbol(),
            price: dec!(100.0) + Decimal::from((i % 20) as u64) * dec!(0.1),
            qty: dec!(0.1) + Decimal::from((i % 10) as u64) * dec!(0.1),
            aggressor: if i % 2 == 0 { Side::Bid } else { Side::Ask },
            event_ts: TimestampMs(1_000 + i as u64),
            exchange_ts: TimestampMs(1_000 + i as u64),
        })
        .collect()
}

// ===========================================================================
// Benchmarks
// ===========================================================================

fn bench_snapshot_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/snapshot_apply");
    for &n in &[5, 20, 100] {
        group.throughput(Throughput::Elements(n as u64 * 2));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{n}x{n}")), &n, |b, &n| {
            b.iter_batched(
                || OrderBook::new(VENUE, symbol(), spec()),
                |mut book| {
                    book.apply_snapshot(&make_snapshot(n, 1));
                    black_box(book)
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_delta_apply(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/delta_apply");
    for &n in &[5, 20, 100] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(format!("{n}x{n}")), &n, |b, &n| {
            let book = make_book(n);
            let deltas = make_delta_chain(book.sequence() + 1, 100);
            b.iter_batched(
                || {
                    // Reset book to known state each iteration
                    let mut b = make_book(n);
                    // Apply first 50 deltas to get the book into a warm state
                    for d in &deltas[..50] {
                        let _ = b.apply_delta(d);
                    }
                    (b, 50)
                },
                |(mut book, start_idx)| {
                    // Apply the remaining deltas
                    for d in &deltas[start_idx..start_idx + 10] {
                        black_box(book.apply_delta(d));
                    }
                    black_box(book)
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_best_bid_ask(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/best_bid_ask");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &book, |b, book| {
            b.iter(|| {
                black_box(book.best_bid());
                black_box(book.best_ask());
                black_box(book.mid_price());
                black_box(book.spread());
            });
        });
    }
    group.finish();
}

fn bench_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/depth");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &book, |b, book| {
            b.iter(|| {
                black_box(book.depth(Side::Bid, n));
                black_box(book.depth(Side::Ask, n));
            });
        });
    }
    group.finish();
}

fn bench_imbalance(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/imbalance");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &book, |b, book| {
            b.iter(|| {
                black_box(book.imbalance(n));
            });
        });
    }
    group.finish();
}

fn bench_vwap(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/vwap");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &book, |b, book| {
            b.iter(|| {
                // Ask-side VWAP: buy into the book for a modest notional
                black_box(book.vwap(Side::Ask, dec!(50.0)));
                // Bid-side VWAP
                black_box(book.vwap(Side::Bid, dec!(50.0)));
            });
        });
    }
    group.finish();
}

fn bench_level_insert_delete(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/level_insert_delete");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{n}x{n}")),
            &book,
            |b, _book| {
                b.iter_batched(
                    || make_book(n),
                    |mut book| {
                        // Insert a new level then delete it
                        book.apply_level_change(Side::Bid, dec!(95.0), dec!(7.0));
                        black_box(book.best_bid());
                        book.apply_level_change(Side::Bid, dec!(95.0), dec!(0.0));
                        black_box(book)
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn bench_analytics_compute(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/analytics_compute");
    let cfg = AnalyticsConfig::default();

    for &n in &[5, 20, 100] {
        let book = make_book(n);
        let mut engine = MarketStateEngine::new(VENUE, symbol(), spec(), cfg);
        // Seed with trades so analytics has data to work with.
        for t in make_trades(64) {
            engine.record_trade(t);
        }
        let now = TimestampMs(10_000);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(n), &(book, engine), |b, (book, engine)| {
            b.iter(|| {
                black_box(engine.compute(book, now));
            });
        });
    }
    group.finish();
}

fn bench_analytics_with_trade_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/analytics_with_trade_load");
    let cfg = AnalyticsConfig {
        trade_window: 256,
        vol_window: 64,
        ..AnalyticsConfig::default()
    };
    let book = make_book(20);
    let trades = make_trades(200);

    group.throughput(Throughput::Elements(1));
    group.bench_function("analytics_with_200_trades", |b| {
        let mut engine = MarketStateEngine::new(VENUE, symbol(), spec(), cfg);
        for t in &trades {
            engine.record_trade(t.clone());
        }
        let now = TimestampMs(10_000);
        b.iter(|| {
            black_box(engine.compute(&book, now));
        });
    });
    group.finish();
}

fn bench_snapshot_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook/snapshot_roundtrip");
    for &n in &[5, 20, 100] {
        let book = make_book(n);
        group.throughput(Throughput::Elements(n as u64 * 2));
        group.bench_with_input(BenchmarkId::from_parameter(n), &book, |b, book| {
            b.iter(|| {
                black_box(book.snapshot(n));
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Event bus benchmarks (requires tokio runtime)
// ---------------------------------------------------------------------------

fn bench_event_bus_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("eventbus/publish");

    // Single subscriber
    {
        let bus = EventBus::new();
        let _sub = bus.market().subscribe();
        group.throughput(Throughput::Elements(1));
        group.bench_function("publish_1_subscriber", |b| {
            let event = MarketEvent::Delta(make_update_delta(1));
            b.iter(|| {
                black_box(bus.market().try_publish(event.clone()));
            });
        });
    }

    // Fan-out: 1, 5, 10 subscribers
    for &num_subs in &[1u32, 5, 10] {
        let bus = EventBus::new();
        let subs: Vec<_> = (0..num_subs).map(|_| bus.market().subscribe()).collect();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{num_subs}_subscribers")),
            &bus,
            |b, bus| {
                let event = MarketEvent::Delta(make_update_delta(1));
                b.iter(|| {
                    black_box(bus.market().try_publish(event.clone()));
                });
            },
        );
        // Keep subs alive until after the benchmark
        drop(subs);
    }
    group.finish();
}

fn bench_event_bus_fanout_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("eventbus/fanout_throughput");

    for &num_subs in &[1u32, 3, 5, 10] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{num_subs}_subscribers")),
            &num_subs,
            |b, &num_subs| {
                let bus = EventBus::new();
                let mut subs: Vec<_> = (0..num_subs).map(|_| bus.market().subscribe()).collect();
                let event = MarketEvent::Delta(make_update_delta(1));

                b.iter(|| {
                    bus.market().try_publish(event.clone());
                    // Drain all subscribers to simulate consumer throughput
                    for sub in subs.iter_mut() {
                        let _ = sub.try_recv();
                    }
                });
                drop(subs);
            },
        );
    }
    group.finish();
}

fn bench_event_bus_burst(c: &mut Criterion) {
    let mut group = c.benchmark_group("eventbus/burst_publish");
    let bus = EventBus::new();
    let _sub = bus.market().subscribe();

    // Benchmark publishing a burst of 100 events
    group.throughput(Throughput::Elements(100));
    group.bench_function("burst_100_deltas", |b| {
        b.iter(|| {
            for seq in 0u64..100 {
                let event = MarketEvent::Delta(make_update_delta(seq));
                let _ = bus.market().try_publish(event);
            }
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    orderbook_benches,
    bench_snapshot_apply,
    bench_delta_apply,
    bench_best_bid_ask,
    bench_depth,
    bench_imbalance,
    bench_vwap,
    bench_level_insert_delete,
    bench_analytics_compute,
    bench_analytics_with_trade_load,
    bench_snapshot_roundtrip,
);

criterion_group!(
    eventbus_benches,
    bench_event_bus_publish,
    bench_event_bus_fanout_throughput,
    bench_event_bus_burst,
);

criterion_main!(orderbook_benches, eventbus_benches);
