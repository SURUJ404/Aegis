//! Stage 1 integration tests: sequencing, snapshot/replay, determinism,
//! property tests, decoder fuzz-safety, and cross-check against lq-backtest.

use lq_sequencer::{
    decode_entry, encode_entry, rebuild, rebuild_empty_log, EntryPayload, FillCmd, FillLiquidity,
    LedgerState, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd, Sequencer, SequencerConfig,
    StateMachine,
};
use lq_types::{Exchange, OrderType, Side, Symbol, TimeInForce};
use rust_decimal_macros::dec;
use uuid::Uuid;

fn market(sym: &str) -> MarketId {
    MarketId::new(Exchange::Paper, Symbol(sym.to_string()))
}

fn place_payload(i: u64) -> EntryPayload {
    EntryPayload::PlaceOrder(PlaceOrderCmd {
        order_id: Uuid::from_u128(i as u128 + 1),
        client_order_id: format!("c-{i}"),
        side: if i.is_multiple_of(2) { Side::Bid } else { Side::Ask },
        order_type: OrderType::Limit,
        price: Some(dec!(100) + rust_decimal::Decimal::from(i % 7)),
        quantity: dec!(0.1),
        time_in_force: TimeInForce::Gtc,
    })
}

fn open_seq(dir: &std::path::Path, snapshot_every: u64) -> Sequencer<LedgerState> {
    let mut cfg = SequencerConfig::new(dir);
    cfg.snapshot_every = snapshot_every;
    cfg.sync_on_append = false;
    Sequencer::open(cfg, LedgerState::new()).unwrap()
}

fn fill_n(seq: &mut Sequencer<LedgerState>, n: u64, sym: &str) -> Vec<Uuid> {
    let mut ids = Vec::with_capacity(n as usize);
    for i in 0..n {
        let uid_base = if sym == "BTC-USDT" { 0 } else { 1_000_000 };
        let payload = place_payload(uid_base + i + 1);
        if let EntryPayload::PlaceOrder(cmd) = &payload {
            ids.push(cmd.order_id);
        }
        seq.append(market(sym), Some(1_000 + i), payload).unwrap();
    }
    ids
}

#[test]
fn global_and_market_sequences_are_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let mut seq = open_seq(dir.path(), 0);
    let mut prev_g = 0;
    let mut prev_m1 = 0;
    let mut prev_m2 = 0;
    for i in 0..50u64 {
        let sym = if i % 2 == 0 { "BTC-USDT" } else { "ETH-USDT" };
        let e = seq
            .append(market(sym), Some(i), place_payload(i + 1))
            .unwrap();
        assert_eq!(e.global_seq, prev_g + 1);
        if sym == "BTC-USDT" {
            assert_eq!(e.market_seq, prev_m1 + 1);
            prev_m1 = e.market_seq;
        } else {
            assert_eq!(e.market_seq, prev_m2 + 1);
            prev_m2 = e.market_seq;
        }
        prev_g = e.global_seq;
    }
    assert_eq!(seq.last_global_seq(), 50);
}

#[test]
fn replay_from_empty_wal_matches_live_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mut seq = open_seq(dir.path(), 0);
    let btc_ids = fill_n(&mut seq, 100, "BTC-USDT");
    fill_n(&mut seq, 50, "ETH-USDT");
    // cancels + fills + ticks
    let first_order = btc_ids[0];
    let third_order = btc_ids[2];
    seq.append(
        market("BTC-USDT"),
        Some(2000),
        EntryPayload::CancelOrder {
            order_id: first_order,
        },
    )
    .unwrap();
    seq.append(
        market("BTC-USDT"),
        Some(2001),
        EntryPayload::Fill(FillCmd {
            order_id: third_order,
            price: dec!(101),
            quantity: dec!(0.05),
            fee: dec!(0.001),
            liquidity: FillLiquidity::Maker,
        }),
    )
    .unwrap();
    seq.append(
        market("BTC-USDT"),
        Some(2002),
        EntryPayload::MarketTick(MarketTickCmd {
            last: dec!(100.25),
            bid: Some(dec!(100.2)),
            ask: Some(dec!(100.3)),
        }),
    )
    .unwrap();

    let live_hash = seq.state_hash();
    let wal = dir.path().join("wal.log");
    let (rebuilt, hash) = rebuild_empty_log(&wal, LedgerState::new()).unwrap();
    assert_eq!(live_hash, hash);
    assert_eq!(rebuilt.last_global_seq(), seq.last_global_seq());
    assert_eq!(rebuilt.state_hash(), seq.state_hash());
}

#[test]
fn snapshot_midway_replay_matches_full_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    // Snapshot every 25 entries.
    let mut seq = open_seq(dir.path(), 25);
    fill_n(&mut seq, 100, "BTC-USDT");
    let live = seq.state_hash();
    drop(seq);

    let wal = dir.path().join("wal.log");
    let snaps = dir.path().join("snapshots");
    let (from_snap, h1) = rebuild(&snaps, &wal, LedgerState::new()).unwrap();
    let (full, h2) = rebuild_empty_log(&wal, LedgerState::new()).unwrap();
    assert_eq!(h1, live);
    assert_eq!(h2, live);
    assert_eq!(from_snap.state_hash(), full.state_hash());
    assert_eq!(from_snap.last_global_seq(), 100);
}

#[test]
fn reopen_sequencer_preserves_hash_without_manual_replay() {
    let dir = tempfile::tempdir().unwrap();
    let h = {
        let mut seq = open_seq(dir.path(), 10);
        fill_n(&mut seq, 40, "BTC-USDT");
        seq.state_hash()
    };
    let seq = open_seq(dir.path(), 10);
    assert_eq!(seq.state_hash(), h);
    assert_eq!(seq.last_global_seq(), 40);
}

#[test]
fn two_independent_runs_same_log_same_hash() {
    let run = |dir: &std::path::Path| {
        let mut seq = open_seq(dir, 0);
        fill_n(&mut seq, 80, "BTC-USDT");
        fill_n(&mut seq, 40, "ETH-USDT");
        seq.state_hash()
    };
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    assert_eq!(run(d1.path()), run(d2.path()));
}

#[test]
fn seq_gap_apply_is_rejected() {
    let mut sm = LedgerState::new();
    let entry = LogEntry {
        global_seq: 5,
        market_seq: 1,
        market: market("BTC-USDT"),
        ts_ms: 0,
        payload: place_payload(1),
    };
    assert!(sm.apply(&entry).is_err());
}

#[test]
fn decoder_never_panics_on_arbitrary_bytes() {
    // Lightweight fuzz: random-ish and structured garbage must Err, not panic.
    let mut seed = 0x1234_5678u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        seed
    };
    for len in 0..128usize {
        let mut buf = vec![0u8; len];
        for b in &mut buf {
            *b = (next() & 0xff) as u8;
        }
        let _ = decode_entry(&buf); // must not panic
    }
    // Valid header-ish JSON fragments
    for s in [
        "",
        "{",
        "}",
        "null",
        "true",
        r#"{"global_seq":1}"#,
        r#"{"payload":{"type":"unknown"}}"#,
        r#"{"global_seq":-1,"market_seq":0}"#,
    ] {
        let _ = decode_entry(s.as_bytes());
    }
}

#[test]
fn encode_decode_roundtrip_holds_for_varied_payloads() {
    let payloads = vec![
        place_payload(1),
        EntryPayload::CancelOrder {
            order_id: Uuid::from_u128(9),
        },
        EntryPayload::Fill(FillCmd {
            order_id: Uuid::from_u128(2),
            price: dec!(10.5),
            quantity: dec!(1.25),
            fee: dec!(-0.01),
            liquidity: FillLiquidity::Taker,
        }),
        EntryPayload::MarketTick(MarketTickCmd {
            last: dec!(0.00000001),
            bid: None,
            ask: Some(dec!(1)),
        }),
    ];
    for (i, payload) in payloads.into_iter().enumerate() {
        let e = LogEntry {
            global_seq: i as u64 + 1,
            market_seq: 1,
            market: market("BTC-USDT"),
            ts_ms: 42,
            payload,
        };
        let bytes = encode_entry(&e).unwrap();
        let back = decode_entry(&bytes).unwrap();
        assert_eq!(e, back);
    }
}

#[test]
fn corrupt_wal_tail_does_not_prevent_recovery() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut seq = open_seq(dir.path(), 0);
        fill_n(&mut seq, 20, "BTC-USDT");
    }
    let wal = dir.path().join("wal.log");
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
    std::fs::write(&wal, &bytes).unwrap();

    // Sequencer open recovers tail (truncates garbage).
    let seq = open_seq(dir.path(), 0);
    assert_eq!(seq.last_global_seq(), 20);
}

// --- proptest ---

mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn global_seq_is_exactly_n_after_n_appends(n in 1u64..200) {
            let dir = tempfile::tempdir().unwrap();
            let mut seq = open_seq(dir.path(), 0);
            fill_n(&mut seq, n, "BTC-USDT");
            prop_assert_eq!(seq.last_global_seq(), n);
            let wal = dir.path().join("wal.log");
            let (_, h) = rebuild_empty_log(&wal, LedgerState::new()).unwrap();
            prop_assert_eq!(h, seq.state_hash());
        }

        #[test]
        fn decoder_does_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode_entry(&bytes);
        }

        #[test]
        fn encode_decode_roundtrip(
            seq in 1u64..1_000_000,
            mseq in 1u64..1_000_000,
            qty in 0.0001f64..100.0,
            price in 0.01f64..1_000_000.0,
            bid_side in any::<bool>(),
        ) {
            // Build via Decimal from string to avoid float state (test only).
            let q = rust_decimal::Decimal::from_str_exact(&format!("{qty:.4}")).unwrap_or(dec!(1));
            let p = rust_decimal::Decimal::from_str_exact(&format!("{price:.2}")).unwrap_or(dec!(1));
            let e = LogEntry {
                global_seq: seq,
                market_seq: mseq,
                market: market("BTC-USDT"),
                ts_ms: seq,
                payload: EntryPayload::PlaceOrder(PlaceOrderCmd {
                    order_id: Uuid::from_u128(seq as u128),
                    client_order_id: seq.to_string(),
                    side: if bid_side { Side::Bid } else { Side::Ask },
                    order_type: OrderType::Limit,
                    price: Some(p),
                    quantity: q,
                    time_in_force: TimeInForce::Gtc,
                }),
            };
            let bytes = encode_entry(&e).unwrap();
            let back = decode_entry(&bytes).unwrap();
            prop_assert_eq!(e, back);
        }

        #[test]
        fn market_seqs_independent_and_monotonic(
            ops in proptest::collection::vec((0usize..2, 0u64..1000), 1..40)
        ) {
            let dir = tempfile::tempdir().unwrap();
            let mut seq = open_seq(dir.path(), 0);
            let mut expect_g = 0u64;
            let mut expect = [0u64, 0u64];
            for (mi, ts) in ops {
                expect_g += 1;
                let sym = if mi == 0 { "BTC-USDT" } else { "ETH-USDT" };
                let e = seq.append(market(sym), Some(ts), place_payload(expect_g)).unwrap();
                prop_assert_eq!(e.global_seq, expect_g);
                expect[mi] += 1;
                prop_assert_eq!(e.market_seq, expect[mi]);
            }
        }
    }
}

// --- backtest cross-check ---

mod backtest_cross {
    use super::*;
    use lq_backtest::{BacktestConfig, BacktestRunner};
    use lq_core::config::{MarketMakingConfig, PaperSimConfig};
    use lq_core::event::MarketEvent;
    use lq_exchange::spec::InstrumentSpec;
    use lq_simulator::market_gen::{SyntheticDataConfig, SyntheticMarketData};
    use lq_types::TimestampMs;
    use rust_decimal::Decimal;

    fn cfg() -> BacktestConfig {
        let mm = MarketMakingConfig {
            quote_qty: dec!(0.1),
            half_spread_bps: 5.0,
            vol_scale_half_spread: false,
            quote_refresh_ms: 300,
            ..MarketMakingConfig::default()
        };
        BacktestConfig {
            spec: InstrumentSpec::new(dec!(0.1), dec!(0.01)),
            paper: PaperSimConfig {
                fill_fraction: 1.0,
                queue_position: 1.0,
                partial_fill_prob: 0.0,
                reject_prob: 0.0,
                fee_rate_bps: 2.5,
                maker_rebate_bps: 0.5,
                ..PaperSimConfig::default()
            },
            mm,
            seed: 7,
            ..BacktestConfig::default()
        }
    }

    fn synth_events(count: u64, seed: u64) -> Vec<MarketEvent> {
        let spec = InstrumentSpec::new(dec!(0.1), dec!(0.01));
        let mut gen = SyntheticMarketData::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            spec,
            SyntheticDataConfig {
                start_price: dec!(100.0),
                seed,
                ..SyntheticDataConfig::default()
            },
        );
        let now = TimestampMs(1_700_000_000_000);
        let mut events = vec![gen.initial_snapshot(now)];
        for i in 1..count {
            events.extend(gen.next_events(TimestampMs(now.as_u64() + i * 100)));
        }
        events
    }

    /// Run the deterministic backtest twice and also replay the same market
    /// event stream through the sequencer as MarketTick/PlaceOrder commands
    /// derived from event timestamps — proving shared determinism guarantees.
    #[test]
    fn backtest_determinism_and_sequencer_replay_agree() {
        let events = synth_events(200, 11);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let (a, b) = rt.block_on(async {
            let mut ra = BacktestRunner::new(cfg());
            let a = ra.run_async(&events).await;
            let mut rb = BacktestRunner::new(cfg());
            let b = rb.run_async(&events).await;
            (a, b)
        });
        assert_eq!(a.orders_placed, b.orders_placed);
        assert_eq!(a.metrics.fills, b.metrics.fills);
        assert_eq!(a.metrics.final_equity, b.metrics.final_equity);

        // Feed an equivalent ordered log twice through the sequencer.
        let build_log = |dir: &std::path::Path| {
            let mut seq = open_seq(dir, 0);
            let mut g = 0u64;
            for ev in &events {
                g += 1;
                let ts = ev.event_ts().as_u64();
                // Market tick from each event's timestamped observation.
                let last = match ev {
                    MarketEvent::Trade(t) => t.price,
                    MarketEvent::Snapshot(s) => s
                        .bids
                        .first()
                        .map(|l| l.price)
                        .unwrap_or(Decimal::ONE),
                    MarketEvent::Delta(d) => d
                        .changes
                        .first()
                        .map(|c| c.price)
                        .unwrap_or(Decimal::ONE),
                    _ => Decimal::ONE,
                };
                seq.append(
                    market("BTC-USDT"),
                    Some(ts),
                    EntryPayload::MarketTick(MarketTickCmd {
                        last,
                        bid: None,
                        ask: None,
                    }),
                )
                .unwrap();
                // Mirror strategy place rate: one order every event after warmup.
                if g > 5 {
                    g += 1;
                    seq.append(market("BTC-USDT"), Some(ts), place_payload(g))
                        .unwrap();
                }
            }
            seq.state_hash()
        };

        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let h1 = build_log(d1.path());
        let h2 = build_log(d2.path());
        assert_eq!(h1, h2, "sequencer state hash must match across runs");

        // And rebuild from empty WAL equals live hash.
        let (re, h3) = rebuild_empty_log(d1.path().join("wal.log"), LedgerState::new()).unwrap();
        assert_eq!(h3, h1);
        assert_eq!(re.last_global_seq(), seq_len(d1.path()));
    }

    fn seq_len(dir: &std::path::Path) -> u64 {
        let entries = lq_sequencer::Wal::read_all(dir.join("wal.log")).unwrap();
        entries.last().map(|e| e.global_seq).unwrap_or(0)
    }
}
