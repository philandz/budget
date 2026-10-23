//! Default categories for new budgets (plan R3 §6.5). Pure data + selection logic: the budget
//! service decides *what* to seed, the category service owns the rows and stays idempotent.

use crate::pb::service::budget::BudgetType;
use crate::pb::service::category::{CategoryType, SeedCategoryItem};

pub const TEMPLATE_HOUSEHOLD_50_30_20: &str = "00000000-0000-0000-0000-000000000001";
pub const TEMPLATE_FREELANCER: &str = "00000000-0000-0000-0000-000000000002";
pub const TEMPLATE_TRIP: &str = "00000000-0000-0000-0000-000000000003";
pub const TEMPLATE_DEBT: &str = "00000000-0000-0000-0000-000000000004";
pub const TEMPLATE_EMERGENCY_FUND: &str = "00000000-0000-0000-0000-000000000005";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    Vi,
    En,
}

impl Locale {
    /// Unknown or empty falls back to Vietnamese (the product default).
    pub fn parse(s: &str) -> Self {
        if s.trim().to_ascii_lowercase().starts_with("en") {
            Locale::En
        } else {
            Locale::Vi
        }
    }
}

struct Def {
    vi: &'static str,
    en: &'static str,
    kind: CategoryType,
    icon: &'static str,
    color: &'static str,
    /// share of expected income, in percent
    pct: Option<u8>,
}

const fn exp(
    vi: &'static str,
    en: &'static str,
    icon: &'static str,
    color: &'static str,
    pct: Option<u8>,
) -> Def {
    Def {
        vi,
        en,
        kind: CategoryType::Expense,
        icon,
        color,
        pct,
    }
}

const fn inc(vi: &'static str, en: &'static str, icon: &'static str, color: &'static str) -> Def {
    Def {
        vi,
        en,
        kind: CategoryType::Income,
        icon,
        color,
        pct: None,
    }
}

const STANDARD_DEFAULT: &[Def] = &[
    exp("Ăn uống", "Food & drink", "🍔", "#f59e0b", None),
    exp("Di chuyển", "Transport", "🚗", "#06b6d4", None),
    exp("Nhà ở", "Housing", "🏠", "#6366f1", None),
    exp(
        "Hóa đơn & tiện ích",
        "Bills & utilities",
        "💡",
        "#0ea5e9",
        None,
    ),
    exp("Sức khỏe", "Health", "💊", "#ef4444", None),
    exp("Giải trí", "Entertainment", "🎮", "#8b5cf6", None),
    exp("Mua sắm", "Shopping", "🛍️", "#ec4899", None),
    exp("Giáo dục", "Education", "📚", "#14b8a6", None),
    exp("Khác", "Other", "📦", "#64748b", None),
    inc("Lương", "Salary", "💰", "#10b981"),
    inc("Thu nhập khác", "Other income", "📥", "#6366f1"),
];

const HOUSEHOLD: &[Def] = &[
    // needs 50%
    exp("Nhà ở", "Housing", "🏠", "#6366f1", Some(20)),
    exp(
        "Hóa đơn & tiện ích",
        "Bills & utilities",
        "💡",
        "#0ea5e9",
        Some(8),
    ),
    exp("Đi chợ & siêu thị", "Groceries", "🛒", "#10b981", Some(12)),
    exp("Di chuyển", "Transport", "🚗", "#06b6d4", Some(6)),
    exp("Sức khỏe", "Health", "💊", "#ef4444", Some(4)),
    // wants 30%
    exp("Ăn ngoài", "Dining out", "🍜", "#f59e0b", Some(10)),
    exp("Giải trí", "Entertainment", "🎮", "#8b5cf6", Some(8)),
    exp("Mua sắm", "Shopping", "🛍️", "#ec4899", Some(12)),
    inc("Lương", "Salary", "💰", "#10b981"),
    inc("Thu nhập khác", "Other income", "📥", "#6366f1"),
];

const FREELANCER: &[Def] = &[
    exp(
        "Chi phí kinh doanh",
        "Business expenses",
        "💼",
        "#6366f1",
        Some(20),
    ),
    exp(
        "Phần dành cho thuế",
        "Tax set-aside",
        "🧾",
        "#ef4444",
        Some(20),
    ),
    exp(
        "Thiết bị & phần mềm",
        "Equipment & software",
        "💻",
        "#0ea5e9",
        Some(5),
    ),
    exp("Sinh hoạt", "Living costs", "🏠", "#f59e0b", Some(40)),
    exp("Khác", "Other", "📦", "#64748b", None),
    inc("Doanh thu khách hàng", "Client income", "💰", "#10b981"),
    inc("Thu nhập khác", "Other income", "📥", "#6366f1"),
];

const TRIP: &[Def] = &[
    exp("Ăn uống", "Food & drink", "🍔", "#f59e0b", None),
    exp("Di chuyển", "Transport", "🚗", "#06b6d4", None),
    exp("Lưu trú", "Accommodation", "🏨", "#6366f1", None),
    exp(
        "Vé & tham quan",
        "Tickets & activities",
        "🎟️",
        "#8b5cf6",
        None,
    ),
    exp("Mua sắm", "Shopping", "🛍️", "#ec4899", None),
    exp("Khác", "Other", "📦", "#64748b", None),
];

const DEBT: &[Def] = &[
    exp("Trả gốc", "Principal payment", "🏦", "#ef4444", None),
    exp("Lãi", "Interest", "📈", "#f59e0b", None),
    exp("Phí", "Fees", "🧾", "#64748b", None),
];

/// Which definitions apply: an explicit built-in template wins; otherwise the budget type decides.
/// Saving (and Invest) budgets hold a fund, not income/expense categories, so they get none.
fn defs_for(template_id: Option<&str>, budget_type: BudgetType) -> &'static [Def] {
    match template_id {
        Some(TEMPLATE_HOUSEHOLD_50_30_20) => return HOUSEHOLD,
        Some(TEMPLATE_FREELANCER) => return FREELANCER,
        Some(TEMPLATE_TRIP) => return TRIP,
        Some(TEMPLATE_DEBT) => return DEBT,
        Some(TEMPLATE_EMERGENCY_FUND) => return &[],
        _ => {}
    }
    match budget_type {
        BudgetType::Standard => STANDARD_DEFAULT,
        BudgetType::Sharing => TRIP,
        _ => &[],
    }
}

pub fn seed_items(
    template_id: Option<&str>,
    budget_type: BudgetType,
    locale: Locale,
    expected_income: i64,
) -> Vec<SeedCategoryItem> {
    defs_for(template_id, budget_type)
        .iter()
        .map(|d| SeedCategoryItem {
            name: match locale {
                Locale::Vi => d.vi.to_string(),
                Locale::En => d.en.to_string(),
            },
            kind: d.kind as i32,
            icon: d.icon.to_string(),
            color: d.color.to_string(),
            planned_amount: match (d.pct, expected_income) {
                (Some(p), inc) if inc > 0 => Some(inc.saturating_mul(p as i64) / 100),
                _ => None,
            },
        })
        .collect()
}
