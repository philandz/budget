use budget::manager::biz::seed::*;
use budget::pb::service::budget::BudgetType;
use budget::pb::service::category::CategoryType;

fn names(items: &[budget::pb::service::category::SeedCategoryItem]) -> Vec<&str> {
    items.iter().map(|i| i.name.as_str()).collect()
}

#[test]
fn standard_without_template_gets_vietnamese_defaults() {
    let items = seed_items(None, BudgetType::Standard, Locale::Vi, 0);
    assert!(names(&items).contains(&"Ăn uống"));
    assert!(items.iter().any(|i| i.kind == CategoryType::Income as i32));
    assert!(items.iter().all(|i| i.planned_amount.is_none()));
}

#[test]
fn locale_changes_names_not_structure() {
    let vi = seed_items(None, BudgetType::Standard, Locale::Vi, 0);
    let en = seed_items(None, BudgetType::Standard, Locale::En, 0);
    assert_eq!(vi.len(), en.len());
    assert!(names(&en).contains(&"Food & drink"));
    for (a, b) in vi.iter().zip(&en) {
        assert_eq!((a.kind, &a.icon), (b.kind, &b.icon));
    }
}

#[test]
fn locale_parsing_defaults_to_vietnamese() {
    assert_eq!(Locale::parse("en-US"), Locale::En);
    assert_eq!(Locale::parse("EN"), Locale::En);
    assert_eq!(Locale::parse("vi"), Locale::Vi);
    assert_eq!(Locale::parse(""), Locale::Vi);
    assert_eq!(Locale::parse("fr"), Locale::Vi);
}

#[test]
fn household_template_splits_50_30_and_plans_from_income() {
    let items = seed_items(
        Some(TEMPLATE_HOUSEHOLD_50_30_20),
        BudgetType::Standard,
        Locale::En,
        10_000_000,
    );
    let total: i64 = items.iter().filter_map(|i| i.planned_amount).sum();
    assert_eq!(total, 8_000_000); // 50% needs + 30% wants; the other 20% is savings, not a category
    let housing = items.iter().find(|i| i.name == "Housing").unwrap();
    assert_eq!(housing.planned_amount, Some(2_000_000));
}

#[test]
fn no_expected_income_means_no_planned_amounts() {
    let items = seed_items(
        Some(TEMPLATE_HOUSEHOLD_50_30_20),
        BudgetType::Standard,
        Locale::Vi,
        0,
    );
    assert!(items.iter().all(|i| i.planned_amount.is_none()));
}

#[test]
fn saving_and_invest_budgets_get_no_categories() {
    assert!(seed_items(None, BudgetType::Saving, Locale::Vi, 0).is_empty());
    assert!(seed_items(None, BudgetType::Invest, Locale::Vi, 0).is_empty());
    assert!(seed_items(
        Some(TEMPLATE_EMERGENCY_FUND),
        BudgetType::Saving,
        Locale::Vi,
        0
    )
    .is_empty());
}

#[test]
fn sharing_budget_gets_trip_categories() {
    let items = seed_items(None, BudgetType::Sharing, Locale::En, 0);
    assert!(names(&items).contains(&"Accommodation"));
    assert!(items.iter().all(|i| i.kind == CategoryType::Expense as i32));
}

#[test]
fn unknown_template_falls_back_to_the_budget_type_default() {
    let items = seed_items(Some("does-not-exist"), BudgetType::Standard, Locale::En, 0);
    assert_eq!(
        items.len(),
        seed_items(None, BudgetType::Standard, Locale::En, 0).len()
    );
}

#[test]
fn names_are_unique_per_kind_within_each_set() {
    for t in [
        None,
        Some(TEMPLATE_HOUSEHOLD_50_30_20),
        Some(TEMPLATE_FREELANCER),
        Some(TEMPLATE_TRIP),
        Some(TEMPLATE_DEBT),
    ] {
        for l in [Locale::Vi, Locale::En] {
            let items = seed_items(t, BudgetType::Standard, l, 0);
            let mut seen = std::collections::HashSet::new();
            for i in &items {
                assert!(
                    seen.insert((i.name.clone(), i.kind)),
                    "duplicate {} in {t:?}",
                    i.name
                );
            }
        }
    }
}
