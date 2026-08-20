//! Baseline market-making strategy with inventory skew.

use lq_core::config::MarketMakingConfig;
use lq_core::models::{MarketRegime, MarketState, QuoteIntent, QuoteLeg, StrategyDecision};
use lq_types::TimestampMs;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{Strategy, StrategyContext};

/// A two-sided market maker that skews quotes away from undesirable inventory.
///
/// ## Model
///
/// Base quote prices sit `half_spread_bps` away from mid:
///
/// ```text
/// bid = mid * (1 - half_spread / 10000)
/// ask = mid * (1 + half_spread / 10000)
/// ```
///
/// Inventory skew moves the midpoint of the quoted spread:
///
/// ```text
/// sk = inventory_net_qty / inventory_max_qty          (clamped to [-1, 1])
/// bid_offset_bps = half_spread * (1 + sk)   // long  => bid further away
/// ask_offset_bps = half_spread * (1 - sk)   // long  => ask closer in
/// ```
///
/// When inventory is at the cap (`|sk| == 1`) the increasing side is not
/// quoted at all. Order-book imbalance tightens the side facing liquidity and
/// widens the side facing pressure:
///
/// ```text
/// bid_offset *= (1 - 0.2 * imbalance)
/// ask_offset *= (1 + 0.2 * imbalance)
/// ```
pub struct MarketMakingStrategy {
    symbol: lq_types::Symbol,
    venue: lq_types::Exchange,
    cfg: MarketMakingConfig,
    last_quote_ts: Option<TimestampMs>,
}

impl MarketMakingStrategy {
    pub fn new(
        symbol: lq_types::Symbol,
        venue: lq_types::Exchange,
        cfg: MarketMakingConfig,
    ) -> Self {
        Self {
            symbol,
            venue,
            cfg,
            last_quote_ts: None,
        }
    }

    fn half_spread_bps(&self, state: &MarketState) -> f64 {
        let mut half = self.cfg.half_spread_bps;
        if self.cfg.vol_scale_half_spread {
            // Widen with realized per-trade volatility (as bps of price).
            let vol_bps = (state.realized_volatility * 10_000.0).min(200.0);
            half += vol_bps * 0.05;
            // Never quote tighter than half the observed spread + 1 bps.
            half = half.max(state.spread_bps * 0.5 + 1.0);
        }
        if state.regime == MarketRegime::Volatile {
            half = half.max(self.cfg.max_spread_bps);
        }
        half.clamp(self.cfg.min_spread_bps, self.cfg.max_spread_bps)
    }

    fn size_multiplier(&self, state: &MarketState) -> Decimal {
        if state.regime == MarketRegime::Volatile {
            Decimal::from(1) / Decimal::from(2)
        } else {
            Decimal::ONE
        }
    }
}

impl Strategy for MarketMakingStrategy {
    fn name(&self) -> &'static str {
        "market-making"
    }

    fn on_market_state(&mut self, ctx: &StrategyContext) -> StrategyDecision {
        let state = ctx.market;

        if !state.is_quotable() {
            return StrategyDecision::StandDown {
                reason: format!("market not quotable: {:?}", state.regime),
            };
        }

        if !ctx.running || ctx.halted {
            return StrategyDecision::StandDown {
                reason: if ctx.halted {
                    "risk engine halted".to_string()
                } else {
                    "strategy not running".to_string()
                },
            };
        }

        // Quote refresh throttle.
        if let Some(last) = self.last_quote_ts {
            if state.event_ts.as_u64().saturating_sub(last.as_u64())
                < self.cfg.quote_refresh_ms
            {
                return StrategyDecision::Hold;
            }
        }

let inv_qty = ctx
            .inventory
            .as_ref()
            .map(|i| i.net_qty)
            .unwrap_or(Decimal::ZERO);
        let max_q = self.cfg.inventory_max_qty;

        let (sk, bid_on, ask_on) = if max_q.is_zero() {
            (0.0f64, true, true)
        } else {
            let s = (inv_qty / max_q).as_f64().clamp(-1.0, 1.0);
            (s, s < 1.0, s > -1.0)
        };

        let half = self.half_spread_bps(state);
        // Inventory skew.
        let bid_off = half * (1.0 + sk);
        let ask_off = half * (1.0 - sk);
        // Order-book imbalance adjustment.
        let imb = state.orderbook_imbalance;
        let bid_off = bid_off * (1.0 - 0.2 * imb);
        let ask_off = ask_off * (1.0 + 0.2 * imb);

        let bid_off_dec =
            Decimal::from_f64_retain(bid_off).unwrap_or_default() / Decimal::from(10_000);
        let ask_off_dec =
            Decimal::from_f64_retain(ask_off).unwrap_or_default() / Decimal::from(10_000);

        let mut bid_price = state.mid * (Decimal::ONE - bid_off_dec);
        let mut ask_price = state.mid * (Decimal::ONE + ask_off_dec);

        // Never quote worse than the current touch. A quote resting outside
        // the best bid/ask sits behind the whole book and can never fill;
        // clamping it to the touch (or letting inventory skew pull it inside)
        // is where a real market maker rests.
        if state.best_bid > Decimal::ZERO {
            bid_price = bid_price.max(state.best_bid);
        }
        if state.best_ask > Decimal::ZERO {
            ask_price = ask_price.min(state.best_ask);
        }

        // Defensive: a clamped quote must never cross the book.
        if bid_on && ask_on && bid_price >= ask_price {
            return StrategyDecision::Hold;
        }

        let size = self.cfg.quote_qty * self.size_multiplier(state);

        let bid = bid_on.then_some(QuoteLeg {
            price: bid_price,
            qty: size,
        });
        let ask = ask_on.then_some(QuoteLeg {
            price: ask_price,
            qty: size,
        });

        self.last_quote_ts = Some(state.event_ts);

        StrategyDecision::Quote(QuoteIntent {
            id: Uuid::new_v4(),
            symbol: self.symbol.clone(),
            venue: self.venue,
            bid,
            ask,
            strategy: self.name().to_string(),
            reason: format!(
                "half={half:.2}bps sk={sk:.2} imb={imb:.2} mid={}",
                state.mid
            ),
            ts: state.event_ts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::{Inventory, MarketState};
    use lq_types::{Exchange, Price, Qty, Side, Symbol, TimestampMs};
    use rust_decimal_macros::dec;

    fn state() -> MarketState {
        MarketState {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            event_ts: TimestampMs(100_000),
            best_bid: dec!(99.9),
            best_ask: dec!(100.1),
            mid: dec!(100.0),
            spread: dec!(0.2),
            spread_bps: 20.0,
            orderbook_imbalance: 0.0,
            microprice: dec!(100.0),
            vwap: dec!(100.0),
            depth_bid: dec!(5.0),
            depth_ask: dec!(5.0),
            num_bid_levels: 10,
            num_ask_levels: 10,
            buy_volume: dec!(1.0),
            sell_volume: dec!(1.0),
            trade_intensity: 1.0,
            realized_volatility: 0.0001,
            price_impact_estimate: 0.0,
            regime: MarketRegime::Normal,
            stale: false,
        }
    }

    fn ctx<'a>(
        market: &'a MarketState,
        inv_qty: Qty,
        running: bool,
        halted: bool,
    ) -> StrategyContext<'a> {
        StrategyContext {
            inventory: Some(Inventory {
                symbol: Symbol("BTC-USDT".into()),
                net_qty: inv_qty,
                avg_entry: dec!(100.0),
                realized_pnl: dec!(0.0),
                event_ts: TimestampMs(0),
            }),
            position: None,
            halted,
            running,
            now: market.event_ts,
            market,
        }
    }

    fn cfg() -> MarketMakingConfig {
        MarketMakingConfig {
            half_spread_bps: 5.0,
            min_spread_bps: 2.0,
            max_spread_bps: 30.0,
            quote_qty: dec!(0.01),
            inventory_target_qty: dec!(0.0),
            inventory_max_qty: dec!(0.5),
            skew_exponent: 1.0,
            quote_refresh_ms: 0,
            vol_scale_half_spread: false,
            ..MarketMakingConfig::default()
        }
    }

    #[test]
    fn flat_inventory_symmetric_quote() {
        let mut s = MarketMakingStrategy::new(
            Symbol("BTC-USDT".into()),
            Exchange::Paper,
            cfg(),
        );
        let market = state();
        let decision = s.on_market_state(&ctx(&market, dec!(0.0), true, false));
        match decision {
            StrategyDecision::Quote(q) => {
                let bid = q.bid.unwrap();
                let ask = q.ask.unwrap();
                assert_eq!(bid.price, dec!(99.95));
                assert_eq!(ask.price, dec!(100.05));
                assert_eq!(bid.qty, dec!(0.01));
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn long_inventory_skews_ask_in() {
        let mut s = MarketMakingStrategy::new(
            Symbol("BTC-USDT".into()),
            Exchange::Paper,
            cfg(),
        );
        let market = state();
        // 100% of max inventory: sk = 1.0
        let decision = s.on_market_state(&ctx(&market, dec!(0.5), true, false));
        match decision {
            StrategyDecision::Quote(q) => {
                // No bid at all (increasing side disabled), ask pulled in.
                assert!(q.bid.is_none());
                let ask = q.ask.unwrap();
                assert!(ask.price < dec!(100.05), "ask={ask:?}");
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn short_inventory_skews_bid_in() {
        let mut s = MarketMakingStrategy::new(
            Symbol("BTC-USDT".into()),
            Exchange::Paper,
            cfg(),
        );
        let market = state();
        let decision = s.on_market_state(&ctx(&market, dec!(-0.25), true, false));
        match decision {
            StrategyDecision::Quote(q) => {
                let bid = q.bid.unwrap();
                let ask = q.ask.unwrap();
                // Half = 5bps; sk = -0.5 => bid_off = 2.5bps, ask_off = 7.5bps.
                assert!(bid.price > dec!(99.95), "bid={bid:?}");
                assert!(ask.price > dec!(100.05), "ask={ask:?}");
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    #[test]
    fn stands_down_when_halted() {
        let mut s = MarketMakingStrategy::new(
            Symbol("BTC-USDT".into()),
            Exchange::Paper,
            cfg(),
        );
        let market = state();
        match s.on_market_state(&ctx(&market, dec!(0.0), true, true)) {
            StrategyDecision::StandDown { .. } => {}
            other => panic!("expected standdown, got {other:?}"),
        }
    }

    #[test]
    fn stands_down_on_stale_market() {
        let mut s = MarketMakingStrategy::new(
            Symbol("BTC-USDT".into()),
            Exchange::Paper,
            cfg(),
        );
        let mut market = state();
        market.stale = true;
        match s.on_market_state(&ctx(&market, dec!(0.0), true, false)) {
            StrategyDecision::StandDown { .. } => {}
            other => panic!("expected standdown, got {other:?}"),
        }
    }

    #[test]
    fn price_is_decimal_type() {
        let _: Price = dec!(1.0);
        let _: Side = Side::Bid;
    }
}

