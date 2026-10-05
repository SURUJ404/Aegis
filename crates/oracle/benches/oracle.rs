//! Criterion benchmarks for the Stage 4 oracle paths.
//!
//! Budgets (informational, updated in `docs/stages/STAGE_4_ORACLE.md`):
//! - `aggregate_5_venues` — daemon-side median + outlier rejection
//! - `oracle_apply_price_accept` — state-machine publication (deviation checked)
//! - `oracle_gate` — pre-trade circuit-breaker lookup

use criterion::{criterion_group, criterion_main, Criterion};
use lq_oracle::{aggregate, AggregateConfig, OracleBook, Observation};
use lq_sequencer::entry::{MarketId, OraclePriceCmd};
use lq_types::{Exchange, Symbol};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn market() -> MarketId {
    MarketId::new(Exchange::Paper, Symbol("BTC-USD".to_string()))
}

fn observations() -> Vec<Observation> {
    let symbol = Symbol("BTC-USD".to_string());
    [
        (Exchange::Okx, dec!(100_000)),
        (Exchange::Binance, dec!(100_001)),
        (Exchange::Bybit, dec!(99_999)),
        (Exchange::Paper, dec!(100_000)),
        (Exchange::Simulated, dec!(130_000)), // obvious outlier
    ]
    .into_iter()
    .map(|(venue, price)| Observation {
        venue,
        symbol: symbol.clone(),
        price,
        ts_ms: 1_000,
    })
    .collect()
}

fn bench_aggregate(c: &mut Criterion) {
    let obs = observations();
    let cfg = AggregateConfig::default();
    c.bench_function("aggregate_5_venues", |b| {
        b.iter(|| aggregate(&obs, &cfg, 1_500))
    });
}

fn bench_apply_price(c: &mut Criterion) {
    let m = market();
    let mut book = OracleBook::new();
    let mut i = 0u64;
    c.bench_function("oracle_apply_price_accept", |b| {
        b.iter(|| {
            i += 1;
            let cmd = OraclePriceCmd {
                price: dec!(100) + Decimal::new((i % 10) as i64, 2),
                observation_ts_ms: i,
                sources: 3,
                override_band: false,
            };
            book.apply_price(&m, &cmd, i)
        });
    });
}

fn bench_gate(c: &mut Criterion) {
    let m = market();
    let mut book = OracleBook::new();
    let cmd = OraclePriceCmd {
        price: dec!(100),
        observation_ts_ms: 1,
        sources: 3,
        override_band: false,
    };
    book.apply_price(&m, &cmd, 1);
    let mut ts = 1u64;
    c.bench_function("oracle_gate", |b| {
        b.iter(|| {
            ts += 1;
            book.gate(&m, ts)
        });
    });
}

criterion_group!(benches, bench_aggregate, bench_apply_price, bench_gate);
criterion_main!(benches);
