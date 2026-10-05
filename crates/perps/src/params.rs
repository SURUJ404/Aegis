//! Margin parameters and operator limits held **inside** the state machine.
//!
//! Everything in here is part of the state hash: replicas must agree on margin
//! ratios and limits byte-for-byte, because they change future accept/reject
//! decisions (deterministic replay requires identical configuration).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Per-market margin parameters (dYdX `x/perpetuals` risk params analogue).
///
/// Ratios are fractions of notional: `0.10` = 10 %. `liquidation_fee_bps` is
/// charged on top of the bankruptcy price when computing the liquidation
/// limit (funds the insurance fund via a slightly worse execution floor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketParams {
    pub initial_margin_ratio: Decimal,
    pub maintenance_margin_ratio: Decimal,
    pub liquidation_fee_bps: Decimal,
}

impl Default for MarketParams {
    fn default() -> Self {
        Self {
            initial_margin_ratio: Decimal::new(1, 1),     // 10 %
            maintenance_margin_ratio: Decimal::new(5, 2), // 5 %
            liquidation_fee_bps: Decimal::ZERO,
        }
    }
}

/// Operator limits absorbed from `lq-risk` (gap rows 16/23): enforced as
/// pre-trade checks inside `apply`, alongside the margin requirement.
///
/// A limit of `0` disables that check (margin remains mandatory regardless).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpsConfig {
    pub default_market_params: MarketParams,
    /// Max absolute order quantity per order. `0` = disabled.
    pub max_order_qty: Decimal,
    /// Max absolute projected position per subaccount/market. `0` = disabled.
    pub max_position_qty: Decimal,
    /// Max order notional (limit price, or reference price for market orders).
    /// `0` = disabled.
    pub max_notional_per_order: Decimal,
    /// Max resting + open orders per subaccount. `0` = disabled.
    pub max_open_orders: u32,
    /// Rolling logical-time window for the liquidation cascade breaker.
    pub liquidation_window_ms: u64,
    /// Max liquidation notional per market within the window. `0` = disabled.
    pub max_liquidation_notional_per_window: Decimal,
}

impl Default for PerpsConfig {
    fn default() -> Self {
        Self {
            default_market_params: MarketParams::default(),
            max_order_qty: Decimal::ZERO,
            max_position_qty: Decimal::ZERO,
            max_notional_per_order: Decimal::ZERO,
            max_open_orders: 0,
            liquidation_window_ms: 60_000,
            max_liquidation_notional_per_window: Decimal::ZERO,
        }
    }
}

/// Monotonic observability counters (part of the state hash).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpsStats {
    pub transfers: u64,
    pub liquidations: u64,
    pub adl_events: u64,
    pub funding_settlements: u64,
    /// Place/replace entries rejected by the pre-trade margin/risk check.
    pub margin_rejected: u64,
    /// Subaccounts newly flagged below maintenance margin.
    pub flagged: u64,
    /// Entries refused by the Stage 4 oracle circuit breakers (halted or
    /// stale market, rejected price publication).
    #[serde(default)]
    pub oracle_rejected: u64,
}
