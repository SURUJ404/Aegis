//! Instrument identification.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A venue-neutral symbol, e.g. `BTC-USDT`. Used to group the same asset across
/// venues so that cross-venue analysis can line up quotes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Symbol(pub String);

impl Symbol {
    pub fn base_quote(&self) -> Option<(&str, &str)> {
        let (base, quote) = self.0.split_once('-')?;
        Some((base, quote))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid symbol `{0}`: expected `BASE-QUOTE`")]
pub struct SymbolParseError(pub String);

impl FromStr for Symbol {
    type Err = SymbolParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.to_ascii_uppercase();
        if s.split_once('-').is_none() {
            return Err(SymbolParseError(s));
        }
        Ok(Self(s))
    }
}

/// The same instrument across all venues.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Instrument {
    pub symbol: Symbol,
}

impl Instrument {
    pub fn new(symbol: impl AsRef<str>) -> Result<Self, SymbolParseError> {
        Ok(Self {
            symbol: Symbol::from_str(symbol.as_ref())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_symbol() {
        let s: Symbol = "btc-usdt".parse().unwrap();
        assert_eq!(s.0, "BTC-USDT");
        assert_eq!(s.base_quote(), Some(("BTC", "USDT")));
    }

    #[test]
    fn rejects_bad_symbol() {
        assert!("BTC".parse::<Symbol>().is_err());
    }
}