//! Stage 3 property tests: randomly generated operation logs must hold the
//! three block invariants after **every** entry, and must replay
//! deterministically (same log ⇒ same outputs, same hash, same wire bytes).
//!
//! Generation is driven by a tiny xorshift seeded from the proptest seed, so
//! a failing case is reproducible without shrinking machinery.

use std::collections::BTreeMap;

use lq_perps::{PerpsConfig, PerpsState};
use lq_sequencer::entry::{
    EntryPayload, FillCmd, FillLiquidity, LogEntry, MarketId, MarketTickCmd, PlaceOrderCmd,
    StpPolicy,
};
use lq_sequencer::state::{ApplyOutput, StateMachine};
use lq_types::{Exchange, OrderType, Side, Symbol, TimeInForce};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

const ENTRIES_PER_LOG: usize = 64;

struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        let mut s = seed ^ 0x9E37_79B9_7F4A_7C15;
        if s == 0 {
            s = 1;
        }
        Self(s)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }

    fn next_uuid(&mut self) -> Uuid {
        let hi = self.next() as u128;
        let lo = self.next() as u128;
        Uuid::from_u128((hi << 64) | lo)
    }
}

fn market(sym: &str) -> MarketId {
    MarketId::new(Exchange::Paper, Symbol(sym.to_string()))
}

fn config() -> PerpsConfig {
    PerpsConfig {
        // Exercises the `Reduce` verdict (qty up to 5 > 3).
        max_order_qty: dec!(3),
        max_notional_per_order: dec!(1_000),
        // Position/open-order caps off so the generator actually trades;
        // they have dedicated deterministic coverage in perps_behavior.
        max_position_qty: Decimal::ZERO,
        max_open_orders: 0,
        liquidation_window_ms: 10_000,
        max_liquidation_notional_per_window: dec!(500),
        ..PerpsConfig::default()
    }
}

struct LogBuilder {
    g: u64,
    seqs: BTreeMap<String, u64>,
}

impl LogBuilder {
    fn new() -> Self {
        Self {
            g: 0,
            seqs: BTreeMap::new(),
        }
    }

    fn next(&mut self, market: &MarketId, ts_ms: u64, payload: EntryPayload) -> LogEntry {
        self.g += 1;
        let key = market.to_string();
        let seq = self.seqs.entry(key).or_insert(0);
        *seq += 1;
        LogEntry {
            global_seq: self.g,
            market_seq: *seq,
            market: market.clone(),
            ts_ms,
            payload,
        }
    }
}

fn generate(seed: u64) -> Vec<LogEntry> {
    let mut rng = SimpleRng::new(seed);
    let markets = [market("BTC-USDT"), market("ETH-USDT")];
    let mut lb = LogBuilder::new();
    let mut placed: Vec<Uuid> = Vec::new();
    let mut entries = Vec::with_capacity(ENTRIES_PER_LOG);

    for i in 0..ENTRIES_PER_LOG {
        let m = &markets[(rng.below(2)) as usize];
        let ts = (i as u64 + 1) * 1_000;
        let payload = match rng.below(9) {
            0 | 1 => {
                // Deposits dominate so subaccounts stay tradable.
                let sub = rng.pick(&[0, 1, 2, 3]);
                let amount = if rng.chance(4) {
                    -Decimal::new(rng.below(400) as i64 + 1, 1) // −0.1..−40.0
                } else {
                    Decimal::new(rng.below(2_000) as i64 + 1, 1) // 0.1..200.0
                };
                EntryPayload::Transfer {
                    subaccount: sub,
                    amount,
                }
            }
            2 => {
                let price = Decimal::new(rng.below(3_000) as i64 + 10, 1); // 1.0..300.9
                EntryPayload::MarketTick(MarketTickCmd {
                    last: price,
                    bid: None,
                    ask: None,
                })
            }
            3..=5 => {
                let id = rng.next_uuid();
                placed.push(id);
                let side = if rng.chance(2) { Side::Bid } else { Side::Ask };
                let price = Decimal::new(rng.below(3_000) as i64 + 10, 1);
                let quantity = Decimal::new(rng.below(50) as i64 + 1, 1); // 0.1..5.0
                let subaccount = rng.pick(&[0, 1, 2, 3]);
                let reduce_only = rng.chance(3);
                let tif = rng.pick(&[
                    TimeInForce::Gtc,
                    TimeInForce::Ioc,
                    TimeInForce::Fok,
                    TimeInForce::PostOnly,
                ]);
                let order_type = match tif {
                    TimeInForce::PostOnly => OrderType::PostOnly,
                    TimeInForce::Ioc => OrderType::ImmediateOrCancel,
                    TimeInForce::Fok => OrderType::FillOrKill,
                    _ => OrderType::Limit,
                };
                EntryPayload::PlaceOrder(PlaceOrderCmd {
                    order_id: id,
                    client_order_id: format!("c-{i}"),
                    side,
                    order_type,
                    price: Some(price),
                    quantity,
                    time_in_force: tif,
                    owner: String::new(),
                    stp: StpPolicy::None,
                    expiration_ms: None,
                    subaccount: Some(subaccount),
                    reduce_only,
                })
            }
            6 => {
                let order_id = if placed.is_empty() {
                    rng.next_uuid()
                } else {
                    placed[rng.below(placed.len() as u64) as usize]
                };
                EntryPayload::CancelOrder { order_id }
            }
            7 => {
                let subaccount = rng.pick(&[0, 1, 2, 3]);
                let max_qty = if rng.chance(2) {
                    Some(Decimal::new(rng.below(50) as i64 + 1, 1))
                } else {
                    None
                };
                EntryPayload::Liquidate {
                    subaccount,
                    max_qty,
                }
            }
            _ => {
                if rng.chance(3) {
                    // Legacy external fill against a previously placed order.
                    let order_id = if placed.is_empty() {
                        rng.next_uuid()
                    } else {
                        placed[rng.below(placed.len() as u64) as usize]
                    };
                    EntryPayload::Fill(FillCmd {
                        order_id,
                        price: Decimal::new(rng.below(3_000) as i64 + 10, 1),
                        quantity: Decimal::new(rng.below(10) as i64 + 1, 1),
                        fee: Decimal::ZERO,
                        liquidity: if rng.chance(2) {
                            FillLiquidity::Maker
                        } else {
                            FillLiquidity::Taker
                        },
                    })
                } else {
                    let rate = rng.pick(&[dec!(0.01), dec!(-0.01), dec!(0.005), Decimal::ZERO]);
                    EntryPayload::SettleFunding { rate }
                }
            }
        };
        entries.push(lb.next(m, ts, payload));
    }
    entries
}

/// Apply the log to a fresh state, asserting the block invariants after
/// every entry. Returns (state, per-entry outputs).
fn run(log: &[LogEntry]) -> (PerpsState, Vec<Vec<ApplyOutput>>) {
    let mut sm = PerpsState::with_config(config());
    let mut outputs = Vec::with_capacity(log.len());
    for (i, e) in log.iter().enumerate() {
        let out = sm.apply(e).unwrap_or_else(|err| panic!("entry {i} must not gap: {err}"));
        sm.check_invariants()
            .unwrap_or_else(|v| panic!("invariant violated after entry {i} ({e:?}): {v}"));
        outputs.push(out);
    }
    (sm, outputs)
}

proptest::proptest! {
    #[test]
    fn random_logs_hold_invariants_and_replay_deterministically(seed in 0u64..u64::MAX) {
        let log = generate(seed);
        let (sm1, o1) = run(&log);
        let (sm2, o2) = run(&log);
        assert_eq!(sm1.state_hash(), sm2.state_hash());
        assert_eq!(o1, o2, "same log must produce identical outputs");

        // Wire roundtrip preserves the canonical hash.
        let bytes = sm1.encode_state().expect("encode");
        let back = PerpsState::decode_state(&bytes).expect("decode");
        assert_eq!(back.state_hash(), sm1.state_hash());
        assert_eq!(back, sm1);
    }

    #[test]
    fn random_logs_keep_collateral_conserved_and_positions_zero_sum(
        seed in 0u64..u64::MAX,
    ) {
        // Redundant with the combined test above (run() asserts after every
        // entry), kept as a separately named property for clear reporting.
        let log = generate(seed ^ 0xA5A5_A5A5_A5A5_A5A5);
        let (sm, _) = run(&log);
        sm.check_invariants().expect("invariants hold at end of log");
    }
}
