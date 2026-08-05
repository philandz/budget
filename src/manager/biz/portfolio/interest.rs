//! Interest math for savings accounts and fixed deposits.
//!
//! Formulas use `actual/365` day-count basis. Inputs are integer `i64`
//! minor units for principal and `i64` days elapsed. Output is `i64`
//! minor units (rounded half-to-even at the end).

use rust_decimal::Decimal;

use super::Money;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterestMethod {
    Simple,
    Compound,
}

impl InterestMethod {
    pub fn from_db(value: &str) -> Self {
        match value {
            "compound" => InterestMethod::Compound,
            _ => InterestMethod::Simple,
        }
    }

    pub fn to_db(self) -> &'static str {
        match self {
            InterestMethod::Simple => "simple",
            InterestMethod::Compound => "compound",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutType {
    AtMaturity,
    Monthly,
    Quarterly,
    OnDemand,
}

impl PayoutType {
    pub fn from_db(value: &str) -> Self {
        match value {
            "monthly" => PayoutType::Monthly,
            "quarterly" => PayoutType::Quarterly,
            "on_demand" => PayoutType::OnDemand,
            _ => PayoutType::AtMaturity,
        }
    }

    pub fn to_db(self) -> &'static str {
        match self {
            PayoutType::AtMaturity => "at_maturity",
            PayoutType::Monthly => "monthly",
            PayoutType::Quarterly => "quarterly",
            PayoutType::OnDemand => "on_demand",
        }
    }
}

const DAYS_PER_YEAR: i64 = 365;

/// Simple-interest total value (principal + accrued interest).
///
/// formula: principal + principal * rate * days / 365
///
/// Returns the total balance after `days` of simple interest, not the
/// interest alone. Use [`simple_interest_only`] when only the interest
/// component is required.
pub fn simple_accrued(principal_minor: i64, annual_rate: Decimal, days: i64) -> Money {
    if days <= 0 || principal_minor <= 0 {
        return Money::from_minor(principal_minor);
    }
    let principal = Decimal::from(principal_minor);
    let interest = principal * annual_rate * Decimal::from(days) / Decimal::from(DAYS_PER_YEAR);
    let total = principal + interest;
    Money::from_minor(round_half_to_even(total))
}

/// Interest amount earned via simple interest (no principal returned).
///
/// formula: principal * rate * days / 365
pub fn simple_interest_only(principal_minor: i64, annual_rate: Decimal, days: i64) -> Money {
    if days <= 0 || principal_minor <= 0 {
        return Money::ZERO;
    }
    let principal = Decimal::from(principal_minor);
    let interest = principal * annual_rate * Decimal::from(days) / Decimal::from(DAYS_PER_YEAR);
    Money::from_minor(round_half_to_even(interest))
}

/// Compound-interest accrued value, compounded daily.
///
/// formula: principal * (1 + rate/365) ^ days
pub fn compound_accrued(principal_minor: i64, annual_rate: Decimal, days: i64) -> Money {
    if days <= 0 || principal_minor <= 0 {
        return Money::from_minor(principal_minor);
    }
    let principal = Decimal::from(principal_minor);
    let daily_rate = annual_rate / Decimal::from(DAYS_PER_YEAR);
    let base = Decimal::from(1) + daily_rate;
    // Use repeated multiplication; no native Decimal::powi.
    let total = principal * decimal_pow_int(base, days);
    Money::from_minor(round_half_to_even(total))
}

/// Compute base^exp for small positive integer exponents using Decimal.
///
/// Uses binary exponentiation (O(log n) multiplications) so long-tenor
/// deposits do not pay linear-time cost. Negative exponents return 1
/// (consistent with the previous simple loop).
fn decimal_pow_int(base: Decimal, exp: i64) -> Decimal {
    if exp <= 0 {
        return Decimal::from(1);
    }
    let mut result = Decimal::from(1);
    let mut b = base;
    let mut e = exp as u64;
    while e > 0 {
        if e & 1 == 1 {
            result *= b;
        }
        e >>= 1;
        if e > 0 {
            b *= b;
        }
    }
    result
}

/// Round a Decimal to nearest integer using banker's rounding
/// (half-to-even). Matches existing budget service convention.
fn round_half_to_even(value: Decimal) -> i64 {
    let rounded = value.round_dp(0);
    // Decimal::round_dp uses banker's rounding by default.
    use std::convert::TryFrom;
    i64::try_from(rounded).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn rate(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn simple_zero_days_returns_principal() {
        let v = simple_accrued(1_000_000, rate("0.05"), 0);
        assert_eq!(v.minor(), 1_000_000);
    }

    #[test]
    fn simple_negative_days_returns_principal() {
        let v = simple_accrued(1_000_000, rate("0.05"), -30);
        assert_eq!(v.minor(), 1_000_000);
    }

    #[test]
    fn simple_one_year_at_five_percent() {
        // 1,000,000 @ 5% for 365 days = 50,000 interest; total 1,050,000.
        let v = simple_accrued(1_000_000, rate("0.05"), 365);
        assert_eq!(v.minor(), 1_050_000);
    }

    #[test]
    fn simple_half_year_at_five_percent() {
        // 1,000,000 @ 5% for 182 days: interest ≈ 24,931.5068...
        // Allow ±1 minor unit for banker's rounding at half boundaries.
        let v = simple_accrued(1_000_000, rate("0.05"), 182);
        assert!(v.minor() == 1_024_931 || v.minor() == 1_024_932);
    }

    #[test]
    fn compound_zero_days_returns_principal() {
        let v = compound_accrued(1_000_000, rate("0.05"), 0);
        assert_eq!(v.minor(), 1_000_000);
    }

    #[test]
    fn compound_one_year_at_five_percent_exceeds_simple() {
        // Compound daily for 365 days should be strictly greater than simple
        // because daily compounding produces fractional extra yield.
        let simple = simple_accrued(1_000_000, rate("0.05"), 365);
        let comp = compound_accrued(1_000_000, rate("0.05"), 365);
        assert!(comp.minor() > simple.minor());
        // Compound for 1 year at 5% daily ≈ 51,267. Compare to simple 50,000.
        // Difference is at most ~3% of interest, here bounded well under 5,000.
        assert!(comp.minor() <= simple.minor() + 5_000);
    }

    #[test]
    fn compound_ten_years_grows() {
        let v = compound_accrued(1_000_000, rate("0.07"), 3650);
        // 1.000.000 * (1 + 0.07/365)^3650 ≈ 2,013,572
        assert!(v.minor() > 2_000_000 && v.minor() < 2_100_000);
    }

    #[test]
    fn method_round_trip() {
        for m in [InterestMethod::Simple, InterestMethod::Compound] {
            assert_eq!(InterestMethod::from_db(m.to_db()), m);
        }
    }

    #[test]
    fn payout_round_trip() {
        for p in [
            PayoutType::AtMaturity,
            PayoutType::Monthly,
            PayoutType::Quarterly,
            PayoutType::OnDemand,
        ] {
            assert_eq!(PayoutType::from_db(p.to_db()), p);
        }
    }

    #[test]
    fn payout_unknown_defaults_at_maturity() {
        assert_eq!(PayoutType::from_db(""), PayoutType::AtMaturity);
    }

    #[test]
    fn zero_principal_returns_zero() {
        assert_eq!(simple_accrued(0, rate("0.05"), 100).minor(), 0);
        assert_eq!(compound_accrued(0, rate("0.05"), 100).minor(), 0);
    }
}
