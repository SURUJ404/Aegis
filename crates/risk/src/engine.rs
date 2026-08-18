//! Risk engine implementation.
//!
//! The risk engine sits **between strategy and execution**: no strategy may
//! submit an order that has not passed `validate_order`. It reads live state
//! from [`EngineState`] (positions, orders, realized PnL) and enforces the
//! configured limits.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use lq_core::config::RiskConfig;
use lq_core::models::{MarketState, Order};
use lq_core::state::EngineState;
use lq_types::{Exchange, TimestampMs};
use parking_lot::Mutex;
use rust_decimal::Decimal;

use crate::decision::{RiskCode, RiskDecision, RiskReason};

/// The risk engine. Thread-safe: shared behind `Arc` in the engine.
pub struct RiskEngine {
    cfg: RiskConfig,
    state: EngineState,
    kill_switch: AtomicBool,
    kill_switch_reason: Mutex<Option<String>>,
    /// Venues currently reconnecting.
    reconnecting: Mutex<Vec<Exchange>>,
    /// Timestamps of recent order submissions (sliding 1s window).
    order_times: Mutex<VecDeque<TimestampMs>>,
    /// Total rejects emitted (observability).
    rejects: Mutex<u64>,
    halts: Mutex<u64>,
}

impl RiskEngine {
    pub fn new(cfg: RiskConfig, state: EngineState) -> Self {
        Self {
            cfg,
            state,
            kill_switch: AtomicBool::new(false),
            kill_switch_reason: Mutex::new(None),
            reconnecting: Mutex::new(Vec::new()),
            order_times: Mutex::new(VecDeque::new()),
            rejects: Mutex::new(0),
            halts: Mutex::new(0),
        }
    }

    /// Engage or release the kill switch.
    pub fn set_kill_switch(&self, engaged: bool, reason: impl Into<String>) {
        self.kill_switch.store(engaged, AtomicOrdering::SeqCst);
        *self.kill_switch_reason.lock() = engaged.then(|| reason.into());
        tracing::warn!(engaged, "kill switch state changed");
    }

    pub fn is_halted(&self) -> bool {
        self.kill_switch.load(AtomicOrdering::SeqCst)
    }

    pub fn halt_reason(&self) -> Option<String> {
        self.kill_switch_reason.lock().clone()
    }

    /// Mark a venue as reconnecting; while reconnecting, orders on it are
    /// rejected. If configured, this also trips the kill switch.
    pub fn on_venue_reconnecting(&self, venue: Exchange) {
        self.reconnecting.lock().push(venue);
        if self.cfg.kill_switch_on_reconnect {
            self.set_kill_switch(true, format!("venue {venue} reconnecting"));
        }
    }

    pub fn on_venue_connected(&self, venue: Exchange) {
        self.reconnecting.lock().retain(|v| *v != venue);
    }

    pub fn is_venue_reconnecting(&self, venue: Exchange) -> bool {
        self.reconnecting.lock().contains(&venue)
    }

    pub fn stats(&self) -> (u64, u64) {
        (*self.rejects.lock(), *self.halts.lock())
    }

    /// Validate a resting/working order against every configured limit.
    pub fn validate_order(&self, order: &Order, mark: Decimal) -> RiskDecision {
        if self.kill_switch.load(AtomicOrdering::SeqCst) {
            *self.rejects.lock() += 1;
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::KillSwitchEngaged,
                self.halt_reason().unwrap_or_else(|| "kill switch engaged".into()),
            ));
        }

        if self.is_venue_reconnecting(order.venue) {
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::VenueReconnecting,
                format!("venue {} is reconnecting", order.venue),
            ));
        }

        if !self.cfg.max_order_qty.is_zero() && order.quantity > self.cfg.max_order_qty {
            *self.rejects.lock() += 1;
            return RiskDecision::Reduce {
                qty: self.cfg.max_order_qty,
                reason: RiskReason::new(
                    RiskCode::MaxOrderSize,
                    format!(
                        "order qty {} > max {}",
                        order.quantity, self.cfg.max_order_qty
                    ),
                ),
            };
        }

        if let Some(price) = order.price {
            if !mark.is_zero() {
                let deviation = ((price - mark).abs() / mark * Decimal::from(10_000)).as_f64();
                if deviation > self.cfg.max_price_deviation_bps {
                    *self.rejects.lock() += 1;
                    return RiskDecision::Reject(RiskReason::new(
                        RiskCode::MaxPriceDeviation,
                        format!("price {price} deviates {deviation:.2}bps from mark {mark}"),
                    ));
                }
            }
            let notional = price * order.quantity;
            if notional > self.cfg.max_notional {
                *self.rejects.lock() += 1;
                return RiskDecision::Reject(RiskReason::new(
                    RiskCode::MaxNotional,
                    format!("notional {notional} > max {}", self.cfg.max_notional),
                ));
            }
        }

        // Projected position after this order.
        let inv = self
            .state
            .inventory
            .get(&order.symbol)
            .map(|i| i.net_qty)
            .unwrap_or(Decimal::ZERO);
        let sign = if order.side == lq_types::Side::Bid {
            Decimal::ONE
        } else {
            Decimal::NEGATIVE_ONE
        };
        let projected = inv + sign * order.quantity;
        if projected.abs() > self.cfg.max_position_qty {
            *self.rejects.lock() += 1;
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::MaxPosition,
                format!(
                    "projected position {projected} exceeds max {}",
                    self.cfg.max_position_qty
                ),
            ));
        }

        // Venue exposure.
        let exposure = self
            .state
            .positions
            .iter()
            .filter(|p| p.venue == order.venue && p.symbol == order.symbol)
            .map(|p| p.notional(mark))
            .sum::<Decimal>();
        if exposure + (order.quantity * mark) > self.cfg.max_exposure_per_venue {
            *self.rejects.lock() += 1;
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::MaxExposurePerVenue,
                format!(
                    "venue {} exposure would exceed {}",
                    order.venue, self.cfg.max_exposure_per_venue
                ),
            ));
        }

        // Open order count on the venue.
        let open = self
            .state
            .orders
            .iter()
            .filter(|o| {
                o.venue == order.venue
                    && o.symbol == order.symbol
                    && !o.status.is_terminal()
            })
            .count();
        if open as u32 >= self.cfg.max_open_orders {
            *self.rejects.lock() += 1;
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::MaxOpenOrders,
                format!("{open} open orders >= max {}", self.cfg.max_open_orders),
            ));
        }

        // Order rate limiting (sliding 1s window).
        if !self.rate_allowed() {
            *self.rejects.lock() += 1;
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::MaxOrderRate,
                format!("order rate > {}/s", self.cfg.max_order_rate_per_sec),
            ));
        }

        // Session PnL (realized, tracked by the position manager).
        let session_pnl = self
            .state
            .inventory
            .iter()
            .map(|i| i.realized_pnl)
            .sum::<Decimal>();
        if session_pnl < -self.cfg.max_daily_loss {
            self.set_kill_switch(true, format!("daily loss limit hit: {session_pnl}"));
            *self.halts.lock() += 1;
            return RiskDecision::Halt(RiskReason::new(
                RiskCode::MaxDailyLoss,
                format!("session PnL {session_pnl} below daily loss limit"),
            ));
        }

        RiskDecision::Allow
    }

    /// Check whether a market state is safe to quote against.
    pub fn check_market(&self, state: &MarketState) -> RiskDecision {
        if state.stale {
            if self.cfg.kill_switch_on_stale {
                self.set_kill_switch(true, format!("market stale for {}", state.symbol));
            }
            return RiskDecision::Reject(RiskReason::new(
                RiskCode::StaleMarket,
                format!("market for {} stale", state.symbol),
            ));
        }
        RiskDecision::Allow
    }

    fn rate_allowed(&self) -> bool {
        let now = TimestampMs::now().as_u64();
        let mut times = self.order_times.lock();
        while times.front().map(|t| now - t.as_u64() >= 1000).unwrap_or(false) {
            times.pop_front();
        }
        if times.len() as f64 >= self.cfg.max_order_rate_per_sec {
            return false;
        }
        times.push_back(TimestampMs(now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::config::RiskConfig;
    use lq_core::models::{Inventory, Order};
    use lq_types::{Exchange, OrderType, Side, Symbol};
    use rust_decimal_macros::dec;

    fn order(side: Side, price: lq_types::Price, qty: lq_types::Qty) -> Order {
        Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            side,
            OrderType::Limit,
            Some(price),
            qty,
        )
    }

    fn engine(over: Option<RiskConfig>) -> (RiskEngine, EngineState) {
        let state = EngineState::new();
        let cfg = over.unwrap_or_else(|| RiskConfig {
            max_position_qty: dec!(1.0),
            max_order_qty: dec!(0.5),
            max_notional: dec!(10_000),
            max_open_orders: 5,
            max_order_rate_per_sec: 1000.0,
            ..RiskConfig::default()
        });
        let risk = RiskEngine::new(cfg, state.clone());
        (risk, state)
    }

    #[test]
    fn allows_valid_order() {
        let (risk, _) = engine(None);
        let o = order(Side::Bid, dec!(100.0), dec!(0.1));
        assert!(risk.validate_order(&o, dec!(100.0)).is_allow());
    }

    #[test]
    fn rejects_over_max_order_size_with_reduce() {
        let (risk, _) = engine(None);
        let o = order(Side::Bid, dec!(100.0), dec!(0.7));
        match risk.validate_order(&o, dec!(100.0)) {
            RiskDecision::Reduce { qty, reason } => {
                assert_eq!(qty, dec!(0.5));
                assert_eq!(reason.code, RiskCode::MaxOrderSize);
            }
            other => panic!("expected reduce, got {other:?}"),
        }
    }

    #[test]
    fn rejects_above_max_position() {
        let (risk, state) = engine(None);
        state.inventory.insert(
            Symbol("BTC-USDT".into()),
            Inventory {
                symbol: Symbol("BTC-USDT".into()),
                net_qty: dec!(0.9),
                avg_entry: dec!(100.0),
                realized_pnl: dec!(0.0),
                event_ts: TimestampMs(1),
            },
        );
        let o = order(Side::Bid, dec!(100.0), dec!(0.2));
        match risk.validate_order(&o, dec!(100.0)) {
            RiskDecision::Reject(r) => assert_eq!(r.code, RiskCode::MaxPosition),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn rejects_price_deviation() {
        let (risk, _) = engine(None);
        let o = order(Side::Bid, dec!(120.0), dec!(0.1));
        match risk.validate_order(&o, dec!(100.0)) {
            RiskDecision::Reject(r) => assert_eq!(r.code, RiskCode::MaxPriceDeviation),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn kill_switch_blocks_everything() {
        let (risk, _) = engine(None);
        risk.set_kill_switch(true, "test");
        let o = order(Side::Bid, dec!(100.0), dec!(0.1));
        match risk.validate_order(&o, dec!(100.0)) {
            RiskDecision::Reject(r) => assert_eq!(r.code, RiskCode::KillSwitchEngaged),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn reconnecting_venue_rejects_orders() {
        let (risk, _) = engine(Some(RiskConfig {
            kill_switch_on_reconnect: false,
            ..RiskConfig::default()
        }));
        risk.on_venue_reconnecting(Exchange::Paper);
        let o = order(Side::Bid, dec!(100.0), dec!(0.1));
        match risk.validate_order(&o, dec!(100.0)) {
            RiskDecision::Reject(r) => assert_eq!(r.code, RiskCode::VenueReconnecting),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn stale_market_rejected() {
        let (risk, _) = engine(Some(RiskConfig {
            kill_switch_on_stale: false,
            ..RiskConfig::default()
        }));
        let mut s = lq_core::models::MarketState {
            venue: Exchange::Paper,
            symbol: Symbol("BTC-USDT".into()),
            event_ts: TimestampMs(1),
            best_bid: dec!(99.0),
            best_ask: dec!(101.0),
            mid: dec!(100.0),
            spread: dec!(2.0),
            spread_bps: 200.0,
            orderbook_imbalance: 0.0,
            microprice: dec!(100.0),
            vwap: dec!(100.0),
            depth_bid: dec!(1.0),
            depth_ask: dec!(1.0),
            num_bid_levels: 10,
            num_ask_levels: 10,
            buy_volume: dec!(0.0),
            sell_volume: dec!(0.0),
            trade_intensity: 0.0,
            realized_volatility: 0.0,
            price_impact_estimate: 0.0,
            regime: lq_core::models::MarketRegime::Stale,
            stale: true,
        };
        s.stale = true;
        match risk.check_market(&s) {
            RiskDecision::Reject(r) => assert_eq!(r.code, RiskCode::StaleMarket),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[test]
    fn unknown_symbol_helper() {
        let _: Symbol = "ETH-USDT".parse().unwrap();
    }
}

