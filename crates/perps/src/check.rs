//! Pre-trade verdicts: the risk limits absorbed from `lq-risk` plus the
//! margin requirement, evaluated **inside** `apply` before the CLOB sees an
//! order (and exported for the Stage 5 gateway's CheckTx-style validation).

use rust_decimal::Decimal;

/// Why a pre-trade check rejected (or would reduce) an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreTradeCode {
    InvalidQuantity,
    InvalidPrice,
    /// No reference price exists for the market yet (no tick, no trade).
    NoMarkPrice,
    /// `PerpsConfig::max_order_qty` exceeded (verdict: `Reduce`).
    MaxOrderQty,
    /// `PerpsConfig::max_position_qty` exceeded.
    MaxPosition,
    /// `PerpsConfig::max_notional_per_order` exceeded.
    MaxNotional,
    /// `PerpsConfig::max_open_orders` exceeded.
    MaxOpenOrders,
    /// Reduce-only order would grow exposure / exceed the position.
    ReduceOnlyExceedsPosition,
    /// Initial margin (incl. open-order reservation) not met after the fill.
    InsufficientMargin,
    /// The fill would land the subaccount below maintenance immediately.
    BelowMaintenance,
    /// Order targets the reserved insurance ledger.
    ReservedSubaccount,
}

impl PreTradeCode {
    /// Stable machine-readable reason string (also used as the
    /// `ApplyOutput::Rejected` reason).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidQuantity => "invalid_quantity",
            Self::InvalidPrice => "invalid_price",
            Self::NoMarkPrice => "no_mark_price",
            Self::MaxOrderQty => "max_order_qty",
            Self::MaxPosition => "max_position",
            Self::MaxNotional => "max_notional",
            Self::MaxOpenOrders => "max_open_orders",
            Self::ReduceOnlyExceedsPosition => "reduce_only_exceeds_position",
            Self::InsufficientMargin => "insufficient_margin",
            Self::BelowMaintenance => "below_maintenance",
            Self::ReservedSubaccount => "reserved_subaccount",
        }
    }
}

/// Outcome of a pre-trade check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreTradeVerdict {
    /// Order may proceed; `margin_required` is the projected initial-margin
    /// requirement (position + open-order reservation) for observability.
    Allow { margin_required: Decimal },
    /// Order is valid but exceeds `max_order_qty`; place only `qty` instead
    /// (mirrors `lq-risk::RiskDecision::Reduce`).
    Reduce { qty: Decimal, code: PreTradeCode },
    /// Order must not reach the book.
    Reject { code: PreTradeCode },
}

impl PreTradeVerdict {
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    pub fn code(&self) -> Option<PreTradeCode> {
        match self {
            Self::Allow { .. } => None,
            Self::Reduce { code, .. } | Self::Reject { code } => Some(*code),
        }
    }

    /// Machine-readable reason for logging/outputs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::Reduce { code, .. } | Self::Reject { code } => code.as_str(),
        }
    }
}
