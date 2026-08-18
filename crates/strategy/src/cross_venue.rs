//! Cross-venue spread/arbitrage analysis.
//!
//! This is deliberately *not* "arbitrage" in the market-maker sense of the
//! word. It only reports an opportunity when the expected gross edge, after
//! every real cost (fees on both venues, latency, slippage and a configured
//! minimum edge), is still positive:
//!
//! ```text
//! gross_bps = (sell_mid - buy_mid) / buy_mid * 10_000
//! cost_bps  = taker_fee(buy) + taker_fee(sell) + slippage(buy) + slippage(sell)
//!             + latency_ms * latency_cost_bps_per_ms
//! net_bps   = gross_bps - cost_bps - min_edge_bps
//! ```
//!
//! An opportunity exists only if `net_bps > 0` and both venues have at least
//! `min_liquidity` resting at the touch.

use lq_core::models::{MarketOrderSignal, MarketState, StrategyDecision};
use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
use rust_decimal::Decimal;

#[derive(Debug, Clone)]
pub struct CrossVenueConfig {
    /// Taker fee per side (bps).
    pub taker_fee_bps: f64,
    /// Maker rebate (bps) — used for quoting cost estimates.
    pub maker_rebate_bps: f64,
    /// Assumed one-way latency in ms.
    pub latency_ms: f64,
    /// Cost of latency per ms of round trip (bps).
    pub latency_cost_bps_per_ms: f64,
    /// Assumed slippage per side (bps).
    pub slippage_bps: f64,
    /// Minimum net edge required (bps).
    pub min_edge_bps: f64,
    /// Minimum touch liquidity required on both venues.
    pub min_liquidity: Qty,
}

impl Default for CrossVenueConfig {
    fn default() -> Self {
        Self {
            taker_fee_bps: 2.5,
            maker_rebate_bps: 0.5,
            latency_ms: 50.0,
            latency_cost_bps_per_ms: 0.01,
            slippage_bps: 1.0,
            min_edge_bps: 1.0,
            min_liquidity: rust_decimal_macros::dec!(0.01),
        }
    }
}

/// A tradeable cross-venue opportunity.
#[derive(Debug, Clone)]
pub struct CrossVenueOpportunity {
    pub symbol: Symbol,
    pub buy_venue: Exchange,
    pub sell_venue: Exchange,
    pub buy_price: Price,
    pub sell_price: Price,
    /// Gross price difference in bps of the buy price.
    pub gross_bps: f64,
    /// All-in expected cost in bps.
    pub cost_bps: f64,
    /// Net expected edge in bps (gross - cost - min_edge).
    pub net_bps: f64,
    /// Effective spread of the buy venue (its spread + its fees).
    pub buy_effective_spread_bps: f64,
    /// Quantity executable given touch liquidity on both venues.
    pub executable_qty: Qty,
    pub ts: TimestampMs,
}

impl CrossVenueOpportunity {
    /// Convert to a strategy decision if the edge is real.
    pub fn to_decision(&self) -> Option<StrategyDecision> {
        if self.net_bps <= 0.0 {
            return None;
        }
        Some(StrategyDecision::MarketOrder(MarketOrderSignal {
            symbol: self.symbol.clone(),
            venue: self.buy_venue,
            side: Side::Bid,
            price: self.buy_price,
            qty: self.executable_qty,
            reason: format!(
                "cross-venue {}->{} net={:.2}bps gross={:.2}bps cost={:.2}bps",
                self.buy_venue, self.sell_venue, self.net_bps, self.gross_bps, self.cost_bps
            ),
        }))
    }
}

#[derive(Debug, Clone)]
pub struct CrossVenueAnalyzer {
    cfg: CrossVenueConfig,
}

impl CrossVenueAnalyzer {
    pub fn new(cfg: CrossVenueConfig) -> Self {
        Self { cfg }
    }

    /// Analyze all venue states for a symbol and return the best opportunity
    /// with positive net edge, if any.
    pub fn analyze(&self, states: &[MarketState]) -> Option<CrossVenueOpportunity> {
        if states.len() < 2 {
            return None;
        }
        let mut best: Option<CrossVenueOpportunity> = None;
        for a in states {
            for b in states {
                if a.venue == b.venue || a.mid.is_zero() || b.mid.is_zero() {
                    continue;
                }
                // Buy where it's cheap (a), sell where it's expensive (b).
                if a.mid >= b.mid {
                    continue;
                }
                // Liquidity check at the touch.
                if a.depth_ask < self.cfg.min_liquidity || b.depth_bid < self.cfg.min_liquidity {
                    continue;
                }

                let gross_bps = ((b.mid - a.mid) / a.mid * Decimal::from(10_000)).as_f64();
                let cost_bps = 2.0 * self.cfg.taker_fee_bps
                    + 2.0 * self.cfg.slippage_bps
                    + self.cfg.latency_ms * self.cfg.latency_cost_bps_per_ms;
                let net_bps = gross_bps - cost_bps - self.cfg.min_edge_bps;

                if net_bps <= 0.0 {
                    continue;
                }

                let executable_qty = a
                    .depth_ask
                    .min(b.depth_bid)
                    .min(self.cfg.min_liquidity * Decimal::from(1000));

                let opp = CrossVenueOpportunity {
                    symbol: a.symbol.clone(),
                    buy_venue: a.venue,
                    sell_venue: b.venue,
                    buy_price: a.best_ask,
                    sell_price: b.best_bid,
                    gross_bps,
                    cost_bps,
                    net_bps,
                    buy_effective_spread_bps: a.spread_bps + 2.0 * self.cfg.taker_fee_bps,
                    executable_qty,
                    ts: a.event_ts,
                };

                if best.as_ref().map(|o| o.net_bps < net_bps).unwrap_or(true) {
                    best = Some(opp);
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{MarketRegime, MarketState};
    use rust_decimal_macros::dec;

    fn state(venue: Exchange, mid: Price, ask_depth: Qty, bid_depth: Qty) -> MarketState {
        MarketState {
            venue,
            symbol: Symbol("BTC-USDT".into()),
            event_ts: TimestampMs(1),
            best_bid: mid - dec!(0.1),
            best_ask: mid + dec!(0.1),
            mid,
            spread: dec!(0.2),
            spread_bps: 20.0,
            orderbook_imbalance: 0.0,
            microprice: mid,
            vwap: mid,
            depth_bid: bid_depth,
            depth_ask: ask_depth,
            num_bid_levels: 10,
            num_ask_levels: 10,
            buy_volume: dec!(1.0),
            sell_volume: dec!(1.0),
            trade_intensity: 1.0,
            realized_volatility: 0.0,
            price_impact_estimate: 0.0,
            regime: MarketRegime::Normal,
            stale: false,
        }
    }

    #[test]
    fn detects_opportunity_when_edge_is_real() {
        // 100.00 vs 100.10 => 10 bps gross, costs ~ 7 bps -> net positive.
        let analyzer = CrossVenueAnalyzer::new(CrossVenueConfig::default());
        let states = vec![
            state(Exchange::Okx, dec!(100.00), dec!(1.0), dec!(1.0)),
            state(Exchange::Binance, dec!(100.10), dec!(1.0), dec!(1.0)),
        ];
        let opp = analyzer.analyze(&states).unwrap();
        assert_eq!(opp.buy_venue, Exchange::Okx);
        assert_eq!(opp.sell_venue, Exchange::Binance);
        assert!((opp.gross_bps - 10.0).abs() < 0.01);
        assert!(opp.net_bps > 0.0);
    }

    #[test]
    fn no_opportunity_below_costs() {
        let analyzer = CrossVenueAnalyzer::new(CrossVenueConfig::default());
        let states = vec![
            state(Exchange::Okx, dec!(100.00), dec!(1.0), dec!(1.0)),
            state(Exchange::Binance, dec!(100.03), dec!(1.0), dec!(1.0)),
        ];
        assert!(analyzer.analyze(&states).is_none());
    }

    #[test]
    fn no_opportunity_without_liquidity() {
        let analyzer = CrossVenueAnalyzer::new(CrossVenueConfig::default());
        let states = vec![
            state(Exchange::Okx, dec!(100.00), dec!(0.0), dec!(1.0)),
            state(Exchange::Binance, dec!(100.10), dec!(1.0), dec!(1.0)),
        ];
        assert!(analyzer.analyze(&states).is_none());
    }

    #[test]
    fn to_decision_produces_market_order() {
        let analyzer = CrossVenueAnalyzer::new(CrossVenueConfig::default());
        let states = vec![
            state(Exchange::Okx, dec!(100.00), dec!(1.0), dec!(1.0)),
            state(Exchange::Binance, dec!(100.10), dec!(1.0), dec!(1.0)),
        ];
        let opp = analyzer.analyze(&states).unwrap();
        let decision = opp.to_decision().unwrap();
        match decision {
            StrategyDecision::MarketOrder(sig) => {
                assert_eq!(sig.side, Side::Bid);
                assert!(sig.reason.contains("cross-venue"));
            }
            other => panic!("expected market order, got {other:?}"),
        }
    }
}
