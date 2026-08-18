//! Execution venue abstraction.
//!
//! Every execution path — paper or live — implements [`ExecutionVenue`]. Live
//! adapters are intentionally *not* implemented in this crate: the paper venue
//! is the only always-available implementation, and live venues (OKX, Binance,
//! Bybit) would sit behind this same trait with explicit opt-in.

use async_trait::async_trait;
use lq_core::models::{Order, Position};
use lq_types::{Exchange, OrderStatus, Symbol};
use uuid::Uuid;

use crate::state_machine::StateError;

/// Result of a successful placement: the venue-side id + final status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderPlacement {
    pub venue_order_id: String,
    pub status: OrderStatus,
}

/// Errors a venue can produce.
#[derive(Debug, thiserror::Error)]
pub enum VenueError {
    #[error("venue rejected order: {0}")]
    Rejected(String),
    #[error("venue timeout")]
    Timeout,
    #[error("venue disconnected")]
    Disconnected,
    #[error("unknown order {0}")]
    UnknownOrder(Uuid),
    #[error("rate limited by venue")]
    RateLimited,
    #[error("invalid order: {0}")]
    Invalid(String),
    #[error("order state error: {0}")]
    State(#[from] StateError),
    #[error("venue internal error: {0}")]
    Internal(String),
}

/// The execution interface every venue implements.
///
/// Implementations are responsible for:
/// - simulating or performing the actual order lifecycle,
/// - publishing [`ExecutionEvent`](lq_core::event::ExecutionEvent)s on the bus,
/// - guaranteeing idempotency for duplicate submissions.
#[async_trait]
pub trait ExecutionVenue: Send + Sync {
    fn venue(&self) -> Exchange;

    /// Submit an order. The caller must have passed risk validation.
    /// The order's status field is updated in place as it progresses.
    async fn place_order(&self, order: &mut Order) -> Result<OrderPlacement, VenueError>;

    async fn cancel_order(&self, order_id: Uuid) -> Result<(), VenueError>;

    /// Cancel all working orders (optionally for a symbol). Returns the number
    /// of cancellation requests issued.
    async fn cancel_all(&self, symbol: Option<&Symbol>) -> Result<usize, VenueError>;

    async fn get_order_status(&self, order_id: Uuid) -> Result<OrderStatus, VenueError>;

    async fn get_position(&self, symbol: &Symbol) -> Result<Position, VenueError>;
}