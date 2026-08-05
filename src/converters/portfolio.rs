//! DB row structs and proto mappers for the Asset Portfolio schema.
//!
//! Mirrors the pattern used by `converters/mod.rs` for the legacy
//! `DbInvestAsset` / `map_invest_asset` pair. Each asset class has a
//! `Db*` row struct and a `map_*` function. Insert-time payloads live
//! in the `New*` structs that the repository takes.

use rust_decimal::Decimal;
use sqlx::FromRow;

use crate::manager::biz::portfolio::gold::GoldUnit;
use crate::manager::biz::portfolio::{AssetClass, AssetStatus, PriceSide};
use crate::pb::service::portfolio as pb;

// ---------------------------------------------------------------------------
// Asset class enum used at insert time. Avoids depending on the biz enum
// to keep the converters module purely structural.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetClassNew {
    SavingsAccount,
    FixedDeposit,
    GoldLot,
    StockLot,
    EtfLot,
    CryptoLot,
}

impl AssetClassNew {
    pub fn from_proto(value: i32) -> Option<Self> {
        match value {
            x if x == pb::PortfolioAssetClass::SavingsAccount as i32 => {
                Some(AssetClassNew::SavingsAccount)
            }
            x if x == pb::PortfolioAssetClass::FixedDeposit as i32 => {
                Some(AssetClassNew::FixedDeposit)
            }
            x if x == pb::PortfolioAssetClass::GoldLot as i32 => Some(AssetClassNew::GoldLot),
            x if x == pb::PortfolioAssetClass::StockLot as i32 => Some(AssetClassNew::StockLot),
            x if x == pb::PortfolioAssetClass::EtfLot as i32 => Some(AssetClassNew::EtfLot),
            x if x == pb::PortfolioAssetClass::CryptoLot as i32 => Some(AssetClassNew::CryptoLot),
            _ => None,
        }
    }

    pub fn to_proto(self) -> i32 {
        match self {
            AssetClassNew::SavingsAccount => pb::PortfolioAssetClass::SavingsAccount as i32,
            AssetClassNew::FixedDeposit => pb::PortfolioAssetClass::FixedDeposit as i32,
            AssetClassNew::GoldLot => pb::PortfolioAssetClass::GoldLot as i32,
            AssetClassNew::StockLot => pb::PortfolioAssetClass::StockLot as i32,
            AssetClassNew::EtfLot => pb::PortfolioAssetClass::EtfLot as i32,
            AssetClassNew::CryptoLot => pb::PortfolioAssetClass::CryptoLot as i32,
        }
    }

    /// True for asset classes that need a market price refresh.
    /// SavingsAccount and FixedDeposit are priced by formula, not
    /// market feed, so the refresh job skips them.
    pub fn is_priceable(&self) -> bool {
        matches!(
            self,
            Self::GoldLot | Self::StockLot | Self::EtfLot | Self::CryptoLot
        )
    }
}

// ---------------------------------------------------------------------------
// Asset root
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbPortfolioAsset {
    pub id: String,
    pub budget_id: String,
    pub asset_class: String,
    pub display_name: String,
    pub currency: String,
    pub status: String,
    pub opened_on: i64,
    pub closed_on: Option<i64>,
    pub legacy_asset_id: Option<String>,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewPortfolioAsset {
    pub id: Option<String>,
    pub budget_id: String,
    pub asset_class: AssetClassNew,
    pub display_name: String,
    pub currency: String,
    pub opened_on: i64,
    pub closed_on: Option<i64>,
    pub legacy_asset_id: Option<String>,
    pub notes: Option<String>,
    pub created_by: String,
}

pub fn map_portfolio_asset(db: DbPortfolioAsset) -> pb::PortfolioAsset {
    let class = match db.asset_class.as_str() {
        "savings_account" => AssetClass::SavingsAccount.to_proto(),
        "fixed_deposit" => AssetClass::FixedDeposit.to_proto(),
        "gold_lot" => AssetClass::GoldLot.to_proto(),
        "stock_lot" => AssetClass::StockLot.to_proto(),
        _ => AssetClass::SavingsAccount.to_proto(),
    };
    pb::PortfolioAsset {
        base: Some(common_base(
            &db.id,
            db.created_at,
            db.updated_at,
            db.deleted_at,
            &db.created_by,
        )),
        budget_id: db.budget_id,
        asset_class: class,
        display_name: db.display_name,
        currency: db.currency,
        status: map_asset_status_to_proto(&db.status),
        opened_on: db.opened_on,
        closed_on: db.closed_on.unwrap_or(0),
        legacy_asset_id: db.legacy_asset_id.unwrap_or_default(),
        notes: db.notes.unwrap_or_default(),
        details: None,
    }
}

// ---------------------------------------------------------------------------
// Savings account
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbSavingsAccount {
    pub asset_id: String,
    pub provider: String,
    pub account_reference_masked: String,
    pub current_balance: i64,
    pub balance_as_of: i64,
    pub annual_rate: String,
    pub interest_method: String,
    pub payout_type: String,
    pub opened_on: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewSavingsAccount {
    pub provider: String,
    pub account_reference_masked: String,
    pub current_balance: i64,
    pub balance_as_of: i64,
    pub annual_rate: String,
    pub interest_method: String,
    pub payout_type: String,
    pub opened_on: i64,
    pub notes: Option<String>,
}

pub fn map_savings_account(db: DbSavingsAccount) -> pb::PortfolioSavingsAccount {
    pb::PortfolioSavingsAccount {
        asset_id: db.asset_id,
        provider: db.provider,
        account_reference_masked: db.account_reference_masked,
        current_balance: db.current_balance,
        balance_as_of: db.balance_as_of,
        annual_rate: parse_rate_to_f64(&db.annual_rate),
        interest_method: map_interest_method_to_proto(&db.interest_method),
        payout_type: map_payout_type_to_proto(&db.payout_type),
        opened_on: db.opened_on,
        notes: db.notes.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Fixed deposit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbFixedDeposit {
    pub asset_id: String,
    pub provider: String,
    pub product_name: String,
    pub principal: i64,
    pub annual_rate: String,
    pub interest_method: String,
    pub payout_type: String,
    pub deposit_date: i64,
    pub maturity_date: i64,
    pub auto_renewal_policy: String,
    pub rollover_from_asset_id: Option<String>,
    pub certificate_reference_masked: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewFixedDeposit {
    pub provider: String,
    pub product_name: String,
    pub principal: i64,
    pub annual_rate: String,
    pub interest_method: String,
    pub payout_type: String,
    pub deposit_date: i64,
    pub maturity_date: i64,
    pub auto_renewal_policy: String,
    pub rollover_from_asset_id: Option<String>,
    pub certificate_reference_masked: Option<String>,
    pub notes: Option<String>,
}

pub fn map_fixed_deposit(db: DbFixedDeposit) -> pb::PortfolioFixedDeposit {
    pb::PortfolioFixedDeposit {
        asset_id: db.asset_id,
        provider: db.provider,
        product_name: db.product_name,
        principal: db.principal,
        annual_rate: parse_rate_to_f64(&db.annual_rate),
        interest_method: map_interest_method_to_proto(&db.interest_method),
        payout_type: map_payout_type_to_proto(&db.payout_type),
        deposit_date: db.deposit_date,
        maturity_date: db.maturity_date,
        auto_renewal_policy: map_auto_renewal_to_proto(&db.auto_renewal_policy),
        rollover_from_asset_id: db.rollover_from_asset_id.unwrap_or_default(),
        certificate_reference_masked: db.certificate_reference_masked.unwrap_or_default(),
        notes: db.notes.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Gold lot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbGoldLot {
    pub asset_id: String,
    pub provider: String,
    pub gold_type: String,
    pub purity: String,
    pub form: String,
    pub quantity_original: String,
    pub unit_original: String,
    pub quantity_grams: String,
    pub purchase_price_per_unit_original: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewGoldLot {
    pub provider: String,
    pub gold_type: String,
    pub purity: String,
    pub form: String,
    pub quantity_original: String,
    pub unit: GoldUnit,
    pub purchase_price_per_unit_original: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub notes: Option<String>,
}

pub fn map_gold_lot(db: DbGoldLot) -> pb::PortfolioGoldLot {
    pb::PortfolioGoldLot {
        asset_id: db.asset_id,
        provider: db.provider,
        gold_type: db.gold_type,
        purity: map_gold_purity_to_proto(&db.purity),
        form: map_gold_form_to_proto(&db.form),
        quantity_original: parse_quantity(&db.quantity_original),
        unit_original: map_gold_unit_to_proto(&db.unit_original),
        quantity_grams: parse_quantity(&db.quantity_grams),
        purchase_price_per_unit_original: db.purchase_price_per_unit_original,
        purchase_cost: db.purchase_cost,
        fees: db.fees,
        purchase_date: db.purchase_date,
        notes: db.notes.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Stock lot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbStockLot {
    pub asset_id: String,
    pub ticker: String,
    pub exchange: String,
    pub quantity_bought: String,
    pub quantity_open: String,
    pub buy_price_per_share: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub settlement_date: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewStockLot {
    pub ticker: String,
    pub exchange: String,
    pub quantity_bought: String,
    pub buy_price_per_share: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub settlement_date: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct DbEtfLot {
    pub asset_id: String,
    pub ticker: String,
    pub exchange: String,
    pub underlying_index: String,
    pub fund_provider: String,
    pub quantity_bought: String,
    pub quantity_open: String,
    pub buy_price_per_unit: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub settlement_date: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewEtfLot {
    pub ticker: String,
    pub exchange: String,
    pub underlying_index: String,
    pub fund_provider: String,
    pub quantity_bought: String,
    pub quantity_open: String,
    pub buy_price_per_unit: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub settlement_date: Option<i64>,
    pub notes: Option<String>,
}

pub fn map_etf_lot(db: DbEtfLot) -> pb::PortfolioEtfLot {
    pb::PortfolioEtfLot {
        asset_id: db.asset_id,
        ticker: db.ticker,
        exchange: map_stock_exchange_to_proto(&db.exchange),
        underlying_index: map_etf_underlying_to_proto(&db.underlying_index),
        fund_provider: db.fund_provider,
        quantity_bought: parse_quantity(&db.quantity_bought),
        quantity_open: parse_quantity(&db.quantity_open),
        buy_price_per_unit: db.buy_price_per_unit,
        purchase_cost: db.purchase_cost,
        fees: db.fees,
        purchase_date: db.purchase_date,
        settlement_date: db.settlement_date.unwrap_or(0),
        notes: db.notes.unwrap_or_default(),
    }
}

fn map_etf_underlying_to_proto(s: &str) -> i32 {
    use pb::EtfUnderlyingIndex as E;
    match s.to_ascii_lowercase().as_str() {
        "vn30" => E::Vn30 as i32,
        "vn100" => E::Vn100 as i32,
        "hnx30" => E::Hnx30 as i32,
        _ => E::Other as i32,
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct DbCryptoLot {
    pub asset_id: String,
    pub symbol: String,
    pub network: String,
    pub custody_wallet: String,
    pub quantity_bought: String,
    pub quantity_open: String,
    pub buy_price_per_unit: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewCryptoLot {
    pub symbol: String,
    pub network: String,
    pub custody_wallet: String,
    pub quantity_bought: String,
    pub quantity_open: String,
    pub buy_price_per_unit: i64,
    pub purchase_cost: i64,
    pub fees: i64,
    pub purchase_date: i64,
    pub notes: Option<String>,
}

pub fn map_crypto_lot(db: DbCryptoLot) -> pb::PortfolioCryptoLot {
    pb::PortfolioCryptoLot {
        asset_id: db.asset_id,
        symbol: db.symbol,
        network: map_crypto_network_to_proto(&db.network),
        custody_wallet: db.custody_wallet,
        quantity_bought: parse_quantity(&db.quantity_bought),
        quantity_open: parse_quantity(&db.quantity_open),
        buy_price_per_unit: db.buy_price_per_unit,
        purchase_cost: db.purchase_cost,
        fees: db.fees,
        purchase_date: db.purchase_date,
        notes: db.notes.unwrap_or_default(),
    }
}

fn map_crypto_network_to_proto(s: &str) -> i32 {
    use pb::CryptoNetwork as N;
    match s.to_ascii_lowercase().as_str() {
        "bitcoin" => N::Bitcoin as i32,
        "ethereum" => N::Ethereum as i32,
        "solana" => N::Solana as i32,
        "bnb_chain" | "bnb-chain" | "bnbchain" => N::BnbChain as i32,
        "polkadot" => N::Polkadot as i32,
        _ => N::Other as i32,
    }
}

pub fn map_stock_lot(db: DbStockLot) -> pb::PortfolioStockLot {
    pb::PortfolioStockLot {
        asset_id: db.asset_id,
        ticker: db.ticker,
        exchange: map_stock_exchange_to_proto(&db.exchange),
        quantity_bought: parse_quantity(&db.quantity_bought),
        quantity_open: parse_quantity(&db.quantity_open),
        buy_price_per_share: db.buy_price_per_share,
        purchase_cost: db.purchase_cost,
        fees: db.fees,
        purchase_date: db.purchase_date,
        settlement_date: db.settlement_date.unwrap_or(0),
        notes: db.notes.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Price observations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbPriceObservation {
    pub id: String,
    pub asset_id: String,
    pub provider: String,
    pub price_side: String,
    pub unit_price: i64,
    pub currency: String,
    pub observed_at: i64,
    pub source_reference: String,
    pub idempotency_key: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewPriceObservation {
    pub id: Option<String>,
    pub asset_id: String,
    pub provider: String,
    pub price_side: PriceSide,
    pub unit_price: i64,
    pub currency: String,
    pub observed_at: i64,
    pub source_reference: String,
    pub idempotency_key: Option<String>,
    pub notes: Option<String>,
}

pub fn map_price_observation(db: DbPriceObservation) -> pb::PriceObservation {
    pb::PriceObservation {
        id: db.id,
        asset_id: db.asset_id,
        provider: db.provider,
        price_side: map_price_side_to_proto(&db.price_side),
        unit_price: db.unit_price,
        currency: db.currency,
        observed_at: db.observed_at,
        source_reference: db.source_reference,
        notes: db.notes.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Activity log
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbActivity {
    pub id: String,
    pub asset_id: String,
    pub budget_id: String,
    pub activity_type: String,
    pub actor_user_id: String,
    pub correlation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub occurred_at: i64,
    pub payload_json: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewActivity {
    pub id: Option<String>,
    pub asset_id: String,
    pub budget_id: String,
    pub activity_type: String,
    pub actor_user_id: String,
    pub correlation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub occurred_at: i64,
    pub payload_json: Option<String>,
}

pub fn map_activity(db: DbActivity) -> pb::PortfolioActivity {
    pb::PortfolioActivity {
        id: db.id,
        asset_id: db.asset_id,
        budget_id: db.budget_id,
        activity_type: map_activity_type_to_proto(&db.activity_type),
        actor_user_id: db.actor_user_id,
        correlation_id: db.correlation_id.unwrap_or_default(),
        idempotency_key: db.idempotency_key.unwrap_or_default(),
        occurred_at: db.occurred_at,
        payload_json: db.payload_json.unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Outbox events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewOutboxEvent {
    pub id: String,
    pub event_type: String,
    pub asset_id: Option<String>,
    pub budget_id: Option<String>,
    pub payload_json: String,
    pub enqueued_at: i64,
}

// ---------------------------------------------------------------------------
// Transfers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct DbTransfer {
    pub id: String,
    pub group_id: String,
    pub source_budget_id: String,
    pub counterparty_budget_id: String,
    pub direction: String,
    pub amount_minor: i64,
    pub currency: String,
    pub status: String,
    pub linked_entry_source_id: Option<String>,
    pub linked_entry_counterparty_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub actor_user_id: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewTransfer {
    pub id: String,
    pub group_id: String,
    pub source_budget_id: String,
    pub counterparty_budget_id: String,
    pub direction: i32,
    pub amount_minor: i64,
    pub currency: String,
    pub status: i32,
    pub linked_entry_source_id: Option<String>,
    pub linked_entry_counterparty_id: Option<String>,
    pub idempotency_key: String,
    pub actor_user_id: String,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub notes: Option<String>,
}

pub fn map_transfer(db: DbTransfer) -> pb::PortfolioTransfer {
    let direction = match db.direction.as_str() {
        "standard_to_invest" => pb::TransferDirection::StandardToInvest as i32,
        "invest_to_standard" => pb::TransferDirection::InvestToStandard as i32,
        "internal_rebalance" => pb::TransferDirection::InternalRebalance as i32,
        _ => pb::TransferDirection::Unspecified as i32,
    };
    let status = match db.status.as_str() {
        "requested" => pb::TransferStatus::Requested as i32,
        "cash_leg_pending" => pb::TransferStatus::CashLegPending as i32,
        "asset_leg_pending" => pb::TransferStatus::AssetLegPending as i32,
        "completed" => pb::TransferStatus::Completed as i32,
        "failed" => pb::TransferStatus::Failed as i32,
        "compensation_pending" => pb::TransferStatus::CompensationPending as i32,
        "compensated" => pb::TransferStatus::Compensated as i32,
        _ => pb::TransferStatus::Unspecified as i32,
    };
    pb::PortfolioTransfer {
        id: db.id,
        group_id: db.group_id,
        source_budget_id: db.source_budget_id,
        counterparty_budget_id: db.counterparty_budget_id,
        direction,
        amount_minor: db.amount_minor,
        currency: db.currency,
        status,
        linked_entry_source_id: db.linked_entry_source_id.unwrap_or_default(),
        linked_entry_counterparty_id: db.linked_entry_counterparty_id.unwrap_or_default(),
        idempotency_key: db.idempotency_key.unwrap_or_default(),
        actor_user_id: db.actor_user_id,
        created_at: db.created_at,
        completed_at: db.completed_at.unwrap_or(0),
        notes: db.notes.unwrap_or_default(),
    }
}

#[allow(dead_code)]
fn transfer_direction_to_db(v: i32) -> &'static str {
    match v {
        x if x == pb::TransferDirection::StandardToInvest as i32 => "standard_to_invest",
        x if x == pb::TransferDirection::InvestToStandard as i32 => "invest_to_standard",
        x if x == pb::TransferDirection::InternalRebalance as i32 => "internal_rebalance",
        _ => "unspecified",
    }
}

#[allow(dead_code)]
fn transfer_status_to_db(v: i32) -> &'static str {
    match v {
        x if x == pb::TransferStatus::Requested as i32 => "requested",
        x if x == pb::TransferStatus::CashLegPending as i32 => "cash_leg_pending",
        x if x == pb::TransferStatus::AssetLegPending as i32 => "asset_leg_pending",
        x if x == pb::TransferStatus::Completed as i32 => "completed",
        x if x == pb::TransferStatus::Failed as i32 => "failed",
        x if x == pb::TransferStatus::CompensationPending as i32 => "compensation_pending",
        x if x == pb::TransferStatus::Compensated as i32 => "compensated",
        _ => "unspecified",
    }
}

// ---------------------------------------------------------------------------
// Common helpers
// ---------------------------------------------------------------------------

fn common_base(
    id: &str,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
    created_by: &str,
) -> crate::pb::common::base::Base {
    crate::pb::common::base::Base {
        id: id.to_string(),
        created_at,
        updated_at,
        deleted_at: deleted_at.unwrap_or(0),
        created_by: created_by.to_string(),
        updated_by: String::new(),
        owner_id: String::new(),
        status: 0,
    }
}

fn parse_rate_to_f64(s: &str) -> f64 {
    use std::str::FromStr;
    Decimal::from_str(s)
        .ok()
        .and_then(|d| f64::try_from(d).ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

fn parse_quantity(s: &str) -> String {
    use std::str::FromStr;
    Decimal::from_str(s)
        .map(|d| d.to_string())
        .unwrap_or_else(|_| s.to_string())
}

fn map_asset_status_to_proto(s: &str) -> i32 {
    let st = AssetStatus::from_db(s);
    match st {
        AssetStatus::Active => pb::PortfolioAssetStatus::Active as i32,
        AssetStatus::Closed => pb::PortfolioAssetStatus::Closed as i32,
        AssetStatus::Matured => pb::PortfolioAssetStatus::Matured as i32,
        AssetStatus::Sold => pb::PortfolioAssetStatus::Sold as i32,
        AssetStatus::Archived => pb::PortfolioAssetStatus::Archived as i32,
        AssetStatus::RolledOver => pb::PortfolioAssetStatus::RolledOver as i32,
        AssetStatus::Withdrawn => pb::PortfolioAssetStatus::Withdrawn as i32,
        AssetStatus::EarlyClosed => pb::PortfolioAssetStatus::EarlyClosed as i32,
    }
}

fn map_interest_method_to_proto(s: &str) -> i32 {
    match s {
        "compound" => pb::InterestMethod::Compound as i32,
        _ => pb::InterestMethod::Simple as i32,
    }
}

fn map_payout_type_to_proto(s: &str) -> i32 {
    match s {
        "monthly" => pb::PayoutType::Monthly as i32,
        "quarterly" => pb::PayoutType::Quarterly as i32,
        "on_demand" => pb::PayoutType::OnDemand as i32,
        _ => pb::PayoutType::AtMaturity as i32,
    }
}

fn map_auto_renewal_to_proto(s: &str) -> i32 {
    match s {
        "principal_only" => pb::AutoRenewalPolicy::PrincipalOnly as i32,
        "principal_and_interest" => pb::AutoRenewalPolicy::PrincipalAndInterest as i32,
        _ => pb::AutoRenewalPolicy::None as i32,
    }
}

fn map_gold_purity_to_proto(s: &str) -> i32 {
    match s {
        "sjc_9999" => pb::GoldPurity::Sjc9999 as i32,
        "pnj_999" => pb::GoldPurity::Pnj999 as i32,
        "pnj_995" => pb::GoldPurity::Pnj995 as i32,
        "doji_9999" => pb::GoldPurity::Doji9999 as i32,
        _ => pb::GoldPurity::Other as i32,
    }
}

fn map_gold_form_to_proto(s: &str) -> i32 {
    match s {
        "bar" => pb::GoldForm::Bar as i32,
        "ring" => pb::GoldForm::Ring as i32,
        "coin" => pb::GoldForm::Coin as i32,
        "jewelry" => pb::GoldForm::Jewelry as i32,
        _ => pb::GoldForm::Other as i32,
    }
}

fn map_gold_unit_to_proto(s: &str) -> i32 {
    match s {
        "chi" => pb::GoldUnit::Chi as i32,
        "luong" => pb::GoldUnit::Luong as i32,
        _ => pb::GoldUnit::Gram as i32,
    }
}

fn map_stock_exchange_to_proto(s: &str) -> i32 {
    match s {
        "HNX" => pb::StockExchange::Hnx as i32,
        "UPCOM" => pb::StockExchange::Upcom as i32,
        _ => pb::StockExchange::Hose as i32,
    }
}

fn map_price_side_to_proto(s: &str) -> i32 {
    match s {
        "bid" => pb::PriceSide::Bid as i32,
        "ask" => pb::PriceSide::Ask as i32,
        _ => pb::PriceSide::Mid as i32,
    }
}

fn map_activity_type_to_proto(s: &str) -> i32 {
    use pb::ActivityType as A;
    match s {
        "CREATED" => A::Created as i32,
        "UPDATED_METADATA" => A::UpdatedMetadata as i32,
        "BALANCE_ADJUSTED" => A::BalanceAdjusted as i32,
        "RATE_RECORDED" => A::RateRecorded as i32,
        "PRICE_OBSERVED" => A::PriceObserved as i32,
        "DISPOSAL_RECORDED" => A::DisposalRecorded as i32,
        "STATUS_CHANGED" => A::StatusChanged as i32,
        "MATURITY_REACHED" => A::MaturityReached as i32,
        "ROLLED_OVER" => A::RolledOver as i32,
        "TRANSFER_INITIATED" => A::TransferInitiated as i32,
        "TRANSFER_COMPLETED" => A::TransferCompleted as i32,
        "ARCHIVED" => A::Archived as i32,
        "DELETED" => A::Deleted as i32,
        _ => A::Unspecified as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_class_new_round_trip() {
        for c in [
            AssetClassNew::SavingsAccount,
            AssetClassNew::FixedDeposit,
            AssetClassNew::GoldLot,
            AssetClassNew::StockLot,
        ] {
            assert_eq!(AssetClassNew::from_proto(c.to_proto()), Some(c));
        }
    }

    #[test]
    fn status_to_proto_uses_proto_constants() {
        // spot-check that the mapping matches proto enum integers.
        assert_eq!(
            map_asset_status_to_proto("active"),
            pb::PortfolioAssetStatus::Active as i32
        );
        assert_eq!(
            map_asset_status_to_proto("rolled_over"),
            pb::PortfolioAssetStatus::RolledOver as i32
        );
        assert_eq!(
            map_asset_status_to_proto("garbage"),
            pb::PortfolioAssetStatus::Active as i32
        );
    }
}
