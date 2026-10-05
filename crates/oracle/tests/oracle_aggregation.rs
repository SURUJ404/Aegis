//! Stage 4 aggregation tests: freshness filtering, median, outlier rejection
//! and determinism of the daemon-side (read path) aggregation.
//!
//! Property tests pin the two load-bearing guarantees: `aggregate` never
//! panics on arbitrary input, and its result is independent of input order
//! (a set function — the daemon's venue arrival order must not matter).

use lq_core::{MarketEvent, MarketTick, OrderBookLevel, OrderBookSnapshot, Trade};
use lq_oracle::{aggregate, AggregateConfig, Aggregation, Observation, ObservationBook};
use lq_sequencer::entry::MarketId;
use lq_types::{Exchange, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn obs(venue: Exchange, price: Decimal, ts_ms: u64) -> Observation {
    Observation {
        venue,
        symbol: Symbol("BTC-USD".to_string()),
        price,
        ts_ms,
    }
}

fn cfg() -> AggregateConfig {
    AggregateConfig {
        min_sources: 3,
        max_observation_age_ms: 5_000,
        outlier_band_bps: dec!(100), // 1 %
        price_scale: 8,
    }
}

fn five_venues() -> Vec<Observation> {
    vec![
        obs(Exchange::Okx, dec!(100_000), 1_000),
        obs(Exchange::Binance, dec!(100_001), 1_000),
        obs(Exchange::Bybit, dec!(99_999), 1_000),
        obs(Exchange::Paper, dec!(100_000), 1_000),
        obs(Exchange::Simulated, dec!(130_000), 1_000),
    ]
}

#[test]
fn median_of_odd_sources_is_middle_price() {
    let obs = vec![
        obs(Exchange::Okx, dec!(101), 1_000),
        obs(Exchange::Binance, dec!(99), 1_000),
        obs(Exchange::Bybit, dec!(100), 1_000),
    ];
    let agg = aggregate(&obs, &cfg(), 1_500);
    assert_eq!(agg.price(), Some(dec!(100)));
    if let Aggregation::Fresh { sources, used, .. } = agg {
        assert_eq!(sources, 3);
        assert_eq!(used.len(), 3);
    } else {
        panic!("expected Fresh, got {agg:?}");
    }
}

#[test]
fn median_of_even_sources_averages_middle_pair() {
    let obs = vec![
        obs(Exchange::Okx, dec!(100), 1_000),
        obs(Exchange::Binance, dec!(102), 1_000),
        obs(Exchange::Bybit, dec!(101), 1_000),
        obs(Exchange::Paper, dec!(103), 1_000),
    ];
    // Outliers disabled so the test isolates the median arithmetic.
    let cfg = AggregateConfig {
        outlier_band_bps: Decimal::ZERO,
        ..cfg()
    };
    let agg = aggregate(&obs, &cfg, 1_500);
    // sorted [100,101,102,103] → (101+102)/2 = 101.5
    assert_eq!(agg.price(), Some(dec!(101.5)));
}

#[test]
fn outlier_is_rejected_and_median_uses_inliers_only() {
    let agg = aggregate(&five_venues(), &cfg(), 1_500);
    let Aggregation::Fresh {
        price,
        sources,
        used,
    } = agg
    else {
        panic!("expected Fresh, got {agg:?}");
    };
    // The 130 000 print is 30 % off the median — far outside the 1 % band.
    assert!(!used.contains(&Exchange::Simulated));
    assert_eq!(sources, 4);
    // sorted inliers [99_999, 100_000, 100_000, 100_001] → 100_000
    assert_eq!(price, dec!(100_000));
}

#[test]
fn stale_observations_are_filtered() {
    // now=10_000, age limit 5 s: the ts=1_000 print is 9 s old.
    let obs = vec![
        obs(Exchange::Okx, dec!(100), 9_000),
        obs(Exchange::Binance, dec!(100), 9_000),
        obs(Exchange::Bybit, dec!(100), 1_000),
    ];
    let agg = aggregate(&obs, &cfg(), 10_000);
    assert_eq!(
        agg,
        Aggregation::InsufficientSources {
            fresh: 2,
            total: 3
        }
    );
}

#[test]
fn future_observations_are_filtered() {
    let obs = vec![
        obs(Exchange::Okx, dec!(100), 2_000),
        obs(Exchange::Binance, dec!(100), 2_000),
        obs(Exchange::Bybit, dec!(100), 99_000), // from the future
    ];
    let agg = aggregate(&obs, &cfg(), 2_500);
    assert_eq!(
        agg,
        Aggregation::InsufficientSources {
            fresh: 2,
            total: 3
        }
    );
}

#[test]
fn non_positive_prices_are_filtered() {
    let obs = vec![
        obs(Exchange::Okx, dec!(100), 1_000),
        obs(Exchange::Binance, dec!(0), 1_000),
        obs(Exchange::Bybit, dec!(-5), 1_000),
    ];
    let agg = aggregate(&obs, &cfg(), 1_500);
    assert_eq!(
        agg,
        Aggregation::InsufficientSources {
            fresh: 1,
            total: 3
        }
    );
}

#[test]
fn consensus_lost_when_outliers_break_quorum() {
    // Two sources that are each other's outlier: no honest median exists.
    let obs = vec![obs(Exchange::Okx, dec!(100), 1_000), obs(Exchange::Bybit, dec!(200), 1_000)];
    let cfg = AggregateConfig {
        min_sources: 2,
        ..cfg()
    };
    let agg = aggregate(&obs, &cfg, 1_500);
    assert_eq!(agg, Aggregation::ConsensusLost { fresh: 2 });
    assert!(!agg.is_fresh());
}

#[test]
fn duplicate_venues_deduplicate_to_newest() {
    let obs = vec![
        obs(Exchange::Okx, dec!(90), 500),   // older, must lose
        obs(Exchange::Okx, dec!(101), 1_000), // newest per venue
        obs(Exchange::Binance, dec!(101), 1_000),
        obs(Exchange::Bybit, dec!(101), 1_000),
    ];
    let agg = aggregate(&obs, &cfg(), 1_500);
    let Aggregation::Fresh { price, sources, .. } = agg else {
        panic!("expected Fresh, got {agg:?}");
    };
    assert_eq!(sources, 3);
    // Had the stale 90 survived, the median would have moved below 101.
    assert_eq!(price, dec!(101));
}

#[test]
fn result_is_independent_of_input_order() {
    let base = five_venues();
    let permutations = [
        vec![0, 1, 2, 3, 4],
        vec![4, 3, 2, 1, 0],
        vec![2, 0, 4, 1, 3],
        vec![3, 4, 0, 2, 1],
    ];
    let first = aggregate(&base, &cfg(), 1_500);
    for p in &permutations {
        let reordered: Vec<Observation> = p.iter().map(|&i| base[i].clone()).collect();
        assert_eq!(
            aggregate(&reordered, &cfg(), 1_500),
            first,
            "input order must not change the aggregation"
        );
    }
}

// ---- MarketEvent extraction (reuse of the lq-market-data event model) ----

fn market() -> MarketId {
    MarketId::new(Exchange::Paper, Symbol("BTC-USD".to_string()))
}

#[test]
fn extract_tick_and_trade() {
    let tick = MarketEvent::Tick(MarketTick {
        venue: Exchange::Okx,
        symbol: Symbol("BTC-USD".to_string()),
        last_price: dec!(100),
        last_qty: Decimal::ONE,
        best_bid: dec!(99),
        best_ask: dec!(101),
        event_ts: TimestampMs(1_000),
    });
    assert_eq!(
        ObservationBook::extract(&tick),
        Some((
            Exchange::Okx,
            Symbol("BTC-USD".to_string()),
            dec!(100),
            1_000
        ))
    );

    let trade = MarketEvent::Trade(Trade {
        venue: Exchange::Binance,
        symbol: Symbol("BTC-USD".to_string()),
        price: dec!(100.5),
        qty: Decimal::ONE,
        aggressor: Side::Bid,
        event_ts: TimestampMs(2_000),
        exchange_ts: TimestampMs(2_000),
    });
    assert_eq!(
        ObservationBook::extract(&trade),
        Some((
            Exchange::Binance,
            Symbol("BTC-USD".to_string()),
            dec!(100.5),
            2_000
        ))
    );
}

#[test]
fn extract_snapshot_uses_touch_mid() {
    let snap = MarketEvent::Snapshot(OrderBookSnapshot {
        venue: Exchange::Bybit,
        symbol: Symbol("BTC-USD".to_string()),
        sequence: 7,
        event_ts: TimestampMs(3_000),
        exchange_ts: TimestampMs(3_000),
        bids: vec![OrderBookLevel::new(dec!(100), Decimal::ONE)],
        asks: vec![OrderBookLevel::new(dec!(102), Decimal::ONE)],
    });
    assert_eq!(
        ObservationBook::extract(&snap),
        Some((
            Exchange::Bybit,
            Symbol("BTC-USD".to_string()),
            dec!(101),
            3_000
        ))
    );
}

#[test]
fn extract_ignores_delta_and_status() {
    let status = MarketEvent::Status {
        venue: Exchange::Okx,
        symbol: Symbol("BTC-USD".to_string()),
        status: lq_core::event::FeedStatus::Stale,
        ts: TimestampMs(4_000),
    };
    assert_eq!(ObservationBook::extract(&status), None);
}

#[test]
fn observation_book_records_and_aggregates() {
    let mut book = ObservationBook::new();
    let m = market();
    for (venue, price, ts) in [
        (Exchange::Okx, dec!(100), 1_000u64),
        (Exchange::Binance, dec!(101), 1_000),
        (Exchange::Bybit, dec!(99), 1_000),
    ] {
        assert!(book.record(
            &m,
            Observation {
                venue,
                symbol: Symbol("BTC-USD".to_string()),
                price,
                ts_ms: ts,
            }
        ));
    }
    // Out-of-order observation for an existing venue is ignored.
    assert!(!book.record(
        &m,
        Observation {
            venue: Exchange::Okx,
            symbol: Symbol("BTC-USD".to_string()),
            price: dec!(500),
            ts_ms: 0,
        }
    ));
    assert_eq!(book.venue_count(&m), 3);

    let agg = book.aggregate_for(&m, &cfg(), 1_500);
    assert_eq!(agg.price(), Some(dec!(100)));
}

#[test]
fn on_market_event_ingests_feed_events() {
    let mut book = ObservationBook::new();
    let m = market();
    let tick = MarketEvent::Tick(MarketTick {
        venue: Exchange::Okx,
        symbol: Symbol("BTC-USD".to_string()),
        last_price: dec!(100),
        last_qty: Decimal::ONE,
        best_bid: dec!(99),
        best_ask: dec!(101),
        event_ts: TimestampMs(1_000),
    });
    assert!(book.on_market_event(&m, &tick));
    assert_eq!(book.venue_count(&m), 1);
    assert!(book
        .observations(&m)
        .iter()
        .any(|o| o.price == dec!(100)));
}

// ---- property tests ----

mod props {
    use super::*;
    use proptest::prelude::*;

    fn arb_observation() -> impl Strategy<Value = Observation> {
        let venue = prop_oneof![
            Just(Exchange::Okx),
            Just(Exchange::Binance),
            Just(Exchange::Bybit),
            Just(Exchange::Paper),
            Just(Exchange::Simulated),
        ];
        (
            venue,
            -500_000i64..500_000,
            0u64..100_000,
        )
            .prop_map(|(venue, price, ts)| Observation {
                venue,
                symbol: Symbol("BTC-USD".to_string()),
                price: Decimal::from(price),
                ts_ms: ts,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn aggregate_never_panics(
            obs in proptest::collection::vec(arb_observation(), 0..40),
            now in 0u64..100_000,
            min_sources in 0u8..8,
        ) {
            let cfg = AggregateConfig {
                min_sources,
                ..AggregateConfig::default()
            };
            let _ = aggregate(&obs, &cfg, now);
        }

        #[test]
        fn fresh_price_stays_within_input_range(
            mut obs in proptest::collection::vec(arb_observation(), 1..30),
            now in 1u64..100_000,
        ) {
            let cfg = AggregateConfig {
                min_sources: 1,
                max_observation_age_ms: 0,      // no age filter
                outlier_band_bps: Decimal::ZERO, // no outlier filter
                price_scale: 8,
            };
            let agg = aggregate(&obs, &cfg, now);
            if let Some(price) = agg.price() {
                let min = obs.iter().filter(|o| o.price > Decimal::ZERO).map(|o| o.price).min();
                let max = obs.iter().map(|o| o.price).max();
                if let (Some(min), Some(max)) = (min, max) {
                    prop_assert!(price >= min && price <= max,
                        "median {price} outside [{min}, {max}]");
                }
            }
            // Permuting the input must not change the result.
            obs.reverse();
            prop_assert_eq!(aggregate(&obs, &cfg, now), agg);
        }
    }
}
