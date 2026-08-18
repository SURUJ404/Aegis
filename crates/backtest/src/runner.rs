//! The backtest runner: replays events through the engine stack.

use std::sync::Arc;

use lq_core::bus::EventBus;
use lq_core::config::{MarketMakingConfig, PaperSimConfig, RiskConfig};
use lq_core::event::{ExecutionEvent, MarketEvent};
use lq_core::models::{MarketState, MarketRegime, Order, StrategyDecision};
use lq_execution::paper::PaperExecutionVenue;
use lq_execution::positions::PositionManager;
use lq_execution::venue::{ExecutionVenue, OrderPlacement};
use lq_exchange::spec::InstrumentSpec;
use lq_orderbook::book::OrderBook;
use lq_risk::engine::RiskEngine;
use lq_risk::decision::RiskDecision;
use lq_strategy::{MarketMakingStrategy, StrategyEngine};
use lq_types::{Amount, Exchange, OrderType, Price, Qty, Side, Symbol, TimestampMs};
use parking_lot::RwLock;
use rust_decimal::Decimal;


use crate::metrics::{compute, BacktestResult, EquitySample, PerfMetrics};

/// Configuration for one backtest run. All knobs are explicit.
#[derive(Debug, Clone)]
pub struct BacktestConfig {
    pub venue: Exchange,
    pub symbol: Symbol,
    pub spec: InstrumentSpec,
    /// Paper-execution assumptions. The runner forces `reject_prob = 0` and
    /// disables latency so runs are deterministic.
    pub paper: PaperSimConfig,
    pub mm: MarketMakingConfig,
    pub risk: RiskConfig,
    pub initial_capital: Amount,
    /// Sample equity every N market events.
    pub equity_sample_every: u64,
    /// Annualization factor for the Sharpe ratio (samples per year).
    pub periods_per_year: f64,
    /// RNG seed for the venue's fill/partial-fill decisions.
    pub seed: u64,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            spec: InstrumentSpec::new(Decimal::new(1, 4), Decimal::new(1, 8)),
            paper: PaperSimConfig::default(),
            mm: MarketMakingConfig::default(),
            risk: RiskConfig::default(),
            initial_capital: Amount::from(100_000),
            equity_sample_every: 10,
            periods_per_year: 252.0 * 6.0,
            seed: 0x5EED,
        }
    }
}

/// Runs a deterministic backtest. Construct once, call [`run`](BacktestRunner::run)
/// with a recorded event sequence.
pub struct BacktestRunner {
    cfg: BacktestConfig,
    book: Arc<RwLock<OrderBook>>,
    state: lq_core::state::EngineState,
    strategies: StrategyEngine,
    risk: RiskEngine,
    venue: Arc<PaperExecutionVenue>,
    bus: Arc<EventBus>,
    seq: u64,
    filled: u64,
    rejected: u64,
    placed: u64,
    fees_total: Amount,
    realized_pnl: Amount,
    trades: u64,
    wins: u64,
    samples: Vec<EquitySample>,
    last_price: Option<Price>,
    vol_ema: f64,
    last_quote_ts: Option<TimestampMs>,
}

impl BacktestRunner {
    pub fn new(cfg: BacktestConfig) -> Self {
        let bus = Arc::new(EventBus::new());
        let state = lq_core::state::EngineState::new();

        let mut paper = cfg.paper.clone();
        paper.reject_prob = 0.0;

        let book = Arc::new(RwLock::new(OrderBook::new(
            cfg.venue,
            cfg.symbol.clone(),
            cfg.spec,
        )));

        // Market orders price against the live backtest book.
        let book_for_prices = book.clone();
        let price_provider: lq_execution::paper::PriceProvider =
            Arc::new(move |sym| {
                let b = book_for_prices.read();
                match (b.best_bid(), b.best_ask()) {
                    (Some(bid), Some(ask)) if b.symbol == *sym => Some((bid, ask)),
                    _ => None,
                }
            });

        let venue = Arc::new(
            PaperExecutionVenue::with_seed(cfg.venue, paper, bus.clone(), cfg.seed, false)
                .with_price_provider(price_provider),
        );

        let mut strategies = StrategyEngine::new();
        strategies.register(Box::new(MarketMakingStrategy::new(
            cfg.symbol.clone(),
            cfg.venue,
            cfg.mm.clone(),
        )));

        let risk = RiskEngine::new(cfg.risk.clone(), state.clone());

        Self {
            cfg,
            book,
            state,
            strategies,
            risk,
            venue,
            bus,
            seq: 0,
            filled: 0,
            rejected: 0,
            placed: 0,
            fees_total: Amount::ZERO,
            realized_pnl: Amount::ZERO,
            trades: 0,
            wins: 0,
            samples: Vec::new(),
            last_price: None,
            vol_ema: 0.0,
            last_quote_ts: None,
        }
    }

pub fn config(&self) -> &BacktestConfig {
        &self.cfg
    }

    /// Blocking entry point for callers with **no** current Tokio runtime
    /// (e.g. a CLI). Builds a current-thread runtime internally. Callers
    /// already inside a runtime must use [`run_async`].
    pub fn run_sync(&mut self, events: &[MarketEvent]) -> BacktestResult {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(async move { self.run_async(events).await })
    }

    /// Replay a recorded event sequence. Deterministic: same input events
    /// produce the same result.
    pub async fn run_async(&mut self, events: &[MarketEvent]) -> BacktestResult {
        let mut exec_sub = self.bus.execution().subscribe();

for event in events {
            self.seq += 1;
            self.on_market_event(event).await;

            // Drain any bus events (accepted/cancelled/market-order fills)
            // published during this step. Yielding first lets the broker task
            // fan them out on the current-thread runtime.
            tokio::task::yield_now().await;
            while let Ok(ev) = exec_sub.try_recv() {
                self.on_execution_event(&ev).await;
            }

            if self.seq % self.cfg.equity_sample_every == 0 {
                self.sample_equity();
            }
        }

// Final mark.
        self.sample_equity();

        let metrics = self.finish_metrics();
        BacktestResult {
            events_seen: self.seq,
            orders_placed: self.placed,
            rejected_orders: self.rejected,
            open_orders_at_end: self.venue.working_order_ids().len(),
            metrics,
        }
    }

    async fn on_market_event(&mut self, event: &MarketEvent) {
        match event {
            MarketEvent::Snapshot(s) => {
                self.book.write().apply_snapshot(s);
            }
            MarketEvent::Delta(d) => {
                let _ = self.book.write().apply_delta(d);
            }
            MarketEvent::Trade(t) => {
                if let Some(last) = self.last_price {
                    let r = ((t.price - last) / last).as_f64();
                    let alpha = 0.02;
                    self.vol_ema += alpha * (r * r - self.vol_ema);
                }
                self.last_price = Some(t.price);
            }
            _ => {}
        }

        self.sweep_maker_fills().await;
        self.run_strategy().await;
    }

    async fn run_strategy(&mut self) {
        let (best_bid, best_ask) = {
            let b = self.book.read();
            match (b.best_bid(), b.best_ask()) {
                (Some(bid), Some(ask)) => (bid, ask),
                _ => return,
            }
        };
        if best_bid <= Amount::ZERO || best_ask <= Amount::ZERO {
            return;
        }

        let market = self.market_state(best_bid, best_ask);
        let halted = self.risk.is_halted();

        let inventory = self
            .state
            .inventory
            .get(&self.cfg.symbol)
            .map(|i| i.clone());
        let position = self
            .state
            .positions
            .get(&(self.cfg.venue, self.cfg.symbol.clone()))
            .map(|p| p.clone());

        let decisions = self
            .strategies
            .on_market_state(&market, inventory.as_ref(), position.as_ref(), halted, true);

        for decision in decisions {
            match decision {
                StrategyDecision::Quote(intent) => {
                    if intent.venue != self.cfg.venue {
                        continue;
                    }
                    // Respect the strategy's quote refresh cadence: leave
                    // resting quotes untouched until the interval elapses.
                    let due = match self.last_quote_ts {
                        Some(last) => market.event_ts.as_u64().saturating_sub(last.as_u64())
                            >= self.cfg.mm.quote_refresh_ms,
                        None => true,
                    };
                    if !due {
                        continue;
                    }
                    self.last_quote_ts = Some(market.event_ts);
                    // Refresh both legs: cancel leftovers first.
                    let _ = self.venue.cancel_all(Some(&intent.symbol)).await;
                    if let Some(bid) = intent.bid {
                        let mut o = Order::new(
                            self.cfg.venue,
                            intent.symbol.clone(),
                            Side::Bid,
                            OrderType::Limit,
                            Some(bid.price),
                            bid.qty,
                        );
                        self.place_checked(&mut o, market.mid).await;
                    }
                    if let Some(ask) = intent.ask {
                        let mut o = Order::new(
                            self.cfg.venue,
                            intent.symbol.clone(),
                            Side::Ask,
                            OrderType::Limit,
                            Some(ask.price),
                            ask.qty,
                        );
                        self.place_checked(&mut o, market.mid).await;
                    }
                }
                StrategyDecision::MarketOrder(sig) => {
                    if sig.venue != self.cfg.venue {
                        continue;
                    }
                    let mut o = Order::new(
                        self.cfg.venue,
                        sig.symbol.clone(),
                        sig.side,
                        OrderType::Market,
                        Some(sig.price),
                        sig.qty,
                    );
                    self.place_checked(&mut o, market.mid).await;
                }
                StrategyDecision::StandDown { .. } => {
                    let _ = self.venue.cancel_all(Some(&self.cfg.symbol)).await;
                }
                StrategyDecision::Hold => {}
            }
        }
    }

async fn place_checked(&mut self, order: &mut Order, mark: Price) {
        match self.risk.validate_order(order, mark) {
            RiskDecision::Allow => {
                self.place(order).await;
            }
            RiskDecision::Reduce { qty, .. } => {
                if qty > Qty::ZERO {
                    order.quantity = qty;
                    self.place(order).await;
                }
            }
            RiskDecision::Reject(r) => {
                self.rejected += 1;
                tracing::debug!(order = %order.order_id, code = ?r.code, "risk reject");
            }
            RiskDecision::Halt(_) => {
                self.rejected += 1;
            }
        }
    }

    async fn place(&mut self, order: &mut Order) {
        match self.venue.place_order(order).await {
            Ok(OrderPlacement { status, .. }) => {
                self.placed += 1;
                if matches!(status, lq_types::OrderStatus::Rejected) {
                    self.rejected += 1;
                }
            }
            Err(_) => self.rejected += 1,
        }
    }

/// Match resting maker orders against the current book.
    async fn sweep_maker_fills(&mut self) {
        let ids = self.venue.working_order_ids();
        for id in ids {
            let Some(snapshot) = self.venue.order_snapshot(id) else {
                continue;
            };
            if snapshot.status.is_terminal() {
                continue;
            }
            let (best_bid, best_ask) = {
                let b = self.book.read();
                match (b.best_bid(), b.best_ask()) {
                    (Some(bid), Some(ask)) => (bid, ask),
                    _ => continue,
                }
            };
            let Some(limit) = snapshot.price else {
                continue;
            };
            let crossed = match snapshot.side {
                Side::Bid => best_ask <= limit,
                Side::Ask => best_bid >= limit,
            };
            if !crossed {
                continue;
            }
            let remaining = (snapshot.quantity - snapshot.filled_quantity).max(Qty::ZERO);
            if remaining <= Qty::ZERO {
                continue;
            }
            if let Ok(fill) = self.venue.report_fill(id, limit, remaining, false).await {
                self.on_execution_event(&ExecutionEvent::Fill(fill)).await;
            }
        }
    }

    async fn on_execution_event(&mut self, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::Fill(fill) => {
                // Realized PnL delta across the fill, used to count trades.
                let before = self
                    .state
                    .inventory
                    .get(&fill.symbol)
                    .map(|i| i.clone())
                    .map(|i| i.realized_pnl)
                    .unwrap_or_default();
                PositionManager::on_execution_event(&self.state, event);
                let after = self
                    .state
                    .inventory
                    .get(&fill.symbol)
                    .map(|i| i.clone())
                    .map(|i| i.realized_pnl)
                    .unwrap_or_default();
                let delta = after - before;
                self.realized_pnl = after;
                self.filled += 1;
                self.fees_total += fill.fee;

                // A fill that reduces the absolute position closes (part of) a
                // round trip.
                let now_positive = self
                    .state
                    .inventory
                    .get(&fill.symbol)
                    .map(|i| i.clone())
                    .map(|i| i.net_qty.is_sign_positive())
                    .unwrap_or(false);
                let signed = match fill.side {
                    Side::Bid => fill.qty,
                    Side::Ask => -fill.qty,
                };
                let is_closing = (signed.is_sign_positive() && !now_positive)
                    || (signed.is_sign_negative() && now_positive);
                if is_closing {
                    self.trades += 1;
                    if delta > Amount::ZERO {
                        self.wins += 1;
                    }
                }
            }
            other => PositionManager::on_execution_event(&self.state, other),
        }
    }

    fn market_state(&self, best_bid: Price, best_ask: Price) -> MarketState {
        let mid = (best_bid + best_ask) / Decimal::from(2);
        let spread = best_ask - best_bid;
        let spread_bps = if mid > Amount::ZERO {
            ((spread / mid) * Decimal::from(10_000)).as_f64()
        } else {
            0.0
        };
        let b = self.book.read();
        let imbalance = b.imbalance(3);
        let depth_bid = b.depth(Side::Bid, 3);
        let depth_ask = b.depth(Side::Ask, 3);
        let num_bid = b.num_levels(Side::Bid) as u32;
        let num_ask = b.num_levels(Side::Ask) as u32;
        let event_ts = b.last_update_ms();
        MarketState {
            venue: self.cfg.venue,
            symbol: self.cfg.symbol.clone(),
            event_ts,
            best_bid,
            best_ask,
            mid,
            spread,
            spread_bps,
            orderbook_imbalance: imbalance,
            microprice: mid,
            vwap: mid,
            depth_bid,
            depth_ask,
            num_bid_levels: num_bid,
            num_ask_levels: num_ask,
            buy_volume: Qty::ZERO,
            sell_volume: Qty::ZERO,
            trade_intensity: 0.0,
            realized_volatility: self.vol_ema.sqrt(),
            price_impact_estimate: 0.0,
            regime: MarketRegime::Normal,
            stale: false,
        }
    }

    fn sample_equity(&mut self) {
        let (bid, ask) = {
            let b = self.book.read();
            (b.best_bid().unwrap_or_default(), b.best_ask().unwrap_or_default())
        };
        let mark = if bid > Amount::ZERO && ask > Amount::ZERO {
            (bid + ask) / Decimal::from(2)
        } else {
            bid
        };

        // Equity = capital + realized pnl + Σ inventory × mark.
        let mut unrealized = Amount::ZERO;
        for entry in self.state.inventory.iter() {
            let inv = entry.value();
            unrealized += inv.net_qty * mark;
        }
        let equity = self.cfg.initial_capital + self.realized_pnl + unrealized;
        self.samples.push(EquitySample {
            events: self.seq,
            equity,
        });
    }

    fn finish_metrics(&self) -> PerfMetrics {
        let mut m = compute(&self.samples, self.cfg.periods_per_year);
        m.fees_total = self.fees_total;
        m.fills = self.filled;
        m.trades = self.trades;
        m.win_rate = if self.trades > 0 {
            self.wins as f64 / self.trades as f64
        } else {
            0.0
        };
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{OrderBookLevel, OrderBookSnapshot};
    use lq_simulator::market_gen::{SyntheticDataConfig, SyntheticMarketData};
    use lq_types::TimestampMs;
    use rust_decimal_macros::dec;

    fn cfg() -> BacktestConfig {
        let mut mm = MarketMakingConfig::default();
        mm.quote_qty = dec!(0.1);
        mm.half_spread_bps = 5.0;
        mm.vol_scale_half_spread = false;
        mm.quote_refresh_ms = 300;
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
let now = TimestampMs::now();
        let mut events = vec![gen.initial_snapshot(now)];
        for i in 1..count {
            events.extend(gen.next_events(TimestampMs(now.as_u64() + i * 100)));
        }
        events
    }

#[test]
    fn deterministic_across_runs() {
        let events = synth_events(300, 11);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (a, b) = rt.block_on(async move {
            let mut ra = BacktestRunner::new(cfg());
            let a = ra.run_async(&events).await;
            let mut rb = BacktestRunner::new(cfg());
            let b = rb.run_async(&events).await;
            (a, b)
        });
        assert_eq!(a.events_seen, b.events_seen);
        assert_eq!(a.orders_placed, b.orders_placed);
        assert_eq!(a.metrics.fills, b.metrics.fills);
        assert_eq!(a.metrics.final_equity, b.metrics.final_equity);
        assert_eq!(a.metrics.max_drawdown, b.metrics.max_drawdown);
    }

    #[test]
    fn fills_when_book_crosses_quote() {
        // Crafted sequence: snapshot at 99.8/100.2, then a delta that moves
        // the ask down to 99.6 so the resting 99.95 bid gets crossed.
        let events = vec![
            MarketEvent::Snapshot(OrderBookSnapshot {
                venue: Exchange::Paper,
                symbol: Symbol("BTC-USDT".into()),
                sequence: 1,
                event_ts: TimestampMs::now(),
                exchange_ts: TimestampMs::now(),
                bids: vec![OrderBookLevel::new(dec!(99.8), dec!(10.0))],
                asks: vec![OrderBookLevel::new(dec!(100.2), dec!(10.0))],
            }),
            MarketEvent::Delta(lq_core::models::OrderBookDelta {
                venue: Exchange::Paper,
                symbol: Symbol("BTC-USDT".into()),
                sequence: 2,
                event_ts: TimestampMs::now(),
                exchange_ts: TimestampMs::now(),
                changes: vec![
                    lq_core::models::LevelChange {
                        side: Side::Bid,
                        price: dec!(99.2),
                        qty: dec!(5.0),
                    },
                    lq_core::models::LevelChange {
                        side: Side::Ask,
                        price: dec!(99.6),
                        qty: dec!(5.0),
                    },
                ],
                clear: false,
            }),
        ];
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(async move {
            let mut runner = BacktestRunner::new(cfg());
            runner.run_async(&events).await
        });
        assert_eq!(res.orders_placed, 2, "expected one two-sided quote");
        assert!(res.metrics.fills >= 1, "expected at least one fill");
        assert_ne!(res.metrics.fees_total, Amount::ZERO, "fees applied");
    }

    #[test]
    fn synth_run_is_reproducible_and_smokes() {
        let events = synth_events(400, 13);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(async move {
            let mut runner = BacktestRunner::new(cfg());
            runner.run_async(&events).await
        });
        assert!(res.orders_placed >= 2, "expected quotes placed");
        assert!(res.metrics.final_equity > Amount::ZERO);
    }

    #[test]
    fn flat_market_quotes_both_sides() {
        let events = vec![MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs::now(),
            exchange_ts: TimestampMs::now(),
            bids: vec![OrderBookLevel::new(dec!(99.0), dec!(10.0))],
            asks: vec![OrderBookLevel::new(dec!(101.0), dec!(10.0))],
})];
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(async move {
            let mut runner = BacktestRunner::new(cfg());
            runner.run_async(&events).await
        });
        // One snapshot → strategy quotes 2 orders (bid+ask).
        assert_eq!(res.orders_placed, 2);
        assert_eq!(res.open_orders_at_end, 2);
    }
}


