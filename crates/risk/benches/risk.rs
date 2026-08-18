//! Risk engine benchmarks: order validation on a healthy account.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use lq_core::config::RiskConfig;
use lq_core::models::Order;
use lq_core::state::EngineState;
use lq_risk::engine::RiskEngine;
use lq_types::{Exchange, OrderType, Side, Symbol};
use rust_decimal_macros::dec;

const VENUE: Exchange = Exchange::Paper;

fn symbol() -> Symbol {
    Symbol("BTC-USDT".into())
}

pub fn bench_risk(c: &mut Criterion) {
    let risk = RiskEngine::new(RiskConfig::default(), EngineState::new());

    let mut group = c.benchmark_group("risk");

    group.bench_function("validate_order/limit", |b| {
        b.iter(|| {
            let order = Order::new(VENUE, symbol(), Side::Bid, OrderType::Limit, Some(dec!(100.0)), dec!(0.01));
            black_box(risk.validate_order(&order, dec!(100.0)));
        });
    });

    group.bench_function("validate_order/market", |b| {
        b.iter(|| {
            let order = Order::new(VENUE, symbol(), Side::Ask, OrderType::Market, None, dec!(0.02));
            black_box(risk.validate_order(&order, dec!(100.0)));
        });
    });

    group.finish();
}

criterion_group!(benches, bench_risk);
criterion_main!(benches);
