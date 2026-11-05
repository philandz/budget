//! Debt schedule engine: pure functions, no I/O (plan §6).
//!
//! The schedule is an ESTIMATE (DB5): it is recomputed from the current outstanding balance, the
//! remaining due dates and the rate periods, and it never changes a recorded number (D6).
//! Money is VND minor units (i64). Rates are kept as *micro-percent* integers (8.9 %/year =
//! 8_900_000) so interest is computed with integers only and rounded half-up once per row.

use chrono::{Datelike, Months, NaiveDate};

pub const MAX_ROWS: usize = 600;
const RATE_SCALE: i128 = 100 * 1_000_000; // percent (/100) and micro (/1e6)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Annuity,
    EqualPrincipal,
    InterestOnly,
    FixedInstallments,
    NoSchedule,
}

impl Method {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "annuity" => Self::Annuity,
            "equal_principal" => Self::EqualPrincipal,
            "interest_only" => Self::InterestOnly,
            "fixed_installments" => Self::FixedInstallments,
            "no_schedule" => Self::NoSchedule,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Annuity => "annuity",
            Self::EqualPrincipal => "equal_principal",
            Self::InterestOnly => "interest_only",
            Self::FixedInstallments => "fixed_installments",
            Self::NoSchedule => "no_schedule",
        }
    }
    /// Scheduled methods need a maturity date / remaining periods.
    pub fn is_scheduled(self) -> bool {
        self != Self::NoSchedule
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayCount {
    Actual365,
    Monthly,
}

impl DayCount {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "actual_365" => Some(Self::Actual365),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Actual365 => "actual_365",
            Self::Monthly => "monthly",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub from: NaiveDate,
    /// percent per year × 1_000_000
    pub micro_pct: i64,
    pub floating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub seq: i32,
    pub due_date: NaiveDate,
    pub principal: i64,
    pub interest: i64,
    pub fee: i64,
    pub total: i64,
    pub balance_after: i64,
    pub micro_pct: i64,
    pub estimated: bool,
}

#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub method: Method,
    pub day_count: DayCount,
    pub outstanding: i64,
    /// Interest is settled up to this date; the first row accrues from here.
    pub interest_through: NaiveDate,
    /// Last scheduled due date that was paid; the next row is the first due date after it
    /// (or after `interest_through` when nothing was paid yet).
    pub last_due: Option<NaiveDate>,
    pub due_day: u32,
    pub maturity: Option<NaiveDate>,
    pub rates: &'a [Rate],
    pub periodic_fee: i64,
    /// Annuity: fixed instalment (principal + interest). Equal principal / fixed installments:
    /// fixed principal per period. Set after a "reduce term" prepayment.
    pub fixed_installment: Option<i64>,
    pub max_rows: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Schedule {
    pub rows: Vec<Row>,
    pub payoff_date: Option<NaiveDate>,
    pub total_interest: i64,
    pub estimated: bool,
}

// ---------------------------------------------------------------------------
// Rate parsing
// ---------------------------------------------------------------------------

/// "8.9" → 8_900_000. Up to 6 decimals, non-negative, at most 1000 %.
pub fn parse_pct(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return None;
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if frac_part.len() > 6 {
        return None;
    }
    let int_val: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac = frac_part.to_string();
    while frac.len() < 6 {
        frac.push('0');
    }
    let frac_val: i64 = frac.parse().ok()?;
    let v = int_val.checked_mul(1_000_000)?.checked_add(frac_val)?;
    if v > 1000 * 1_000_000 {
        return None;
    }
    Some(v)
}

/// 8_900_000 → "8.9"; 0 → "0".
pub fn fmt_pct(micro: i64) -> String {
    let int = micro / 1_000_000;
    let frac = micro % 1_000_000;
    if frac == 0 {
        return int.to_string();
    }
    let mut f = format!("{frac:06}");
    while f.ends_with('0') {
        f.pop();
    }
    format!("{int}.{f}")
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

/// The due date of `(year, month)`: `due_day` clipped to the month (31 = last day).
pub fn due_in_month(year: i32, month: u32, due_day: u32) -> NaiveDate {
    let day = due_day.clamp(1, 31).min(last_day_of_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
}

/// First due date strictly after `after`.
pub fn next_due_after(after: NaiveDate, due_day: u32) -> NaiveDate {
    let this = due_in_month(after.year(), after.month(), due_day);
    if this > after {
        return this;
    }
    let next = after
        .with_day(1)
        .expect("day 1")
        .checked_add_months(Months::new(1))
        .expect("month add");
    due_in_month(next.year(), next.month(), due_day)
}

/// Due date count helper: the `n`-th due date strictly after `after` (n >= 1).
pub fn nth_due_after(after: NaiveDate, due_day: u32, n: u32) -> NaiveDate {
    let mut d = after;
    for _ in 0..n.max(1) {
        d = next_due_after(d, due_day);
    }
    d
}

// ---------------------------------------------------------------------------
// Interest
// ---------------------------------------------------------------------------

/// The rate in force on `date`: the latest period starting on or before it; before the first
/// period, the first one applies (the owner may enter the rate after the loan began).
pub fn rate_on(rates: &[Rate], date: NaiveDate) -> Option<&Rate> {
    let mut best: Option<&Rate> = None;
    for r in rates {
        if r.from <= date && best.map(|b| r.from >= b.from).unwrap_or(true) {
            best = Some(r);
        }
    }
    best.or_else(|| rates.iter().min_by_key(|r| r.from))
}

fn div_round_half_up(num: i128, den: i128) -> i128 {
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

pub struct Accrual {
    pub interest: i64,
    pub days: i64,
    /// Rate at the end of the accrual window (micro-percent).
    pub micro_pct: i64,
    pub estimated: bool,
}

/// Interest on `balance` over `[from, to)` (to exclusive of the day after; `days = to - from`).
/// The window is split at every rate change. `period_days` is the length of the nominal due
/// period, used only by the `monthly` day count (rate/12 per full period, prorated otherwise).
pub fn accrue(
    balance: i64,
    from: NaiveDate,
    to: NaiveDate,
    rates: &[Rate],
    day_count: DayCount,
    period_days: i64,
) -> Accrual {
    let days = (to - from).num_days().max(0);
    if balance <= 0 || days == 0 || rates.is_empty() {
        let r = rate_on(rates, to);
        return Accrual {
            interest: 0,
            days,
            micro_pct: r.map(|r| r.micro_pct).unwrap_or(0),
            estimated: r.map(|r| r.floating).unwrap_or(false),
        };
    }
    // Segment boundaries inside the window.
    let mut cuts: Vec<NaiveDate> = rates
        .iter()
        .map(|r| r.from)
        .filter(|d| *d > from && *d < to)
        .collect();
    cuts.sort();
    cuts.dedup();
    let mut start = from;
    let mut weighted: i128 = 0; // Σ micro_pct × days
    let mut estimated = false;
    for end in cuts.into_iter().chain(std::iter::once(to)) {
        let seg_days = (end - start).num_days() as i128;
        if seg_days > 0 {
            if let Some(r) = rate_on(rates, start) {
                weighted += r.micro_pct as i128 * seg_days;
                estimated |= r.floating;
            }
        }
        start = end;
    }
    let den_days: i128 = match day_count {
        DayCount::Actual365 => 365,
        DayCount::Monthly => 12 * period_days.max(1) as i128,
    };
    let interest = div_round_half_up(balance as i128 * weighted, RATE_SCALE * den_days) as i64;
    let end_rate = rate_on(rates, to);
    Accrual {
        interest,
        days,
        micro_pct: end_rate.map(|r| r.micro_pct).unwrap_or(0),
        estimated,
    }
}

// ---------------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------------

/// Due dates of the remaining schedule: every due date after the anchor up to maturity, and a
/// stub row on the maturity date when it does not fall on a due date.
fn due_dates(i: &Inputs) -> Vec<NaiveDate> {
    let maturity = match i.maturity {
        Some(m) => m,
        None => return vec![],
    };
    let anchor = i.last_due.unwrap_or(i.interest_through);
    let mut out = vec![];
    let mut d = next_due_after(anchor, i.due_day);
    while d <= maturity && out.len() < i.max_rows.min(MAX_ROWS) {
        out.push(d);
        d = next_due_after(d, i.due_day);
    }
    if out
        .last()
        .map(|l| *l < maturity)
        .unwrap_or(maturity > anchor)
        && out.len() < MAX_ROWS
    {
        out.push(maturity);
    }
    out
}

fn annuity_payment(balance: i64, monthly_rate: f64, n: i64) -> i64 {
    if n <= 0 {
        return balance;
    }
    if monthly_rate <= 0.0 {
        return (balance as f64 / n as f64).round() as i64;
    }
    let p = balance as f64 * monthly_rate / (1.0 - (1.0 + monthly_rate).powi(-(n as i32)));
    p.round() as i64
}

/// Projects the remaining schedule. Never mutates recorded data.
pub fn project(i: &Inputs) -> Schedule {
    let mut out = Schedule::default();
    if !i.method.is_scheduled() || i.outstanding <= 0 {
        return out;
    }
    let dates = due_dates(i);
    if dates.is_empty() {
        return out;
    }
    let n_total = dates.len() as i64;
    let mut balance = i.outstanding;
    let mut prev = i.interest_through;
    let mut nominal_prev = {
        // The due date before the first row, for the `monthly` day count proration.
        let first = dates[0];
        first
            .with_day(1)
            .and_then(|d| d.checked_sub_months(Months::new(1)))
            .map(|d| due_in_month(d.year(), d.month(), i.due_day))
            .unwrap_or(prev)
    };
    let flat_interest_base = i.outstanding;
    let mut payment: Option<i64> = i.fixed_installment.filter(|_| i.method == Method::Annuity);
    let mut last_rate: Option<i64> = None;

    for (idx, due) in dates.iter().enumerate() {
        if balance <= 0 {
            break;
        }
        let remaining = n_total - idx as i64; // including this row
        let is_last = remaining == 1;
        let period_days = (*due - nominal_prev).num_days().max(1);
        let acc = accrue(balance, prev, *due, i.rates, i.day_count, period_days);
        let mut interest = acc.interest;
        let rate_pct = acc.micro_pct;

        let principal = match i.method {
            Method::Annuity => {
                if i.fixed_installment.is_none()
                    && (payment.is_none() || last_rate != Some(rate_pct))
                {
                    // Recompute at the start and at every rate change (bank practice).
                    payment = Some(annuity_payment(
                        balance,
                        rate_pct as f64 / 1_000_000.0 / 100.0 / 12.0,
                        remaining,
                    ));
                }
                let pay = payment.unwrap_or(balance);
                if is_last {
                    balance
                } else {
                    (pay - interest).clamp(0, balance)
                }
            }
            Method::EqualPrincipal => {
                if is_last {
                    balance
                } else if let Some(fixed) = i.fixed_installment {
                    fixed.clamp(0, balance)
                } else {
                    div_round_half_up(balance as i128, remaining as i128) as i64
                }
            }
            Method::InterestOnly => {
                if is_last {
                    balance
                } else {
                    0
                }
            }
            Method::FixedInstallments => {
                // Flat: equal principal parts, flat interest on the opening balance.
                interest = div_round_half_up(
                    flat_interest_base as i128 * rate_pct as i128,
                    RATE_SCALE * 12,
                ) as i64;
                if is_last {
                    balance
                } else if let Some(fixed) = i.fixed_installment {
                    fixed.clamp(0, balance)
                } else {
                    div_round_half_up(balance as i128, remaining as i128) as i64
                }
            }
            Method::NoSchedule => 0,
        }
        .min(balance);
        last_rate = Some(rate_pct);

        balance -= principal;
        let total = principal + interest + i.periodic_fee;
        out.total_interest += interest;
        out.estimated |= acc.estimated;
        out.rows.push(Row {
            seq: idx as i32 + 1,
            due_date: *due,
            principal,
            interest,
            fee: i.periodic_fee,
            total,
            balance_after: balance,
            micro_pct: rate_pct,
            estimated: acc.estimated,
        });
        out.payoff_date = Some(*due);
        prev = *due;
        nominal_prev = *due;
    }
    out
}

// ---------------------------------------------------------------------------
// Prepayment fee
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepayRule {
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
    pub free_allowance_per_month: i64,
    /// percent × 10_000 (DECIMAL(7,4))
    pub fee_pct_1e4: i64,
    pub min_fee: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeeQuote {
    pub fee: i64,
    pub allowance_used: i64,
    pub allowance_remaining: i64,
    pub taxable: i64,
    pub has_rule: bool,
}

/// The rule in force on `date` (latest `valid_from` wins).
pub fn rule_on(rules: &[PrepayRule], date: NaiveDate) -> Option<&PrepayRule> {
    rules
        .iter()
        .filter(|r| r.valid_from <= date && r.valid_to.map(|t| date <= t).unwrap_or(true))
        .max_by_key(|r| r.valid_from)
}

/// Fee for prepaying `amount` on `date` when `prepaid_this_month` was already prepaid in the same
/// calendar month (DB9): free allowance first, then `fee_pct` on the excess, at least `min_fee`.
pub fn prepay_fee(
    rules: &[PrepayRule],
    date: NaiveDate,
    prepaid_this_month: i64,
    amount: i64,
) -> FeeQuote {
    let rule = match rule_on(rules, date) {
        Some(r) => r,
        None => {
            return FeeQuote {
                allowance_used: prepaid_this_month,
                allowance_remaining: i64::MAX,
                ..Default::default()
            }
        }
    };
    let remaining = (rule.free_allowance_per_month - prepaid_this_month).max(0);
    let taxable = (amount - remaining).max(0);
    let fee = if taxable > 0 {
        let pct_fee =
            div_round_half_up(taxable as i128 * rule.fee_pct_1e4 as i128, 100 * 10_000) as i64;
        pct_fee.max(rule.min_fee)
    } else {
        0
    };
    FeeQuote {
        fee,
        allowance_used: prepaid_this_month,
        allowance_remaining: (remaining - amount).max(0),
        taxable,
        has_rule: true,
    }
}

/// The instalment to keep after a "reduce term" prepayment (see `Inputs::fixed_installment`).
pub fn current_installment(method: Method, rows: &[Row]) -> Option<i64> {
    let r = rows.first()?;
    match method {
        Method::Annuity => Some(r.principal + r.interest),
        Method::EqualPrincipal | Method::FixedInstallments => Some(r.principal),
        _ => None,
    }
}
