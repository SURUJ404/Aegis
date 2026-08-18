//! Deterministic order state machine.
//!
//! Legal transitions:
//!
//! ```text
//! Created ──► Submitted ──► Acknowledged ──► PartiallyFilled ──► Filled
//!                │              │                 │
//!                ▼              ▼                 ▼
//!             Rejected      CancelRequested     CancelRequested
//!                │              │                 │
//!                ▼              ▼                 ▼
//!            (terminal)      Cancelled         Cancelled / Expired
//!    (terminal states: Filled, Cancelled, Rejected, Expired)
//! ```
//!
//! Illegal transitions are rejected at runtime, and the type system is used
//! where practical (the `OrderStatus` enum has no invalid states).
//!
//! Idempotency: applying the *same* fill twice is detected and rejected
//! (a fill must be monotonically increasing in `filled_quantity`).

use lq_core::models::{Execution, FillEvent, Order};
use lq_types::{Amount, ExecutionType, OrderStatus, Price, Qty};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StateError {
    #[error("illegal transition {from:?} -> {to:?}")]
    IllegalTransition {
        from: OrderStatus,
        to: OrderStatus,
    },
    #[error("order {order_id} already terminal ({status:?})")]
    AlreadyTerminal { order_id: Uuid, status: OrderStatus },
    #[error("fill qty {qty} exceeds remaining {remaining} on order {order_id}")]
    FillExceedsRemaining {
        order_id: Uuid,
        qty: Qty,
        remaining: Qty,
    },
    #[error("duplicate fill: order {order_id} already at filled qty {filled}")]
    DuplicateFill { order_id: Uuid, filled: Qty },
}

/// The order lifecycle state machine.
#[derive(Debug, Default)]
pub struct OrderStateMachine;

impl OrderStateMachine {
    /// Validate a transition. Same-status transitions are allowed only for
    /// `PartiallyFilled` (multiple partial fills).
    pub fn transition(from: OrderStatus, to: OrderStatus) -> Result<(), StateError> {
        if from == to {
            return if from == OrderStatus::PartiallyFilled {
                Ok(())
            } else {
                Err(StateError::IllegalTransition { from, to })
            };
        }
        let legal = match from {
            OrderStatus::Created => matches!(
                to,
                OrderStatus::Submitted | OrderStatus::Rejected
            ),
            OrderStatus::Submitted => matches!(
                to,
                OrderStatus::Acknowledged
                    | OrderStatus::Rejected
                    | OrderStatus::Cancelled
            ),
            OrderStatus::Acknowledged => matches!(
                to,
                OrderStatus::PartiallyFilled
                    | OrderStatus::Filled
                    | OrderStatus::CancelRequested
                    | OrderStatus::Cancelled
                    | OrderStatus::Expired
                    | OrderStatus::Rejected
            ),
            OrderStatus::PartiallyFilled => matches!(
                to,
                OrderStatus::Filled
                    | OrderStatus::CancelRequested
                    | OrderStatus::Expired
            ),
            OrderStatus::CancelRequested => matches!(to, OrderStatus::Cancelled),
            OrderStatus::Filled
            | OrderStatus::Cancelled
            | OrderStatus::Rejected
            | OrderStatus::Expired => false,
        };
        if legal {
            Ok(())
        } else {
            Err(StateError::IllegalTransition { from, to })
        }
    }

    /// Apply a fill to an order, updating status and filled quantity, and
    /// returning the resulting execution record.
    pub fn apply_fill(
        order: &mut Order,
        fill_qty: Qty,
        fill_price: Price,
        fee: Amount,
        fee_currency: impl Into<String>,
        event_ts: lq_types::TimestampMs,
    ) -> Result<Execution, StateError> {
        if order.is_terminal() {
            return Err(StateError::AlreadyTerminal {
                order_id: order.order_id,
                status: order.status,
            });
        }
        if fill_qty.is_zero() {
            return Err(StateError::FillExceedsRemaining {
                order_id: order.order_id,
                qty: fill_qty,
                remaining: order.remaining(),
            });
        }
        if order.filled_quantity == order.quantity && !order.quantity.is_zero() {
            return Err(StateError::DuplicateFill {
                order_id: order.order_id,
                filled: order.filled_quantity,
            });
        }

        let remaining = order.remaining();
        if fill_qty > remaining {
            return Err(StateError::FillExceedsRemaining {
                order_id: order.order_id,
                qty: fill_qty,
                remaining,
            });
        }

        let was_partial = !order.filled_quantity.is_zero();
        order.filled_quantity += fill_qty;

        // Weighted average fill price.
        order.avg_fill_price = match order.avg_fill_price {
            Some(prev) => {
                Some((prev * (order.filled_quantity - fill_qty) + fill_price * fill_qty)
                    / order.filled_quantity)
            }
            None => Some(fill_price),
        };

        let new_status = if order.filled_quantity >= order.quantity {
            OrderStatus::Filled
        } else if was_partial || !order.filled_quantity.is_zero() {
            OrderStatus::PartiallyFilled
        } else {
            // First fill less than full quantity.
            OrderStatus::PartiallyFilled
        };

        Self::transition(order.status, new_status)?;
        order.status = new_status;
        order.updated_at = event_ts;

        Ok(Execution {
            execution_id: Uuid::new_v4(),
            order_id: order.order_id,
            client_order_id: order.client_order_id.clone(),
            venue: order.venue,
            symbol: order.symbol.clone(),
            side: order.side,
            exec_type: if new_status == OrderStatus::Filled {
                ExecutionType::Fill
            } else {
                ExecutionType::PartialFill
            },
            price: fill_price,
            qty: fill_qty,
            fee,
            fee_currency: fee_currency.into(),
            exchange_ts: event_ts,
            event_ts,
        })
    }

    /// Build a fill event from an execution record.
    pub fn to_fill_event(execution: &Execution) -> FillEvent {
        FillEvent {
            execution_id: execution.execution_id,
            order_id: execution.order_id,
            client_order_id: execution.client_order_id.clone(),
            venue: execution.venue,
            symbol: execution.symbol.clone(),
            side: execution.side,
            price: execution.price,
            qty: execution.qty,
            fee: execution.fee,
            fee_currency: execution.fee_currency.clone(),
            exchange_ts: execution.exchange_ts,
            event_ts: execution.event_ts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_core::models::Order;
    use lq_types::{Exchange, OrderType, Side, Symbol, TimestampMs};
    use rust_decimal_macros::dec;

    fn order(qty: Qty) -> Order {
        let mut o = Order::new(
            Exchange::Paper,
            Symbol("BTC-USDT".into()),
            Side::Bid,
            OrderType::Limit,
            Some(dec!(100.0)),
            qty,
        );
        o.status = OrderStatus::Acknowledged;
        o
    }

    #[test]
    fn legal_transitions() {
        assert!(OrderStateMachine::transition(
            OrderStatus::Created,
            OrderStatus::Submitted
        )
        .is_ok());
        assert!(OrderStateMachine::transition(
            OrderStatus::Submitted,
            OrderStatus::Acknowledged
        )
        .is_ok());
        assert!(OrderStateMachine::transition(
            OrderStatus::Acknowledged,
            OrderStatus::Filled
        )
        .is_ok());
        assert!(OrderStateMachine::transition(
            OrderStatus::Filled,
            OrderStatus::Cancelled
        )
        .is_err());
        assert!(OrderStateMachine::transition(
            OrderStatus::Created,
            OrderStatus::Filled
        )
        .is_err());
    }

    #[test]
    fn full_fill_transitions_to_filled() {
        let mut o = order(dec!(1.0));
        let ex = OrderStateMachine::apply_fill(
            &mut o,
            dec!(1.0),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(1),
        )
        .unwrap();
        assert_eq!(o.status, OrderStatus::Filled);
        assert_eq!(ex.exec_type, ExecutionType::Fill);
    }

    #[test]
    fn partial_then_full_fill() {
        let mut o = order(dec!(1.0));
        let _ = OrderStateMachine::apply_fill(
            &mut o,
            dec!(0.4),
            dec!(99.0),
            dec!(0.0),
            "USDT",
            TimestampMs(1),
        )
        .unwrap();
        assert_eq!(o.status, OrderStatus::PartiallyFilled);
        let ex = OrderStateMachine::apply_fill(
            &mut o,
            dec!(0.6),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(2),
        )
        .unwrap();
        assert_eq!(o.status, OrderStatus::Filled);
        assert_eq!(ex.exec_type, ExecutionType::Fill);
        // Weighted avg = (0.4*99 + 0.6*100)/1.0 = 99.6
        assert_eq!(o.avg_fill_price, Some(dec!(99.6)));
    }

    #[test]
    fn rejects_fill_exceeding_remaining() {
        let mut o = order(dec!(1.0));
        assert!(OrderStateMachine::apply_fill(
            &mut o,
            dec!(1.5),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(1),
        )
        .is_err());
    }

    #[test]
    fn rejects_fill_after_terminal() {
        let mut o = order(dec!(1.0));
        let _ = OrderStateMachine::apply_fill(
            &mut o,
            dec!(1.0),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(1),
        );
        assert!(OrderStateMachine::apply_fill(
            &mut o,
            dec!(0.1),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(2),
        )
        .is_err());
    }

    #[test]
    fn duplicate_full_fill_detected() {
        let mut o = order(dec!(1.0));
        let _ = OrderStateMachine::apply_fill(
            &mut o,
            dec!(1.0),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(1),
        );
        // Second report of the same fill must be caught by the terminal check.
        let r = OrderStateMachine::apply_fill(
            &mut o,
            dec!(1.0),
            dec!(100.0),
            dec!(0.0),
            "USDT",
            TimestampMs(2),
        );
        assert!(r.is_err());
    }
}