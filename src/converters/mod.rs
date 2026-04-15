use crate::pb::service::budget::{Budget, BudgetMember, BudgetRole, BudgetType, RolloverPolicy};
use crate::pb::common::base::Base;

// ---------------------------------------------------------------------------
// DB row structs
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub struct DbBudget {
    pub id:          String,
    pub org_id:      String,
    pub name:        String,
    pub budget_type: String,
    pub currency:    String,
    pub status:      String,
    pub created_by:  String,
    pub created_at:  i64,
    pub updated_at:  i64,
    pub deleted_at:  Option<i64>,
    // populated by JOIN with budget_members when fetching for a specific user
    pub my_role:     Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbBudgetMember {
    pub budget_id:    String,
    pub user_id:      String,
    pub role:         String,
    pub display_name: Option<String>,
    pub email:        Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbTemplate {
    pub id:          String,
    pub name:        String,
    pub description: Option<String>,
    pub budget_type: String,
}

// ---------------------------------------------------------------------------
// String ↔ Enum helpers (single source of truth)
// ---------------------------------------------------------------------------

pub fn budget_type_to_db(t: BudgetType) -> &'static str {
    match t {
        BudgetType::Standard    => "standard",
        BudgetType::Saving      => "saving",
        BudgetType::Debt        => "debt",
        BudgetType::Invest      => "invest",
        BudgetType::Sharing     => "sharing",
        BudgetType::Unspecified => "standard",
    }
}

pub fn budget_type_from_db(s: &str) -> BudgetType {
    match s {
        "standard" => BudgetType::Standard,
        "saving"   => BudgetType::Saving,
        "debt"     => BudgetType::Debt,
        "invest"   => BudgetType::Invest,
        "sharing"  => BudgetType::Sharing,
        _          => BudgetType::Unspecified,
    }
}

pub fn budget_role_to_db(r: BudgetRole) -> &'static str {
    match r {
        BudgetRole::Owner       => "owner",
        BudgetRole::Manager     => "manager",
        BudgetRole::Contributor => "contributor",
        BudgetRole::Viewer      => "viewer",
        BudgetRole::Unspecified => "viewer",
    }
}

pub fn budget_role_from_db(s: &str) -> BudgetRole {
    match s {
        "owner"       => BudgetRole::Owner,
        "manager"     => BudgetRole::Manager,
        "contributor" => BudgetRole::Contributor,
        "viewer"      => BudgetRole::Viewer,
        _             => BudgetRole::Unspecified,
    }
}

pub fn rollover_policy_from_db(s: &str) -> RolloverPolicy {
    match s {
        "carry_forward" => RolloverPolicy::CarryForward,
        _               => RolloverPolicy::Reset,
    }
}

pub fn rollover_policy_to_db(p: RolloverPolicy) -> &'static str {
    match p {
        RolloverPolicy::CarryForward  => "carry_forward",
        RolloverPolicy::Reset         => "reset",
        RolloverPolicy::Unspecified   => "reset",
    }
}

// ---------------------------------------------------------------------------
// DB row → Proto message converters
// ---------------------------------------------------------------------------

pub fn map_budget(db: DbBudget) -> Budget {
    Budget {
        base: Some(Base {
            id:         db.id,
            created_at: db.created_at,
            updated_at: db.updated_at,
            deleted_at: db.deleted_at.unwrap_or(0),
            created_by: db.created_by,
            ..Default::default()
        }),
        org_id:      db.org_id,
        name:        db.name,
        budget_type: budget_type_from_db(&db.budget_type) as i32,
        currency:    db.currency,
        my_role:     budget_role_from_db(
            db.my_role.as_deref().unwrap_or("viewer"),
        ) as i32,
    }
}

pub fn map_budget_member(db: DbBudgetMember) -> BudgetMember {
    BudgetMember {
        budget_id:    db.budget_id,
        user_id:      db.user_id,
        display_name: db.display_name.unwrap_or_default(),
        email:        db.email.unwrap_or_default(),
        role:         budget_role_from_db(&db.role) as i32,
    }
}
