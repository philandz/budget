use anyhow::Result;
use philand_configs::BudgetServiceConfig;
use philand_time::now_unix;
use sqlx::MySqlPool;

use crate::converters::{
    budget_role_from_db, budget_role_to_db, budget_type_to_db, rollover_policy_from_db,
    rollover_policy_to_db, DbBudget, DbBudgetMember, DbTemplate,
};
use crate::pb::service::budget::{BudgetRole, BudgetType, RolloverPolicy};

pub struct BudgetRepository {
    pool: MySqlPool,
}

impl BudgetRepository {
    /// Test-only constructor. Builds a lazy MySQL pool pointing at whatever
    /// DATABASE_URL specifies (falling back to a local default) without
    /// actually opening a connection. Safe to use in tests that exercise
    /// code paths that never query the DB (e.g. `resolve_role` Step 0
    /// bypass); unsafe in tests that do query, because every query will
    /// fail at execution time.
    #[doc(hidden)]
    pub async fn test_only_default_pool() -> std::sync::Arc<Self> {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "mysql://root@127.0.0.1:3306/philand".to_string());
        let pool = sqlx::MySqlPool::connect_lazy(&database_url).expect("invalid DATABASE_URL");
        std::sync::Arc::new(Self { pool })
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl BudgetRepository {
    pub async fn new(config: &BudgetServiceConfig) -> Result<Self> {
        let pool = sqlx::MySqlPool::connect(&config.database_url).await?;
        let mut migrator =
            sqlx::migrate::Migrator::new(std::path::Path::new("./migrations")).await?;
        migrator.set_ignore_missing(true);

        if let Err(e) = migrator.run(&pool).await {
            let err_str = format!("{}", e);
            if err_str.contains("partially applied") {
                tracing::warn!("Partial migration detected: {}", e);
                if err_str.contains("20260507090228") {
                    let has_avatar: bool = sqlx::query_scalar(
                        "SELECT COUNT(*) > 0 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'users' AND COLUMN_NAME = 'avatar'"
                    )
                    .fetch_one(&pool)
                    .await.unwrap_or(false);
                    if has_avatar {
                        sqlx::query(
                            "INSERT IGNORE INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time) VALUES (20260507090228, 'add_avatar_to_users', NOW(), true, 0x00, 0)"
                        )
                        .execute(&pool)
                        .await.ok();
                    }
                }
                sqlx::query("DELETE FROM _sqlx_migrations WHERE success = false")
                    .execute(&pool)
                    .await
                    .ok();
            } else {
                return Err(anyhow::anyhow!("{}", e));
            }
        }
        Ok(Self { pool })
    }

    // -----------------------------------------------------------------------
    // Budget CRUD
    // -----------------------------------------------------------------------

    pub async fn create_budget(
        &self,
        org_id: &str,
        name: &str,
        budget_type: BudgetType,
        currency: &str,
        created_by: &str,
    ) -> Result<DbBudget> {
        let id = new_id();
        let now = now_unix();
        let type_str = budget_type_to_db(budget_type);

        sqlx::query(
            "INSERT INTO budgets (id, org_id, owner_id, name, budget_type, currency, status, created_by, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 'active', ?, ?, ?)"
        )
        .bind(&id).bind(org_id).bind(created_by).bind(name).bind(type_str).bind(currency)
        .bind(created_by).bind(now).bind(now)
        .execute(&self.pool)
        .await?;

        // Add creator as owner
        let member_id = new_id();
        sqlx::query(
            "INSERT INTO budget_members (id, budget_id, user_id, role, created_at, updated_at)
             VALUES (?, ?, ?, 'owner', ?, ?)",
        )
        .bind(&member_id)
        .bind(&id)
        .bind(created_by)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        self.get_budget_for_user(&id, created_by, None).await
    }

    pub async fn get_budget_for_user(
        &self,
        budget_id: &str,
        user_id: &str,
        override_role: Option<&str>,
    ) -> Result<DbBudget> {
        let now = chrono::Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let start_of_month = format!("{year}-{month}-01");

        let fallback_role = override_role.unwrap_or("");
        let row = sqlx::query_as::<_, DbBudget>(
            r#"SELECT b.id, b.org_id, b.name, b.budget_type, b.currency, b.status,
                    b.created_by, b.created_at, b.updated_at, b.deleted_at,
                    CASE WHEN bm.role IS NOT NULL THEN bm.role ELSE ? END AS my_role,
                    el.monthly_limit AS envelope_limit,
                    COALESCE(mc.member_count, 0) AS member_count,
                    COALESCE(cs.current_spend, 0) AS current_spend
               FROM budgets b
               LEFT JOIN budget_members bm ON bm.budget_id = b.id AND bm.user_id = ?
               LEFT JOIN budget_envelope_limits el ON el.budget_id = b.id
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, COUNT(*) AS member_count
                   FROM budget_members
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) mc ON mc.budget_id = b.id
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) AS current_spend
                   FROM entries
                   WHERE kind = 'expense'
                     AND entry_date >= ?
                     AND entry_date < DATE_ADD(?, INTERVAL 1 MONTH)
                     AND deleted_at IS NULL
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) cs ON cs.budget_id = b.id
               WHERE b.id = ? AND b.deleted_at IS NULL"#,
        )
        .bind(fallback_role)
        .bind(user_id)
        .bind(&start_of_month)
        .bind(&start_of_month)
        .bind(budget_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_budget_by_id(&self, budget_id: &str) -> Result<DbBudget> {
        let now = chrono::Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let start_of_month = format!("{}-{:02}-01", year, month);

        let row = sqlx::query_as::<_, DbBudget>(
            r#"SELECT b.id, b.org_id, b.name, b.budget_type, b.currency, b.status,
                    b.created_by, b.created_at, b.updated_at, b.deleted_at,
                    '' AS my_role,
                    el.monthly_limit AS envelope_limit,
                    COALESCE(mc.member_count, 0) AS member_count,
                    COALESCE(cs.current_spend, 0) AS current_spend
               FROM budgets b
               LEFT JOIN budget_envelope_limits el ON el.budget_id = b.id
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, COUNT(*) AS member_count
                   FROM budget_members
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) mc ON mc.budget_id = b.id
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) AS current_spend
                   FROM entries
                   WHERE kind = 'expense'
                     AND entry_date >= ?
                     AND entry_date < DATE_ADD(?, INTERVAL 1 MONTH)
                     AND deleted_at IS NULL
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) cs ON cs.budget_id = b.id
               WHERE b.id = ? AND b.deleted_at IS NULL"#,
        )
        .bind(&start_of_month)
        .bind(&start_of_month)
        .bind(budget_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn update_budget(
        &self,
        budget_id: &str,
        name: &str,
        budget_type: BudgetType,
        updated_by: &str,
    ) -> Result<DbBudget> {
        let now = now_unix();
        let type_str = budget_type_to_db(budget_type);
        sqlx::query(
            "UPDATE budgets SET name = ?, budget_type = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL"
        )
        .bind(name).bind(type_str).bind(now).bind(budget_id)
        .execute(&self.pool)
        .await?;
        self.get_budget_for_user(budget_id, updated_by, None).await
    }

    pub async fn delete_budget(&self, budget_id: &str) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            "UPDATE budgets SET deleted_at = ?, status = 'deleted', updated_at = ? WHERE id = ?",
        )
        .bind(now)
        .bind(now)
        .bind(budget_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_budgets_for_user(
        &self,
        org_id: &str,
        user_id: &str,
    ) -> Result<Vec<DbBudget>> {
        let now = chrono::Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let start_of_month = format!("{}-{:02}-01", year, month);

        let rows = sqlx::query_as::<_, DbBudget>(
            r#"SELECT
                  b.id, b.org_id, b.name, b.budget_type, b.currency, b.status,
                  b.created_by, b.created_at, b.updated_at, b.deleted_at,
                  bm.role AS my_role,
                  el.monthly_limit AS envelope_limit,
                  COALESCE(mc.member_count, 0) AS member_count,
                  COALESCE(cs.current_spend, 0) AS current_spend
               FROM budgets b
               INNER JOIN budget_members bm ON bm.budget_id COLLATE utf8mb4_0900_ai_ci = b.id COLLATE utf8mb4_0900_ai_ci AND bm.user_id COLLATE utf8mb4_0900_ai_ci = ? COLLATE utf8mb4_0900_ai_ci
               LEFT JOIN budget_envelope_limits el ON el.budget_id COLLATE utf8mb4_0900_ai_ci = b.id COLLATE utf8mb4_0900_ai_ci
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, COUNT(*) AS member_count
                   FROM budget_members
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) mc ON mc.budget_id = b.id COLLATE utf8mb4_0900_ai_ci
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) AS current_spend
                   FROM entries
                   WHERE kind = 'expense'
                     AND entry_date >= ?
                     AND entry_date < DATE_ADD(?, INTERVAL 1 MONTH)
                     AND deleted_at IS NULL
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) cs ON cs.budget_id = b.id COLLATE utf8mb4_0900_ai_ci
               WHERE b.org_id = ? AND b.deleted_at IS NULL
               ORDER BY b.created_at ASC"#,
        )
        .bind(user_id)
        .bind(&start_of_month)
        .bind(&start_of_month)
        .bind(org_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn list_budgets_admin(
        &self,
        org_id: &str,
        budget_type: &str,
        name_search: &str,
        page: i32,
        page_size: i32,
    ) -> Result<(Vec<DbBudget>, i32)> {
        let now = chrono::Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let start_of_month = format!("{}-{:02}-01", year, month);

        let offset = (page - 1) * page_size;

        let base_where = if org_id.is_empty() {
            String::new()
        } else {
            format!(" AND b.org_id = '{}'", org_id)
        };

        let type_filter = if budget_type.is_empty() {
            String::new()
        } else {
            format!(" AND b.budget_type = '{}'", budget_type)
        };

        let search_filter = if name_search.is_empty() {
            String::new()
        } else {
            format!(" AND b.name LIKE '%{}%'", name_search)
        };

        let count_query = format!(
            "SELECT COUNT(*) FROM budgets b WHERE b.deleted_at IS NULL{}{}{}",
            base_where, type_filter, search_filter
        );
        let total: i32 = sqlx::query_scalar(&count_query)
            .fetch_one(&self.pool)
            .await?;

        let query = format!(
            r#"SELECT
                  b.id, b.org_id, b.name, b.budget_type, b.currency, b.status,
                  b.created_by, b.created_at, b.updated_at, b.deleted_at,
                  bm.role AS my_role,
                  el.monthly_limit AS envelope_limit,
                  COALESCE(mc.member_count, 0) AS member_count,
                  COALESCE(cs.current_spend, 0) AS current_spend
               FROM budgets b
               LEFT JOIN budget_members bm ON bm.budget_id COLLATE utf8mb4_0900_ai_ci = b.id COLLATE utf8mb4_0900_ai_ci AND bm.user_id = ''
               LEFT JOIN budget_envelope_limits el ON el.budget_id COLLATE utf8mb4_0900_ai_ci = b.id COLLATE utf8mb4_0900_ai_ci
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, COUNT(*) AS member_count
                   FROM budget_members
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) mc ON mc.budget_id = b.id COLLATE utf8mb4_0900_ai_ci
               LEFT JOIN (
                   SELECT budget_id COLLATE utf8mb4_0900_ai_ci AS budget_id, CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) AS current_spend
                   FROM entries
                   WHERE kind = 'expense'
                     AND entry_date >= ?
                     AND entry_date < DATE_ADD(?, INTERVAL 1 MONTH)
                     AND deleted_at IS NULL
                   GROUP BY budget_id COLLATE utf8mb4_0900_ai_ci
               ) cs ON cs.budget_id = b.id COLLATE utf8mb4_0900_ai_ci
               WHERE b.deleted_at IS NULL{}{}{}
               ORDER BY b.created_at DESC
               LIMIT ? OFFSET ?"#,
            base_where, type_filter, search_filter
        );

        let rows = sqlx::query_as::<_, DbBudget>(&query)
            .bind(&start_of_month)
            .bind(&start_of_month)
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok((rows, total))
    }

    // -----------------------------------------------------------------------
    // Role authority
    // -----------------------------------------------------------------------

    pub async fn get_budget_org_id(&self, budget_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT org_id FROM budgets WHERE id = ? AND deleted_at IS NULL")
                .bind(budget_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v))
    }

    pub async fn get_budget_is_private(&self, budget_id: &str) -> Result<bool> {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT is_private FROM budgets WHERE id = ? AND deleted_at IS NULL")
                .bind(budget_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v).unwrap_or(false))
    }

    pub async fn get_member_role(
        &self,
        budget_id: &str,
        user_id: &str,
    ) -> Result<Option<BudgetRole>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT role FROM budget_members WHERE budget_id = ? AND user_id = ?")
                .bind(budget_id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(r,)| budget_role_from_db(&r)))
    }

    // -----------------------------------------------------------------------
    // Members
    // -----------------------------------------------------------------------

    pub async fn add_member(
        &self,
        budget_id: &str,
        user_id: &str,
        role: BudgetRole,
    ) -> Result<DbBudgetMember> {
        let id = new_id();
        let now = now_unix();
        let role_str = budget_role_to_db(role);
        sqlx::query(
            "INSERT INTO budget_members (id, budget_id, user_id, role, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON DUPLICATE KEY UPDATE role = VALUES(role), updated_at = VALUES(updated_at)",
        )
        .bind(&id)
        .bind(budget_id)
        .bind(user_id)
        .bind(role_str)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get_member(budget_id, user_id).await
    }

    pub async fn update_member_role(
        &self,
        budget_id: &str,
        user_id: &str,
        role: BudgetRole,
    ) -> Result<DbBudgetMember> {
        let now = now_unix();
        let role_str = budget_role_to_db(role);
        sqlx::query(
            "UPDATE budget_members SET role = ?, updated_at = ? WHERE budget_id = ? AND user_id = ?"
        )
        .bind(role_str).bind(now).bind(budget_id).bind(user_id)
        .execute(&self.pool)
        .await?;
        self.get_member(budget_id, user_id).await
    }

    pub async fn remove_member(&self, budget_id: &str, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM budget_members WHERE budget_id = ? AND user_id = ?")
            .bind(budget_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_members(&self, budget_id: &str) -> Result<Vec<DbBudgetMember>> {
        let rows = sqlx::query_as::<_, DbBudgetMember>(
            "SELECT bm.budget_id, bm.user_id, bm.role,
                    u.display_name, u.email, u.avatar
             FROM budget_members bm
             LEFT JOIN users u ON u.id COLLATE utf8mb4_0900_ai_ci = bm.user_id COLLATE utf8mb4_0900_ai_ci
             WHERE bm.budget_id = ?
             ORDER BY bm.created_at ASC",
        )
        .bind(budget_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn get_member(&self, budget_id: &str, user_id: &str) -> Result<DbBudgetMember> {
        let row = sqlx::query_as::<_, DbBudgetMember>(
            "SELECT bm.budget_id, bm.user_id, bm.role,
                    u.display_name, u.email, u.avatar
             FROM budget_members bm
             LEFT JOIN users u ON u.id COLLATE utf8mb4_0900_ai_ci = bm.user_id COLLATE utf8mb4_0900_ai_ci
             WHERE bm.budget_id = ? AND bm.user_id = ?",
        )
        .bind(budget_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    // -----------------------------------------------------------------------
    // Envelope limits
    // -----------------------------------------------------------------------

    pub async fn set_envelope_limit(&self, budget_id: &str, monthly_limit: i64) -> Result<()> {
        if monthly_limit == 0 {
            sqlx::query("DELETE FROM budget_envelope_limits WHERE budget_id = ?")
                .bind(budget_id)
                .execute(&self.pool)
                .await?;
        } else {
            let id = new_id();
            let now = now_unix();
            sqlx::query(
                "INSERT INTO budget_envelope_limits (id, budget_id, monthly_limit, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON DUPLICATE KEY UPDATE monthly_limit = VALUES(monthly_limit), updated_at = VALUES(updated_at)"
            )
            .bind(&id).bind(budget_id).bind(monthly_limit).bind(now).bind(now)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn get_envelope_limit(&self, budget_id: &str) -> Result<Option<i64>> {
        let row = sqlx::query(
            "SELECT CAST(CAST(monthly_limit AS CHAR) AS UNSIGNED) FROM budget_envelope_limits WHERE budget_id = ?"
        )
        .bind(budget_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => {
                let val: String = sqlx::Row::get(&r, 0);
                Ok(Some(val.parse::<i64>().unwrap_or(0)))
            }
            None => Ok(None),
        }
    }

    /// Sum of expense entries in the current calendar month (shared DB with Entry service).
    pub async fn get_current_month_spend(&self, budget_id: &str) -> Result<i64> {
        let now = chrono::Utc::now();
        let year = now.format("%Y").to_string();
        let month = now.format("%m").to_string();
        let start_of_month = format!("{}-{:02}-01", year, month);
        let row = sqlx::query(
            "SELECT CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) FROM entries
             WHERE budget_id = ? AND kind = 'expense'
               AND entry_date >= ?
               AND entry_date < DATE_ADD(?, INTERVAL 1 MONTH)
               AND deleted_at IS NULL",
        )
        .bind(budget_id)
        .bind(&start_of_month)
        .bind(&start_of_month)
        .fetch_one(&self.pool)
        .await?;
        let val: i64 = sqlx::Row::get(&row, 0);
        Ok(val)
    }

    // -----------------------------------------------------------------------
    // Rollover policy
    // -----------------------------------------------------------------------

    pub async fn set_rollover_policy(&self, budget_id: &str, policy: RolloverPolicy) -> Result<()> {
        let id = new_id();
        let now = now_unix();
        let p = rollover_policy_to_db(policy);
        sqlx::query(
            "INSERT INTO budget_rollover_policies (id, budget_id, policy, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON DUPLICATE KEY UPDATE policy = VALUES(policy), updated_at = VALUES(updated_at)",
        )
        .bind(&id)
        .bind(budget_id)
        .bind(p)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_rollover_policy(&self, budget_id: &str) -> Result<RolloverPolicy> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT policy FROM budget_rollover_policies WHERE budget_id = ?")
                .bind(budget_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row
            .map(|(p,)| rollover_policy_from_db(&p))
            .unwrap_or(RolloverPolicy::Reset))
    }

    // -----------------------------------------------------------------------
    // Templates
    // -----------------------------------------------------------------------

    pub async fn list_templates(&self) -> Result<Vec<DbTemplate>> {
        let rows = sqlx::query_as::<_, DbTemplate>(
            "SELECT id, name, description, budget_type FROM budget_templates ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Invest assets
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_invest_asset(
        &self,
        budget_id: &str,
        asset_type: &str,
        name: &str,
        created_by: &str,
        principal: Option<i64>,
        annual_rate: Option<f64>,
        interest_type: Option<&str>,
        start_date: Option<&str>,
        maturity_date: Option<&str>,
        bank_name: Option<&str>,
        quantity: Option<f64>,
        unit: Option<&str>,
        cost_basis_per_unit: Option<i64>,
        ticker: Option<&str>,
        exchange: Option<&str>,
        avg_cost_per_share: Option<i64>,
        purchase_date: Option<&str>,
        notes: Option<&str>,
    ) -> Result<crate::converters::DbInvestAsset> {
        let id = new_id();
        let now = now_unix();
        sqlx::query(
            "INSERT INTO invest_assets (id, budget_id, asset_type, name, status, created_by, created_at, updated_at,
             principal, annual_rate, interest_type, start_date, maturity_date, bank_name,
             quantity, unit, cost_basis_per_unit, ticker, exchange, avg_cost_per_share,
             purchase_date, notes)
             VALUES (?, ?, ?, ?, 'active', ?, ?, ?,
             ?, ?, ?, ?, ?, ?,
             ?, ?, ?, ?, ?, ?,
             ?, ?)"
        )
        .bind(&id).bind(budget_id).bind(asset_type).bind(name).bind(created_by).bind(now).bind(now)
        .bind(principal).bind(annual_rate).bind(interest_type).bind(start_date).bind(maturity_date).bind(bank_name)
        .bind(quantity).bind(unit).bind(cost_basis_per_unit).bind(ticker).bind(exchange).bind(avg_cost_per_share)
        .bind(purchase_date).bind(notes)
        .execute(&self.pool).await?;
        self.get_invest_asset(&id).await
    }

    pub async fn get_invest_asset(
        &self,
        asset_id: &str,
    ) -> Result<crate::converters::DbInvestAsset> {
        let row = sqlx::query_as::<_, crate::converters::DbInvestAsset>(
            "SELECT id, budget_id, asset_type, name, status,
                    principal, annual_rate, interest_type, start_date, maturity_date, bank_name,
                    quantity, unit, cost_basis_per_unit, ticker, exchange, avg_cost_per_share,
                    purchase_date, notes, created_by, created_at, updated_at
             FROM invest_assets WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(asset_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_invest_assets(
        &self,
        budget_id: &str,
    ) -> Result<Vec<crate::converters::DbInvestAsset>> {
        let rows = sqlx::query_as::<_, crate::converters::DbInvestAsset>(
            "SELECT id, budget_id, asset_type, name, status,
                    principal, annual_rate, interest_type, start_date, maturity_date, bank_name,
                    quantity, unit, cost_basis_per_unit, ticker, exchange, avg_cost_per_share,
                    purchase_date, notes, created_by, created_at, updated_at
             FROM invest_assets WHERE budget_id = ? AND deleted_at IS NULL
             ORDER BY created_at ASC",
        )
        .bind(budget_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_invest_asset(
        &self,
        asset_id: &str,
        name: Option<&str>,
        annual_rate: Option<f64>,
        maturity_date: Option<&str>,
        bank_name: Option<&str>,
        quantity: Option<f64>,
        unit: Option<&str>,
        cost_basis_per_unit: Option<i64>,
        avg_cost_per_share: Option<i64>,
        notes: Option<&str>,
    ) -> Result<crate::converters::DbInvestAsset> {
        let now = now_unix();
        let mut parts: Vec<String> = vec!["updated_at = ?".to_string()];
        if name.is_some() {
            parts.push("name = ?".to_string());
        }
        if annual_rate.is_some() {
            parts.push("annual_rate = ?".to_string());
        }
        if maturity_date.is_some() {
            parts.push("maturity_date = ?".to_string());
        }
        if bank_name.is_some() {
            parts.push("bank_name = ?".to_string());
        }
        if quantity.is_some() {
            parts.push("quantity = ?".to_string());
        }
        if unit.is_some() {
            parts.push("unit = ?".to_string());
        }
        if cost_basis_per_unit.is_some() {
            parts.push("cost_basis_per_unit = ?".to_string());
        }
        if avg_cost_per_share.is_some() {
            parts.push("avg_cost_per_share = ?".to_string());
        }
        if notes.is_some() {
            parts.push("notes = ?".to_string());
        }
        let sql = format!(
            "UPDATE invest_assets SET {} WHERE id = ? AND deleted_at IS NULL",
            parts.join(", ")
        );
        let mut q = sqlx::query(&sql).bind(now);
        if let Some(v) = name {
            q = q.bind(v);
        }
        if let Some(v) = annual_rate {
            q = q.bind(v);
        }
        if let Some(v) = maturity_date {
            q = q.bind(v);
        }
        if let Some(v) = bank_name {
            q = q.bind(v);
        }
        if let Some(v) = quantity {
            q = q.bind(v);
        }
        if let Some(v) = unit {
            q = q.bind(v);
        }
        if let Some(v) = cost_basis_per_unit {
            q = q.bind(v);
        }
        if let Some(v) = avg_cost_per_share {
            q = q.bind(v);
        }
        if let Some(v) = notes {
            q = q.bind(v);
        }
        q.bind(asset_id).execute(&self.pool).await?;
        self.get_invest_asset(asset_id).await
    }

    pub async fn delete_invest_asset(&self, asset_id: &str) -> Result<()> {
        let now = now_unix();
        sqlx::query("UPDATE invest_assets SET deleted_at = ?, updated_at = ? WHERE id = ?")
            .bind(now)
            .bind(now)
            .bind(asset_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Price snapshots
    // -----------------------------------------------------------------------

    pub async fn add_price_snapshot(
        &self,
        asset_id: &str,
        price: i64,
        snapshot_date: &str,
        source: &str,
    ) -> Result<crate::converters::DbPriceSnapshot> {
        let id = new_id();
        let now = now_unix();
        sqlx::query(
            "INSERT INTO invest_price_snapshots (id, asset_id, price, source, snapshot_date, created_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON DUPLICATE KEY UPDATE price = VALUES(price), source = VALUES(source), created_at = VALUES(created_at)"
        )
        .bind(&id).bind(asset_id).bind(price).bind(source).bind(snapshot_date).bind(now)
        .execute(&self.pool).await?;
        self.get_latest_price_snapshot(asset_id)
            .await
            .map(|opt| opt.ok_or_else(|| anyhow::anyhow!("snapshot not found after insert")))?
    }

    pub async fn get_latest_price_snapshot(
        &self,
        asset_id: &str,
    ) -> Result<Option<crate::converters::DbPriceSnapshot>> {
        let row = sqlx::query_as::<_, crate::converters::DbPriceSnapshot>(
            "SELECT id, asset_id, price, source, snapshot_date, created_at
             FROM invest_price_snapshots WHERE asset_id = ?
             ORDER BY snapshot_date DESC, created_at DESC LIMIT 1",
        )
        .bind(asset_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn list_price_snapshots(
        &self,
        asset_id: &str,
        limit: i32,
    ) -> Result<Vec<crate::converters::DbPriceSnapshot>> {
        let rows = sqlx::query_as::<_, crate::converters::DbPriceSnapshot>(
            "SELECT id, asset_id, price, source, snapshot_date, created_at
             FROM invest_price_snapshots WHERE asset_id = ?
             ORDER BY snapshot_date DESC LIMIT ?",
        )
        .bind(asset_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
