//! Redemptions for the saving fund: fixed-deposit maturity and gold sales.
//!
//! Both can credit the proceeds to the fund's cash pocket (`FundingSource::CashPocket`). The
//! pocket credit is applied before the asset transaction commits and compensated if the commit
//! fails, so value never sits in both the asset and the pocket.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tonic::Status;
use uuid::Uuid;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::biz::portfolio::lifecycle::{next_status, Transition};
use crate::manager::biz::portfolio::AssetStatus;
use crate::pb::service::budget::BudgetRole;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}

/// Result of selling part (or all) of a gold lot.
#[derive(Debug, PartialEq, Eq)]
pub struct GoldSaleSplit {
    pub cost_allocated: i64,
    pub remaining_grams: Decimal,
    pub fully_sold: bool,
}

/// Splits the lot's cost for a sale of `qty_sold` grams. Uses proportional cost, and the exact
/// remaining cost when the lot is emptied so rounding never leaves a residue.
pub fn gold_sale_split(
    total_grams: Decimal,
    sold_before: Decimal,
    qty_sold: Decimal,
    purchase_cost: i64,
    cost_allocated_before: i64,
) -> Result<GoldSaleSplit, Status> {
    if qty_sold <= Decimal::ZERO {
        return Err(Status::invalid_argument("quantity must be > 0"));
    }
    let open = total_grams - sold_before;
    if qty_sold > open {
        return Err(Status::invalid_argument(
            "quantity exceeds the grams still held",
        ));
    }
    let remaining = open - qty_sold;
    let fully_sold = remaining.is_zero();
    let cost_allocated = if fully_sold {
        purchase_cost - cost_allocated_before
    } else {
        (Decimal::from(purchase_cost) * qty_sold / total_grams)
            .round()
            .to_i64()
            .unwrap_or(0)
    };
    Ok(GoldSaleSplit {
        cost_allocated,
        remaining_grams: remaining,
        fully_sold,
    })
}

fn parse_decimal(value: &str) -> Result<Decimal, Status> {
    value
        .parse::<Decimal>()
        .map_err(|_| Status::invalid_argument(format!("invalid decimal: {value}")))
}

fn opt(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

impl PortfolioBiz {
    pub async fn record_fixed_deposit_maturity(
        &self,
        user_id: &str,
        req: &pb::RecordFixedDepositMaturityRequest,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        if req.budget_id.is_empty() || req.asset_id.is_empty() {
            return Err(Status::invalid_argument(
                "budget_id and asset_id are required",
            ));
        }
        if req.actual_settlement_value <= 0 {
            return Err(Status::invalid_argument(
                "actual_settlement_value must be > 0",
            ));
        }
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;

        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .get_asset(&mut tx, &req.budget_id, &req.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        if asset.asset_class != "fixed_deposit" {
            return Err(Status::invalid_argument("asset is not a fixed deposit"));
        }
        let current = AssetStatus::from_db(&asset.status);
        let next = next_status(current, Transition::Withdraw).map_err(|_| {
            Status::failed_precondition(format!(
                "asset is {}; only active or matured deposits can be settled",
                asset.status
            ))
        })?;
        let settled_on = if req.settlement_date > 0 {
            req.settlement_date
        } else {
            now_unix()
        };
        self.repo
            .update_status(
                &mut tx,
                &req.budget_id,
                &req.asset_id,
                next,
                Some(settled_on),
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            &req.budget_id,
            &req.asset_id,
            user_id,
            opt(&req.idempotency_key),
            "MATURITY_SETTLED",
            &format!(
                r#"{{"settlement_value":{},"settlement_date":{}}}"#,
                req.actual_settlement_value, settled_on
            ),
        )
        .await?;

        self.credit_redemption(
            user_id,
            &req.budget_id,
            req.funding_source,
            &req.asset_id,
            req.actual_settlement_value,
            opt(&req.idempotency_key),
        )
        .await?;
        if let Err(e) = tx.commit().await {
            self.reverse_redemption(
                user_id,
                &req.budget_id,
                req.funding_source,
                req.actual_settlement_value,
                opt(&req.idempotency_key),
            )
            .await;
            return Err(internal(e));
        }
        self.reload_asset(&req.budget_id, &req.asset_id).await
    }

    pub async fn record_gold_disposal(
        &self,
        user_id: &str,
        req: &pb::RecordGoldDisposalRequest,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        if req.budget_id.is_empty() || req.asset_id.is_empty() {
            return Err(Status::invalid_argument(
                "budget_id and asset_id are required",
            ));
        }
        if req.sale_proceeds < 0 || req.sale_fees < 0 {
            return Err(Status::invalid_argument("proceeds and fees must be >= 0"));
        }
        let qty = parse_decimal(&req.quantity_grams_sold)?;
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;

        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .get_asset(&mut tx, &req.budget_id, &req.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        if asset.asset_class != "gold_lot" {
            return Err(Status::invalid_argument("asset is not a gold lot"));
        }
        let current = AssetStatus::from_db(&asset.status);
        if current != AssetStatus::Active {
            return Err(Status::failed_precondition(format!(
                "asset is {}; only active lots can be sold",
                asset.status
            )));
        }
        let lot = self
            .repo
            .get_gold_lot(&mut tx, &req.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("gold lot not found"))?;
        let total = parse_decimal(&lot.quantity_grams)?;
        let sold_before = self
            .repo
            .gold_sold_grams(&mut tx, &req.asset_id)
            .await
            .map_err(internal)?;
        let allocated_before = self
            .repo
            .gold_cost_allocated(&mut tx, &req.asset_id)
            .await
            .map_err(internal)?;
        let split = gold_sale_split(total, sold_before, qty, lot.purchase_cost, allocated_before)?;
        let realized = req.sale_proceeds - req.sale_fees - split.cost_allocated;
        let when = if req.disposal_date > 0 {
            req.disposal_date
        } else {
            now_unix()
        };

        self.repo
            .record_gold_disposal(
                &mut tx,
                &req.asset_id,
                &Uuid::new_v4().to_string(),
                when,
                qty.to_string(),
                req.sale_proceeds,
                req.sale_fees,
                realized,
                split.cost_allocated,
            )
            .await
            .map_err(internal)?;
        if split.fully_sold {
            self.repo
                .update_status(
                    &mut tx,
                    &req.budget_id,
                    &req.asset_id,
                    AssetStatus::Sold,
                    Some(when),
                )
                .await
                .map_err(internal)?;
        }
        self.append_activity(
            &mut tx,
            &req.budget_id,
            &req.asset_id,
            user_id,
            opt(&req.idempotency_key),
            "DISPOSAL_RECORDED",
            &format!(
                r#"{{"grams":"{}","proceeds":{},"pnl":{}}}"#,
                qty, req.sale_proceeds, realized
            ),
        )
        .await?;

        let net = req.sale_proceeds - req.sale_fees;
        self.credit_redemption(
            user_id,
            &req.budget_id,
            req.funding_source,
            &req.asset_id,
            net,
            opt(&req.idempotency_key),
        )
        .await?;
        if let Err(e) = tx.commit().await {
            self.reverse_redemption(
                user_id,
                &req.budget_id,
                req.funding_source,
                net,
                opt(&req.idempotency_key),
            )
            .await;
            return Err(internal(e));
        }
        self.reload_asset(&req.budget_id, &req.asset_id).await
    }

    async fn reload_asset(
        &self,
        budget_id: &str,
        asset_id: &str,
    ) -> Result<pb::PortfolioAsset, Status> {
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let a = self
            .repo
            .get_asset(&mut tx, budget_id, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(a))
    }

    /// Undo a pocket credit when the asset transaction failed to commit.
    async fn reverse_redemption(
        &self,
        user_id: &str,
        budget_id: &str,
        funding_source: i32,
        amount: i64,
        key: Option<&str>,
    ) {
        if funding_source != pb::FundingSource::CashPocket as i32 || amount <= 0 {
            return;
        }
        if let Some(k) = key {
            self.refund_purchase(user_id, budget_id, -amount, &format!("redeem:{k}"))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn partial_sale_allocates_proportional_cost() {
        let s = gold_sale_split(d("10"), d("0"), d("4"), 10_000_000, 0).unwrap();
        assert_eq!(s.cost_allocated, 4_000_000);
        assert_eq!(s.remaining_grams, d("6"));
        assert!(!s.fully_sold);
    }

    #[test]
    fn final_sale_takes_exact_remaining_cost() {
        // 3 equal sales of a lot costing 10 with awkward rounding: allocations must sum to 10.
        let total = d("3");
        let mut sold = d("0");
        let mut allocated = 0i64;
        for i in 0..3 {
            let s = gold_sale_split(total, sold, d("1"), 10, allocated).unwrap();
            allocated += s.cost_allocated;
            sold += d("1");
            assert_eq!(s.fully_sold, i == 2);
        }
        assert_eq!(allocated, 10);
    }

    #[test]
    fn rejects_bad_quantities() {
        assert!(gold_sale_split(d("10"), d("0"), d("0"), 100, 0).is_err());
        assert!(gold_sale_split(d("10"), d("0"), d("-1"), 100, 0).is_err());
        assert!(gold_sale_split(d("10"), d("8"), d("3"), 100, 0).is_err());
    }
}
