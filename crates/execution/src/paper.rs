//! Paper execution venue.
//!
//! Simulates the full order lifecycle with explicit, configurable assumptions
//! ([`PaperSimConfig`](lq_core::config::PaperSimConfig)):
//!
//! - **Latency**: base + jitter applied on submit, cancel and ack paths.
//! - **Rejects**: a configurable probability that the venue rejects an order.
//! - **Fills**: driven externally by the paper exchange (matching against the
//!   simulated book); queue position and partial-fill probabilities apply.
//! - **Fees**: taker fee and maker rebate in bps, computed per fill.
//! - **Slippage**: applied to market/taker orders.
//!
//! Nothing here is hidden: every knob is a field of the config. With
//! `base_latency_ms = 0`, `reject_prob = 0` and latency disabled, the venue is
//! fully deterministic and suitable for backtests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use async_trait::async_trait;
use lq_core::bus::EventBus;
use lq_core::config::PaperSimConfig;
use lq_core::event::ExecutionEvent;
use lq_core::models::{FillEvent, Order, Position};
use lq_types::{
    Amount, Exchange, OrderStatus, OrderType, Price, Qty, Side, Symbol, TimestampMs,
};
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use uuid::Uuid;

use crate::state_machine::OrderStateMachine;
use crate::venue::{ExecutionVenue, OrderPlacement, VenueError};

/// Provider of the current touch (best bid, best ask) for a symbol. Used to
/// price market/taker orders. In the paper exchange this reads the simulated
/// book; in pure unit tests it can be a stub.
pub type PriceProvider = Arc<dyn Fn(&Symbol) -> Option<(Price, Price)> + Send + Sync>;

/// Latency simulation: sleeps only when enabled.
pub struct LatencySim {
    enabled: bool,
    base_ms: f64,
    jitter_ms: f64,
    rng: Mutex<StdRng>,
}

impl LatencySim {
    pub fn new(enabled: bool, base_ms: f64, jitter_ms: f64, seed: u64) -> Self {
        Self {
            enabled,
            base_ms,
            jitter_ms,
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
        }
    }

    pub async fn simulate(&self) -> Result<(), VenueError> {
        if !self.enabled {
            return Ok(());
        }
        let jitter = if self.jitter_ms > 0.0 {
            self.rng.lock().gen_range(0.0..self.jitter_ms)
        } else {
            0.0
        };
        let ms = (self.base_ms + jitter).max(0.0);
        tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
        Ok(())
    }
}

struct PaperOrder {
    order: Order,
    resting: bool,
}

/// The paper execution venue.
pub struct PaperExecutionVenue {
    venue: Exchange,
    cfg: PaperSimConfig,
    rng: Mutex<StdRng>,
    latency: LatencySim,
    orders: Mutex<HashMap<Uuid, PaperOrder>>,
    positions: Mutex<HashMap<Symbol, Position>>,
    bus: Arc<EventBus>,
    prices: Mutex<Option<PriceProvider>>,
    fills: AtomicU64,
    rejects: AtomicU64,
    publish: bool,
}

impl PaperExecutionVenue {
    pub fn new(venue: Exchange, cfg: PaperSimConfig, bus: Arc<EventBus>) -> Self {
        Self::with_seed(venue, cfg, bus, 0x5EED_2024, false)
    }

    /// Construct with explicit RNG seed and latency toggle (deterministic
    /// backtests use `simulate_latency = false`).
    pub fn with_seed(
        venue: Exchange,
        cfg: PaperSimConfig,
        bus: Arc<EventBus>,
        seed: u64,
        simulate_latency: bool,
    ) -> Self {
        Self {
            venue,
            cfg: cfg.clone(),
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
            latency: LatencySim::new(
                simulate_latency,
                cfg.base_latency_ms,
                cfg.latency_jitter_ms,
                seed,
            ),
            orders: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            bus,
            prices: Mutex::new(None),
            fills: AtomicU64::new(0),
            rejects: AtomicU64::new(0),
            publish: true,
        }
    }

    pub fn with_price_provider(mut self, provider: PriceProvider) -> Self {
        *self.prices.get_mut() = Some(provider);
        self
    }

    /// Toggle bus event publishing. Backtests disable publishing and apply
    /// fill events synchronously from `report_fill`'s return value, which
    /// removes the (non-deterministic) async broker delivery from the replay.
    pub fn with_publishing(mut self, publish: bool) -> Self {
        self.publish = publish;
        self
    }

    /// Attach a price provider after construction (used by the paper
    /// exchange once the shared book exists).
    pub fn set_price_provider(&self, provider: PriceProvider) {
        *self.prices.lock() = Some(provider);
    }

    /// Snapshot of working (non-terminal) order ids, sorted for a
    /// deterministic iteration order. HashMap iteration order is randomized
    /// per process, which would make backtest fill ordering (and thus
    /// average-entry blending) non-reproducible.
    pub fn working_order_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .orders
            .lock()
            .iter()
            .filter(|(_, o)| !o.order.status.is_terminal())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Cloned view of a tracked order (for the paper exchange's matching).
    pub fn order_snapshot(&self, order_id: Uuid) -> Option<Order> {
        self.orders.lock().get(&order_id).map(|po| po.order.clone())
    }

    pub fn stats(&self) -> (u64, u64) {
        (
            self.fills.load(AtomicOrdering::Relaxed),
            self.rejects.load(AtomicOrdering::Relaxed),
        )
    }

    /// Report a fill against a working order. Used by the paper exchange to
    /// match resting orders against the simulated book. Returns the resulting
    /// fill event for synchronous consumers (backtests).
    pub async fn report_fill(
        &self,
        order_id: Uuid,
        price: Price,
        qty: Qty,
        taker: bool,
    ) -> Result<FillEvent, VenueError> {
        let symbol = {
            let orders = self.orders.lock();
            let Some(po) = orders.get(&order_id) else {
                return Err(VenueError::UnknownOrder(order_id));
            };
            po.order.symbol.clone()
        };

        let fee_bps = if taker {
            self.cfg.fee_rate_bps
        } else {
            -self.cfg.maker_rebate_bps
        };
        let notional = price * qty;
        let fee = notional
            * (Amount::from_f64_retain(fee_bps).unwrap_or_default()
                / Amount::from(10_000));

        let (side, event_ts, fill_event) = {
            let mut orders = self.orders.lock();
            let Some(po) = orders.get_mut(&order_id) else {
                return Err(VenueError::UnknownOrder(order_id));
            };
            let now = TimestampMs::now();
            let execution =
                OrderStateMachine::apply_fill(&mut po.order, qty, price, fee, "USDT", now)?;
            let fill = OrderStateMachine::to_fill_event(&execution);
            if po.order.status == OrderStatus::Filled {
                po.resting = false;
            }
            self.fills.fetch_add(1, AtomicOrdering::Relaxed);
            self.apply_fill_to_position(&symbol, po.order.side, price, qty, fee);
            let side = po.order.side;
            if self.publish {
                let event = ExecutionEvent::Fill(fill.clone());
                let _ = self.bus.execution().try_publish(event);
            }
            (side, now, fill)
        };
        let _ = (side, event_ts);
        Ok(fill_event)
    }

    /// Keep the venue's internal position view consistent with fills. The
    /// engine-level position tracking lives in the position manager.
    fn apply_fill_to_position(&self, symbol: &Symbol, side: Side, price: Price, qty: Qty, fee: Amount) {
        let mut positions = self.positions.lock();
        let mut pos = positions
            .remove(symbol)
            .unwrap_or_else(|| Position {
                venue: self.venue,
                symbol: symbol.clone(),
                ..Position::default()
            });
        let signed = match side {
            Side::Bid => qty,
            Side::Ask => -qty,
        };
        let old = pos.net_qty;
        let new = old + signed;
        let old_abs = old.abs();

        if old_abs > Qty::ZERO && (old.is_sign_positive() != signed.is_sign_positive()) {
            // Closing (part of) a position: realize PnL against avg entry.
            let closing = signed.abs().min(old_abs);
            let pnl = if old.is_sign_positive() {
                (price - pos.avg_entry) * closing
            } else {
                (pos.avg_entry - price) * closing
            };
            pos.realized_pnl += pnl - fee;
        } else {
            // Increasing a position: blend average entry; opening fee is
            // charged against PnL immediately.
            let total = old_abs + signed.abs();
            if total > Qty::ZERO {
                pos.avg_entry = (old_abs * pos.avg_entry + signed.abs() * price) / total;
            }
            pos.realized_pnl -= fee;
        }
        pos.net_qty = new;
        pos.event_ts = TimestampMs::now();
        positions.insert(symbol.clone(), pos);
    }
}

#[async_trait]
impl ExecutionVenue for PaperExecutionVenue {
    fn venue(&self) -> Exchange {
        self.venue
    }

    async fn place_order(&self, order: &mut Order) -> Result<OrderPlacement, VenueError> {
        if order.quantity <= Qty::ZERO {
            return Err(VenueError::Invalid("quantity must be positive".into()));
        }
        if order.venue != self.venue {
            return Err(VenueError::Invalid(format!(
                "order venue {} != venue {}",
                order.venue, self.venue
            )));
        }

        // Simulated venue rejection.
        if self.rng.lock().gen_range(0.0..1.0) < self.cfg.reject_prob {
            self.rejects.fetch_add(1, AtomicOrdering::Relaxed);
            order.status = OrderStatus::Rejected;
            if self.publish {
                let _ = self.bus.execution().try_publish(ExecutionEvent::Rejected {
                    order_id: order.order_id,
                    venue: self.venue,
                    reason: "simulated venue rejection".into(),
                    ts: TimestampMs::now(),
                });
            }
            return Err(VenueError::Rejected("simulated venue rejection".into()));
        }

        self.latency.simulate().await?;

        let venue_order_id = Uuid::new_v4().to_string();
        let mut po = PaperOrder {
            order: Order {
                order_id: order.order_id,
                venue_order_id: venue_order_id.clone(),
                status: OrderStatus::Acknowledged,
                ..order.clone()
            },
            resting: order.order_type.is_limit(),
        };

        // Market orders fill immediately at the current touch + slippage.
        if matches!(order.order_type, OrderType::Market) {
            let prices = self.prices.lock();
            let (_, best_ask) = prices
                .as_ref()
                .and_then(|p| p(&po.order.symbol))
                .ok_or_else(|| VenueError::Internal("no price provider".into()))?;
            let slippage = Amount::from_f64_retain(self.cfg.slippage_bps).unwrap_or_default()
                / Amount::from(10_000);
            let fill_price = match order.side {
                Side::Bid => best_ask * (Amount::ONE + slippage),
                Side::Ask => {
                    let (best_bid, _) = prices
                        .as_ref()
                        .and_then(|p| p(&po.order.symbol))
                        .ok_or_else(|| VenueError::Internal("no price provider".into()))?;
                    best_bid * (Amount::ONE - slippage)
                }
            };
            drop(prices);
            let fill_qty = po.order.quantity;
            let fee = fill_price * fill_qty
                * (Amount::from_f64_retain(self.cfg.fee_rate_bps).unwrap_or_default()
                    / Amount::from(10_000));
            let execution = OrderStateMachine::apply_fill(
                &mut po.order,
                fill_qty,
                fill_price,
                fee,
                "USDT",
                TimestampMs::now(),
            )?;
            let fill = OrderStateMachine::to_fill_event(&execution);
            self.fills.fetch_add(1, AtomicOrdering::Relaxed);
            if self.publish {
                let _ = self.bus.execution().try_publish(ExecutionEvent::Fill(fill));
            }
            let status = po.order.status;
            let filled_qty = po.order.filled_quantity;
            let avg_price = po.order.avg_fill_price;
            self.orders
                .lock()
                .insert(order.order_id, po);
            order.venue_order_id = venue_order_id.clone();
            order.status = status;
            order.filled_quantity = filled_qty;
            order.avg_fill_price = avg_price;
            return Ok(OrderPlacement {
                venue_order_id,
                status,
            });
        }

        // Resting limit order.
        if self.publish {
            let _ = self.bus.execution().try_publish(ExecutionEvent::New {
                order_id: order.order_id,
                venue: self.venue,
                ts: TimestampMs::now(),
            });
            let _ = self.bus.execution().try_publish(ExecutionEvent::Acknowledged {
                order_id: order.order_id,
                venue: self.venue,
                ts: TimestampMs::now(),
            });
        }

        let status = po.order.status;
        self.orders.lock().insert(order.order_id, po);
        order.venue_order_id = venue_order_id.clone();
        order.status = status;
        Ok(OrderPlacement {
            venue_order_id,
            status,
        })
    }

    async fn cancel_order(&self, order_id: Uuid) -> Result<(), VenueError> {
        self.latency.simulate().await?;
        let mut orders = self.orders.lock();
        let Some(po) = orders.get_mut(&order_id) else {
            return Err(VenueError::UnknownOrder(order_id));
        };
        if po.order.status.is_terminal() {
            return Err(VenueError::UnknownOrder(order_id));
        }
        if self.publish {
            let _ = self.bus.execution().try_publish(ExecutionEvent::CancelRequested {
                order_id,
                venue: self.venue,
                ts: TimestampMs::now(),
            });
        }
        po.order.status = OrderStatus::Cancelled;
        po.order.updated_at = TimestampMs::now();
        if self.publish {
            let _ = self.bus.execution().try_publish(ExecutionEvent::Cancelled {
                order_id,
                venue: self.venue,
                ts: TimestampMs::now(),
            });
        }
        Ok(())
    }

    async fn cancel_all(&self, symbol: Option<&Symbol>) -> Result<usize, VenueError> {
        let ids: Vec<Uuid> = self
            .orders
            .lock()
            .iter()
            .filter(|(_, o)| {
                !o.order.status.is_terminal()
                    && symbol.map(|s| o.order.symbol == *s).unwrap_or(true)
            })
            .map(|(id, _)| *id)
            .collect();
        let count = ids.len();
        for id in ids {
            let _ = self.cancel_order(id).await;
        }
        Ok(count)
    }

    async fn get_order_status(&self, order_id: Uuid) -> Result<OrderStatus, VenueError> {
        self.orders
            .lock()
            .get(&order_id)
            .map(|o| o.order.status)
            .ok_or(VenueError::UnknownOrder(order_id))
    }

    async fn get_position(&self, symbol: &Symbol) -> Result<Position, VenueError> {
        Ok(self
            .positions
            .lock()
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| Position {
                venue: self.venue,
                symbol: symbol.clone(),
                ..Position::default()
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::config::PaperSimConfig;
    use rust_decimal_macros::dec;

    fn cfg() -> PaperSimConfig {
        PaperSimConfig {
            reject_prob: 0.0,
            fee_rate_bps: 2.5,
            maker_rebate_bps: 0.5,
            slippage_bps: 1.0,
            ..PaperSimConfig::default()
        }
    }

    #[tokio::test]
    async fn resting_limit_order_acks() {
        let bus = Arc::new(EventBus::new());
        let venue = PaperExecutionVenue::with_seed(Exchange::Paper, cfg(), bus.clone(), 1, false);
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.0)),
            dec!(0.1),
        );
        let placement = venue.place_order(&mut o).await.unwrap();
        assert_eq!(placement.status, OrderStatus::Acknowledged);
        assert_eq!(venue.working_order_ids().len(), 1);
    }

    #[tokio::test]
    async fn market_order_fills_immediately() {
        let bus = Arc::new(EventBus::new());
        let venue = PaperExecutionVenue::with_seed(Exchange::Paper, cfg(), bus.clone(), 1, false)
            .with_price_provider(Arc::new(|_| {
                Some((dec!(99.0), dec!(101.0)))
            }));
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Market,
            None,
            dec!(0.1),
        );
        let placement = venue.place_order(&mut o).await.unwrap();
        assert_eq!(placement.status, OrderStatus::Filled);
        // Slippage: fill at ask * (1 + 1bp) = 101 * 1.0001 = 101.0101
        assert_eq!(o.avg_fill_price, Some(dec!(101.0101)));
        assert_eq!(venue.working_order_ids().len(), 0);
    }

    #[tokio::test]
    async fn simulated_rejection() {
        let bus = Arc::new(EventBus::new());
        let mut c = cfg();
        c.reject_prob = 1.0;
        let venue = PaperExecutionVenue::with_seed(Exchange::Paper, c, bus.clone(), 1, false);
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.0)),
            dec!(0.1),
        );
        let err = venue.place_order(&mut o).await.unwrap_err();
        assert!(matches!(err, VenueError::Rejected(_)));
        assert_eq!(o.status, OrderStatus::Rejected);
    }

    #[tokio::test]
    async fn cancel_working_order() {
        let bus = Arc::new(EventBus::new());
        let venue = PaperExecutionVenue::with_seed(Exchange::Paper, cfg(), bus.clone(), 1, false);
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.0)),
            dec!(0.1),
        );
        venue.place_order(&mut o).await.unwrap();
        venue.cancel_order(o.order_id).await.unwrap();
        assert_eq!(venue.get_order_status(o.order_id).await.unwrap(), OrderStatus::Cancelled);
        assert_eq!(venue.working_order_ids().len(), 0);
    }

    #[tokio::test]
    async fn report_fill_updates_order() {
        let bus = Arc::new(EventBus::new());
        let venue = PaperExecutionVenue::with_seed(Exchange::Paper, cfg(), bus.clone(), 1, false);
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.0)),
            dec!(0.1),
        );
        venue.place_order(&mut o).await.unwrap();
        venue
            .report_fill(o.order_id, dec!(100.0), dec!(0.04), false)
            .await
            .unwrap();
        assert_eq!(venue.get_order_status(o.order_id).await.unwrap(), OrderStatus::PartiallyFilled);
        venue
            .report_fill(o.order_id, dec!(100.0), dec!(0.06), false)
            .await
            .unwrap();
        assert_eq!(venue.get_order_status(o.order_id).await.unwrap(), OrderStatus::Filled);
    }
}