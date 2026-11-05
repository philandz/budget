//! Thin gRPC handler for the Debt book service; the logic lives in `DebtBiz`.

use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::manager::biz::debt::DebtBiz;
use crate::manager::validate;
use crate::pb::service::debt::debt_service_server::DebtService;
use crate::pb::service::debt::*;

pub struct DebtHandler {
    biz: Arc<DebtBiz>,
}

impl DebtHandler {
    pub fn new(biz: Arc<DebtBiz>) -> Self {
        Self { biz }
    }
}

macro_rules! call {
    ($self:ident, $request:ident, $method:ident) => {{
        let user_id = validate::user_id_from_metadata($request.metadata())?;
        let user_type = validate::user_type_from_metadata($request.metadata());
        let req = $request.into_inner();
        Ok(Response::new(
            $self
                .biz
                .$method(&user_id, req, user_type.as_deref())
                .await?,
        ))
    }};
}

#[tonic::async_trait]
impl DebtService for DebtHandler {
    async fn create_debt_account(
        &self,
        request: Request<CreateDebtAccountRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, create_account)
    }
    async fn update_debt_account(
        &self,
        request: Request<UpdateDebtAccountRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, update_account)
    }
    async fn close_debt_account(
        &self,
        request: Request<DebtAccountRef>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, close_account)
    }
    async fn reopen_debt_account(
        &self,
        request: Request<DebtAccountRef>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, reopen_account)
    }
    async fn settle_debt_account(
        &self,
        request: Request<DebtAccountRef>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, settle_account)
    }
    async fn list_debt_accounts(
        &self,
        request: Request<ListDebtAccountsRequest>,
    ) -> Result<Response<ListDebtAccountsResponse>, Status> {
        call!(self, request, list_accounts)
    }
    async fn get_debt_account(
        &self,
        request: Request<DebtAccountRef>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, get_account)
    }
    async fn set_rate_period(
        &self,
        request: Request<SetRatePeriodRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, set_rate)
    }
    async fn delete_rate_period(
        &self,
        request: Request<DeleteRatePeriodRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, delete_rate)
    }
    async fn set_prepay_rule(
        &self,
        request: Request<SetPrepayRuleRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, set_prepay_rule)
    }
    async fn delete_prepay_rule(
        &self,
        request: Request<DeletePrepayRuleRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, delete_prepay_rule)
    }
    async fn get_schedule(
        &self,
        request: Request<GetScheduleRequest>,
    ) -> Result<Response<GetScheduleResponse>, Status> {
        call!(self, request, get_schedule)
    }
    async fn quote_payment(
        &self,
        request: Request<QuotePaymentRequest>,
    ) -> Result<Response<QuotePaymentResponse>, Status> {
        call!(self, request, quote_payment)
    }
    async fn record_payment(
        &self,
        request: Request<RecordPaymentRequest>,
    ) -> Result<Response<RecordPaymentResponse>, Status> {
        call!(self, request, record_payment)
    }
    async fn adjust_outstanding(
        &self,
        request: Request<AdjustOutstandingRequest>,
    ) -> Result<Response<DebtAccount>, Status> {
        call!(self, request, adjust_outstanding)
    }
    async fn list_debt_payments(
        &self,
        request: Request<ListDebtPaymentsRequest>,
    ) -> Result<Response<ListDebtPaymentsResponse>, Status> {
        call!(self, request, list_payments)
    }
    async fn list_debt_events(
        &self,
        request: Request<ListDebtEventsRequest>,
    ) -> Result<Response<ListDebtEventsResponse>, Status> {
        call!(self, request, list_events)
    }
    async fn get_debt_book_summary(
        &self,
        request: Request<GetDebtBookSummaryRequest>,
    ) -> Result<Response<DebtBookSummary>, Status> {
        let user_id = validate::user_id_from_metadata(request.metadata())?;
        let user_type = validate::user_type_from_metadata(request.metadata());
        let req = request.into_inner();
        Ok(Response::new(
            self.biz
                .book_summary(&user_id, &req.budget_id, user_type.as_deref())
                .await?,
        ))
    }
}
