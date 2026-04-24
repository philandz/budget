use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::manager::biz::BudgetBiz;
use crate::manager::validate;
use crate::pb::service::budget::{
    budget_service_server::BudgetService, AddBudgetMemberRequest, AddBudgetMemberResponse,
    AddPriceSnapshotRequest, BudgetRole, BudgetType, CheckRoleRequest, CheckRoleResponse,
    CreateBudgetRequest, CreateBudgetResponse, CreateInvestAssetRequest, DeleteBudgetRequest,
    DeleteBudgetResponse, DeleteInvestAssetRequest, DeleteInvestAssetResponse, GetBudgetRequest,
    GetBudgetResponse, GetBurnRateRequest, GetBurnRateResponse, GetInvestPortfolioSummaryRequest,
    GetLatestPriceSnapshotRequest, GetRolloverPolicyRequest, GetRolloverPolicyResponse,
    InvestAsset, InvestPortfolioSummary, ListBudgetMembersRequest, ListBudgetMembersResponse,
    ListBudgetsRequest, ListBudgetsResponse, ListInvestAssetsRequest, ListInvestAssetsResponse,
    ListPriceSnapshotsRequest, ListPriceSnapshotsResponse, ListTemplatesRequest,
    ListTemplatesResponse, PriceSnapshot, RemoveBudgetMemberRequest, RemoveBudgetMemberResponse,
    SetEnvelopeLimitRequest, SetEnvelopeLimitResponse, SetRolloverPolicyRequest,
    SetRolloverPolicyResponse, UpdateBudgetMemberRoleRequest, UpdateBudgetMemberRoleResponse,
    UpdateBudgetRequest, UpdateBudgetResponse, UpdateInvestAssetRequest,
};

pub struct BudgetHandler {
    biz: Arc<BudgetBiz>,
}

impl BudgetHandler {
    pub fn new(biz: Arc<BudgetBiz>) -> Self {
        Self { biz }
    }
}

#[tonic::async_trait]
impl BudgetService for BudgetHandler {
    async fn create_budget(
        &self,
        request: Request<CreateBudgetRequest>,
    ) -> Result<Response<CreateBudgetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let budget_type = BudgetType::try_from(req.budget_type).unwrap_or(BudgetType::Standard);
        let budget = self
            .biz
            .create_budget(
                &user_id,
                &req.org_id,
                &req.name,
                budget_type,
                &req.currency,
                if req.template_id.is_empty() {
                    None
                } else {
                    Some(&req.template_id)
                },
            )
            .await?;
        Ok(Response::new(CreateBudgetResponse {
            budget: Some(budget),
        }))
    }

    async fn get_budget(
        &self,
        request: Request<GetBudgetRequest>,
    ) -> Result<Response<GetBudgetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let budget = self.biz.get_budget(&user_id, &req.budget_id).await?;
        Ok(Response::new(GetBudgetResponse {
            budget: Some(budget),
        }))
    }

    async fn update_budget(
        &self,
        request: Request<UpdateBudgetRequest>,
    ) -> Result<Response<UpdateBudgetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        validate::budget_name(&req.name)?;
        let budget_type = BudgetType::try_from(req.budget_type).unwrap_or(BudgetType::Standard);
        let budget = self
            .biz
            .update_budget(&user_id, &req.budget_id, &req.name, budget_type)
            .await?;
        Ok(Response::new(UpdateBudgetResponse {
            budget: Some(budget),
        }))
    }

    async fn delete_budget(
        &self,
        request: Request<DeleteBudgetRequest>,
    ) -> Result<Response<DeleteBudgetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        self.biz.delete_budget(&user_id, &req.budget_id).await?;
        Ok(Response::new(DeleteBudgetResponse { success: true }))
    }

    async fn list_budgets(
        &self,
        request: Request<ListBudgetsRequest>,
    ) -> Result<Response<ListBudgetsResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let budgets = self.biz.list_budgets(&user_id, &req.org_id).await?;
        Ok(Response::new(ListBudgetsResponse { budgets }))
    }

    async fn check_role(
        &self,
        request: Request<CheckRoleRequest>,
    ) -> Result<Response<CheckRoleResponse>, Status> {
        let req = request.into_inner();
        let (role, is_member) = self.biz.check_role(&req.user_id, &req.budget_id).await?;
        Ok(Response::new(CheckRoleResponse {
            role: role as i32,
            is_member,
        }))
    }

    async fn add_budget_member(
        &self,
        request: Request<AddBudgetMemberRequest>,
    ) -> Result<Response<AddBudgetMemberResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let role = BudgetRole::try_from(req.role).unwrap_or(BudgetRole::Viewer);
        let member = self
            .biz
            .add_member(&caller_id, &req.budget_id, &req.user_id, role)
            .await?;
        Ok(Response::new(AddBudgetMemberResponse {
            member: Some(member),
        }))
    }

    async fn update_budget_member_role(
        &self,
        request: Request<UpdateBudgetMemberRoleRequest>,
    ) -> Result<Response<UpdateBudgetMemberRoleResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let role = BudgetRole::try_from(req.role).unwrap_or(BudgetRole::Viewer);
        let member = self
            .biz
            .update_member_role(&caller_id, &req.budget_id, &req.user_id, role)
            .await?;
        Ok(Response::new(UpdateBudgetMemberRoleResponse {
            member: Some(member),
        }))
    }

    async fn remove_budget_member(
        &self,
        request: Request<RemoveBudgetMemberRequest>,
    ) -> Result<Response<RemoveBudgetMemberResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        self.biz
            .remove_member(&caller_id, &req.budget_id, &req.user_id)
            .await?;
        Ok(Response::new(RemoveBudgetMemberResponse { success: true }))
    }

    async fn list_budget_members(
        &self,
        request: Request<ListBudgetMembersRequest>,
    ) -> Result<Response<ListBudgetMembersResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let members = self.biz.list_members(&caller_id, &req.budget_id).await?;
        Ok(Response::new(ListBudgetMembersResponse { members }))
    }

    async fn set_envelope_limit(
        &self,
        request: Request<SetEnvelopeLimitRequest>,
    ) -> Result<Response<SetEnvelopeLimitResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        validate::envelope_limit(req.monthly_limit)?;
        let envelope = self
            .biz
            .set_envelope_limit(&caller_id, &req.budget_id, req.monthly_limit)
            .await?;
        Ok(Response::new(SetEnvelopeLimitResponse {
            envelope: Some(envelope),
        }))
    }

    async fn get_burn_rate(
        &self,
        request: Request<GetBurnRateRequest>,
    ) -> Result<Response<GetBurnRateResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let envelope = self.biz.get_burn_rate(&caller_id, &req.budget_id).await?;
        Ok(Response::new(GetBurnRateResponse {
            envelope: Some(envelope),
        }))
    }

    async fn set_rollover_policy(
        &self,
        request: Request<SetRolloverPolicyRequest>,
    ) -> Result<Response<SetRolloverPolicyResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let policy = validate::rollover_policy(req.policy)?;
        let policy = self
            .biz
            .set_rollover_policy(&caller_id, &req.budget_id, policy)
            .await?;
        Ok(Response::new(SetRolloverPolicyResponse {
            policy: policy as i32,
        }))
    }

    async fn get_rollover_policy(
        &self,
        request: Request<GetRolloverPolicyRequest>,
    ) -> Result<Response<GetRolloverPolicyResponse>, Status> {
        let caller_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let policy = self
            .biz
            .get_rollover_policy(&caller_id, &req.budget_id)
            .await?;
        Ok(Response::new(GetRolloverPolicyResponse {
            policy: policy as i32,
        }))
    }

    async fn list_templates(
        &self,
        _request: Request<ListTemplatesRequest>,
    ) -> Result<Response<ListTemplatesResponse>, Status> {
        let templates = self.biz.list_templates().await?;
        Ok(Response::new(ListTemplatesResponse { templates }))
    }

    // -----------------------------------------------------------------------
    // Invest assets
    // -----------------------------------------------------------------------

    async fn create_invest_asset(
        &self,
        request: Request<CreateInvestAssetRequest>,
    ) -> Result<Response<InvestAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let asset = self.biz.create_invest_asset(&user_id, &req).await?;
        Ok(Response::new(asset))
    }

    async fn update_invest_asset(
        &self,
        request: Request<UpdateInvestAssetRequest>,
    ) -> Result<Response<InvestAsset>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let asset_id = req.asset_id.clone();
        let asset = self
            .biz
            .update_invest_asset(&user_id, &asset_id, &req)
            .await?;
        Ok(Response::new(asset))
    }

    async fn delete_invest_asset(
        &self,
        request: Request<DeleteInvestAssetRequest>,
    ) -> Result<Response<DeleteInvestAssetResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        self.biz
            .delete_invest_asset(&user_id, &req.asset_id)
            .await?;
        Ok(Response::new(DeleteInvestAssetResponse { success: true }))
    }

    async fn list_invest_assets(
        &self,
        request: Request<ListInvestAssetsRequest>,
    ) -> Result<Response<ListInvestAssetsResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let assets = self
            .biz
            .list_invest_assets(&user_id, &req.budget_id)
            .await?;
        Ok(Response::new(ListInvestAssetsResponse { assets }))
    }

    async fn get_invest_portfolio_summary(
        &self,
        request: Request<GetInvestPortfolioSummaryRequest>,
    ) -> Result<Response<InvestPortfolioSummary>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let summary = self
            .biz
            .get_invest_portfolio_summary(&user_id, &req.budget_id)
            .await?;
        Ok(Response::new(summary))
    }

    async fn add_price_snapshot(
        &self,
        request: Request<AddPriceSnapshotRequest>,
    ) -> Result<Response<PriceSnapshot>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let snap = self
            .biz
            .add_price_snapshot(&user_id, &req.asset_id, req.price, &req.snapshot_date)
            .await?;
        Ok(Response::new(snap))
    }

    async fn get_latest_price_snapshot(
        &self,
        request: Request<GetLatestPriceSnapshotRequest>,
    ) -> Result<Response<PriceSnapshot>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let snap = self
            .biz
            .get_latest_price_snapshot(&user_id, &req.asset_id)
            .await?;
        Ok(Response::new(snap))
    }

    async fn list_price_snapshots(
        &self,
        request: Request<ListPriceSnapshotsRequest>,
    ) -> Result<Response<ListPriceSnapshotsResponse>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let req = request.into_inner();
        let limit = if req.limit <= 0 { 90 } else { req.limit };
        let snapshots = self
            .biz
            .list_price_snapshots(&user_id, &req.asset_id, limit)
            .await?;
        Ok(Response::new(ListPriceSnapshotsResponse { snapshots }))
    }
}
