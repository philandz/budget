//! Fund cash pocket: DB row struct and proto enum conversions (single source of truth).

use sqlx::FromRow;

use crate::pb::service::portfolio as pb;

pub fn kind_to_db(k: pb::FundCashMovementKind) -> &'static str {
    use pb::FundCashMovementKind as K;
    match k {
        K::OpeningBalance => "opening_balance",
        K::TransferIn => "transfer_in",
        K::TransferOut => "transfer_out",
        K::AssetPurchase => "asset_purchase",
        K::AssetRedemption => "asset_redemption",
        K::InterestPayout => "interest_payout",
        K::Fee => "fee",
        K::Adjustment => "adjustment",
        K::Unspecified => "adjustment",
    }
}

pub fn kind_from_db(s: &str) -> pb::FundCashMovementKind {
    use pb::FundCashMovementKind as K;
    match s {
        "opening_balance" => K::OpeningBalance,
        "transfer_in" => K::TransferIn,
        "transfer_out" => K::TransferOut,
        "asset_purchase" => K::AssetPurchase,
        "asset_redemption" => K::AssetRedemption,
        "interest_payout" => K::InterestPayout,
        "fee" => K::Fee,
        "adjustment" => K::Adjustment,
        other => {
            tracing::warn!("unknown fund cash movement kind {other:?}; treating as adjustment");
            K::Adjustment
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct DbFundCashAccount {
    pub budget_id: String,
    pub currency: String,
    pub balance_minor: i64,
    pub version: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct DbFundCashMovement {
    pub id: String,
    pub budget_id: String,
    pub kind: String,
    pub amount_minor: i64,
    pub ref_type: Option<String>,
    pub ref_id: Option<String>,
    pub transfer_group_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub actor_user_id: String,
    pub note: Option<String>,
    pub occurred_on: String,
    pub created_at: i64,
}

pub fn map_movement(m: DbFundCashMovement) -> pb::FundCashMovement {
    pb::FundCashMovement {
        id: m.id,
        budget_id: m.budget_id,
        kind: kind_from_db(&m.kind) as i32,
        amount_minor: m.amount_minor,
        ref_type: m.ref_type.unwrap_or_default(),
        ref_id: m.ref_id.unwrap_or_default(),
        transfer_group_id: m.transfer_group_id.unwrap_or_default(),
        actor_user_id: m.actor_user_id,
        note: m.note.unwrap_or_default(),
        occurred_on: m.occurred_on,
        created_at: m.created_at,
    }
}

/// Insert payload for one movement.
#[derive(Debug, Clone)]
pub struct NewFundMovement {
    pub budget_id: String,
    pub currency: String,
    pub kind: pb::FundCashMovementKind,
    /// Signed: positive into the pocket, negative out of it.
    pub amount_minor: i64,
    pub ref_type: Option<String>,
    pub ref_id: Option<String>,
    pub transfer_group_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub actor_user_id: String,
    pub note: Option<String>,
    pub occurred_on: String,
}
