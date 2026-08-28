//! Quantity type: decimal quantity stored as rust_decimal::Decimal.
//!
//! Used for fractional shares, grams of gold, and any other non-money
//! scalar that needs more than integer precision.

use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quantity(Decimal);

impl Quantity {
    pub const ZERO: Quantity = Quantity(Decimal::ZERO);

    pub fn from_decimal(value: Decimal) -> Self {
        Quantity(value)
    }

    pub fn from_grams(value: Decimal) -> Self {
        Quantity(value)
    }

    pub fn parse(value: &str) -> Result<Self, QuantityParseError> {
        Ok(Quantity(
            Decimal::from_str(value).map_err(|_| QuantityParseError(value.to_string()))?,
        ))
    }

    pub fn decimal(self) -> Decimal {
        self.0
    }

    pub fn is_positive(self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn is_zero(self) -> bool {
        self.0 == Decimal::ZERO
    }

    pub fn checked_sub(self, other: Quantity) -> Option<Quantity> {
        self.0.checked_sub(other.0).map(Quantity)
    }

    pub fn to_display_string(self) -> String {
        self.0.to_string()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid quantity string: {0}")]
pub struct QuantityParseError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_decimal_round_trip() {
        let q = Quantity::from_decimal(Decimal::from_str("1.5").unwrap());
        assert_eq!(q.decimal(), Decimal::from_str("1.5").unwrap());
    }

    #[test]
    fn parse_round_trip() {
        let q = Quantity::parse("37.5").unwrap();
        assert_eq!(q.decimal(), Decimal::from_str("37.5").unwrap());
    }

    #[test]
    fn predicates() {
        assert!(Quantity::from_decimal(Decimal::from_str("0.00000001").unwrap()).is_positive());
        assert!(Quantity::ZERO.is_zero());
        assert!(!Quantity::from_decimal(Decimal::from_str("-1").unwrap()).is_positive());
    }

    #[test]
    fn checked_sub_handles_underflow_at_zero() {
        let a = Quantity::from_decimal(Decimal::ZERO);
        let b = Quantity::from_decimal(Decimal::from_str("2").unwrap());
        // 0 - 2 = -2 is valid; checked_sub only fails on overflow at the
        // underlying Decimal type's lower bound.
        assert_eq!(
            a.checked_sub(b).unwrap().decimal(),
            Decimal::from_str("-2").unwrap()
        );
    }
}
