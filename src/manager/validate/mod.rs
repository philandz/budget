#![allow(clippy::result_large_err)]
use crate::pb::service::budget::{BudgetRole, BudgetType, RolloverPolicy};
use tonic::Status;

pub fn budget_name(name: &str) -> Result<(), Status> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Status::invalid_argument("Budget name must not be empty"));
    }
    if name.len() > 255 {
        return Err(Status::invalid_argument(
            "Budget name must be 255 characters or fewer",
        ));
    }
    Ok(())
}

pub fn currency(currency: &str) -> Result<(), Status> {
    let c = currency.trim();
    if c.is_empty() {
        return Err(Status::invalid_argument("Currency must not be empty"));
    }
    if c.len() > 8 {
        return Err(Status::invalid_argument(
            "Currency code must be 8 characters or fewer",
        ));
    }
    Ok(())
}

pub fn budget_type(t: i32) -> Result<BudgetType, Status> {
    BudgetType::try_from(t).map_err(|_| Status::invalid_argument("Unknown budget type"))
}

/// budget_type is immutable after creation: switching a budget between kinds
/// (e.g. standard -> sharing) would orphan its data. `Unspecified` means "not
/// provided" and keeps the current type; any other differing value is refused.
pub fn resolve_budget_type(
    current: BudgetType,
    requested: BudgetType,
) -> Result<BudgetType, Status> {
    if requested == BudgetType::Unspecified || requested == current {
        return Ok(current);
    }
    Err(Status::failed_precondition(
        "budget_type cannot be changed after the budget is created",
    ))
}

pub fn budget_role(r: i32) -> Result<BudgetRole, Status> {
    BudgetRole::try_from(r).map_err(|_| Status::invalid_argument("Unknown budget role"))
}

pub fn rollover_policy(p: i32) -> Result<RolloverPolicy, Status> {
    RolloverPolicy::try_from(p).map_err(|_| Status::invalid_argument("Unknown rollover policy"))
}

pub fn envelope_limit(limit: i64) -> Result<(), Status> {
    if limit < 0 {
        return Err(Status::invalid_argument(
            "Envelope limit must not be negative",
        ));
    }
    Ok(())
}

/// Extract user_id from gRPC metadata ("x-user-id" header injected by the gateway).
pub fn user_id_from_metadata(meta: &tonic::metadata::MetadataMap) -> Result<String, Status> {
    meta.get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Status::unauthenticated("missing x-user-id metadata"))
}

/// Extract user_type from gRPC metadata ("x-user-type" header injected by the gateway).
pub fn user_type_from_metadata(meta: &tonic::metadata::MetadataMap) -> Option<String> {
    meta.get("x-user-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub const DEFAULT_TIMEZONE: &str = "Asia/Ho_Chi_Minh";

/// Validates optional period settings: start day 1..=28 and an IANA timezone name.
pub fn period_settings(
    start_day: Option<i32>,
    timezone: Option<&str>,
) -> Result<(Option<u8>, Option<String>), Status> {
    let day = match start_day {
        Some(d) if (1..=28).contains(&d) => Some(d as u8),
        Some(_) => {
            return Err(Status::invalid_argument(
                "period_start_day must be between 1 and 28",
            ))
        }
        None => None,
    };
    let tz = match timezone.map(str::trim) {
        Some("") | None => None,
        Some(name) => {
            name.parse::<chrono_tz::Tz>()
                .map_err(|_| Status::invalid_argument("timezone must be an IANA name"))?;
            Some(name.to_string())
        }
    };
    Ok((day, tz))
}

/// Sharing budgets are VND-only until FX exists. Empty means "default to VND".
pub fn sharing_currency(budget_type: BudgetType, currency: &str) -> Result<String, Status> {
    if budget_type != BudgetType::Sharing {
        return Ok(currency.to_string());
    }
    let c = currency.trim();
    if c.is_empty() || c.eq_ignore_ascii_case("VND") {
        Ok("VND".to_string())
    } else {
        Err(Status::invalid_argument(
            "Sharing budgets only support VND currency",
        ))
    }
}

/// Status values a caller may set via SetBudgetStatus.
pub fn settable_status(status: &str) -> Result<&str, Status> {
    match status {
        "active" | "archived" => Ok(status),
        _ => Err(Status::invalid_argument(
            "status must be 'active' or 'archived'",
        )),
    }
}
