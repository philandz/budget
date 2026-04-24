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
