#![allow(clippy::result_large_err)]
use philand_configs::BudgetServiceConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::Status;

use crate::converters::{map_budget, map_budget_member, map_budget_member_with, DbTemplate};
use crate::manager::client::{self, IdentityClient};
use crate::manager::repository::BudgetRepository;
use crate::pb::service::budget::{
    Budget, BudgetMember, BudgetRole, BudgetTemplate, BudgetType, EnvelopeLimit, ListBudgetParams,
    PageMeta, RolloverPolicy,
};
use crate::pb::shared::organization::OrgRole;

pub mod portfolio;

pub struct BudgetBiz {
    pub repo: Arc<BudgetRepository>,
    pub config: BudgetServiceConfig,
    pub identity_client: Arc<Mutex<IdentityClient>>,
}

impl BudgetBiz {
    pub fn new(
        repo: BudgetRepository,
        config: BudgetServiceConfig,
        identity_client: IdentityClient,
    ) -> Self {
        Self {
            repo: Arc::new(repo),
            config,
            identity_client: Arc::new(Mutex::new(identity_client)),
        }
    }

    /// Test-only constructor. Builds a `BudgetBiz` whose repo uses a
    /// lazy MySQL pool (no DB connection is opened) and whose identity
    /// client points at an unreachable address with a lazy channel. Only
    /// safe to call in tests that never query the repo or call the
    /// identity gRPC client through it.
    #[doc(hidden)]
    pub async fn test_only_no_clients() -> Self {
        let repo = BudgetRepository::test_only_default_pool().await;
        Self {
            repo,
            config: philand_configs::BudgetServiceConfig::default_for_tests(),
            identity_client: Arc::new(Mutex::new(IdentityClient::test_only_unreachable())),
        }
    }

    /// Test-only accessor exposing `resolve_role` for integration tests.
    #[doc(hidden)]
    pub async fn resolve_role_for_test(
        &self,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<BudgetRole, tonic::Status> {
        self.resolve_role(budget_id, user_id, user_type).await
    }

    fn internal(e: impl ToString) -> Status {
        Status::internal(e.to_string())
    }

    // -----------------------------------------------------------------------
    // Budget CRUD
    // -----------------------------------------------------------------------

    pub async fn create_budget(
        &self,
        user_id: &str,
        org_id: &str,
        name: &str,
        budget_type: BudgetType,
        currency: &str,
        _template_id: Option<&str>,
    ) -> Result<Budget, Status> {
        let db = self
            .repo
            .create_budget(org_id, name, budget_type, currency, user_id)
            .await
            .map_err(Self::internal)?;
        Ok(map_budget(db))
    }

    pub async fn get_budget(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<Budget, Status> {
        let (role, is_member) = self.check_role(user_id, budget_id, user_type).await?;
        if !is_member {
            return Err(Status::permission_denied("Not a member of this budget"));
        }
        let role_label = match role {
            BudgetRole::Owner => "owner",
            BudgetRole::Manager => "manager",
            BudgetRole::Contributor => "contributor",
            BudgetRole::Viewer => "viewer",
            BudgetRole::Unspecified => "",
        };
        let db = self
            .repo
            .get_budget_for_user(budget_id, user_id, Some(role_label))
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        Ok(map_budget(db))
    }

    pub async fn get_budget_admin(&self, budget_id: &str) -> Result<Budget, Status> {
        let db = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        Ok(map_budget(db))
    }

    pub async fn list_members_admin(
        &self,
        budget_id: &str,
        bearer: &str,
    ) -> Result<Vec<BudgetMember>, Status> {
        let budget = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(Self::internal)?;
        let org_users = {
            let mut identity = self.identity_client.lock().await;
            // Admin path: always use service_actor since gateway already verified super_admin
            match identity.list_org_users(bearer, &budget.org_id, true).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        "list_org_users (admin) failed for org {}: {} — falling back to DB-only rows",
                        budget.org_id,
                        e
                    );
                    Vec::new()
                }
            }
        };
        let rows = self
            .repo
            .list_members(budget_id)
            .await
            .map_err(Self::internal)?;
        let by_user: std::collections::HashMap<String, client::OrgUserInfo> = org_users
            .into_iter()
            .filter(|u| !u.user_id.is_empty())
            .map(|u| (u.user_id.clone(), u))
            .collect();
        Ok(rows
            .into_iter()
            .map(|row| {
                let enriched = by_user.get(&row.user_id).cloned();
                map_budget_member_with(row, enriched.as_ref())
            })
            .collect())
    }

    pub async fn update_budget(
        &self,
        user_id: &str,
        budget_id: &str,
        name: &str,
        budget_type: BudgetType,
        is_private: bool,
        user_type: Option<&str>,
    ) -> Result<Budget, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let db = self
            .repo
            .update_budget(budget_id, name, budget_type, is_private, user_id)
            .await
            .map_err(Self::internal)?;
        Ok(map_budget(db))
    }

    pub async fn delete_budget(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Owner, user_type)
            .await?;
        self.repo
            .delete_budget_cascade(budget_id)
            .await
            .map_err(Self::internal)
    }

    pub async fn force_close_budget(&self, budget_id: &str) -> Result<Budget, Status> {
        // Verify budget exists
        let _db = self.repo.get_budget_by_id(budget_id).await
            .map_err(|_| Status::not_found("Budget not found"))?;
        self.repo.force_close_budget(budget_id).await.map_err(Self::internal)?;
        // Re-fetch to return updated budget
        let db = self.repo.get_budget_by_id(budget_id).await
            .map_err(|_| Status::not_found("Budget not found"))?;
        Ok(map_budget(db))
    }

    pub async fn list_budgets(&self, user_id: &str, org_id: &str) -> Result<Vec<Budget>, Status> {
        let rows = self
            .repo
            .list_budgets_for_user(org_id, user_id)
            .await
            .map_err(Self::internal)?;
        Ok(rows.into_iter().map(map_budget).collect())
    }

    pub async fn list_budgets_paged(
        &self,
        user_id: &str,
        org_id: &str,
        params: ListBudgetParams,
    ) -> Result<(Vec<Budget>, PageMeta), Status> {
        let q = params.q.as_ref().filter(|s| !s.is_empty());
        let budget_type = params.budget_type.filter(|&t| t != 0);
        let role = params.role.filter(|&r| r != 0);
        let sort_by = params.sort_by.as_ref().filter(|s| !s.is_empty());
        let sort_dir = params.sort_dir.as_ref().filter(|s| !s.is_empty());
        let page = params.page.unwrap_or(1).max(1);
        let page_size = params.page_size.unwrap_or(20).clamp(1, 100).max(1);

        let budget_type_db = budget_type.and_then(|t| {
            BudgetType::try_from(t)
                .ok()
                .filter(|&bt| bt != BudgetType::Unspecified)
        });
        let role_db = role.and_then(|r| {
            BudgetRole::try_from(r)
                .ok()
                .filter(|&br| br != BudgetRole::Unspecified)
        });

        let (rows, total) = self
            .repo
            .list_budgets_paged(
                org_id,
                user_id,
                q.map(String::as_str),
                budget_type_db.map(crate::converters::budget_type_to_db),
                role_db.map(crate::converters::budget_role_to_db),
                sort_by.map(String::as_str),
                sort_dir.map(String::as_str),
                page,
                page_size,
            )
            .await
            .map_err(Self::internal)?;

        let total_pages = if total == 0 {
            0
        } else {
            ((total + page_size as i64 - 1) / page_size as i64) as i32
        };

        let meta = PageMeta {
            page,
            page_size,
            total_pages,
            total_rows: total,
        };

        Ok((rows.into_iter().map(map_budget).collect(), meta))
    }

    pub async fn list_budgets_admin(
        &self,
        caller_id: &str,
        org_id: &str,
        budget_type: &str,
        name_search: &str,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<Budget>, i32), Status> {
        // Gateway has already verified super_admin via identity GetProfile.
        // We trust the x-user-id header from gateway.
        let _ = caller_id; // unused, gateway already verified

        let (rows, total) = self
            .repo
            .list_budgets_admin(org_id, budget_type, name_search, page, page_size)
            .await
            .map_err(Self::internal)?;
        Ok((rows.into_iter().map(map_budget).collect(), total))
    }

    // -----------------------------------------------------------------------
    // Role authority (internal — called by Entry, Category, Sharing services)
    // -----------------------------------------------------------------------

    /// 4-step fallback:
    /// 0. Super admin → full access (Owner).
    /// 1. Explicit budget_members row → use it directly.
    /// 2. is_private gate → if private and no explicit row, deny (Unspecified).
    /// 3. Org-role fallback → OrOwner → BudgetOwner, OrAdmin → BudgetManager, else Unspecified.
    async fn resolve_role(
        &self,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<BudgetRole, Status> {
        // Step 0: super admin bypass - check user_type header first (set by gateway)
        if user_type == Some("super_admin") {
            return Ok(BudgetRole::Owner);
        }

        // Step 1: explicit budget_members row
        if let Some(role) = self
            .repo
            .get_member_role(budget_id, user_id)
            .await
            .map_err(Self::internal)?
        {
            return Ok(role);
        }

        // Step 2: is_private gate
        let is_private = self
            .repo
            .get_budget_is_private(budget_id)
            .await
            .map_err(Self::internal)?;
        if is_private {
            return Ok(BudgetRole::Unspecified);
        }

        // Step 3: org-role fallback
        let org_id = self
            .repo
            .get_budget_org_id(budget_id)
            .await
            .map_err(Self::internal)?
            .ok_or_else(|| Status::not_found("Budget not found"))?;

        let mut client = self.identity_client.lock().await;
        let org_role = client.get_org_role(user_id, &org_id).await?;

        let budget_role = match org_role {
            OrgRole::OrOwner => BudgetRole::Owner,
            OrgRole::OrAdmin => BudgetRole::Manager,
            _ => BudgetRole::Unspecified,
        };
        Ok(budget_role)
    }

    pub async fn check_role(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<(BudgetRole, bool), Status> {
        let role = self.resolve_role(budget_id, user_id, user_type).await?;
        let is_member = role != BudgetRole::Unspecified;
        Ok((role, is_member))
    }

    // -----------------------------------------------------------------------
    // Members
    // -----------------------------------------------------------------------

    pub async fn add_member(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_id: &str,
        role: BudgetRole,
        user_type: Option<&str>,
        system_actor: bool,
    ) -> Result<BudgetMember, Status> {
        if !system_actor {
            self.assert_min_role(budget_id, caller_id, BudgetRole::Manager, user_type)
                .await?;
        }
        if role == BudgetRole::Owner {
            return Err(Status::invalid_argument(
                "Cannot directly assign Owner role; use transfer ownership",
            ));
        }
        let db = self
            .repo
            .add_member(budget_id, user_id, role)
            .await
            .map_err(Self::internal)?;
        Ok(map_budget_member(db))
    }

    pub async fn update_member_role(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_id: &str,
        role: BudgetRole,
        user_type: Option<&str>,
    ) -> Result<BudgetMember, Status> {
        self.assert_min_role(budget_id, caller_id, BudgetRole::Owner, user_type)
            .await?;
        if role == BudgetRole::Owner {
            return Err(Status::invalid_argument(
                "Cannot assign Owner role directly",
            ));
        }
        let db = self
            .repo
            .update_member_role(budget_id, user_id, role)
            .await
            .map_err(Self::internal)?;
        Ok(map_budget_member(db))
    }

    pub async fn remove_member(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        self.assert_min_role(budget_id, caller_id, BudgetRole::Manager, user_type)
            .await?;
        let target_role = self
            .repo
            .get_member_role(budget_id, user_id)
            .await
            .map_err(Self::internal)?;
        if target_role == Some(BudgetRole::Owner) {
            return Err(Status::permission_denied("Cannot remove the budget owner"));
        }
        self.repo
            .remove_member(budget_id, user_id)
            .await
            .map_err(Self::internal)
    }

    pub async fn list_members(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
        bearer: &str,
    ) -> Result<Vec<BudgetMember>, Status> {
        self.assert_member(budget_id, caller_id, user_type).await?;

        // The budget DB sits on a different MySQL host than identity's
        // `users` table, so the LEFT JOIN in `repo.list_members` returns
        // NULL `display_name`/`email` for everyone. Override with live
        // data fetched in one round-trip from identity service via
        // `list_org_users(org_id)`. Per-member `GetUser` would also
        // work but requires super-admin permission.
        let budget = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(Self::internal)?;

        let org_users = {
            let mut identity = self.identity_client.lock().await;
            let caller_is_super_admin = user_type == Some("super_admin");
            match identity
                .list_org_users(bearer, &budget.org_id, caller_is_super_admin)
                .await
            {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        "list_org_users failed for org {}: {} — falling back to DB-only rows",
                        budget.org_id,
                        e
                    );
                    Vec::new()
                }
            }
        };

        let rows = self
            .repo
            .list_members(budget_id)
            .await
            .map_err(Self::internal)?;

        let by_user: std::collections::HashMap<String, client::OrgUserInfo> = org_users
            .into_iter()
            .filter(|u| !u.user_id.is_empty())
            .map(|u| (u.user_id.clone(), u))
            .collect();

        Ok(rows
            .into_iter()
            .map(|row| {
                let enriched = by_user.get(&row.user_id).cloned();
                map_budget_member_with(row, enriched.as_ref())
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Envelope limits & burn-rate
    // -----------------------------------------------------------------------

    pub async fn set_envelope_limit(
        &self,
        caller_id: &str,
        budget_id: &str,
        monthly_limit: i64,
        user_type: Option<&str>,
    ) -> Result<EnvelopeLimit, Status> {
        self.assert_min_role(budget_id, caller_id, BudgetRole::Manager, user_type)
            .await?;
        self.repo
            .set_envelope_limit(budget_id, monthly_limit)
            .await
            .map_err(Self::internal)?;
        self.build_envelope(budget_id, monthly_limit).await
    }

    pub async fn get_burn_rate(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<EnvelopeLimit, Status> {
        self.assert_member(budget_id, caller_id, user_type).await?;
        let limit = self
            .repo
            .get_envelope_limit(budget_id)
            .await
            .map_err(Self::internal)?
            .unwrap_or(0);
        self.build_envelope(budget_id, limit).await
    }

    async fn build_envelope(
        &self,
        budget_id: &str,
        monthly_limit: i64,
    ) -> Result<EnvelopeLimit, Status> {
        // `entries` lives in the entry-service DB; budget's DB does not have
        // it. If the query errors (table missing, etc.), degrade to 0 spend
        // rather than failing the whole burn-rate request.
        let current_spend = self
            .repo
            .get_current_month_spend(budget_id)
            .await
            .map_err(Self::internal)
            .unwrap_or(0);

        let burn_rate_pct = if monthly_limit > 0 {
            let now = chrono::Utc::now();
            let (year, month, day) = (
                now.format("%Y").to_string().parse::<i32>().unwrap_or(2026),
                now.format("%m").to_string().parse::<i32>().unwrap_or(1),
                now.format("%d").to_string().parse::<f64>().unwrap_or(1.0),
            );
            let days_in_month = days_in_month(year, month) as f64;
            let projected = (current_spend as f64 / day) * days_in_month;
            (projected / monthly_limit as f64) * 100.0
        } else {
            0.0
        };

        Ok(EnvelopeLimit {
            budget_id: budget_id.to_string(),
            monthly_limit,
            current_spend,
            burn_rate_pct,
            limit_exceeded: monthly_limit > 0 && current_spend >= monthly_limit,
        })
    }

    // -----------------------------------------------------------------------
    // Rollover policy
    // -----------------------------------------------------------------------

    pub async fn set_rollover_policy(
        &self,
        caller_id: &str,
        budget_id: &str,
        policy: RolloverPolicy,
        user_type: Option<&str>,
    ) -> Result<RolloverPolicy, Status> {
        self.assert_min_role(budget_id, caller_id, BudgetRole::Manager, user_type)
            .await?;
        self.repo
            .set_rollover_policy(budget_id, policy)
            .await
            .map_err(Self::internal)?;
        Ok(policy)
    }

    pub async fn get_rollover_policy(
        &self,
        caller_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<RolloverPolicy, Status> {
        self.assert_member(budget_id, caller_id, user_type).await?;
        self.repo
            .get_rollover_policy(budget_id)
            .await
            .map_err(Self::internal)
    }

    // -----------------------------------------------------------------------
    // Templates
    // -----------------------------------------------------------------------

    pub async fn list_templates(&self) -> Result<Vec<BudgetTemplate>, Status> {
        let rows: Vec<DbTemplate> = self.repo.list_templates().await.map_err(Self::internal)?;
        Ok(rows
            .into_iter()
            .map(|t| BudgetTemplate {
                id: t.id,
                name: t.name,
                description: t.description.unwrap_or_default(),
                budget_type: crate::converters::budget_type_from_db(&t.budget_type) as i32,
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    async fn assert_member(
        &self,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let role = self.resolve_role(budget_id, user_id, user_type).await?;
        if role == BudgetRole::Unspecified {
            return Err(Status::permission_denied("Not a member of this budget"));
        }
        Ok(())
    }

    async fn assert_min_role(
        &self,
        budget_id: &str,
        user_id: &str,
        min_role: BudgetRole,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let role = self.resolve_role(budget_id, user_id, user_type).await?;

        let ok = match min_role {
            BudgetRole::Viewer => role != BudgetRole::Unspecified,
            BudgetRole::Contributor => matches!(
                role,
                BudgetRole::Owner | BudgetRole::Manager | BudgetRole::Contributor
            ),
            BudgetRole::Manager => matches!(role, BudgetRole::Owner | BudgetRole::Manager),
            BudgetRole::Owner => role == BudgetRole::Owner,
            BudgetRole::Unspecified => true,
        };

        if !ok {
            return Err(Status::permission_denied(format!(
                "Requires {:?} role or higher",
                min_role
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Invest assets
    // -----------------------------------------------------------------------

    pub async fn create_invest_asset(
        &self,
        user_id: &str,
        req: &crate::pb::service::budget::CreateInvestAssetRequest,
        user_type: Option<&str>,
    ) -> Result<crate::pb::service::budget::InvestAsset, Status> {
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let asset_type = crate::pb::service::budget::AssetType::try_from(req.asset_type)
            .unwrap_or(crate::pb::service::budget::AssetType::SavingsDeposit);
        let type_str = crate::converters::asset_type_to_db(asset_type);
        let db = self
            .repo
            .create_invest_asset(
                &req.budget_id,
                type_str,
                &req.name,
                user_id,
                req.principal,
                req.annual_rate,
                req.interest_type.as_deref(),
                req.start_date.as_deref(),
                req.maturity_date.as_deref(),
                req.bank_name.as_deref(),
                req.quantity,
                req.unit.as_deref(),
                req.cost_basis_per_unit,
                req.ticker.as_deref(),
                req.exchange.as_deref(),
                req.avg_cost_per_share,
                req.purchase_date.as_deref(),
                req.notes.as_deref(),
            )
            .await
            .map_err(Self::internal)?;
        let (cv, cb, pnl, pct, lu) = self.compute_asset_value(&db).await?;
        Ok(crate::converters::map_invest_asset(
            db, cv, cb, pnl, pct, lu,
        ))
    }

    pub async fn update_invest_asset(
        &self,
        user_id: &str,
        asset_id: &str,
        req: &crate::pb::service::budget::UpdateInvestAssetRequest,
        user_type: Option<&str>,
    ) -> Result<crate::pb::service::budget::InvestAsset, Status> {
        let db_existing = self
            .repo
            .get_invest_asset(asset_id)
            .await
            .map_err(Self::internal)?;
        self.assert_min_role(
            &db_existing.budget_id,
            user_id,
            BudgetRole::Manager,
            user_type,
        )
        .await?;
        let db = self
            .repo
            .update_invest_asset(
                asset_id,
                req.name.as_deref(),
                req.annual_rate,
                req.maturity_date.as_deref(),
                req.bank_name.as_deref(),
                req.quantity,
                req.unit.as_deref(),
                req.cost_basis_per_unit,
                req.avg_cost_per_share,
                req.notes.as_deref(),
            )
            .await
            .map_err(Self::internal)?;
        let (cv, cb, pnl, pct, lu) = self.compute_asset_value(&db).await?;
        Ok(crate::converters::map_invest_asset(
            db, cv, cb, pnl, pct, lu,
        ))
    }

    pub async fn delete_invest_asset(
        &self,
        user_id: &str,
        asset_id: &str,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let db = self
            .repo
            .get_invest_asset(asset_id)
            .await
            .map_err(Self::internal)?;
        self.assert_min_role(&db.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.repo
            .delete_invest_asset(asset_id)
            .await
            .map_err(Self::internal)
    }

    pub async fn list_invest_assets(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<Vec<crate::pb::service::budget::InvestAsset>, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let rows = self
            .repo
            .list_invest_assets(budget_id)
            .await
            .map_err(Self::internal)?;
        let mut assets = Vec::with_capacity(rows.len());
        for db in rows {
            let (cv, cb, pnl, pct, lu) = self.compute_asset_value(&db).await?;
            assets.push(crate::converters::map_invest_asset(
                db, cv, cb, pnl, pct, lu,
            ));
        }
        Ok(assets)
    }

    pub async fn get_invest_portfolio_summary(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<crate::pb::service::budget::InvestPortfolioSummary, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let rows = self
            .repo
            .list_invest_assets(budget_id)
            .await
            .map_err(Self::internal)?;
        let mut total_cv: i64 = 0;
        let mut total_cb: i64 = 0;
        let mut assets = Vec::with_capacity(rows.len());
        for db in rows {
            let (cv, cb, pnl, pct, lu) = self.compute_asset_value(&db).await?;
            total_cv += cv;
            total_cb += cb;
            assets.push(crate::converters::map_invest_asset(
                db, cv, cb, pnl, pct, lu,
            ));
        }
        let total_pnl = total_cv - total_cb;
        let total_pct = if total_cb > 0 {
            (total_pnl as f64 / total_cb as f64) * 100.0
        } else {
            0.0
        };
        Ok(crate::pb::service::budget::InvestPortfolioSummary {
            budget_id: budget_id.to_string(),
            total_current_value: total_cv,
            total_cost_basis: total_cb,
            total_unrealized_pnl: total_pnl,
            total_pnl_pct: total_pct,
            assets,
        })
    }

    pub async fn add_price_snapshot(
        &self,
        user_id: &str,
        asset_id: &str,
        price: i64,
        snapshot_date: &str,
        user_type: Option<&str>,
    ) -> Result<crate::pb::service::budget::PriceSnapshot, Status> {
        let db_asset = self
            .repo
            .get_invest_asset(asset_id)
            .await
            .map_err(Self::internal)?;
        self.assert_member(&db_asset.budget_id, user_id, user_type)
            .await?;
        let date = if snapshot_date.is_empty() {
            chrono::Utc::now().format("%Y-%m-%d").to_string()
        } else {
            snapshot_date.to_string()
        };
        let db = self
            .repo
            .add_price_snapshot(asset_id, price, &date, "manual")
            .await
            .map_err(Self::internal)?;
        Ok(crate::converters::map_price_snapshot(db))
    }

    pub async fn get_latest_price_snapshot(
        &self,
        user_id: &str,
        asset_id: &str,
        user_type: Option<&str>,
    ) -> Result<crate::pb::service::budget::PriceSnapshot, Status> {
        let db_asset = self
            .repo
            .get_invest_asset(asset_id)
            .await
            .map_err(Self::internal)?;
        self.assert_member(&db_asset.budget_id, user_id, user_type)
            .await?;
        let db = self
            .repo
            .get_latest_price_snapshot(asset_id)
            .await
            .map_err(Self::internal)?
            .ok_or_else(|| Status::not_found("No price snapshot found"))?;
        Ok(crate::converters::map_price_snapshot(db))
    }

    pub async fn list_price_snapshots(
        &self,
        user_id: &str,
        asset_id: &str,
        limit: i32,
        user_type: Option<&str>,
    ) -> Result<Vec<crate::pb::service::budget::PriceSnapshot>, Status> {
        let db_asset = self
            .repo
            .get_invest_asset(asset_id)
            .await
            .map_err(Self::internal)?;
        self.assert_member(&db_asset.budget_id, user_id, user_type)
            .await?;
        let rows = self
            .repo
            .list_price_snapshots(asset_id, limit.clamp(1, 365))
            .await
            .map_err(Self::internal)?;
        Ok(rows
            .into_iter()
            .map(crate::converters::map_price_snapshot)
            .collect())
    }

    async fn compute_asset_value(
        &self,
        db: &crate::converters::DbInvestAsset,
    ) -> Result<(i64, i64, i64, f64, String), Status> {
        match db.asset_type.as_str() {
            "savings_deposit" => {
                let principal = db.principal.unwrap_or(0);
                let annual_rate = db.annual_rate.unwrap_or(0.0);
                let interest_type = db.interest_type.as_deref().unwrap_or("simple");
                let start = db.start_date.as_deref().unwrap_or("");
                let today = chrono::Utc::now().date_naive();
                let days_elapsed = if let Ok(sd) = start.parse::<chrono::NaiveDate>() {
                    (today - sd).num_days().max(0) as f64
                } else {
                    0.0
                };
                let daily_rate = annual_rate / 365.0;
                let accrued = if interest_type == "compound" {
                    (principal as f64 * (1.0 + daily_rate).powf(days_elapsed)).round() as i64
                } else {
                    (principal as f64 + principal as f64 * daily_rate * days_elapsed).round() as i64
                };
                let pnl = accrued - principal;
                let pct = if principal > 0 {
                    (pnl as f64 / principal as f64) * 100.0
                } else {
                    0.0
                };
                Ok((accrued, principal, pnl, pct, today.to_string()))
            }
            "gold" | "stock" => {
                let snapshot = self
                    .repo
                    .get_latest_price_snapshot(&db.id)
                    .await
                    .map_err(Self::internal)?;
                let qty = db.quantity.unwrap_or(0.0);
                let (qty, cb_per_unit) = if db.asset_type == "gold" {
                    (qty, db.cost_basis_per_unit.unwrap_or(0))
                } else {
                    (qty, db.avg_cost_per_share.unwrap_or(0))
                };
                let cost_basis = (qty * cb_per_unit as f64).round() as i64;
                let (current_value, last_updated) = if let Some(snap) = snapshot {
                    let cv = (qty * snap.price as f64).round() as i64;
                    (cv, snap.snapshot_date)
                } else {
                    (cost_basis, String::new())
                };
                let pnl = current_value - cost_basis;
                let pct = if cost_basis > 0 {
                    (pnl as f64 / cost_basis as f64) * 100.0
                } else {
                    0.0
                };
                Ok((current_value, cost_basis, pnl, pct, last_updated))
            }
            _ => Ok((0, 0, 0, 0.0, String::new())),
        }
    }
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
