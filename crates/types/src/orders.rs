//! Order semantics: type, lifecycle status and time-in-force.

use serde::{Deserialize, Serialize};

/// Order placement type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
    /// Limit order that must be added to the book (never taker).
    PostOnly,
    /// IOC: immediate or cancel.
    ImmediateOrCancel,
    /// FOK: fill or kill.
    FillOrKill,
}

impl OrderType {
    pub fn is_limit(self) -> bool {
        matches!(
            self,
            Self::Limit | Self::PostOnly | Self::ImmediateOrCancel | Self::FillOrKill
        )
    }
}

/// Deterministic order lifecycle. Transitions are checked by the order state
/// machine in `lq-execution`; this enum is the canonical truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Constructed in memory, not yet handed to a venue.
    Created,
    /// Sent over the wire, awaiting acknowledgement.
    Submitted,
    /// Acknowledged by the venue; resting or being worked.
    Acknowledged,
    /// Partially filled; remaining quantity resting.
    PartiallyFilled,
    /// Fully filled.
    Filled,
    /// Cancel requested, awaiting venue acknowledgement.
    CancelRequested,
    /// Cancelled by the venue.
    Cancelled,
    /// Rejected by the venue (or by our risk engine before submission).
    Rejected,
    /// Expired by time-in-force or venue policy.
    Expired,
}

impl OrderStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Cancelled | Self::Rejected | Self::Expired
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Submitted => "submitted",
            Self::Acknowledged => "acknowledged",
            Self::PartiallyFilled => "partially_filled",
            Self::Filled => "filled",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

/// Time-in-force semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    PostOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses() {
        for s in [
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Rejected,
            OrderStatus::Expired,
        ] {
            assert!(s.is_terminal());
        }
        assert!(!OrderStatus::Acknowledged.is_terminal());
    }
}