//! Supported venues.
//!
//! `Exchange` identifies a real or simulated venue. `VenueInstrument` pairs a venue
//! with that venue's own symbol string; `Instrument` is the venue-neutral instrument
//! used for cross-venue analysis.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::instrument::Symbol;

/// A trading venue. The live variants exist for structural completeness and are
/// **disabled by default**: the engine refuses to route real orders unless an
/// explicit opt-in flag is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exchange {
    /// Simulated / paper venue backed by a local matching engine.
    Paper,
    /// Synthetic market data feed used for simulation, backtesting and demos.
    Simulated,
    /// OKX live venue. Live order routing requires explicit opt-in.
    Okx,
    /// Binance live venue. Live order routing requires explicit opt-in.
    Binance,
    /// Bybit live venue. Live order routing requires explicit opt-in.
    Bybit,
}

impl Exchange {
    /// Whether this venue is live (routes real money) or simulated.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Okx | Self::Binance | Self::Bybit)
    }

    /// The canonical short name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Simulated => "simulated",
            Self::Okx => "okx",
            Self::Binance => "binance",
            Self::Bybit => "bybit",
        }
    }
}

impl fmt::Display for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Exchange {
    type Err = UnknownExchange;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "paper" => Ok(Self::Paper),
            "simulated" | "sim" => Ok(Self::Simulated),
            "okx" => Ok(Self::Okx),
            "binance" => Ok(Self::Binance),
            "bybit" => Ok(Self::Bybit),
            other => Err(UnknownExchange(other.to_string())),
        }
    }
}

/// A symbol on a specific venue, e.g. OKX's `BTC-USDT`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VenueInstrument {
    pub venue: Exchange,
    pub symbol: String,
}

impl VenueInstrument {
    pub fn new(venue: Exchange, symbol: impl Into<String>) -> Self {
        Self {
            venue,
            symbol: symbol.into(),
        }
    }

    /// Map to the venue-neutral [`Symbol`] used for cross-venue grouping.
    pub fn normalized_symbol(&self) -> Symbol {
        // Simple normalization: keep as-is for now; venues with divergent
        // symbol formats plug in here.
        Symbol::from_str(&self.symbol)
            .unwrap_or_else(|_| Symbol(self.symbol.to_ascii_uppercase()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown exchange: {0}")]
pub struct UnknownExchange(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exchange_names() {
        assert_eq!("okx".parse::<Exchange>().unwrap(), Exchange::Okx);
        assert_eq!("BINANCE".parse::<Exchange>().unwrap(), Exchange::Binance);
        assert!("nyse".parse::<Exchange>().is_err());
    }

    #[test]
    fn live_flag() {
        assert!(!Exchange::Paper.is_live());
        assert!(Exchange::Okx.is_live());
    }
}