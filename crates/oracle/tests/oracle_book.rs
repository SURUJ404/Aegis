//! Stage 4 `OracleBook` tests: validation order, deviation circuit breaker
//! (reject + latch + clear), override re-baseline, log-driven staleness gate
//! and canonical hashing.
//!
//! All time is caller-supplied logical time (`entry_ts_ms`) — these tests
//! prove the breaker never needs a clock.

use lq_oracle::{OracleBook, OracleOutcome, OracleParams};
use lq_sequencer::entry::{MarketId, OraclePriceCmd};
use lq_sequencer::hash::{finish, new_hasher, StateHash};
use lq_types::{Exchange, Symbol};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn market(sym: &str) -> MarketId {
    MarketId::new(Exchange::Paper, Symbol(sym.to_string()))
}

fn cmd(price: Decimal, observation_ts_ms: u64, sources: u8) -> OraclePriceCmd {
    OraclePriceCmd {
        price,
        observation_ts_ms,
        sources,
        override_band: false,
    }
}

fn overridden(price: Decimal, observation_ts_ms: u64, sources: u8) -> OraclePriceCmd {
    OraclePriceCmd {
        price,
        observation_ts_ms,
        sources,
        override_band: true,
    }
}

fn hash(book: &OracleBook) -> StateHash {
    let mut h = new_hasher();
    book.write_hash(&mut h);
    finish(h)
}

fn assert_accepted(out: OracleOutcome) {
    assert_eq!(out, OracleOutcome::Accepted, "expected acceptance");
}

fn assert_rejected(out: OracleOutcome, reason: &str) {
    match out {
        OracleOutcome::Rejected { reason: r } => assert_eq!(r, reason),
        OracleOutcome::Accepted => panic!("expected rejection ({reason}), got acceptance"),
    }
}

// ---- acceptance / validation order ----

#[test]
fn first_publication_is_accepted_and_sets_price() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 3), 1_100));
    assert_eq!(book.price(&m), Some(dec!(100)));
    assert_eq!(book.stats().published, 1);
    assert_eq!(book.stats().rejected, 0);
    let e = book.entry(&m).unwrap();
    assert_eq!(e.published_ts_ms, 1_100);
    assert_eq!(e.observation_ts_ms, 1_000);
    assert_eq!(e.sources, 3);
    assert!(!e.halted);
    assert!(book.covered(&m));
    assert_eq!(book.gate(&m, 1_100), None);
}

#[test]
fn invalid_price_rejected() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_rejected(
        book.apply_price(&m, &cmd(Decimal::ZERO, 1_000, 1), 1_100),
        "invalid_price",
    );
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(-1), 1_000, 1), 1_100),
        "invalid_price",
    );
    assert!(!book.covered(&m));
    assert_eq!(book.stats().rejected, 2);
}

#[test]
fn observation_in_future_rejected() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(100), 2_000, 1), 1_100),
        "observation_in_future",
    );
    assert!(!book.covered(&m));
}

#[test]
fn stale_observation_rejected_but_check_disabled_when_zero() {
    let m = market("BTC-USD");

    // Default max_staleness_ms = 30_000: a 40 s old observation fails.
    let mut book = OracleBook::new();
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(100), 0, 1), 40_000),
        "stale_observation",
    );
    assert!(!book.covered(&m));

    // Fresh observation passes.
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 39_000, 1), 40_000));

    // Zero disables the check entirely (mirrors the risk-limit convention).
    let params = OracleParams {
        max_staleness_ms: 0,
        ..OracleParams::default()
    };
    let mut off = OracleBook::with_params(params);
    assert_accepted(off.apply_price(&m, &cmd(dec!(100), 0, 1), 10_000_000));
}

#[test]
fn min_sources_enforced() {
    let m = market("BTC-USD");
    let params = OracleParams {
        min_sources: 3,
        ..OracleParams::default()
    };
    let mut book = OracleBook::with_params(params);
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(100), 1_000, 2), 1_100),
        "insufficient_sources",
    );
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 3), 1_100));
    assert_eq!(book.price(&m), Some(dec!(100)));
    assert_eq!(book.stats().published, 1);
}

#[test]
fn validation_order_is_first_failure_wins() {
    // All four validators fail at once: price is checked first, so the
    // reason must be `invalid_price` and later checks must not run.
    let m = market("BTC-USD");
    let params = OracleParams {
        min_sources: 5,
        ..OracleParams::default()
    };
    let mut book = OracleBook::with_params(params);
    assert_rejected(
        book.apply_price(&m, &cmd(Decimal::ZERO, 99_999, 0), 1_000),
        "invalid_price",
    );
    assert_eq!(book.stats().rejected, 1);
}

// ---- deviation circuit breaker ----

#[test]
fn deviation_rejects_and_halts_market() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new(); // 10 % band
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100));

    // A 100 % move exceeds the 10 % band.
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100),
        "oracle_deviation",
    );
    assert_eq!(book.price(&m), Some(dec!(100)), "price must not move");
    assert!(book.is_halted(&m));
    assert_eq!(book.stats().halts, 1);
    assert_eq!(book.stats().rejected, 1);
    assert_eq!(book.gate(&m, 2_100), Some("oracle_halted"));
}

#[test]
fn halt_latches_once_across_repeat_deviation_rejections() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100));
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100),
        "oracle_deviation",
    );
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(300), 3_000, 1), 3_100),
        "oracle_deviation",
    );
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(1), 4_000, 1), 4_100),
        "oracle_deviation",
    );
    // Rejections count each time; halts count only transitions.
    assert_eq!(book.stats().rejected, 3);
    assert_eq!(book.stats().halts, 1);
    assert_eq!(book.price(&m), Some(dec!(100)));
    assert_eq!(book.gate(&m, 4_100), Some("oracle_halted"));
}

#[test]
fn in_band_price_after_halt_clears_latch_without_override() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100));
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100),
        "oracle_deviation",
    );
    assert!(book.is_halted(&m));

    // |105 − 100| = 5 % ≤ 10 % band → accepted, latch cleared.
    assert_accepted(book.apply_price(&m, &cmd(dec!(105), 3_000, 1), 3_100));
    assert!(!book.is_halted(&m));
    assert_eq!(book.price(&m), Some(dec!(105)));
    assert_eq!(book.gate(&m, 3_100), None);
}

#[test]
fn override_rebaselines_halted_market() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100));
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100),
        "oracle_deviation",
    );
    assert!(book.is_halted(&m));

    // Explicit operator acknowledgement accepts the outlier and re-baselines.
    assert_accepted(book.apply_price(&m, &overridden(dec!(180), 3_000, 1), 3_100));
    assert!(!book.is_halted(&m));
    assert_eq!(book.price(&m), Some(dec!(180)));
    assert_eq!(book.gate(&m, 3_100), None);

    // The new baseline is 180: an 11 % move from it still trips the breaker.
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 4_000, 1), 4_100),
        "oracle_deviation",
    );
    assert_eq!(book.price(&m), Some(dec!(180)));
}

#[test]
fn deviation_disabled_when_bps_zero() {
    let m = market("BTC-USD");
    let params = OracleParams {
        max_deviation_bps: Decimal::ZERO,
        ..OracleParams::default()
    };
    let mut book = OracleBook::with_params(params);
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100));
    assert_accepted(book.apply_price(&m, &cmd(dec!(1_000_000), 2_000, 1), 2_100));
    assert_eq!(book.price(&m), Some(dec!(1_000_000)));
    assert_eq!(book.stats().halts, 0);
}

#[test]
fn override_before_any_baseline_is_accepted() {
    // Deviation needs a previous price; the first publication always passes.
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &overridden(dec!(50), 1_000, 1), 1_100));
    assert_eq!(book.price(&m), Some(dec!(50)));
}

// ---- staleness gate (log-driven) ----

#[test]
fn staleness_gate_trips_and_clears_with_fresh_publish() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new(); // max_staleness_ms = 30_000
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_000));

    assert_eq!(book.gate(&m, 1_000), None);
    assert_eq!(book.gate(&m, 31_000), None); // exactly 30 s: still fresh
    assert_eq!(book.gate(&m, 31_001), Some("oracle_stale"));
    assert_eq!(book.gate(&m, 1_000_000), Some("oracle_stale"));

    // A fresh publication re-baselines the clock.
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 40_000, 1), 40_001));
    assert_eq!(book.gate(&m, 40_001), None);
}

#[test]
fn staleness_gate_disabled_when_zero() {
    let params = OracleParams {
        max_staleness_ms: 0,
        ..OracleParams::default()
    };
    let m = market("BTC-USD");
    let mut book = OracleBook::with_params(params);
    // Observation staleness check also off, so this publish is legal.
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 0, 1), 1_000));
    assert_eq!(book.gate(&m, 100_000_000), None);
}

#[test]
fn halted_market_reports_halted_even_when_also_stale() {
    let m = market("BTC-USD");
    let mut book = OracleBook::new();
    assert_accepted(book.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_000));
    assert_rejected(
        book.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100),
        "oracle_deviation",
    );
    // Long after staleness — halt has priority in the gate.
    assert_eq!(book.gate(&m, 1_000_000), Some("oracle_halted"));
}

#[test]
fn uncovered_markets_pass_the_gate() {
    let m = market("BTC-USD");
    let book = OracleBook::new();
    assert_eq!(book.gate(&m, 1_000), None);
    assert!(!book.covered(&m));
    assert_eq!(book.markets().len(), 0);
}

// ---- hashing / serialization ----

#[test]
fn hash_reflects_price_params_and_latches() {
    let m = market("BTC-USD");

    let mut a = OracleBook::new();
    let mut b = OracleBook::new();
    assert_eq!(hash(&a), hash(&b), "fresh books must hash equal");

    a.apply_price(&m, &cmd(dec!(100), 1_000, 1), 1_100);
    b.apply_price(&m, &cmd(dec!(101), 1_000, 1), 1_100);
    assert_ne!(hash(&a), hash(&b), "different prices must differ");

    // Same price, but a latched halt on one side must differ too.
    b.apply_price(&m, &cmd(dec!(200), 2_000, 1), 2_100); // deviates → halt
    assert_ne!(hash(&a), hash(&b), "halt latch must be hashed");
    assert!(b.is_halted(&m));
}

#[test]
fn serde_roundtrip_preserves_equality_and_hash() {
    let m = market("BTC-USD");
    let mut book = OracleBook::with_params(OracleParams {
        max_staleness_ms: 7_777,
        max_deviation_bps: dec!(250),
        min_sources: 2,
    });
    assert_accepted(book.apply_price(&m, &cmd(dec!(123.45), 1_000, 2), 1_100));
    book.apply_price(&m, &cmd(dec!(999), 2_000, 2), 2_100); // rejected + halts

    let json = serde_json::to_string(&book).expect("serialize");
    let back: OracleBook = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, book);
    assert_eq!(hash(&back), hash(&book));
    assert_eq!(back.stats(), book.stats());
    assert_eq!(back.params(), book.params());
}
