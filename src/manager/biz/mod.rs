#![allow(clippy::result_large_err)]
use philand_configs::BudgetServiceConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::Status;

use crate::converters::{map_budget, map_budget_member, map_budget_member_with, DbTemplate};
use crate::manager::client::{self, CategoryClient, IdentityClient};
use crate::manager::repository::BudgetRepository;
use crate::pb::service::budget::{
    Budget, BudgetMember, BudgetRole, BudgetTemplate, BudgetType, EnvelopeLimit, ListBudgetParams,
    PageMeta, RolloverPolicy,
};
use crate::pb::shared::organization::OrgRole;

pub mod envelope;

/// `[first day of this month, first day of next month)` as YYYY-MM-DD strings (UTC).
fn month_window(now: chrono::DateTime<chrono::Utc>) -> (String, String) {
    use chrono::Datelike;
    let first = chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), 1).expect("valid month start");
    let next = if now.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
    }
    .expect("valid next month");
    (first.format("%Y-%m-%d").to_string(), next.format("%Y-%m-%d").to_string())
}
pub mod portfolio;
pub mod seed;

pub struct BudgetBiz {
    pub repo: Arc<BudgetRepository>,
    pub config: BudgetServiceConfig,
    pub identity_client: Arc<Mutex<IdentityClient>>,
    pub category_client: Option<Arc<Mutex<CategoryClient>>>,
    entry_client: Option<Arc<Mutex<crate::manager::client::EntryClient>>>,
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
            category_client: None,
            entry_client: None,
        }
    }

    /// Lets budget reads fill `current_spend` from the Entry service (no direct reads of `entries`).
    pub fn with_entry_client(mut self, client: crate::manager::client::EntryClient) -> Self {
        self.entry_client = Some(Arc::new(Mutex::new(client)));
        self
    }

    /// Fills `current_spend` (expenses in the current calendar month) for the given rows.
    /// Best effort: if Entry is unreachable the spend stays 0 and the read still succeeds.
    async fn fill_spend(&self, caller: &str, rows: &mut [crate::converters::DbBudget]) {
        let Some(client) = &self.entry_client else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        let now = chrono::Utc::now();
        let (from, to) = month_window(now);
        let ids = rows.iter().map(|r| r.id.clone()).collect();
        match client.lock().await.expense_by_budget(caller, ids, &from, &to).await {
            Ok(map) => {
                for r in rows.iter_mut() {
                    r.current_spend = Some(map.get(&r.id).copied().unwrap_or(0));
                }
            }
            Err(e) => tracing::warn!(error = %e, "entry spend lookup failed; current_spend left at 0"),
        }
    }

    /// Enables seeding default categories on budget creation.
    pub fn with_category_client(mut self, client: CategoryClient) -> Self {
        self.category_client = Some(Arc::new(Mutex::new(client)));
        self
    }

    /// Seeds template/default categories for a freshly created budget. Best effort: a failure
    /// is logged and the budget is kept, so the user can retry from an empty state in the UI.
    pub async fn seed_new_budget(
        &self,
        user_id: &str,
        budget: &Budget,
        template_id: Option<&str>,
        locale: &str,
        expected_income: i64,
    ) {
        let Some(client) = &self.category_client else {
            return;
        };
        let budget_id = budget.base.as_ref().map(|b| b.id.clone()).unwrap_or_default();
        let budget_type = BudgetType::try_from(budget.budget_type).unwrap_or(BudgetType::Standard);
        let items = seed::seed_items(
            template_id,
            budget_type,
            seed::Locale::parse(locale),
            expected_income,
        );
        if items.is_empty() {
            return;
        }
        if let Err(e) = client
            .lock()
            .await
            .seed_categories(user_id, &budget_id, items)
            .await
        {
            tracing::warn!(%budget_id, error = %e, "seeding default categories failed");
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
            category_client: None,
            entry_client: None,
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
        let currency = crate::manager::validate::sharing_currency(budget_type, currency)?;
        let db = self
            .repo
            .create_budget(org_id, name, budget_type, &currency, user_id)
            .await
            .map_err(Self::internal)?;
        Ok(map_budget(db))
    }

    /// Archive / reopen a sharing budget. Owner or Manager only.
    pub async fn set_budget_status(
        &self,
        user_id: &str,
        budget_id: &str,
        status: &str,
    ) -> Result<Budget, Status> {
        let status = crate::manager::validate::settable_status(status)?;
        let (role, is_member) = self.check_role(user_id, budget_id, None).await?;
        if !is_member || !matches!(role, BudgetRole::Owner | BudgetRole::Manager) {
            return Err(Status::permission_denied(
                "Only an owner or manager can archive or reopen a budget",
            ));
        }
        let db = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        if crate::converters::budget_type_from_db(&db.budget_type) != BudgetType::Sharing {
            return Err(Status::failed_precondition(
                "Only sharing budgets can be archived",
            ));
        }
        if db.status != "active" && db.status != "archived" {
            return Err(Status::failed_precondition("Budget is not open"));
        }
        self.repo
            .set_budget_status(budget_id, status)
            .await
            .map_err(Self::internal)?;
        self.get_budget_admin(budget_id).await
    }

    /// Applies validated period settings to a budget and returns the refreshed budget.
    pub async fn apply_period(
        &self,
        user_id: &str,
        budget_id: &str,
        start_day: Option<i32>,
        timezone: Option<&str>,
    ) -> Result<(), Status> {
        let (day, tz) = crate::manager::validate::period_settings(start_day, timezone)?;
        if day.is_none() && tz.is_none() {
            return Ok(());
        }
        let _ = user_id;
        self.repo
            .set_budget_period(budget_id, day, tz.as_deref())
            .await
            .map_err(Self::internal)
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
        let mut db = self
            .repo
            .get_budget_for_user(budget_id, user_id, Some(role_label))
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        self.fill_spend(user_id, std::slice::from_mut(&mut db)).await;
        Ok(map_budget(db))
    }

    pub async fn get_budget_admin(&self, budget_id: &str) -> Result<Budget, Status> {
        let mut db = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        self.fill_spend("system", std::slice::from_mut(&mut db)).await;
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
        let current = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        let budget_type = crate::manager::validate::resolve_budget_type(
            crate::converters::budget_type_from_db(&current.budget_type),
            budget_type,
        )?;
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
        let _db = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        self.repo
            .force_close_budget(budget_id)
            .await
            .map_err(Self::internal)?;
        // Re-fetch to return updated budget
        let db = self
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("Budget not found"))?;
        Ok(map_budget(db))
    }

    pub async fn list_budgets(&self, user_id: &str, org_id: &str) -> Result<Vec<Budget>, Status> {
        let mut rows = self
            .repo
            .list_budgets_for_user(org_id, user_id)
            .await
            .map_err(Self::internal)?;
        self.fill_spend(user_id, &mut rows).await;
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

        let (mut rows, total) = self
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

        self.fill_spend(user_id, &mut rows).await;

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

        let (mut rows, total) = self
            .repo
            .list_budgets_admin(org_id, budget_type, name_search, page, page_size)
            .await
            .map_err(Self::internal)?;
        self.fill_spend(caller_id, &mut rows).await;
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
        // Follow the budget's own period (start day + timezone). Spend comes from the Entry service;
        // if that fails, degrade to 0 spend rather than failing the request.
        let now = chrono::Utc::now();
        let (start_day, tz) = match self.repo.get_budget_by_id(budget_id).await {
            Ok(b) => (
                b.period_start_day.unwrap_or(1).clamp(1, 28) as u8,
                b.timezone
                    .unwrap_or_else(|| crate::manager::validate::DEFAULT_TIMEZONE.to_string()),
            ),
            Err(_) => (1, crate::manager::validate::DEFAULT_TIMEZONE.to_string()),
        };
        let period = envelope::current_period(now, start_day, &tz);
        let current_spend = match &self.entry_client {
            Some(client) => client
                .lock()
                .await
                .expense_by_budget(
                    "system",
                    vec![budget_id.to_string()],
                    &philand_time::period::ymd(period.0),
                    &philand_time::period::ymd(period.1),
                )
                .await
                .map(|m| m.get(budget_id).copied().unwrap_or(0))
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "entry spend lookup failed; using 0");
                    0
                }),
            None => 0,
        };

        let burn_rate_pct = envelope::burn_rate_pct(
            current_spend,
            monthly_limit,
            envelope::local_today(now, &tz),
            period,
        );

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


#[cfg(test)]
mod month_window_tests {
    use super::month_window;
    use chrono::TimeZone;

    #[test]
    fn mid_month() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 10, 23, 8, 0, 0).unwrap();
        assert_eq!(month_window(now), ("2026-10-01".into(), "2026-11-01".into()));
    }

    #[test]
    fn december_rolls_into_next_year() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 12, 31, 23, 0, 0).unwrap();
        assert_eq!(month_window(now), ("2026-12-01".into(), "2027-01-01".into()));
    }
}
