//! Strategy benchmarks: baseline market-making decision cost on a fresh state.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use lq_core::config::MarketMakingConfig;
use lq_core::models::{Inventory, MarketRegime, MarketState};
use lq_strategy::StrategyContext;
use lq_strategy::Strategy;
use lq_strategy::MarketMakingStrategy;
use lq_types::{Exchange, Symbol, TimestampMs};
use rust_decimal_macros::dec;

const VENUE: Exchange = Exchange::Paper;

fn symbol() -> Symbol {
    Symbol("BTC-USDT".into())
}

fn state(ts: u64) -> MarketState {
    MarketState {
        venue: VENUE,
        symbol: symbol(),
        event_ts: TimestampMs(ts),
        best_bid: dec!(99.95),
        best_ask: dec!(100.05),
        mid: dec!(100.0),
        spread: dec!(0.1),
        spread_bps: 10.0,
        orderbook_imbalance: 0.1,
        microprice: dec!(100.0),
        vwap: dec!(100.0),
        depth_bid: dec!(10.0),
        depth_ask: dec!(10.0),
        num_bid_levels: 10,
        num_ask_levels: 10,
        buy_volume: dec!(2.0),
        sell_volume: dec!(1.5),
        trade_intensity: 1.5,
        realized_volatility: 0.0002,
        price_impact_estimate: 0.0001,
        regime: MarketRegime::Normal,
        stale: false,
    }
}

fn inventory() -> Inventory {
    Inventory {
        symbol: symbol(),
        net_qty: dec!(0.01),
        avg_entry: dec!(99.0),
        realized_pnl: dec!(0.0),
        event_ts: TimestampMs(1_000),
    }
}

pub fn bench_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy");

    group.bench_function("market_making/decision", |b| {
        let mut strategy = MarketMakingStrategy::new(
            symbol(),
            VENUE,
            MarketMakingConfig {
                quote_refresh_ms: 1,
                ..MarketMakingConfig::default()
            },
        );
        b.iter_custom(|iters| {
            // Bump event_ts each call so the quote-refresh throttle is always
            // satisfied and every iteration exercises the full decision path.
            let start = std::time::Instant::now();
            for i in 0..iters {
                let s = state(10_000 + i * 10);
                black_box(strategy.on_market_state(&StrategyContext {
                    inventory: None,
                    position: None,
                    halted: false,
                    running: true,
                    now: s.event_ts,
                    market: &s,
                }));
            }
            start.elapsed()
        });
    });

    group.bench_function("market_making/decision_with_inventory", |b| {
        let mut strategy = MarketMakingStrategy::new(
            symbol(),
            VENUE,
            MarketMakingConfig {
                quote_refresh_ms: 1,
                ..MarketMakingConfig::default()
            },
        );
        let inv = inventory();
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for i in 0..iters {
                let s = state(20_000 + i * 10);
                black_box(strategy.on_market_state(&StrategyContext {
                    inventory: Some(inv.clone()),
                    position: None,
                    halted: false,
                    running: true,
                    now: s.event_ts,
                    market: &s,
                }));
            }
            start.elapsed()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_strategy);
criterion_main!(benches);
