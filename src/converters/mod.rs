use crate::pb::common::base::Base;
use crate::pb::service::budget::{Budget, BudgetMember, BudgetRole, BudgetType, RolloverPolicy};

// ---------------------------------------------------------------------------
// DB row structs
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub struct DbBudget {
    pub id: String,
    pub org_id: String,
    pub name: String,
    pub budget_type: String,
    pub currency: String,
    pub status: String,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    // populated by JOIN with budget_members when fetching for a specific user
    pub my_role: Option<String>,
    // envelope limit (0 if not set)
    pub envelope_limit: Option<i64>,
    // member count for this budget
    pub member_count: Option<i32>,
    // current month spend for this budget
    pub current_spend: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbBudgetMember {
    pub budget_id: String,
    pub user_id: String,
    pub role: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbTemplate {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub budget_type: String,
}

// ---------------------------------------------------------------------------
// String ↔ Enum helpers (single source of truth)
// ---------------------------------------------------------------------------

pub fn budget_type_to_db(t: BudgetType) -> &'static str {
    match t {
        BudgetType::Standard => "standard",
        BudgetType::Saving => "saving",
        BudgetType::Debt => "debt",
        BudgetType::Invest => "invest",
        BudgetType::Sharing => "sharing",
        BudgetType::Unspecified => "standard",
    }
}

pub fn budget_type_from_db(s: &str) -> BudgetType {
    match s {
        "standard" => BudgetType::Standard,
        "saving" => BudgetType::Saving,
        "debt" => BudgetType::Debt,
        "invest" => BudgetType::Invest,
        "sharing" => BudgetType::Sharing,
        _ => BudgetType::Unspecified,
    }
}

pub fn budget_role_to_db(r: BudgetRole) -> &'static str {
    match r {
        BudgetRole::Owner => "owner",
        BudgetRole::Manager => "manager",
        BudgetRole::Contributor => "contributor",
        BudgetRole::Viewer => "viewer",
        BudgetRole::Unspecified => "viewer",
    }
}

pub fn budget_role_from_db(s: &str) -> BudgetRole {
    match s {
        "owner" => BudgetRole::Owner,
        "manager" => BudgetRole::Manager,
        "contributor" => BudgetRole::Contributor,
        "viewer" => BudgetRole::Viewer,
        _ => BudgetRole::Unspecified,
    }
}

pub fn rollover_policy_from_db(s: &str) -> RolloverPolicy {
    match s {
        "carry_forward" => RolloverPolicy::CarryForward,
        _ => RolloverPolicy::Reset,
    }
}

pub fn rollover_policy_to_db(p: RolloverPolicy) -> &'static str {
    match p {
        RolloverPolicy::CarryForward => "carry_forward",
        RolloverPolicy::Reset => "reset",
        RolloverPolicy::Unspecified => "reset",
    }
}

// ---------------------------------------------------------------------------
// DB row → Proto message converters
// ---------------------------------------------------------------------------

pub fn map_budget(db: DbBudget) -> Budget {
    Budget {
        base: Some(Base {
            id: db.id,
            created_at: db.created_at,
            updated_at: db.updated_at,
            deleted_at: db.deleted_at.unwrap_or(0),
            created_by: db.created_by,
            ..Default::default()
        }),
        org_id: db.org_id,
        name: db.name,
        budget_type: budget_type_from_db(&db.budget_type) as i32,
        currency: db.currency,
        my_role: budget_role_from_db(db.my_role.as_deref().unwrap_or("viewer")) as i32,
        envelope_limit: db.envelope_limit.unwrap_or(0),
        current_spend: db.current_spend.unwrap_or(0),
        burn_rate_pct: if db.envelope_limit.unwrap_or(0) > 0 {
            (db.current_spend.unwrap_or(0) as f64 / db.envelope_limit.unwrap_or(0) as f64) * 100.0
        } else {
            0.0
        },
        member_count: db.member_count.unwrap_or(0),
    }
}

pub fn map_budget_member(db: DbBudgetMember) -> BudgetMember {
    // For pending members (invited but not yet registered) the users JOIN
    // returns NULL for display_name / email because no user row exists yet.
    // In that case user_id holds the invited email address, so use it as the
    // fallback for both fields so callers always get something meaningful.
    let email = db
        .email
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| db.user_id.clone());
    let display_name = db
        .display_name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| db.user_id.clone());

    BudgetMember {
        budget_id: db.budget_id,
        user_id: db.user_id,
        display_name,
        email,
        role: budget_role_from_db(&db.role) as i32,
        avatar: db.avatar.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Invest asset DB structs
// ---------------------------------------------------------------------------

use crate::pb::service::budget::{AssetStatus, AssetType, InvestAsset, PriceSnapshot};

#[derive(Debug, sqlx::FromRow)]
pub struct DbInvestAsset {
    pub id: String,
    pub budget_id: String,
    pub asset_type: String,
    pub name: String,
    pub status: String,
    pub principal: Option<i64>,
    pub annual_rate: Option<f64>,
    pub interest_type: Option<String>,
    pub start_date: Option<String>,
    pub maturity_date: Option<String>,
    pub bank_name: Option<String>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub cost_basis_per_unit: Option<i64>,
    pub ticker: Option<String>,
    pub exchange: Option<String>,
    pub avg_cost_per_share: Option<i64>,
    pub purchase_date: Option<String>,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbPriceSnapshot {
    pub id: String,
    pub asset_id: String,
    pub price: i64,
    pub source: String,
    pub snapshot_date: String,
    pub created_at: i64,
}

pub fn asset_type_to_db(t: AssetType) -> &'static str {
    match t {
        AssetType::SavingsDeposit => "savings_deposit",
        AssetType::Gold => "gold",
        AssetType::Stock => "stock",
        AssetType::Unspecified => "savings_deposit",
    }
}

pub fn asset_type_from_db(s: &str) -> AssetType {
    match s {
        "savings_deposit" => AssetType::SavingsDeposit,
        "gold" => AssetType::Gold,
        "stock" => AssetType::Stock,
        _ => AssetType::Unspecified,
    }
}

pub fn asset_status_to_db(s: AssetStatus) -> &'static str {
    match s {
        AssetStatus::Active => "active",
        AssetStatus::Matured => "matured",
        AssetStatus::Sold => "sold",
        AssetStatus::Closed => "closed",
        AssetStatus::Unspecified => "active",
    }
}

pub fn asset_status_from_db(s: &str) -> AssetStatus {
    match s {
        "active" => AssetStatus::Active,
        "matured" => AssetStatus::Matured,
        "sold" => AssetStatus::Sold,
        "closed" => AssetStatus::Closed,
        _ => AssetStatus::Unspecified,
    }
}

/// Map a DbInvestAsset to proto InvestAsset with computed values.
/// `current_value`, `cost_basis`, `unrealized_pnl`, `pnl_pct`, `last_updated`
/// must be computed by the biz layer and passed in.
pub fn map_invest_asset(
    db: DbInvestAsset,
    current_value: i64,
    cost_basis: i64,
    unrealized_pnl: i64,
    pnl_pct: f64,
    last_updated: String,
) -> InvestAsset {
    InvestAsset {
        id: db.id,
        budget_id: db.budget_id,
        asset_type: asset_type_from_db(&db.asset_type) as i32,
        name: db.name,
        status: asset_status_from_db(&db.status) as i32,
        principal: db.principal,
        annual_rate: db.annual_rate,
        interest_type: db.interest_type,
        start_date: db.start_date,
        maturity_date: db.maturity_date,
        bank_name: db.bank_name,
        quantity: db.quantity,
        unit: db.unit,
        cost_basis_per_unit: db.cost_basis_per_unit,
        ticker: db.ticker,
        exchange: db.exchange,
        avg_cost_per_share: db.avg_cost_per_share,
        purchase_date: db.purchase_date,
        notes: db.notes,
        current_value,
        cost_basis,
        unrealized_pnl,
        pnl_pct,
        last_updated,
    }
}

pub fn map_price_snapshot(db: DbPriceSnapshot) -> PriceSnapshot {
    PriceSnapshot {
        id: db.id,
        asset_id: db.asset_id,
        price: db.price,
        source: db.source,
        snapshot_date: db.snapshot_date,
    }
}
