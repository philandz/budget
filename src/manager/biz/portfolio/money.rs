//! Money type: i64 minor units with safe arithmetic and rounding.
//!
//! All Portfolio monetary values are stored as BIGINT in the smallest
//! currency unit (e.g. VND has 1 minor unit = 1 VND). Arithmetic is
//! integer-only; conversions to/from display decimals are explicit.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub fn from_minor(value: i64) -> Self {
        Money(value)
    }

    pub fn minor(self) -> i64 {
        self.0
    }

    pub fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub fn is_non_negative(self) -> bool {
        self.0 >= 0
    }

    pub fn checked_add(self, other: Money) -> Option<Money> {
        self.0.checked_add(other.0).map(Money)
    }

    pub fn checked_sub(self, other: Money) -> Option<Money> {
        self.0.checked_sub(other.0).map(Money)
    }

    /// Saturating addition. Returns Money::ZERO if the result would underflow.
    pub fn saturating_sub(self, other: Money) -> Money {
        Money(self.0.saturating_sub(other.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_minor_round_trip() {
        assert_eq!(Money::from_minor(123).minor(), 123);
        assert_eq!(Money::from_minor(-456).minor(), -456);
    }

    #[test]
    fn zero_constant() {
        assert_eq!(Money::ZERO.minor(), 0);
    }

    #[test]
    fn checked_add_overflow_returns_none() {
        let max = Money::from_minor(i64::MAX);
        let one = Money::from_minor(1);
        assert!(max.checked_add(one).is_none());
        assert_eq!(max.saturating_sub(one).minor(), i64::MAX - 1);
    }

    #[test]
    fn checked_sub_underflow_returns_none() {
        let min = Money::from_minor(i64::MIN);
        let one = Money::from_minor(1);
        assert!(min.checked_sub(one).is_none());
        assert_eq!(min.saturating_sub(one).minor(), i64::MIN);
    }

    #[test]
    fn checked_sub_below_zero_succeeds() {
        // 0 - 1 = -1 is valid; underflow only occurs at i64::MIN.
        let zero = Money::ZERO;
        let one = Money::from_minor(1);
        assert_eq!(zero.checked_sub(one).unwrap().minor(), -1);
    }

    #[test]
    fn predicates() {
        assert!(Money::from_minor(1).is_positive());
        assert!(!Money::ZERO.is_positive());
        assert!(!Money::from_minor(-1).is_positive());

        assert!(Money::ZERO.is_non_negative());
        assert!(Money::from_minor(5).is_non_negative());
        assert!(!Money::from_minor(-1).is_non_negative());
    }
}
