//! Risk decision types.

use lq_types::Qty;

/// Machine-readable reason for a risk decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskCode {
    /// Kill switch engaged; no orders may be submitted.
    KillSwitchEngaged,
    /// Order would exceed the maximum position.
    MaxPosition,
    /// Order size exceeds the maximum per-order size.
    MaxOrderSize,
    /// Order notional exceeds the maximum per-order notional.
    MaxNotional,
    /// Too many open orders on the venue.
    MaxOpenOrders,
    /// Order submission rate exceeded.
    MaxOrderRate,
    /// Session loss exceeded the daily loss limit.
    MaxDailyLoss,
    /// Venue exposure limit exceeded.
    MaxExposurePerVenue,
    /// Order price deviates too far from the market.
    MaxPriceDeviation,
    /// Market data is stale; quoting halted.
    StaleMarket,
    /// A venue is reconnecting; orders on it are suspended until resync.
    VenueReconnecting,
    /// Order rejected because its inputs are invalid.
    InvalidOrder,
    /// Duplicate client order id.
    DuplicateOrder,
    /// Symbol not known to the engine.
    UnknownSymbol,
}

impl RiskCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KillSwitchEngaged => "kill_switch_engaged",
            Self::MaxPosition => "max_position",
            Self::MaxOrderSize => "max_order_size",
            Self::MaxNotional => "max_notional",
            Self::MaxOpenOrders => "max_open_orders",
            Self::MaxOrderRate => "max_order_rate",
            Self::MaxDailyLoss => "max_daily_loss",
            Self::MaxExposurePerVenue => "max_exposure_per_venue",
            Self::MaxPriceDeviation => "max_price_deviation",
            Self::StaleMarket => "stale_market",
            Self::VenueReconnecting => "venue_reconnecting",
            Self::InvalidOrder => "invalid_order",
            Self::DuplicateOrder => "duplicate_order",
            Self::UnknownSymbol => "unknown_symbol",
        }
    }
}

/// A human- and machine-readable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskReason {
    pub code: RiskCode,
    pub detail: String,
}

impl RiskReason {
    pub fn new(code: RiskCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// Verdict of the risk engine on a proposed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    /// Order may proceed.
    Allow,
    /// Order must not be submitted.
    Reject(RiskReason),
    /// Order may proceed with a reduced quantity.
    Reduce { qty: Qty, reason: RiskReason },
    /// Trading must stop entirely.
    Halt(RiskReason),
}

impl RiskDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn is_halt(&self) -> bool {
        matches!(self, Self::Halt(_))
    }

    pub fn reason(&self) -> Option<&RiskReason> {
        match self {
            Self::Allow => None,
            Self::Reject(r) | Self::Reduce { reason: r, .. } | Self::Halt(r) => Some(r),
        }
    }
}