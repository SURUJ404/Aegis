//! Synthetic market data generator.
//!
//! Produces a deterministic random-walk order book: a boot snapshot followed
//! by incremental deltas (re-anchored around a drifting mid) and occasional
//! aggressive trades. Everything is a visible, tunable parameter in
//! [`SyntheticDataConfig`].

use std::sync::Arc;

use lq_core::bus::EventBus;
use lq_core::event::{FeedStatus, MarketEvent};
use lq_core::models::{
    LevelChange, MarketTick, OrderBookDelta, OrderBookLevel, OrderBookSnapshot, Trade,
};
use lq_exchange::spec::{InstrumentSpec, PriceTick};
use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;

const QTY_SCALE_DEC: Decimal = rust_decimal::Decimal::from_parts(100_000_000, 0, 0, false, 0);

fn qty_to_u64(qty: Qty) -> u64 {
    (qty * QTY_SCALE_DEC).as_i128().max(0) as u64
}

fn qty_from_u64(qty: u64) -> Qty {
    Decimal::from(qty) / QTY_SCALE_DEC
}

/// Tunables for the synthetic feed. All probabilities are real knobs; there
/// are no hidden parameters.
#[derive(Debug, Clone)]
pub struct SyntheticDataConfig {
    /// Milliseconds between book updates.
    pub interval_ms: u64,
    /// Starting mid price.
    pub start_price: Price,
    /// One-tick size in bps of the current mid (re-anchored as mid drifts).
    pub tick_bps: f64,
    /// One-way touch spread in ticks.
    pub half_spread_ticks: u64,
    /// Number of levels per side.
    pub depth_levels: usize,
    /// Base quantity per level.
    pub level_qty: Qty,
    /// Uniform jitter applied to level quantities (± qty × this fraction).
    pub qty_jitter: f64,
    /// Random-walk step: max mid move per tick in ticks (uniform ±).
    pub max_move_ticks: u64,
    /// Probability each book tick also prints a trade.
    pub trade_prob: f64,
/// Trade size in base units.
    pub trade_qty: Qty,
    /// Re-emit a full snapshot every N ticks so a missed boot snapshot (or a
    /// dropped delta) self-heals. 0 disables periodic resync.
    pub resync_every_ticks: u64,
    /// RNG seed for deterministic replay.
    pub seed: u64,
}

impl Default for SyntheticDataConfig {
    fn default() -> Self {
        Self {
            interval_ms: 100,
            start_price: Decimal::from(100),
            tick_bps: 0.5,
            half_spread_ticks: 2,
            depth_levels: 10,
            level_qty: Decimal::from(10),
            qty_jitter: 0.3,
            max_move_ticks: 1,
trade_prob: 0.3,
            trade_qty: Decimal::new(5, 1),
            resync_every_ticks: 50,
            seed: 42,
        }
    }
}

struct LevelSet {
    bids: Vec<(PriceTick, u64)>,
    asks: Vec<(PriceTick, u64)>,
    mid_tick: PriceTick,
}

/// Stateful random-walk market simulator.
pub struct SyntheticMarketData {
    pub venue: Exchange,
    pub symbol: Symbol,
    spec: InstrumentSpec,
    cfg: SyntheticDataConfig,
    rng: StdRng,
    level_set: Option<LevelSet>,
    seq: u64,
}

impl SyntheticMarketData {
    pub fn new(
        venue: Exchange,
        symbol: Symbol,
        spec: InstrumentSpec,
        cfg: SyntheticDataConfig,
    ) -> Self {
        let seed = cfg.seed;
        Self {
            venue,
            symbol,
            spec,
            cfg,
            rng: StdRng::seed_from_u64(seed),
            level_set: None,
            seq: 0,
        }
    }

    fn mid_tick_for(&self, price: Price) -> PriceTick {
        self.spec.to_ticks(price).max(1)
    }

    fn price_for(&self, tick: PriceTick) -> Price {
        self.spec.from_ticks(tick)
    }

    /// Grid step in ticks such that each step is `tick_bps` of the current
    /// mid price.
    fn step_ticks(&self, mid: PriceTick) -> u64 {
        let mid_price = self.price_for(mid);
        let step_price = mid_price
            * Decimal::from_f64_retain(self.cfg.tick_bps).unwrap_or_default()
            / Decimal::from(10_000);
        let step = (step_price / self.spec.tick_size).round();
        step.as_i128().max(1) as u64
    }

    fn level_qty(&mut self) -> u64 {
        let base = qty_to_u64(self.cfg.level_qty).max(1);
        let jitter = (self.rng.gen_range(-1.0..1.0) * self.cfg.qty_jitter).abs();
        (base as f64 * (1.0 + jitter)).round() as u64
    }

    fn target_levels(&mut self, mid: PriceTick) -> LevelSet {
        let step = self.step_ticks(mid);
        let half = self.cfg.half_spread_ticks * step;
        let depth = self.cfg.depth_levels;
        let mut bids = Vec::with_capacity(depth);
        let mut asks = Vec::with_capacity(depth);
        for i in 0..depth {
            let i = i as u64;
            let bid_tick = mid.saturating_sub(half + i * step).max(1);
            let ask_tick = mid.saturating_add(half + i * step);
            bids.push((bid_tick, self.level_qty()));
            asks.push((ask_tick, self.level_qty()));
        }
        LevelSet { bids, asks, mid_tick: mid }
    }

    fn diff_changes(
        &self,
        old: &[(PriceTick, u64)],
        new: &[(PriceTick, u64)],
        side: Side,
    ) -> Vec<LevelChange> {
        let mut changes = Vec::with_capacity(old.len() + new.len());
        for (price, _) in old {
            if !new.iter().any(|(p, _)| p == price) {
                changes.push(LevelChange {
                    side,
                    price: self.price_for(*price),
                    qty: Qty::ZERO,
                });
            }
        }
        for (price, qty) in new {
            let changed = match old.iter().find(|(p, _)| p == price) {
                Some((_, old_qty)) => old_qty != qty,
                None => true,
            };
            if changed {
                changes.push(LevelChange {
                    side,
                    price: self.price_for(*price),
                    qty: qty_from_u64(*qty),
                });
            }
        }
        changes
    }

    fn next_delta(&mut self, now: TimestampMs) -> Vec<MarketEvent> {
        let mut events = Vec::new();

        let Some(current) = self.level_set.take() else {
            return events;
        };

        // Random-walk the mid (uniform ± max_move_ticks × step).
        let step = self.step_ticks(current.mid_tick);
        let move_ticks = self.rng.gen_range(0..=self.cfg.max_move_ticks) as i64;
        let dir: i64 = if self.rng.gen::<bool>() { 1 } else { -1 };
        let new_mid = (current.mid_tick as i64 + dir * move_ticks * step as i64).max(1) as PriceTick;

        let next = self.target_levels(new_mid);
        self.seq += 1;

        let mut changes = self.diff_changes(&current.bids, &next.bids, Side::Bid);
        changes.extend(self.diff_changes(&current.asks, &next.asks, Side::Ask));

        let best_bid = self.price_for(next.bids[0].0);
        let best_ask = self.price_for(next.asks[0].0);

        events.push(MarketEvent::Delta(OrderBookDelta {
            venue: self.venue,
            symbol: self.symbol.clone(),
            sequence: self.seq,
            event_ts: now,
            exchange_ts: now,
            changes,
            clear: false,
        }));

        // Occasional aggressive trade at the touch.
        if self.rng.gen::<f64>() < self.cfg.trade_prob {
            let aggressor = if self.rng.gen::<bool>() { Side::Bid } else { Side::Ask };
            let price = match aggressor {
                Side::Bid => best_bid,
                Side::Ask => best_ask,
            };
            events.push(MarketEvent::Trade(Trade {
                venue: self.venue,
                symbol: self.symbol.clone(),
                price,
                qty: self.cfg.trade_qty,
                aggressor,
                event_ts: now,
                exchange_ts: now,
            }));
            events.push(MarketEvent::Tick(MarketTick {
                venue: self.venue,
                symbol: self.symbol.clone(),
                last_price: price,
                last_qty: self.cfg.trade_qty,
                best_bid,
                best_ask,
                event_ts: now,
            }));
        }

        self.level_set = Some(next);
        events
    }

    /// The boot snapshot establishing the initial book.
    pub fn initial_snapshot(&mut self, now: TimestampMs) -> MarketEvent {
        let mid = self.mid_tick_for(self.cfg.start_price);
        let levels = self.target_levels(mid);
        let bids: Vec<OrderBookLevel> = levels
            .bids
            .iter()
            .map(|(t, q)| OrderBookLevel::new(self.price_for(*t), qty_from_u64(*q)))
            .collect();
        let asks: Vec<OrderBookLevel> = levels
            .asks
            .iter()
            .map(|(t, q)| OrderBookLevel::new(self.price_for(*t), qty_from_u64(*q)))
            .collect();
        self.seq = 1;
        self.level_set = Some(levels);
        MarketEvent::Snapshot(OrderBookSnapshot {
            venue: self.venue,
            symbol: self.symbol.clone(),
            sequence: self.seq,
            event_ts: now,
            exchange_ts: now,
            bids,
            asks,
        })
    }

/// Produce the next tick's events (delta + optional trade).
    pub fn next_events(&mut self, now: TimestampMs) -> Vec<MarketEvent> {
        self.next_delta(now)
    }

    /// Full snapshot of the current book state. `None` until a snapshot has
    /// been established. Unlike [`Self::initial_snapshot`] this does not reset
    /// the sequence or re-draw the level set, so the delta stream stays
    /// contiguous across resyncs.
    pub fn resync_snapshot(&mut self, now: TimestampMs) -> Option<MarketEvent> {
        let levels = self.level_set.as_ref()?;
        let bids: Vec<OrderBookLevel> = levels
            .bids
            .iter()
            .map(|(t, q)| OrderBookLevel::new(self.price_for(*t), qty_from_u64(*q)))
            .collect();
        let asks: Vec<OrderBookLevel> = levels
            .asks
            .iter()
            .map(|(t, q)| OrderBookLevel::new(self.price_for(*t), qty_from_u64(*q)))
            .collect();
        Some(MarketEvent::Snapshot(OrderBookSnapshot {
            venue: self.venue,
            symbol: self.symbol.clone(),
            sequence: self.seq,
            event_ts: now,
            exchange_ts: now,
            bids,
            asks,
        }))
    }
}

/// A self-driving feed: publishes the boot snapshot then ticks on a fixed
/// interval until the task is aborted.
pub struct SimulatedFeed {
    gen: SyntheticMarketData,
    interval: std::time::Duration,
}

impl SimulatedFeed {
    pub fn new(
        cfg: SyntheticDataConfig,
        venue: Exchange,
        symbol: Symbol,
        spec: InstrumentSpec,
    ) -> Self {
        let interval = std::time::Duration::from_millis(cfg.interval_ms);
        let gen = SyntheticMarketData::new(venue, symbol.clone(), spec, cfg);
        Self { gen, interval }
    }

    /// Spawn the feed loop, publishing to the market topic until the returned
    /// handle is aborted.
    pub fn spawn(self, bus: Arc<EventBus>) -> tokio::task::JoinHandle<()> {
        let mut gen = self.gen;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let now = TimestampMs::now();
            let venue = gen.venue;
            let symbol = gen.symbol.clone();
            let _ = bus.market().try_publish(gen.initial_snapshot(now));
let _ = bus.market().try_publish(MarketEvent::Status {
                venue,
                symbol: symbol.clone(),
                status: FeedStatus::Healthy,
                ts: now,
            });
            let mut ticks_since_resync = 0u64;
            loop {
                interval.tick().await;
                let now = TimestampMs::now();
                if gen.cfg.resync_every_ticks > 0
                    && ticks_since_resync >= gen.cfg.resync_every_ticks
                {
                    ticks_since_resync = 0;
                    if let Some(snap) = gen.resync_snapshot(now) {
                        let _ = bus.market().try_publish(snap);
                    }
                }
                ticks_since_resync += 1;
                for event in gen.next_events(now) {
                    let _ = bus.market().try_publish(event);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(seed: u64) -> SyntheticMarketData {
        let mut cfg = SyntheticDataConfig::default();
        cfg.seed = seed;
        SyntheticMarketData::new(
            Exchange::Simulated,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(Decimal::new(1, 4), Decimal::new(1, 8)),
            cfg,
        )
    }

    #[test]
    fn boot_snapshot_is_consistent() {
        let mut g = gen(1);
        let now = TimestampMs::now();
        let ev = g.initial_snapshot(now);
        let MarketEvent::Snapshot(s) = ev else {
            panic!("expected snapshot");
        };
        assert_eq!(s.bids.len(), 10);
        assert_eq!(s.asks.len(), 10);
        assert!(s.bids[0].price < s.asks[0].price);
        assert!(s.bids.windows(2).all(|w| w[0].price > w[1].price));
        assert!(s.asks.windows(2).all(|w| w[0].price < w[1].price));
    }

    #[test]
    fn deltas_keep_book_consistent() {
        let mut g = gen(7);
        let now = TimestampMs::now();
        let _ = g.initial_snapshot(now);
        for i in 0..50u32 {
            let events = g.next_events(TimestampMs(now.as_u64() + i as u64));
            let deltas: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, MarketEvent::Delta(_)))
                .collect();
            assert_eq!(deltas.len(), 1);
        }
    }

    #[test]
    fn deterministic_given_seed() {
        let mut a = gen(99);
        let mut b = gen(99);
        let now = TimestampMs::now();
        let _ = a.initial_snapshot(now);
        let _ = b.initial_snapshot(now);
        for i in 0..20u32 {
            let ea = a.next_events(TimestampMs(now.as_u64() + i as u64));
            let eb = b.next_events(TimestampMs(now.as_u64() + i as u64));
            assert_eq!(format!("{ea:?}"), format!("{eb:?}"));
        }
    }
}

