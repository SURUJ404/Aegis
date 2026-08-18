//! Order book benchmarks: snapshot apply, delta apply, and full analytics
//! (MarketStateEngine) computation.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use lq_core::models::{LevelChange, OrderBookDelta, OrderBookLevel, OrderBookSnapshot};
use lq_exchange::spec::InstrumentSpec;
use lq_orderbook::analytics::{AnalyticsConfig, MarketStateEngine};
use lq_orderbook::book::OrderBook;
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

fn snapshot() -> OrderBookSnapshot {
    let levels = |offset: u32| {
        (0..20)
            .map(|i| {
                OrderBookLevel::new(
                    Decimal::from_f64_retain(100.0 + offset as f64 + i as f64 * 0.1).unwrap(),
                    Decimal::from_f64_retain(10.0 - (i % 5) as f64).unwrap(),
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
        bids: levels(0),
        asks: levels(0),
    }
}

fn delta() -> OrderBookDelta {
    OrderBookDelta {
        venue: VENUE,
        symbol: symbol(),
        sequence: 2,
        event_ts: TimestampMs(2),
        exchange_ts: TimestampMs(2),
        changes: vec![
            LevelChange {
                side: Side::Bid,
                price: dec!(99.9),
                qty: dec!(5.0),
            },
            LevelChange {
                side: Side::Ask,
                price: dec!(100.1),
                qty: dec!(5.0),
            },
            LevelChange {
                side: Side::Bid,
                price: dec!(99.8),
                qty: dec!(0.0),
            },
            LevelChange {
                side: Side::Ask,
                price: dec!(100.2),
                qty: dec!(0.0),
            },
        ],
        clear: false,
    }
}

pub fn bench_orderbook(c: &mut Criterion) {
    c.bench_function("orderbook/apply_snapshot_20x20", |b| {
        b.iter(|| {
            let mut book = OrderBook::new(VENUE, symbol(), spec());
            book.apply_snapshot(&snapshot());
            black_box(book)
        });
    });

    c.bench_function("orderbook/apply_delta_4_changes", |b| {
        let mut book = OrderBook::new(VENUE, symbol(), spec());
        book.apply_snapshot(&snapshot());
        let d = delta();
        b.iter(|| {
            let _ = black_box(book.apply_delta(&d));
        });
    });

    c.bench_function("orderbook/analytics_compute", |b| {
        let mut book = OrderBook::new(VENUE, symbol(), spec());
        book.apply_snapshot(&snapshot());
        let engine = MarketStateEngine::new(VENUE, symbol(), spec(), AnalyticsConfig::default());
        b.iter(|| {
            black_box(engine.compute(&book, TimestampMs(3)));
        });
    });
}

criterion_group!(benches, bench_orderbook);
criterion_main!(benches);
