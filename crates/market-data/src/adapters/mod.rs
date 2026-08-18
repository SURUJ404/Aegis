//! Venue adapters.
//!
//! Each adapter implements [`FeedDecoder`] and converts one venue's wire
//! protocol into normalized [`MarketEvent`]s, maintaining a local sequence
//! counter so downstream [`BookStore`] gap detection works.

use lq_types::Price;
use rust_decimal::Decimal;
use serde_json::Value;

pub mod binance;
pub mod bybit;
pub mod okx;

/// Parse a decimal that may arrive as a JSON string (venues differ; we accept
/// both number and string forms).
pub(crate) fn dec_from_value(v: &Value) -> Option<Price> {
    match v {
        Value::String(s) => {
            if s.is_empty() {
                return Some(Decimal::ZERO);
            }
            let mut s = s.as_str();
            // Binance uses exponent notation; rust_decimal parses it directly,
            // but strip leading zeros / whitespace first.
            s = s.trim();
            if s.is_empty() {
                return Some(Decimal::ZERO);
            }
            // Some venues send "nan"/"inf" on malformed rows — map to 0.
            if s.eq_ignore_ascii_case("nan") || s.eq_ignore_ascii_case("inf") {
                return Some(Decimal::ZERO);
            }
            Decimal::from_str_exact(s).ok().or_else(|| Decimal::from_scientific(s).ok())
        }
        Value::Number(n) => Decimal::from_str_exact(&n.to_string()).ok(),
        _ => None,
    }
}

/// Parse a qty/price pair array like `["65432.1","0.5"]` or `[65432.1,0.5]`.
pub(crate) fn pair_at(v: &Value, i: usize) -> Option<(Price, Price)> {
    let arr = v.as_array()?;
    let px = arr.get(i)?.clone();
    let qty = arr.get(i + 1)?.clone();
    Some((dec_from_value(&px)?, dec_from_value(&qty)?))
}
