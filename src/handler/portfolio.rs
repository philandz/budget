//! Thin gRPC handler for the Asset Portfolio service.
//!
//! Each method extracts user metadata, translates the request to the
//! biz-layer parameter shape, and returns the proto response. Business
//! logic lives in `PortfolioBiz`.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::validate;
use crate::pb::service::portfolio::portfolio_service_server::PortfolioService;
use crate::pb::service::portfolio::{
    ArchiveAssetRequest, CreateCryptoLotRequest, CreateEtfLotRequest, CreateFixedDepositRequest,
    CreateGoldLotRequest, CreateSavingsAccountRequest, CreateStockLotRequest, GetAssetRequest,
    GetAssetResponse, GetPortfolioSummaryRequest, GetPortfolioSummaryResponse,
    ListAssetActivityRequest, ListAssetActivityResponse, ListAssetsRequest, ListAssetsResponse,
    ListPriceObservationsRequest, ListPriceObservationsResponse, PortfolioAsset, PriceObservation,
    RecordPriceObservationRequest, RecordStockDisposalRequest, UpdateAssetMetadataRequest,
    ValuatedAsset,
};

pub struct PortfolioHandler {
    biz: Arc<PortfolioBiz>,
}

impl PortfolioHandler {
    pub fn new(biz: Arc<PortfolioBiz>) -> Self {
        Self { biz }
    }
}

#[tonic::async_trait]
impl PortfolioService for PortfolioHandler {
    // -----------------------------------------------------------------
    // Asset root
    // -----------------------------------------------------------------

    async fn list_assets(
        &self,
        request: Request<ListAssetsRequest>,
    ) -> Result<Response<ListAssetsResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        // Direct valuation: single transaction across N assets, no
        // N+1 subtype fetch per asset.
        let assets = self
            .biz
            .list_valuated(&user_id, &req.budget_id, user_type.as_deref())
            .await?;
        let total_rows = assets.len() as i64;
        Ok(Response::new(ListAssetsResponse {
            assets,
            page: 0,
            page_size: total_rows as i32,
            total_rows,
        }))
    }

    async fn get_asset(
        &self,
        request: Request<GetAssetRequest>,
    ) -> Result<Response<GetAssetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let summary = self
            .biz
            .get_portfolio_summary(&user_id, &req.budget_id, user_type.as_deref())
            .await?;
        let asset = summary
            .assets
            .into_iter()
            .find(|v| {
                v.asset
                    .as_ref()
                    .map(|a| {
                        a.base
                            .as_ref()
                            .map(|b| b.id == req.asset_id)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .ok_or_else(|| Status::not_found("asset not found"))?;
        Ok(Response::new(GetAssetResponse { asset: Some(asset) }))
    }

    async fn update_asset_metadata(
        &self,
        request: Request<UpdateAssetMetadataRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let asset = self
            .biz
            .update_metadata(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                if req.display_name.is_empty() {
                    None
                } else {
                    Some(req.display_name.as_str())
                },
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn archive_asset(
        &self,
        request: Request<ArchiveAssetRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let asset = self
            .biz
            .archive_asset(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn get_portfolio_summary(
        &self,
        request: Request<GetPortfolioSummaryRequest>,
    ) -> Result<Response<GetPortfolioSummaryResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let summary = self
            .biz
            .get_portfolio_summary(&user_id, &req.budget_id, user_type.as_deref())
            .await?;
        Ok(Response::new(GetPortfolioSummaryResponse {
            summary: Some(summary),
        }))
    }

    // -----------------------------------------------------------------
    // Class-specific create
    // -----------------------------------------------------------------

    async fn create_savings_account(
        &self,
        request: Request<CreateSavingsAccountRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let interest_method = i32_method_to_db(req.interest_method);
        let payout_type = i32_payout_to_db(req.payout_type);
        let asset = self
            .biz
            .create_savings_account(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.provider,
                &req.account_reference_masked,
                req.current_balance,
                req.balance_as_of,
                &req.annual_rate.to_string(),
                &interest_method,
                &payout_type,
                req.opened_on,
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn create_fixed_deposit(
        &self,
        request: Request<CreateFixedDepositRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let interest_method = i32_method_to_db(req.interest_method);
        let payout_type = i32_payout_to_db(req.payout_type);
        let auto_renewal = i32_auto_renewal_to_db(req.auto_renewal_policy);
        let asset = self
            .biz
            .create_fixed_deposit(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.provider,
                &req.product_name,
                req.principal,
                &req.annual_rate.to_string(),
                &interest_method,
                &payout_type,
                req.deposit_date,
                req.maturity_date,
                &auto_renewal,
                &req.certificate_reference_masked,
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn create_gold_lot(
        &self,
        request: Request<CreateGoldLotRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let purity = i32_purity_to_db(req.purity);
        let form = i32_form_to_db(req.form);
        let unit = parse_gold_unit(req.unit_original);
        // purchase_cost = quantity * price + fees; proto has no cost field.
        let qty_f: f64 = req
            .quantity_original
            .parse()
            .map_err(|_| Status::invalid_argument("invalid quantity_original: not a number"))?;
        if qty_f < 0.0 {
            return Err(Status::invalid_argument("quantity_original must be >= 0"));
        }
        let purchase_cost =
            (qty_f * req.purchase_price_per_unit_original as f64).round() as i64 + req.fees;
        let asset = self
            .biz
            .create_gold_lot(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.provider,
                &req.gold_type,
                &purity,
                &form,
                &req.quantity_original,
                unit,
                req.purchase_price_per_unit_original,
                purchase_cost,
                req.fees,
                req.purchase_date,
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn create_stock_lot(
        &self,
        request: Request<CreateStockLotRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let exchange = i32_exchange_to_db(req.exchange);
        // Server derives purchase_cost from quantity_bought * buy_price_per_share + fees
        // because the proto message has no purchase_cost field (lot semantics).
        let qty_f: f64 = req
            .quantity_bought
            .parse()
            .map_err(|_| Status::invalid_argument("invalid quantity_bought: not a number"))?;
        if qty_f < 0.0 {
            return Err(Status::invalid_argument("quantity_bought must be >= 0"));
        }
        let purchase_cost = (qty_f * req.buy_price_per_share as f64).round() as i64 + req.fees;
        let asset = self
            .biz
            .create_stock_lot(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.ticker,
                &exchange,
                &req.quantity_bought,
                req.buy_price_per_share,
                purchase_cost,
                req.fees,
                req.purchase_date,
                if req.settlement_date == 0 {
                    None
                } else {
                    Some(req.settlement_date)
                },
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn create_etf_lot(
        &self,
        request: Request<CreateEtfLotRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let exchange = i32_exchange_to_db(req.exchange);
        let qty_f: f64 = req
            .quantity_bought
            .parse()
            .map_err(|_| Status::invalid_argument("invalid quantity_bought: not a number"))?;
        if qty_f < 0.0 {
            return Err(Status::invalid_argument("quantity_bought must be >= 0"));
        }
        let purchase_cost = (qty_f * req.buy_price_per_unit as f64).round() as i64 + req.fees;
        let asset = self
            .biz
            .create_etf_lot(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.ticker,
                &exchange,
                &req.underlying_index,
                &req.fund_provider,
                &req.quantity_bought,
                req.buy_price_per_unit,
                purchase_cost,
                req.fees,
                req.purchase_date,
                if req.settlement_date == 0 {
                    None
                } else {
                    Some(req.settlement_date)
                },
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn create_crypto_lot(
        &self,
        request: Request<CreateCryptoLotRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let qty_f: f64 = req
            .quantity_bought
            .parse()
            .map_err(|_| Status::invalid_argument("invalid quantity_bought: not a number"))?;
        if qty_f < 0.0 {
            return Err(Status::invalid_argument("quantity_bought must be >= 0"));
        }
        let purchase_cost = (qty_f * req.buy_price_per_unit as f64).round() as i64 + req.fees;
        let asset = self
            .biz
            .create_crypto_lot(
                &user_id,
                &req.budget_id,
                &req.display_name,
                &req.currency,
                &req.symbol,
                &req.network,
                &req.custody_wallet,
                &req.quantity_bought,
                req.buy_price_per_unit,
                purchase_cost,
                req.fees,
                req.purchase_date,
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    // -----------------------------------------------------------------
    // Observations and activity
    // -----------------------------------------------------------------

    async fn record_price_observation(
        &self,
        request: Request<RecordPriceObservationRequest>,
    ) -> Result<Response<PriceObservation>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let price_side = i32_price_side_to_db(req.price_side);
        // No `provider` field on the request — server defaults to "manual".
        let obs = self
            .biz
            .record_price_observation(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                "manual",
                &price_side,
                req.unit_price,
                &req.currency,
                req.observed_at,
                &req.source_reference,
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                if req.notes.is_empty() {
                    None
                } else {
                    Some(req.notes.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(obs))
    }

    async fn list_price_observations(
        &self,
        request: Request<ListPriceObservationsRequest>,
    ) -> Result<Response<ListPriceObservationsResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let observations = self
            .biz
            .list_price_observations(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                req.limit,
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(ListPriceObservationsResponse {
            observations,
        }))
    }

    async fn list_asset_activity(
        &self,
        request: Request<ListAssetActivityRequest>,
    ) -> Result<Response<ListAssetActivityResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let activities = self
            .biz
            .list_asset_activity(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                req.limit,
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(ListAssetActivityResponse { activities }))
    }

    // -----------------------------------------------------------------
    // Disposals
    // -----------------------------------------------------------------

    async fn record_stock_disposal(
        &self,
        request: Request<RecordStockDisposalRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let asset = self
            .biz
            .record_stock_disposal(
                &user_id,
                &req.budget_id,
                &req.asset_id,
                &req.quantity_sold,
                req.sale_proceeds,
                req.sale_fees,
                req.disposal_date,
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                user_type.as_deref(),
            )
            .await?;
        Ok(Response::new(asset))
    }

    async fn record_gold_disposal(
        &self,
        _request: Request<crate::pb::service::portfolio::RecordGoldDisposalRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        Err(Status::unimplemented("gold disposal arrives in Phase 3.1"))
    }

    async fn record_fixed_deposit_maturity(
        &self,
        _request: Request<crate::pb::service::portfolio::RecordFixedDepositMaturityRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        Err(Status::unimplemented("maturity arrives in Phase 3.2"))
    }

    async fn record_fixed_deposit_rollover(
        &self,
        _request: Request<crate::pb::service::portfolio::RecordFixedDepositRolloverRequest>,
    ) -> Result<Response<PortfolioAsset>, Status> {
        Err(Status::unimplemented("rollover arrives in Phase 3.3"))
    }

    async fn run_backfill(
        &self,
        request: Request<crate::pb::service::portfolio::RunBackfillRequest>,
    ) -> Result<Response<crate::pb::service::portfolio::RunBackfillResponse>, Status> {
        let _user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        if user_type.as_deref() != Some("super_admin") {
            return Err(Status::permission_denied("super_admin only"));
        }
        let migrated = self.biz.run_backfill(&req.budget_id).await?;
        Ok(Response::new(
            crate::pb::service::portfolio::RunBackfillResponse {
                migrated_count: migrated,
                skipped_count: 0,
            },
        ))
    }

    async fn create_transfer(
        &self,
        request: Request<crate::pb::service::portfolio::CreatePortfolioTransferRequest>,
    ) -> Result<Response<crate::pb::service::portfolio::CreatePortfolioTransferResponse>, Status>
    {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        let resp = self
            .biz
            .create_transfer(&user_id, &req, user_type.as_deref())
            .await?;
        Ok(Response::new(resp))
    }
}

// ---------------------------------------------------------------------------
// Enum-to-DB string conversion helpers
// ---------------------------------------------------------------------------

fn i32_method_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::InterestMethod as E;
    match E::try_from(v).unwrap_or(E::Simple) {
        E::Compound => "compound".to_string(),
        E::Simple => "simple".to_string(),
        _ => "simple".to_string(),
    }
}

fn i32_payout_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::PayoutType as E;
    match E::try_from(v).unwrap_or(E::AtMaturity) {
        E::AtMaturity => "at_maturity".to_string(),
        E::Monthly => "monthly".to_string(),
        E::Quarterly => "quarterly".to_string(),
        E::OnDemand => "on_demand".to_string(),
        _ => "at_maturity".to_string(),
    }
}

fn i32_auto_renewal_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::AutoRenewalPolicy as E;
    match E::try_from(v).unwrap_or(E::None) {
        E::None => "none".to_string(),
        E::PrincipalOnly => "principal_only".to_string(),
        E::PrincipalAndInterest => "principal_and_interest".to_string(),
        _ => "none".to_string(),
    }
}

fn i32_purity_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::GoldPurity as E;
    match E::try_from(v).unwrap_or(E::Other) {
        E::Sjc9999 => "sjc_9999".to_string(),
        E::Pnj999 => "pnj_999".to_string(),
        E::Pnj995 => "pnj_995".to_string(),
        E::Doji9999 => "doji_9999".to_string(),
        E::Other => "other".to_string(),
        _ => "other".to_string(),
    }
}

fn i32_form_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::GoldForm as E;
    match E::try_from(v).unwrap_or(E::Other) {
        E::Bar => "bar".to_string(),
        E::Ring => "ring".to_string(),
        E::Coin => "coin".to_string(),
        E::Jewelry => "jewelry".to_string(),
        E::Other => "other".to_string(),
        _ => "other".to_string(),
    }
}

fn parse_gold_unit(v: i32) -> crate::manager::biz::portfolio::GoldUnit {
    use crate::manager::biz::portfolio::GoldUnit;
    use crate::pb::service::portfolio::GoldUnit as P;
    match P::try_from(v).unwrap_or(P::Gram) {
        P::Chi => GoldUnit::Chi,
        P::Luong => GoldUnit::Luong,
        P::Gram => GoldUnit::Gram,
        _ => GoldUnit::Gram,
    }
}

fn i32_exchange_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::StockExchange as E;
    match E::try_from(v).unwrap_or(E::Hose) {
        E::Hose => "HOSE".to_string(),
        E::Hnx => "HNX".to_string(),
        E::Upcom => "UPCOM".to_string(),
        _ => "HOSE".to_string(),
    }
}

fn i32_price_side_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::PriceSide as E;
    match E::try_from(v).unwrap_or(E::Mid) {
        E::Bid => "bid".to_string(),
        E::Ask => "ask".to_string(),
        E::Mid => "mid".to_string(),
        _ => "mid".to_string(),
    }
}

// Stub: PortfolioActivity is referenced through generated RPC return types
// (typed imports cover all reachable usages); keep this symbol as a
// compile-time anchor.
#[allow(dead_code)]
fn _silence_unused() {
    let _: std::marker::PhantomData<ValuatedAsset> = std::marker::PhantomData;
}
