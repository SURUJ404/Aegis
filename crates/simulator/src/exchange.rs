//! Paper exchange: a local matching venue driven by simulated market data.
//!
//! Holds the venue's [`OrderBook`] (shared with the price provider used for
//! market-order pricing), consumes [`MarketEvent`]s to keep the book current,
//! and matches resting orders whenever the book crosses them. Fill behaviour
//! is fully governed by [`PaperSimConfig`]: `fill_fraction` (probability a
//! crossed order actually fills), `partial_fill_prob` / `partial_fill_fraction`
//! and `queue_position`. Fees and rebates are applied per the config.

use std::sync::Arc;

use lq_core::bus::EventBus;
use lq_core::config::PaperSimConfig;
use lq_core::event::MarketEvent;
use lq_execution::paper::PaperExecutionVenue;
use lq_execution::venue::VenueError;
use lq_exchange::spec::InstrumentSpec;
use lq_orderbook::book::OrderBook as LocalBook;
use lq_types::{Exchange, Qty, Side, Symbol};
use parking_lot::{Mutex, RwLock};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;

/// A single venue's paper trading loop. Not `Send`-safe to share by itself;
/// the engine drives it from one task.
pub struct PaperExchange {
    pub venue: Exchange,
    pub symbol: Symbol,
    book: Arc<RwLock<LocalBook>>,
    ven: Arc<PaperExecutionVenue>,
    cfg: PaperSimConfig,
    rng: Mutex<StdRng>,
    matched: u64,
}

impl PaperExchange {
    pub fn new(
        venue: Exchange,
        symbol: Symbol,
        spec: InstrumentSpec,
        ven: Arc<PaperExecutionVenue>,
        cfg: PaperSimConfig,
        seed: u64,
    ) -> Self {
        let book = Arc::new(RwLock::new(LocalBook::new(venue, symbol.clone(), spec)));
        Self {
            venue,
            symbol,
            book,
            ven,
            cfg,
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
            matched: 0,
        }
    }

    /// Shared handle to the local book (read for market-state / pricing).
    pub fn book(&self) -> Arc<RwLock<LocalBook>> {
        self.book.clone()
    }

    /// Wire the venue's price provider to this exchange's book so market
    /// orders price against the live simulated touch. Called once by the
    /// engine after construction.
    pub fn connect_prices(&self) {
        let book = self.book.clone();
        let symbol = self.symbol.clone();
        self.ven.set_price_provider(Arc::new(move |sym| {
            if sym != &symbol {
                return None;
            }
            let b = book.read();
            match (b.best_bid(), b.best_ask()) {
                (Some(bid), Some(ask)) => Some((bid, ask)),
                _ => None,
            }
        }));
    }

    /// Apply a market event and then sweep resting orders for fills.
    pub async fn on_market_event(&mut self, event: &MarketEvent) -> Result<(), VenueError> {
        match event {
            MarketEvent::Snapshot(s) => {
                if s.symbol == self.symbol {
                    self.book.write().apply_snapshot(s);
                }
            }
            MarketEvent::Delta(d) => {
                if d.symbol == self.symbol {
                    let _ = self.book.write().apply_delta(d);
                }
            }
            _ => {}
        }
        self.sweep().await
    }

    /// Number of fills produced by matching (for metrics).
    pub fn matched(&self) -> u64 {
        self.matched
    }

    /// Check every working order against the book and fill those that cross.
    pub async fn sweep(&mut self) -> Result<(), VenueError> {
        let ids = self.ven.working_order_ids();
        for id in ids {
            let Some(order) = self.ven.order_snapshot(id) else {
                continue;
            };
            if order.status.is_terminal() {
                continue;
            }
            let (best_bid, best_ask) = {
                let b = self.book.read();
                match (b.best_bid(), b.best_ask()) {
                    (Some(bid), Some(ask)) => (bid, ask),
                    _ => continue,
                }
            };
            let Some(limit) = order.price else {
                continue;
            };
            let crossed = match order.side {
                Side::Bid => best_ask <= limit,
                Side::Ask => best_bid >= limit,
            };
            if !crossed {
                continue;
            }

            // Queue position: only a fraction of the order is at the front.
            let effective_qty =
                order.quantity * Decimal::from_f64_retain(self.cfg.queue_position.max(0.0).min(1.0))
                    .unwrap_or_default();
            if effective_qty <= Qty::ZERO {
                continue;
            }

            let mut rng = self.rng.lock();
            if rng.gen::<f64>() > self.cfg.fill_fraction {
                continue;
            }
            let fill_qty = if rng.gen::<f64>() < self.cfg.partial_fill_prob {
                let frac = self
                    .cfg
                    .partial_fill_fraction
                    .clamp(0.0, 1.0);
                effective_qty * Decimal::from_f64_retain(frac).unwrap_or_default()
            } else {
                effective_qty
            };
            drop(rng);

            // Never fill more than the remaining unfilled quantity.
            let remaining = (order.quantity - order.filled_quantity).max(Qty::ZERO);
            let fill_qty = fill_qty.min(remaining);
            if fill_qty <= Qty::ZERO {
                continue;
            }
            self.ven
                .report_fill(id, limit, fill_qty, false)
                .await?;
            self.matched += 1;
        }
        Ok(())
    }
}

/// Convenience constructor used by the trading engine: builds the local book
/// plus a fully-wired [`PaperExecutionVenue`].
pub struct PaperMarketBuilder {
    pub cfg: PaperSimConfig,
    pub seed: u64,
}

impl PaperMarketBuilder {
    pub fn new(cfg: PaperSimConfig) -> Self {
        Self { cfg, seed: 0x5EED }
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    pub fn build(
        &self,
        venue: Exchange,
        symbol: Symbol,
        spec: InstrumentSpec,
        bus: Arc<EventBus>,
    ) -> (Arc<PaperExecutionVenue>, PaperExchange) {
        let ven = Arc::new(PaperExecutionVenue::with_seed(
            venue,
            self.cfg.clone(),
            bus,
            self.seed,
            true,
        ));
        let exchange = PaperExchange::new(venue, symbol.clone(), spec, ven.clone(), self.cfg.clone(), self.seed);
        exchange.connect_prices();
        (ven, exchange)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::event::MarketEvent;
    use lq_core::models::{Order, OrderBookLevel, OrderBookSnapshot};
    use lq_execution::venue::ExecutionVenue;
    use lq_types::{OrderType, TimestampMs};
    use rust_decimal_macros::dec;

    fn spec() -> InstrumentSpec {
        InstrumentSpec::new(dec!(0.1), dec!(0.01))
    }

    fn snapshot(symbol: &Symbol) -> MarketEvent {
        let bids = vec![
            OrderBookLevel::new(dec!(99.8), dec!(10.0)),
            OrderBookLevel::new(dec!(99.6), dec!(10.0)),
        ];
        let asks = vec![
            OrderBookLevel::new(dec!(100.2), dec!(10.0)),
            OrderBookLevel::new(dec!(100.4), dec!(10.0)),
        ];
        MarketEvent::Snapshot(OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: symbol.clone(),
            sequence: 1,
            event_ts: TimestampMs::now(),
            exchange_ts: TimestampMs::now(),
            bids,
            asks,
        })
    }

    #[tokio::test]
    async fn crossed_bid_fills() {
        let bus = Arc::new(EventBus::new());
        let cfg = PaperSimConfig {
            fill_fraction: 1.0,
            partial_fill_prob: 0.0,
            queue_position: 1.0,
            ..PaperSimConfig::default()
        };
        let (ven, mut ex) = PaperMarketBuilder::new(cfg)
            .with_seed(1)
            .build(Exchange::Paper, Symbol("BTC-USDT".into()), spec(), bus);
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        // Rest a bid at 100.3: above the 100.2 ask → immediately crossed.
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.3)),
            dec!(1.0),
        );
        ven.place_order(&mut o).await.unwrap();
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        let status = ven.get_order_status(o.order_id).await.unwrap();
        assert_eq!(status, lq_types::OrderStatus::Filled);
        assert_eq!(ex.matched(), 1);
    }

    #[tokio::test]
    async fn uncrossed_order_does_not_fill() {
        let bus = Arc::new(EventBus::new());
        let cfg = PaperSimConfig {
            fill_fraction: 1.0,
            ..PaperSimConfig::default()
        };
        let (ven, mut ex) = PaperMarketBuilder::new(cfg)
            .with_seed(2)
            .build(Exchange::Paper, Symbol("BTC-USDT".into()), spec(), bus);
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(99.0)),
            dec!(1.0),
        );
        ven.place_order(&mut o).await.unwrap();
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        let status = ven.get_order_status(o.order_id).await.unwrap();
        assert_eq!(status, lq_types::OrderStatus::Acknowledged);
        assert_eq!(ex.matched(), 0);
    }

    #[tokio::test]
    async fn fill_fraction_limits_matches() {
        let bus = Arc::new(EventBus::new());
        let cfg = PaperSimConfig {
            fill_fraction: 0.0,
            ..PaperSimConfig::default()
        };
        let (ven, mut ex) = PaperMarketBuilder::new(cfg)
            .with_seed(3)
            .build(Exchange::Paper, Symbol("BTC-USDT".into()), spec(), bus);
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.3)),
            dec!(1.0),
        );
        ven.place_order(&mut o).await.unwrap();
        ex.on_market_event(&snapshot(&Symbol("BTC-USDT".into())))
            .await
            .unwrap();

        let status = ven.get_order_status(o.order_id).await.unwrap();
        assert_eq!(status, lq_types::OrderStatus::Acknowledged);
        assert_eq!(ex.matched(), 0);
    }
}
