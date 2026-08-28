//! Rate type: decimal annual rate stored as rust_decimal::Decimal.
//!
//! Annual rate is stored as `Decimal` to avoid floating-point precision
//! loss on long-tenor interest calculations. Conversion to `f64` only
//! happens at the proto boundary.

use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate(Decimal);

impl Rate {
    pub const ZERO: Rate = Rate(Decimal::ZERO);

    /// Build a rate from a fractional value (e.g. 0.055 for 5.5% p.a.).
    pub fn from_fraction(value: Decimal) -> Self {
        Rate(value)
    }

    /// Build a rate from a percentage value (e.g. 5.5 for 5.5% p.a.).
    pub fn from_percent(percent: Decimal) -> Self {
        Rate(percent / Decimal::from(100))
    }

    /// Build a rate from a string representation. Used at the DB/proto boundary.
    pub fn parse(value: &str) -> Result<Self, RateParseError> {
        Ok(Rate(
            Decimal::from_str(value).map_err(|_| RateParseError(value.to_string()))?,
        ))
    }

    pub fn fraction(self) -> Decimal {
        self.0
    }

    pub fn percent(self) -> Decimal {
        self.0 * Decimal::from(100)
    }

    pub fn as_f64(self) -> f64 {
        // Conversion at proto boundary. Lossy by design. Returns 0.0 when
        // the value exceeds f64::MAX instead of NaN so downstream JSON
        // serialization does not silently drop the field.
        match f64::try_from(self.0) {
            Ok(v) if v.is_finite() => v,
            _ => 0.0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid rate string: {0}")]
pub struct RateParseError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_fraction_round_trip() {
        let r = Rate::from_fraction(Decimal::from_str("0.055").unwrap());
        assert_eq!(r.fraction(), Decimal::from_str("0.055").unwrap());
    }

    #[test]
    fn from_percent_converts_correctly() {
        let r = Rate::from_percent(Decimal::from_str("5.5").unwrap());
        assert_eq!(r.fraction(), Decimal::from_str("0.055").unwrap());
        assert_eq!(r.percent(), Decimal::from_str("5.5").unwrap());
    }

    #[test]
    fn parse_round_trip() {
        let r = Rate::parse("0.055").unwrap();
        assert_eq!(r.fraction(), Decimal::from_str("0.055").unwrap());
    }

    #[test]
    fn parse_rejects_invalid() {
        assert!(Rate::parse("not a number").is_err());
    }

    #[test]
    fn as_f64_matches_fraction() {
        let r = Rate::from_fraction(Decimal::from_str("0.055").unwrap());
        assert!((r.as_f64() - 0.055).abs() < 1e-9);
    }
}
