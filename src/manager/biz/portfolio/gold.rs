//! Gold unit conversion. Vietnam market conventions:
//!   1 chi  = 3.75 grams
//!   1 luong = 37.5 grams
//! Quantities in the database are normalized to grams; the original
//! display unit and value are preserved alongside.

use rust_decimal::Decimal;
use std::str::FromStr;

use crate::pb::service::portfolio as pb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoldUnit {
    Chi,
    Luong,
    Gram,
}

impl GoldUnit {
    pub fn from_db(value: &str) -> Self {
        match value {
            "chi" => GoldUnit::Chi,
            "luong" => GoldUnit::Luong,
            _ => GoldUnit::Gram,
        }
    }

    pub fn to_db(self) -> &'static str {
        match self {
            GoldUnit::Chi => "chi",
            GoldUnit::Luong => "luong",
            GoldUnit::Gram => "gram",
        }
    }

    pub fn from_proto(value: i32) -> Option<Self> {
        match value {
            x if x == pb::GoldUnit::Chi as i32 => Some(GoldUnit::Chi),
            x if x == pb::GoldUnit::Luong as i32 => Some(GoldUnit::Luong),
            x if x == pb::GoldUnit::Gram as i32 => Some(GoldUnit::Gram),
            _ => None,
        }
    }
}

/// Convert a quantity in its original display unit to grams.
pub fn grams_from_quantity(quantity: Decimal, unit: GoldUnit) -> Decimal {
    match unit {
        GoldUnit::Chi => quantity * Decimal::from_str("3.75").unwrap(),
        GoldUnit::Luong => quantity * Decimal::from_str("37.5").unwrap(),
        GoldUnit::Gram => quantity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chi_to_grams() {
        let q = Decimal::from_str("1").unwrap();
        assert_eq!(
            grams_from_quantity(q, GoldUnit::Chi),
            Decimal::from_str("3.75").unwrap()
        );
    }

    #[test]
    fn luong_to_grams() {
        let q = Decimal::from_str("1").unwrap();
        assert_eq!(
            grams_from_quantity(q, GoldUnit::Luong),
            Decimal::from_str("37.5").unwrap()
        );
    }

    #[test]
    fn gram_passthrough() {
        let q = Decimal::from_str("12.345").unwrap();
        assert_eq!(grams_from_quantity(q, GoldUnit::Gram), q);
    }

    #[test]
    fn multiple_chi_to_grams() {
        let q = Decimal::from_str("10").unwrap();
        let grams = grams_from_quantity(q, GoldUnit::Chi);
        assert_eq!(grams, Decimal::from_str("37.5").unwrap());
    }

    #[test]
    fn db_round_trip() {
        for u in [GoldUnit::Chi, GoldUnit::Luong, GoldUnit::Gram] {
            assert_eq!(GoldUnit::from_db(u.to_db()), u);
        }
    }

    #[test]
    fn db_unknown_defaults_gram() {
        assert_eq!(GoldUnit::from_db(""), GoldUnit::Gram);
        assert_eq!(GoldUnit::from_db("garbage"), GoldUnit::Gram);
    }
}
