//! Market microstructure analytics: turns a book + recent tape into a
//! [`MarketState`] consumed by strategies.
//!
//! All quantities are computed from bounded rolling windows so the cost per
//! update is `O(window)` worst case with small constants. Strategies are
//! forbidden from computing their own microstructure from raw data — this is
//! the single place it happens.

use std::collections::VecDeque;

use lq_core::models::{MarketRegime, MarketState, Trade};
use lq_exchange::spec::InstrumentSpec;
use lq_types::{Exchange, Price, Side, Symbol, TimestampMs};
use rust_decimal::{Decimal, MathematicalOps};

use crate::book::OrderBook;

/// Parameters controlling [`MarketStateEngine`].
#[derive(Debug, Clone, Copy)]
pub struct AnalyticsConfig {
    /// Number of levels used for depth / imbalance.
    pub depth_levels: usize,
    /// Rolling window of recent trades used for intensity and volume.
    pub trade_window: usize,
    /// Rolling window of returns used for realized volatility.
    pub vol_window: usize,
    /// A book untouched for this long is stale.
    pub stale_after_ms: u64,
    /// Spread (bps) above which we classify the market as volatile.
    pub volatile_spread_bps: f64,
    /// Annualized-vol threshold for the volatile regime (0 disables).
    pub volatile_vol_threshold: f64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            depth_levels: 10,
            trade_window: 256,
            vol_window: 64,
            stale_after_ms: 5_000,
            volatile_spread_bps: 20.0,
            volatile_vol_threshold: 0.0,
        }
    }
}

/// Computes [`MarketState`] for one `(Exchange, Symbol)`.
#[derive(Debug, Clone)]
pub struct MarketStateEngine {
    pub venue: Exchange,
    pub symbol: Symbol,
    cfg: AnalyticsConfig,
    trades: VecDeque<Trade>,
    log_returns: VecDeque<f64>,
    last_price: Option<Price>,
}

impl MarketStateEngine {
    pub fn new(venue: Exchange, symbol: Symbol, _spec: InstrumentSpec, cfg: AnalyticsConfig) -> Self {
        Self {
            venue,
            symbol,
            cfg,
            trades: VecDeque::with_capacity(cfg.trade_window),
            log_returns: VecDeque::with_capacity(cfg.vol_window),
            last_price: None,
        }
    }

    pub fn record_trade(&mut self, trade: Trade) {
        if let Some(last) = self.last_price {
            if last > Price::ZERO {
                let r = (trade.price / last).ln().as_f64();
                self.log_returns.push_back(r);
                if self.log_returns.len() > self.cfg.vol_window {
                    self.log_returns.pop_front();
                }
            }
        }
        self.last_price = Some(trade.price);
        self.trades.push_back(trade);
        if self.trades.len() > self.cfg.trade_window {
            self.trades.pop_front();
        }
    }

    /// Compute the current market state from a book.
    pub fn compute(&self, book: &OrderBook, now: TimestampMs) -> MarketState {
        let best_bid = book.best_bid().unwrap_or(Price::ZERO);
        let best_ask = book.best_ask().unwrap_or(Price::ZERO);
        let mid = if best_bid > Price::ZERO && best_ask > Price::ZERO {
            (best_bid + best_ask) / Decimal::TWO
        } else {
            self.last_price.unwrap_or(Price::ZERO)
        };
        let spread = if best_ask >= best_bid && best_bid > Price::ZERO {
            best_ask - best_bid
        } else {
            Price::ZERO
        };
        let spread_bps = if mid.is_zero() {
            0.0
        } else {
            (spread / mid * Decimal::from(10_000)).as_f64()
        };

        let imbalance = book.imbalance(self.cfg.depth_levels);

        // Order-flow microprice: weight the touch by opposite-side depth.
        let microprice = self.microprice(book, mid);

        let depth_bid = book.depth(Side::Bid, self.cfg.depth_levels);
        let depth_ask = book.depth(Side::Ask, self.cfg.depth_levels);

        let (buy_volume, sell_volume, vwap, trade_intensity) = self.tape_stats(now);

        let realized_vol = self.realized_volatility();
        let price_impact = self.price_impact_estimate();

        let age_ms = now.as_u64().saturating_sub(book.last_update_ms().as_u64());
        let stale = age_ms > self.cfg.stale_after_ms;

        let regime = self.classify(
            stale,
            best_bid,
            best_ask,
            spread_bps,
            imbalance,
            realized_vol,
        );

        MarketState {
            venue: self.venue,
            symbol: self.symbol.clone(),
            event_ts: now,
            best_bid,
            best_ask,
            mid,
            spread,
            spread_bps,
            orderbook_imbalance: imbalance,
            microprice,
            vwap,
            depth_bid,
            depth_ask,
            num_bid_levels: book.num_levels(Side::Bid) as u32,
            num_ask_levels: book.num_levels(Side::Ask) as u32,
            buy_volume,
            sell_volume,
            trade_intensity,
            realized_volatility: realized_vol,
            price_impact_estimate: price_impact,
            regime,
            stale,
        }
    }

    /// Order-flow microprice: `(bid*ask_size + ask*bid_size) / (bid_size + ask_size)`.
    fn microprice(&self, book: &OrderBook, mid: Price) -> Price {
        let (Some((bid_price, bid_size)), Some((ask_price, ask_size))) =
            (book.top_level(Side::Bid), book.top_level(Side::Ask))
        else {
            return mid;
        };
        let total = bid_size + ask_size;
        if total.is_zero() {
            return mid;
        }
        (bid_price * ask_size + ask_price * bid_size) / total
    }

    fn tape_stats(&self, now: TimestampMs) -> (Decimal, Decimal, Price, f64) {
        let mut buy = Decimal::ZERO;
        let mut sell = Decimal::ZERO;
        let mut notional = Decimal::ZERO;
        let mut qty = Decimal::ZERO;
        let mut first_ts = now;
        let mut n = 0u32;
        for t in &self.trades {
            match t.aggressor {
                Side::Bid => buy += t.qty,
                Side::Ask => sell += t.qty,
            }
            notional += t.price * t.qty;
            qty += t.qty;
            if t.event_ts < first_ts {
                first_ts = t.event_ts;
            }
            n += 1;
        }
        let window_secs = (now.as_u64().saturating_sub(first_ts.as_u64())) as f64 / 1000.0;
        let intensity = if window_secs > 0.0 {
            n as f64 / window_secs
        } else {
            0.0
        };
        let vwap = if qty.is_zero() {
            Price::ZERO
        } else {
            notional / qty
        };
        (buy, sell, vwap, intensity)
    }

    /// Realized volatility as the stddev of rolling log returns (per-trade).
    fn realized_volatility(&self) -> f64 {
        if self.log_returns.len() < 2 {
            return 0.0;
        }
        let n = self.log_returns.len() as f64;
        let mean = self.log_returns.iter().sum::<f64>() / n;
        let var = self
            .log_returns
            .iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        var.sqrt()
    }

    /// Amihud-style illiquidity: average `|return| / dollar volume` in the window.
    fn price_impact_estimate(&self) -> f64 {
        if self.log_returns.len() < 2 {
            return 0.0;
        }
        let mut sum = 0.0;
        for (i, r) in self.log_returns.iter().enumerate().skip(1) {
            let trade = self.trades.get(i).map(|t| t.price * t.qty);
            if let Some(dv) = trade {
                if dv > Decimal::ZERO {
                    sum += r.abs() / dv.as_f64().abs().max(1e-9);
                }
            }
        }
        sum / (self.log_returns.len() - 1) as f64
    }

    fn classify(
        &self,
        stale: bool,
        best_bid: Price,
        best_ask: Price,
        spread_bps: f64,
        imbalance: f64,
        realized_vol: f64,
    ) -> MarketRegime {
        if stale {
            return MarketRegime::Stale;
        }
        if best_bid.is_zero() || best_ask.is_zero() || best_ask <= best_bid {
            return MarketRegime::NoLiquidity;
        }
        if spread_bps > self.cfg.volatile_spread_bps {
            return MarketRegime::Volatile;
        }
        if self.cfg.volatile_vol_threshold > 0.0 && realized_vol > self.cfg.volatile_vol_threshold {
            return MarketRegime::Volatile;
        }
        if imbalance > 0.6 {
            return MarketRegime::OneSidedBid;
        }
        if imbalance < -0.6 {
            return MarketRegime::OneSidedAsk;
        }
        MarketRegime::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::OrderBookLevel;
    use lq_types::Qty;
    use rust_decimal_macros::dec;

    fn book() -> OrderBook {
        let mut b = OrderBook::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
        );
        b.apply_snapshot(&lq_core::models::OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![
                OrderBookLevel { price: dec!(100.0), qty: dec!(2.0) },
                OrderBookLevel { price: dec!(99.9), qty: dec!(1.0) },
            ],
            asks: vec![
                OrderBookLevel { price: dec!(100.1), qty: dec!(1.0) },
                OrderBookLevel { price: dec!(100.2), qty: dec!(2.0) },
            ],
        });
        b
    }

    #[test]
    fn computes_balanced_state() {
        let engine = MarketStateEngine::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
            AnalyticsConfig::default(),
        );
        let b = book();
        let s = engine.compute(&b, TimestampMs(1_000));
        assert_eq!(s.mid, dec!(100.05));
        assert_eq!(s.spread, dec!(0.1));
        assert!(!s.stale);
        assert_eq!(s.regime, MarketRegime::Normal);
        // microprice: (100*1 + 100.1*2)/3 = 100.0666...
        assert!(s.microprice > dec!(100.05));
    }

    #[test]
    fn one_sided_book_has_positive_imbalance() {
        let engine = MarketStateEngine::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
            AnalyticsConfig::default(),
        );
        let mut b = OrderBook::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
        );
        b.apply_snapshot(&lq_core::models::OrderBookSnapshot {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            sequence: 1,
            event_ts: TimestampMs(1),
            exchange_ts: TimestampMs(1),
            bids: vec![
                OrderBookLevel { price: dec!(100.0), qty: dec!(10.0) },
                OrderBookLevel { price: dec!(99.9), qty: dec!(5.0) },
            ],
            asks: vec![OrderBookLevel { price: dec!(100.1), qty: dec!(0.5) }],
        });
        let s = engine.compute(&b, TimestampMs(1_000));
        assert!(s.orderbook_imbalance > 0.5);
    }

    #[test]
    fn stale_detection() {
        let engine = MarketStateEngine::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
            AnalyticsConfig {
                stale_after_ms: 100,
                ..AnalyticsConfig::default()
            },
        );
        let b = book();
        // Book's last event was at t=1; we query at t=10_000 -> stale.
        let s = engine.compute(&b, TimestampMs(10_000));
        assert!(s.stale);
        assert_eq!(s.regime, MarketRegime::Stale);
        assert!(!s.is_quotable());
    }

    #[test]
    fn trade_stats_accumulate() {
        let mut engine = MarketStateEngine::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            InstrumentSpec::new(dec!(0.1), dec!(0.01)),
            AnalyticsConfig::default(),
        );
        for i in 0..10 {
            engine.record_trade(Trade {
                venue: Exchange::Paper,
                symbol: Symbol("BTC-USDT".into()),
                price: dec!(100.0),
                qty: dec!(0.5),
                aggressor: Side::Bid,
                event_ts: TimestampMs(i),
                exchange_ts: TimestampMs(i),
            });
        }
        let s = engine.compute(&book(), TimestampMs(10_000));
        assert_eq!(s.buy_volume, dec!(5.0));
        assert_eq!(s.sell_volume, dec!(0.0));
        assert!(s.trade_intensity > 0.0);
    }

    #[test]
    fn qty_type_is_used() {
        let _: Qty = dec!(1.0);
    }
}