//! Pure price-time matching. No RNG, no wall clock, no `HashMap`.
//!
//! All functions are free of side effects except the explicit `&mut`
//! book/orders updates in [`match_taker`]; everything is a deterministic
//! function of (book, orders, incoming order, logical `ts_ms`).

use std::collections::BTreeMap;

use lq_sequencer::entry::StpPolicy;
use lq_types::{OrderStatus, OrderType, Price, Qty, Side, TimeInForce};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::book::Book;
use crate::order::ClobOrder;

/// Execution policy derived from order type + time-in-force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Rest remainder on the book.
    Gtc,
    /// Fill what you can, cancel the remainder (also: market orders).
    Ioc,
    /// Fill entirely or leave the book untouched.
    Fok,
    /// Must rest; reject if it would cross.
    PostOnly,
}

/// Effective policy: order type wins over TIF when both are specific.
pub fn exec_policy(order_type: OrderType, tif: TimeInForce) -> Policy {
    match order_type {
        OrderType::Market => Policy::Ioc,
        OrderType::PostOnly => Policy::PostOnly,
        OrderType::ImmediateOrCancel => Policy::Ioc,
        OrderType::FillOrKill => Policy::Fok,
        OrderType::Limit => match tif {
            TimeInForce::Gtc => Policy::Gtc,
            TimeInForce::Ioc => Policy::Ioc,
            TimeInForce::Fok => Policy::Fok,
            TimeInForce::PostOnly => Policy::PostOnly,
        },
    }
}

/// Would a limit at `price` on `taker_side` cross the current touch?
pub fn would_cross(book: &Book, taker_side: Side, price: Price) -> bool {
    match taker_side {
        Side::Bid => book
            .best_price(Side::Ask)
            .map(|ask| ask <= price)
            .unwrap_or(false),
        Side::Ask => book
            .best_price(Side::Bid)
            .map(|bid| bid >= price)
            .unwrap_or(false),
    }
}

/// One executed trade produced by [`match_taker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecFill {
    pub maker_order_id: Uuid,
    pub price: Price,
    pub quantity: Qty,
}

/// Result of a match run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    pub fills: Vec<ExecFill>,
    pub remaining: Qty,
    /// Set when self-trade prevention aborted the taker mid-loop.
    pub abort: Option<StpAbort>,
    /// Resting makers cancelled by STP during the loop (in encounter order).
    pub stp_cancelled: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StpAbort {
    Taker,
    Both,
}

/// Read-only mirror of [`match_taker`] used for FOK pre-checks. Returns the
/// quantity that would remain unfilled and whether STP would abort the taker.
///
/// Simulates on a clone of (book, orders) so multi-level fills and
/// `CancelResting` skips behave exactly like the mutating loop.
pub fn simulate(
    book: &Book,
    orders: &BTreeMap<Uuid, ClobOrder>,
    taker_side: Side,
    limit: Option<Price>,
    owner: &str,
    stp: StpPolicy,
    quantity: Qty,
) -> (Qty, bool) {
    let mut book = book.clone();
    let mut orders = orders.clone();
    let mut remaining = quantity;
    loop {
        if remaining <= Decimal::ZERO {
            return (remaining, false);
        }
        let Some((price, id)) = book.best_opposite(taker_side) else {
            break;
        };
        if !crosses(taker_side, limit, price) {
            break;
        }
        let maker = orders.get(&id).expect("book/order consistency");
        if stp_applies(owner, &maker.owner, stp) {
            match stp {
                StpPolicy::CancelResting => {
                    // Maker would be cancelled; the taker continues past it.
                    book.remove(taker_side.opposite(), price, id);
                    continue;
                }
                StpPolicy::CancelTaker | StpPolicy::CancelBoth => {
                    return (remaining, true);
                }
                StpPolicy::None => unreachable!("stp_applies guards None"),
            }
        }
        let fill = remaining.min(maker.remaining());
        remaining -= fill;
        apply_maker_fill(&mut book, &mut orders, taker_side, price, id, fill);
    }
    (remaining, false)
}

/// Execute the match loop against the live book, mutating makers and the
/// taker order (which must already be inserted into `orders`).
pub fn match_taker(
    book: &mut Book,
    orders: &mut BTreeMap<Uuid, ClobOrder>,
    taker_id: Uuid,
    taker_side: Side,
    limit: Option<Price>,
    ts_ms: u64,
) -> MatchOutcome {
    let mut fills = Vec::new();
    let mut abort: Option<StpAbort> = None;
    let mut stp_cancelled = Vec::new();

    // Snapshot taker attributes (owner/stp) without holding a borrow.
    let (owner, stp) = {
        let t = &orders[&taker_id];
        (t.owner.clone(), t.stp)
    };

    let mut remaining = orders[&taker_id].quantity;
    loop {
        if remaining <= Decimal::ZERO {
            break;
        }
        let Some((price, maker_id)) = book.best_opposite(taker_side) else {
            break;
        };
        if !crosses(taker_side, limit, price) {
            break;
        }

        // Self-trade prevention against the resting maker.
        let maker_owner = orders[&maker_id].owner.clone();
        if stp_applies(&owner, &maker_owner, stp) {
            match stp {
                StpPolicy::CancelResting => {
                    remove_maker(book, orders, taker_side, price, maker_id);
                    {
                        let m = orders.get_mut(&maker_id).expect("maker exists");
                        m.status = OrderStatus::Cancelled;
                        m.updated_ts_ms = ts_ms;
                        m.book_price = None;
                    }
                    stp_cancelled.push(maker_id);
                    continue;
                }
                StpPolicy::CancelTaker => {
                    abort = Some(StpAbort::Taker);
                    break;
                }
                StpPolicy::CancelBoth => {
                    remove_maker(book, orders, taker_side, price, maker_id);
                    {
                        let m = orders.get_mut(&maker_id).expect("maker exists");
                        m.status = OrderStatus::Cancelled;
                        m.updated_ts_ms = ts_ms;
                        m.book_price = None;
                    }
                    stp_cancelled.push(maker_id);
                    abort = Some(StpAbort::Both);
                    break;
                }
                StpPolicy::None => unreachable!("stp_applies guards None"),
            }
        }

        let maker_rem = orders[&maker_id].remaining();
        let fill_qty = remaining.min(maker_rem);

        // Mutate maker.
        {
            let m = orders.get_mut(&maker_id).expect("book/order consistency");
            m.filled_quantity += fill_qty;
            m.updated_ts_ms = ts_ms;
            if m.remaining() <= Decimal::ZERO {
                m.status = OrderStatus::Filled;
            } else {
                m.status = OrderStatus::PartiallyFilled;
            }
        }
        if orders[&maker_id].remaining() <= Decimal::ZERO {
            book.pop_front(taker_side.opposite(), price);
            let m = orders.get_mut(&maker_id).expect("maker exists");
            m.book_price = None;
        }

        // Mutate taker.
        {
            let t = orders.get_mut(&taker_id).expect("taker exists");
            t.filled_quantity += fill_qty;
            t.updated_ts_ms = ts_ms;
            if t.remaining() <= Decimal::ZERO {
                t.status = OrderStatus::Filled;
            } else {
                t.status = OrderStatus::PartiallyFilled;
            }
        }

        remaining -= fill_qty;
        fills.push(ExecFill {
            maker_order_id: maker_id,
            price,
            quantity: fill_qty,
        });
    }

    MatchOutcome {
        fills,
        remaining,
        abort,
        stp_cancelled,
    }
}

fn crosses(taker_side: Side, limit: Option<Price>, price: Price) -> bool {
    match (taker_side, limit) {
        (Side::Bid, Some(lim)) => price <= lim,
        (Side::Ask, Some(lim)) => price >= lim,
        (_, None) => true,
    }
}

/// STP triggers only when both owners are equal and the taker has an owner.
fn stp_applies(taker_owner: &str, maker_owner: &str, stp: StpPolicy) -> bool {
    stp != StpPolicy::None && !taker_owner.is_empty() && taker_owner == maker_owner
}

/// Remove the resting maker from the book (status update done by caller).
fn remove_maker(
    book: &mut Book,
    orders: &mut BTreeMap<Uuid, ClobOrder>,
    taker_side: Side,
    price: Price,
    id: Uuid,
) {
    book.remove(taker_side.opposite(), price, id);
    let _ = orders;
}

/// Apply a fill to the maker: reduce remaining, pop from book when full.
fn apply_maker_fill(
    book: &mut Book,
    orders: &mut BTreeMap<Uuid, ClobOrder>,
    taker_side: Side,
    price: Price,
    id: Uuid,
    qty: Qty,
) {
    {
        let m = orders.get_mut(&id).expect("maker exists");
        m.filled_quantity += qty;
        if m.remaining() <= Decimal::ZERO {
            m.status = OrderStatus::Filled;
        } else {
            m.status = OrderStatus::PartiallyFilled;
        }
    }
    if orders[&id].remaining() <= Decimal::ZERO {
        book.pop_front(taker_side.opposite(), price);
        orders.get_mut(&id).expect("maker exists").book_price = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lq_sequencer::entry::MarketId;
    use lq_types::{Exchange, Symbol};
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    fn resting(id: u128, side: Side, price: Price, qty: Qty) -> ClobOrder {
        ClobOrder {
            order_id: Uuid::from_u128(id),
            client_order_id: format!("c-{id}"),
            market: MarketId::new(Exchange::Paper, Symbol("BTC-USDT".into())),
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: qty,
            filled_quantity: Decimal::ZERO,
            status: OrderStatus::Acknowledged,
            time_in_force: TimeInForce::Gtc,
            owner: String::new(),
            stp: StpPolicy::None,
            expiration_ms: None,
            place_global_seq: id as u64,
            created_ts_ms: 0,
            updated_ts_ms: 0,
            book_price: Some(price),
        }
    }

    #[test]
    fn exec_policy_from_types() {
        assert_eq!(exec_policy(OrderType::Limit, TimeInForce::Gtc), Policy::Gtc);
        assert_eq!(
            exec_policy(OrderType::Market, TimeInForce::Gtc),
            Policy::Ioc
        );
        assert_eq!(
            exec_policy(OrderType::Limit, TimeInForce::PostOnly),
            Policy::PostOnly
        );
        assert_eq!(
            exec_policy(OrderType::FillOrKill, TimeInForce::Gtc),
            Policy::Fok
        );
    }

    #[test]
    fn price_time_priority_best_price_then_fifo() {
        let mut book = Book::new();
        let mut orders = BTreeMap::new();
        // Two asks at 100 (first placed), one at 101.
        for (id, price) in [(1u128, dec!(100)), (2, dec!(100)), (3, dec!(101))] {
            let o = resting(id, Side::Ask, price, dec!(1));
            book.rest(Side::Ask, price, o.order_id);
            orders.insert(o.order_id, o);
        }
        // Incoming bid sweeps: 100 first (better), FIFO within 100.
        let taker = resting(99, Side::Bid, dec!(101), dec!(2));
        orders.insert(taker.order_id, taker);
        let out = match_taker(
            &mut book,
            &mut orders,
            Uuid::from_u128(99),
            Side::Bid,
            Some(dec!(101)),
            0,
        );
        assert_eq!(out.fills.len(), 2);
        assert_eq!(out.fills[0].maker_order_id, Uuid::from_u128(1));
        assert_eq!(out.fills[0].price, dec!(100));
        assert_eq!(out.fills[1].maker_order_id, Uuid::from_u128(2));
        assert_eq!(out.remaining, Decimal::ZERO);
        assert_eq!(orders[&Uuid::from_u128(1)].status, OrderStatus::Filled);
        assert_eq!(
            orders[&Uuid::from_u128(3)].status,
            OrderStatus::Acknowledged
        );
    }

    #[test]
    fn does_not_cross_when_limit_is_worse_than_touch() {
        let mut book = Book::new();
        let mut orders = BTreeMap::new();
        let o = resting(1, Side::Ask, dec!(100), dec!(1));
        book.rest(Side::Ask, dec!(100), o.order_id);
        orders.insert(o.order_id, o);
        let taker = resting(99, Side::Bid, dec!(99), dec!(1));
        orders.insert(taker.order_id, taker);
        let out = match_taker(
            &mut book,
            &mut orders,
            Uuid::from_u128(99),
            Side::Bid,
            Some(dec!(99)),
            0,
        );
        assert!(out.fills.is_empty());
        assert_eq!(out.remaining, dec!(1));
    }

    #[test]
    fn simulate_matches_mutating_loop() {
        let mut book = Book::new();
        let mut orders = BTreeMap::new();
        for id in 1u128..=3 {
            let o = resting(id, Side::Ask, dec!(100), dec!(1));
            book.rest(Side::Ask, dec!(100), o.order_id);
            orders.insert(o.order_id, o);
        }
        let (sim_rem, sim_abort) = simulate(
            &book,
            &orders,
            Side::Bid,
            Some(dec!(100)),
            "",
            StpPolicy::None,
            dec!(2),
        );
        assert_eq!(sim_rem, Decimal::ZERO);
        assert!(!sim_abort);

        let taker = resting(99, Side::Bid, dec!(100), dec!(2));
        orders.insert(taker.order_id, taker);
        let out = match_taker(
            &mut book,
            &mut orders,
            Uuid::from_u128(99),
            Side::Bid,
            Some(dec!(100)),
            0,
        );
        assert_eq!(out.remaining, sim_rem);
    }
}
