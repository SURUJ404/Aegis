//! Shared engine state: the observable projection of positions, orders,
//! executions, market state and risk status.
//!
//! Everything here is behind interior mutability so that the control API,
//! observability and persistence can all read a consistent view without a
//! central lock.

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

use dashmap::DashMap;
use lq_types::{Exchange, Symbol, TimestampMs};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{Execution, Inventory, MarketState, Order, Position};

/// High-level risk status, updated by the risk engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskStatus {
    /// Whether risk limits are actively enforced (always true in production;
    /// configurable off only for tests/demos).
    pub armed: bool,
    /// Kill switch engaged: no new orders may be submitted.
    pub halted: bool,
    pub halt_reason: Option<String>,
    pub updated_at: TimestampMs,
}

impl RiskStatus {
    pub fn armed() -> Self {
        Self {
            armed: true,
            halted: false,
            halt_reason: None,
            updated_at: TimestampMs::now(),
        }
    }
}

/// The shared observable state of the engine. Cheap to clone (Arc inside).
#[derive(Debug, Clone)]
pub struct EngineState {
    /// Positions keyed by `(Exchange, Symbol)`.
    pub positions: Arc<DashMap<(Exchange, Symbol), Position>>,
    /// Per-symbol inventory (aggregated across venues).
    pub inventory: Arc<DashMap<Symbol, Inventory>>,
    /// Orders keyed by order id.
    pub orders: Arc<DashMap<Uuid, Order>>,
    /// Executions keyed by execution id.
    pub executions: Arc<DashMap<Uuid, Execution>>,
    /// Latest computed market state per `(Exchange, Symbol)`.
    pub market_state: Arc<DashMap<(Exchange, Symbol), MarketState>>,
    /// Current risk status.
    pub risk: Arc<RwLock<RiskStatus>>,
    pub strategy_running: Arc<AtomicBool>,
    pub started_at: TimestampMs,
}

impl EngineState {
    pub fn new() -> Self {
        Self {
            positions: Arc::new(DashMap::new()),
            inventory: Arc::new(DashMap::new()),
            orders: Arc::new(DashMap::new()),
            executions: Arc::new(DashMap::new()),
            market_state: Arc::new(DashMap::new()),
            risk: Arc::new(RwLock::new(RiskStatus::armed())),
            strategy_running: Arc::new(AtomicBool::new(false)),
            started_at: TimestampMs::now(),
        }
    }

    pub fn is_strategy_running(&self) -> bool {
        self.strategy_running.load(AtomicOrdering::SeqCst)
    }

    pub fn set_strategy_running(&self, running: bool) {
        self.strategy_running.store(running, AtomicOrdering::SeqCst);
    }

    pub fn risk_snapshot(&self) -> RiskStatus {
        self.risk.read().clone()
    }

    pub fn is_halted(&self) -> bool {
        self.risk.read().halted
    }

    /// Record a position update.
    pub fn update_position(&self, position: Position) {
        let key = (position.venue, position.symbol.clone());
        self.positions.insert(key, position);
    }
}

impl Default for EngineState {
    fn default() -> Self {
        Self::new()
    }
}