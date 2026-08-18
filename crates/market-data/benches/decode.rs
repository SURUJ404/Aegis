//! Market-data decode benchmarks: venue-native JSON -> normalized events.
//!
//! The bus is constructed once inside a throwaway runtime; publishing to the
//! bounded channel still works after that runtime is gone, so the benchmark
//! isolates decode cost.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use lq_core::bus::EventBus;
use lq_market_data::adapters::binance::BinanceDecoder;
use lq_market_data::FeedDecoder;
use lq_types::Symbol;

/// A realistic 20-level Binance depth frame (the decoder emits a snapshot).
const DEPTH_FRAME: &str = r#"{"stream":"btcusdt@depth20@100ms","data":{"lastUpdateId":1027024,"bids":[["4.00000200","12.00000000"],["3.99960700","431.00000000"],["3.99924600","12.00000000"],["3.99886000","100.00000000"],["3.99840000","77.00000000"],["3.99800000","250.00000000"],["3.99720000","480.00000000"],["3.99660000","280.00000000"],["3.99500000","700.00000000"],["3.99440000","120.00000000"],["3.99380000","100.00000000"],["3.99320000","80.00000000"],["3.99200000","90.00000000"],["3.99120000","50.00000000"],["3.99060000","65.00000000"],["3.98980000","30.00000000"],["3.98800000","35.00000000"],["3.98700000","40.00000000"],["3.98620000","45.00000000"],["3.98460000","55.00000000"]],"asks":[["4.00000600","100.00000000"],["4.00010000","531.00000000"],["4.00030000","100.00000000"],["4.00050000","200.00000000"],["4.00090000","45.00000000"],["4.00130000","150.00000000"],["4.00170000","120.00000000"],["4.00210000","90.00000000"],["4.00250000","80.00000000"],["4.00310000","60.00000000"],["4.00370000","70.00000000"],["4.00430000","40.00000000"],["4.00490000","50.00000000"],["4.00550000","30.00000000"],["4.00610000","25.00000000"],["4.00670000","20.00000000"],["4.00730000","15.00000000"],["4.00790000","10.00000000"],["4.00850000","5.00000000"],["4.00910000","2.00000000"]],"E":1720000000000}}"#;

pub fn bench_decode(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let bus = rt.block_on(async { Arc::new(EventBus::new()) });
    let mut decoder = BinanceDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));

    c.bench_function("decode/binance_depth20", |b| {
        b.iter(|| {
            black_box(decoder.on_text(DEPTH_FRAME, &bus).unwrap());
        });
    });

    // Warm the snapshot flag so the frame parses as an update rather than a first snapshot.
    decoder = BinanceDecoder::new(bus.clone(), Symbol("BTC-USDT".into()));
    c.bench_function("decode/binance_depth20_fresh", |b| {
        b.iter(|| {
            black_box(decoder.on_text(DEPTH_FRAME, &bus).unwrap());
        });
    });
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
