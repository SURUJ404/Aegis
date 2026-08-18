//! Position and inventory tracking.
//!
//! Consumes [`ExecutionEvent`]s from the bus and updates the shared
//! [`EngineState`]: order statuses, per-venue positions and per-symbol
//! inventory. This is the *system of record* for PnL.

use lq_core::event::ExecutionEvent;
use lq_core::models::{Inventory, Position};
use lq_core::state::EngineState;
use lq_types::{Amount, Side, Symbol};

use crate::state_machine::OrderStateMachine;

/// Stateless position manager: everything is stored in [`EngineState`].
#[derive(Debug, Default)]
pub struct PositionManager;

impl PositionManager {
    pub fn on_execution_event(state: &EngineState, event: &ExecutionEvent) {
        match event {
            ExecutionEvent::New { order_id, ts, .. } => {
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    if OrderStateMachine::transition(o.status, lq_types::OrderStatus::Submitted)
                        .is_ok()
                    {
                        o.status = lq_types::OrderStatus::Submitted;
                        o.updated_at = *ts;
                    }
                }
            }
            ExecutionEvent::Acknowledged { order_id, ts, .. } => {
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    if OrderStateMachine::transition(o.status, lq_types::OrderStatus::Acknowledged)
                        .is_ok()
                    {
                        o.status = lq_types::OrderStatus::Acknowledged;
                        o.updated_at = *ts;
                    }
                }
            }
            ExecutionEvent::CancelRequested { order_id, ts, .. } => {
                let _ = ts;
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    o.status = lq_types::OrderStatus::CancelRequested;
                    o.updated_at = *ts;
                }
            }
            ExecutionEvent::Cancelled { order_id, ts, .. } => {
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    if OrderStateMachine::transition(
                        o.status,
                        lq_types::OrderStatus::Cancelled,
                    )
                    .is_ok()
                    {
                        o.status = lq_types::OrderStatus::Cancelled;
                        o.updated_at = *ts;
                    }
                }
            }
            ExecutionEvent::Rejected { order_id, ts, .. } => {
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    o.status = lq_types::OrderStatus::Rejected;
                    o.updated_at = *ts;
                }
            }
            ExecutionEvent::Expired { order_id, ts, .. } => {
                if let Some(mut o) = state.orders.get_mut(order_id) {
                    o.status = lq_types::OrderStatus::Expired;
                    o.updated_at = *ts;
                }
            }
            ExecutionEvent::Fill(fill) => {
                // Update the stored order through the state machine.
                if let Some(mut o) = state.orders.get_mut(&fill.order_id) {
                    let _ = OrderStateMachine::apply_fill(
                        &mut o,
                        fill.qty,
                        fill.price,
                        fill.fee,
                        &fill.fee_currency,
                        fill.event_ts,
                    );
                }
                Self::apply_fill_to_position(state, fill);
            }
            ExecutionEvent::Trade { .. } => {}
        }
    }

    fn apply_fill_to_position(state: &EngineState, fill: &lq_core::models::FillEvent) {
        let key = (fill.venue, fill.symbol.clone());
        let mut pos = state
            .positions
            .get(&key)
            .map(|p| p.clone())
            .unwrap_or_else(|| Position {
                venue: fill.venue,
                symbol: fill.symbol.clone(),
                ..Position::default()
            });

        let signed = match fill.side {
            Side::Bid => fill.qty,
            Side::Ask => -fill.qty,
        };
        let old = pos.net_qty;
        let new = old + signed;
        let old_abs = old.abs();

        if old_abs > Amount::ZERO && (old.is_sign_positive() != signed.is_sign_positive()) {
            // Closing (part of) the position: realize PnL against avg entry,
            // net of the closing fee.
            let closing = signed.abs().min(old_abs);
            let pnl = if old.is_sign_positive() {
                (fill.price - pos.avg_entry) * closing
            } else {
                (pos.avg_entry - fill.price) * closing
            };
            pos.realized_pnl += pnl - fill.fee;
        } else {
            // Increasing the position: blend average entry. The opening fee is
            // charged against PnL immediately so round-trip accounting nets it.
            let total = old_abs + signed.abs();
            if total > Amount::ZERO {
                pos.avg_entry = (old_abs * pos.avg_entry + signed.abs() * fill.price) / total;
            }
            pos.realized_pnl -= fill.fee;
        }
        pos.net_qty = new;
        pos.event_ts = fill.event_ts;
        state.positions.insert(key, pos);

        Self::recompute_inventory(state, &fill.symbol);
    }

    fn recompute_inventory(state: &EngineState, symbol: &Symbol) {
        let mut net_qty = Amount::ZERO;
        let mut realized_pnl = Amount::ZERO;
        let mut entry_notional = Amount::ZERO;
        for p in state.positions.iter().filter(|p| p.symbol == *symbol) {
            net_qty += p.net_qty;
            realized_pnl += p.realized_pnl;
            entry_notional += (p.net_qty * p.avg_entry).abs();
        }
        let avg_entry = if net_qty.abs() > Amount::ZERO {
            entry_notional / net_qty.abs()
        } else {
            Amount::ZERO
        };
        state.inventory.insert(
            symbol.clone(),
            Inventory {
                symbol: symbol.clone(),
                net_qty,
                avg_entry,
                realized_pnl,
                event_ts: lq_types::TimestampMs::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::Order;
    use lq_types::{Exchange, OrderType, Side, Symbol};
    use rust_decimal_macros::dec;

    #[test]
    fn fills_update_inventory_and_position() {
        let state = EngineState::new();
        let symbol = Symbol("BTC-USDT".into());
        let mut o = Order::new(Exchange::Paper, symbol.clone(), Side::Bid, OrderType::Limit, Some(dec!(100.0)), dec!(1.0));
        o.status = lq_types::OrderStatus::Acknowledged;
        let oid = o.order_id;
        state.orders.insert(oid, o);

        let fill = lq_core::models::FillEvent {
            execution_id: uuid::Uuid::new_v4(),
            order_id: oid,
            client_order_id: "c1".into(),
            venue: Exchange::Paper,
            symbol: symbol.clone(),
            side: Side::Bid,
            price: dec!(100.0),
            qty: dec!(0.5),
            fee: dec!(0.1),
            fee_currency: "USDT".into(),
            exchange_ts: lq_types::TimestampMs(1),
            event_ts: lq_types::TimestampMs(1),
        };
        PositionManager::on_execution_event(&state, &ExecutionEvent::Fill(fill.clone()));

        let pos = state
            .positions
            .get(&(Exchange::Paper, symbol.clone()))
            .map(|p| p.clone())
            .unwrap();
        assert_eq!(pos.net_qty, dec!(0.5));

        let inv = state.inventory.get(&symbol).map(|i| i.clone()).unwrap();
        assert_eq!(inv.net_qty, dec!(0.5));
        // Opening fee is charged immediately.
        assert_eq!(inv.realized_pnl, dec!(-0.1));

        // Close out the position at a profit.
        let mut o = Order::new(Exchange::Paper, symbol.clone(), Side::Ask, OrderType::Limit, Some(dec!(101.0)), dec!(0.5));
        o.status = lq_types::OrderStatus::Acknowledged;
        let oid2 = o.order_id;
        state.orders.insert(oid2, o);
        let sell = lq_core::models::FillEvent {
            execution_id: uuid::Uuid::new_v4(),
            order_id: oid2,
            client_order_id: "c2".into(),
            venue: Exchange::Paper,
            symbol: symbol.clone(),
            side: Side::Ask,
            price: dec!(101.0),
            qty: dec!(0.5),
            fee: dec!(0.05),
            fee_currency: "USDT".into(),
            exchange_ts: lq_types::TimestampMs(2),
            event_ts: lq_types::TimestampMs(2),
        };
        PositionManager::on_execution_event(&state, &ExecutionEvent::Fill(sell));

        let inv = state.inventory.get(&symbol).map(|i| i.clone()).unwrap();
        assert_eq!(inv.net_qty, dec!(0.0));
        // PnL = (101 - 100) * 0.5 - 0.1 - 0.05
        assert_eq!(inv.realized_pnl, dec!(0.35));
    }

    #[test]
    fn cancelled_order_updates_state() {
        let state = EngineState::new();
        let symbol = Symbol("BTC-USDT".into());
        let mut o = Order::new(Exchange::Paper, symbol, Side::Bid, OrderType::Limit, Some(dec!(100.0)), dec!(1.0));
        o.status = lq_types::OrderStatus::Acknowledged;
        let oid = o.order_id;
        state.orders.insert(oid, o);
        PositionManager::on_execution_event(
            &state,
            &ExecutionEvent::Cancelled {
                order_id: oid,
                venue: Exchange::Paper,
                ts: lq_types::TimestampMs(1),
            },
        );
        assert_eq!(
            state.orders.get(&oid).map(|o| o.status).unwrap(),
            lq_types::OrderStatus::Cancelled
        );
    }
}

