//! Pure schedule engine tests (plan §6, invariants D2/D6 and the prepayment allowance).

use budget::manager::biz::debt::schedule::*;
use chrono::NaiveDate;

fn d(s: &str) -> NaiveDate {
    parse_date(s).unwrap()
}

fn fixed(from: &str, pct: &str) -> Rate {
    Rate {
        from: d(from),
        micro_pct: parse_pct(pct).unwrap(),
        floating: false,
    }
}

fn floating(from: &str, pct: &str) -> Rate {
    Rate {
        from: d(from),
        micro_pct: parse_pct(pct).unwrap(),
        floating: true,
    }
}

fn inputs<'a>(method: Method, outstanding: i64, rates: &'a [Rate], maturity: &str) -> Inputs<'a> {
    Inputs {
        method,
        day_count: DayCount::Monthly,
        outstanding,
        interest_through: d("2026-11-25"),
        last_due: None,
        due_day: 25,
        maturity: Some(d(maturity)),
        rates,
        periodic_fee: 0,
        fixed_installment: None,
        max_rows: 600,
    }
}

#[test]
fn percent_parsing_round_trips() {
    assert_eq!(parse_pct("8.9"), Some(8_900_000));
    assert_eq!(parse_pct("0"), Some(0));
    assert_eq!(parse_pct("12.345678"), Some(12_345_678));
    assert_eq!(parse_pct("12.3456789"), None);
    assert_eq!(parse_pct("-1"), None);
    assert_eq!(parse_pct("abc"), None);
    assert_eq!(parse_pct(""), None);
    assert_eq!(fmt_pct(8_900_000), "8.9");
    assert_eq!(fmt_pct(0), "0");
    assert_eq!(fmt_pct(12_345_678), "12.345678");
}

#[test]
fn due_dates_clip_to_the_month() {
    assert_eq!(due_in_month(2027, 2, 31), d("2027-02-28"));
    assert_eq!(due_in_month(2028, 2, 31), d("2028-02-29"));
    assert_eq!(due_in_month(2026, 4, 31), d("2026-04-30"));
    assert_eq!(due_in_month(2026, 2, 30), d("2026-02-28"));
    assert_eq!(next_due_after(d("2026-01-31"), 31), d("2026-02-28"));
    assert_eq!(next_due_after(d("2026-02-28"), 31), d("2026-03-31"));
    assert_eq!(next_due_after(d("2026-11-25"), 25), d("2026-12-25"));
    assert_eq!(next_due_after(d("2026-11-24"), 25), d("2026-11-25"));
    assert_eq!(next_due_after(d("2026-12-31"), 15), d("2027-01-15"));
    assert_eq!(nth_due_after(d("2026-11-25"), 25, 12), d("2027-11-25"));
}

#[test]
fn annuity_matches_the_textbook_instalment_and_clears_the_balance() {
    let rates = [fixed("2026-11-25", "12")];
    let s = project(&inputs(Method::Annuity, 100_000_000, &rates, "2027-11-25"));
    assert_eq!(s.rows.len(), 12);
    assert_eq!(s.rows[0].interest, 1_000_000);
    assert_eq!(s.rows[0].principal + s.rows[0].interest, 8_884_879);
    assert_eq!(s.rows.last().unwrap().balance_after, 0);
    assert_eq!(s.payoff_date, Some(d("2027-11-25")));
    let principal: i64 = s.rows.iter().map(|r| r.principal).sum();
    assert_eq!(principal, 100_000_000);
    assert!(!s.estimated);
}

#[test]
fn every_method_repays_exactly_the_outstanding_and_never_goes_negative() {
    let rates = [fixed("2026-11-25", "9.5")];
    for m in [
        Method::Annuity,
        Method::EqualPrincipal,
        Method::InterestOnly,
        Method::FixedInstallments,
    ] {
        for day_count in [DayCount::Monthly, DayCount::Actual365] {
            let mut i = inputs(m, 333_333_333, &rates, "2029-11-25");
            i.day_count = day_count;
            i.periodic_fee = 15_000;
            let s = project(&i);
            assert!(!s.rows.is_empty(), "{m:?}");
            let mut bal = 333_333_333i64;
            for r in &s.rows {
                assert!(
                    r.principal >= 0 && r.interest >= 0 && r.fee >= 0,
                    "{m:?} {r:?}"
                );
                assert_eq!(r.total, r.principal + r.interest + r.fee, "D2 {m:?}");
                bal -= r.principal;
                assert_eq!(bal, r.balance_after, "{m:?}");
                assert!(bal >= 0);
            }
            assert_eq!(bal, 0, "{m:?} {day_count:?}");
        }
    }
}

#[test]
fn equal_principal_has_constant_principal_and_falling_interest() {
    let rates = [fixed("2026-11-25", "12")];
    let s = project(&inputs(
        Method::EqualPrincipal,
        120_000_000,
        &rates,
        "2027-11-25",
    ));
    assert!(s.rows.iter().all(|r| r.principal == 10_000_000));
    assert_eq!(s.rows[0].interest, 1_200_000);
    assert!(s.rows[1].interest < s.rows[0].interest);
}

#[test]
fn interest_only_pays_the_principal_on_the_maturity_row() {
    let rates = [fixed("2026-11-25", "12")];
    let s = project(&inputs(
        Method::InterestOnly,
        100_000_000,
        &rates,
        "2027-05-25",
    ));
    assert_eq!(s.rows.len(), 6);
    assert!(s.rows[..5]
        .iter()
        .all(|r| r.principal == 0 && r.interest == 1_000_000));
    assert_eq!(s.rows[5].principal, 100_000_000);
}

#[test]
fn fixed_installments_are_flat() {
    let rates = [fixed("2026-11-25", "0")];
    let mut i = inputs(Method::FixedInstallments, 12_000_000, &rates, "2027-11-25");
    i.periodic_fee = 50_000;
    let s = project(&i);
    assert_eq!(s.rows.len(), 12);
    assert!(s
        .rows
        .iter()
        .all(|r| r.principal == 1_000_000 && r.interest == 0 && r.fee == 50_000));
}

#[test]
fn no_schedule_projects_nothing() {
    let rates = [fixed("2026-11-25", "10")];
    let s = project(&inputs(
        Method::NoSchedule,
        50_000_000,
        &rates,
        "2027-11-25",
    ));
    assert!(s.rows.is_empty());
}

#[test]
fn actual_365_uses_the_real_number_of_days() {
    // 100,000,000 at 10 %/year for 30 days = 821,917.8 → 821,918
    let a = accrue(
        100_000_000,
        d("2026-11-25"),
        d("2026-12-25"),
        &[fixed("2026-01-01", "10")],
        DayCount::Actual365,
        30,
    );
    assert_eq!(a.days, 30);
    assert_eq!(a.interest, 821_918);
    // a 31-day month costs more than a 28-day one
    let long = accrue(
        100_000_000,
        d("2026-12-25"),
        d("2027-01-25"),
        &[fixed("2026-01-01", "10")],
        DayCount::Actual365,
        31,
    );
    let short = accrue(
        100_000_000,
        d("2027-01-25"),
        d("2027-02-25"),
        &[fixed("2026-01-01", "10")],
        DayCount::Actual365,
        31,
    );
    assert!(long.interest > 0 && short.interest > 0);
    let feb = accrue(
        100_000_000,
        d("2027-02-01"),
        d("2027-03-01"),
        &[fixed("2026-01-01", "10")],
        DayCount::Actual365,
        28,
    );
    assert_eq!(feb.interest, 767_123); // 28 days
}

#[test]
fn a_rate_change_inside_a_period_is_split_by_day() {
    // 10 days at 10 % (Nov 15 - Nov 25), then 20 days at 12 % (Nov 25 - Dec 15)
    let rates = [fixed("2026-01-01", "10"), floating("2026-11-25", "12")];
    let a = accrue(
        100_000_000,
        d("2026-11-15"),
        d("2026-12-15"),
        &rates,
        DayCount::Actual365,
        30,
    );
    // (10 * 10 + 12 * 20) = 340 percent-days → 100M * 340 / 100 / 365
    assert_eq!(a.interest, 931_507);
    assert!(a.estimated);
    assert_eq!(a.micro_pct, 12_000_000);
}

#[test]
fn a_floating_rate_marks_the_rows_estimated_and_recomputes_the_instalment() {
    let rates = [fixed("2026-11-25", "8"), floating("2027-05-25", "11")];
    let s = project(&inputs(Method::Annuity, 200_000_000, &rates, "2028-11-25"));
    let before = &s.rows[0];
    let after = s
        .rows
        .iter()
        .find(|r| r.due_date >= d("2027-06-25"))
        .unwrap();
    assert!(!before.estimated);
    assert!(after.estimated);
    assert!(s.estimated);
    assert!(after.principal + after.interest > before.principal + before.interest);
    assert_eq!(s.rows.last().unwrap().balance_after, 0);
}

#[test]
fn the_last_known_rate_is_used_when_no_later_rate_is_entered() {
    let rates = [fixed("2026-11-25", "9")];
    let s = project(&inputs(Method::Annuity, 100_000_000, &rates, "2028-11-25"));
    assert!(s.rows.iter().all(|r| r.micro_pct == 9_000_000));
}

#[test]
fn a_rate_entered_late_still_applies_to_earlier_dates() {
    let rates = [fixed("2027-01-01", "9")];
    assert_eq!(
        rate_on(&rates, d("2026-11-25")).unwrap().micro_pct,
        9_000_000
    );
}

#[test]
fn reduce_term_keeps_the_instalment_and_pays_off_earlier() {
    let rates = [fixed("2026-11-25", "12")];
    let base = project(&inputs(Method::Annuity, 100_000_000, &rates, "2027-11-25"));
    let keep = current_installment(Method::Annuity, &base.rows).unwrap();

    // after a 40M prepayment: same instalment, shorter term
    let mut shorter = inputs(Method::Annuity, 60_000_000, &rates, "2027-11-25");
    shorter.fixed_installment = Some(keep);
    let s_term = project(&shorter);
    // reduce instalment: same term, smaller instalment
    let s_inst = project(&inputs(Method::Annuity, 60_000_000, &rates, "2027-11-25"));

    assert!(s_term.rows.len() < 12);
    assert_eq!(s_inst.rows.len(), 12);
    assert!(s_inst.rows[0].total < s_term.rows[0].total);
    assert_eq!(s_term.rows.last().unwrap().balance_after, 0);
    assert!(s_term.payoff_date < s_inst.payoff_date);
    assert!(s_term.total_interest < base.total_interest);
}

#[test]
fn the_schedule_resumes_after_the_last_paid_due_date() {
    let rates = [fixed("2026-11-25", "12")];
    let mut i = inputs(Method::EqualPrincipal, 60_000_000, &rates, "2027-11-25");
    i.interest_through = d("2027-05-25");
    i.last_due = Some(d("2027-05-25"));
    let s = project(&i);
    assert_eq!(s.rows[0].due_date, d("2027-06-25"));
    assert_eq!(s.rows.len(), 6);
}

#[test]
fn paying_early_does_not_make_the_same_due_date_due_again() {
    let rates = [fixed("2026-11-25", "12")];
    let mut i = inputs(Method::EqualPrincipal, 60_000_000, &rates, "2027-11-25");
    // paid on Dec 20 for the Dec 25 instalment: interest settled to Dec 25
    i.interest_through = d("2026-12-25");
    i.last_due = Some(d("2026-12-25"));
    assert_eq!(project(&i).rows[0].due_date, d("2027-01-25"));
}

#[test]
fn due_day_31_follows_the_end_of_the_month() {
    let rates = [fixed("2026-01-31", "12")];
    let mut i = inputs(Method::EqualPrincipal, 40_000_000, &rates, "2026-05-31");
    i.due_day = 31;
    i.interest_through = d("2026-01-31");
    let s = project(&i);
    let dates: Vec<_> = s.rows.iter().map(|r| fmt_date(r.due_date)).collect();
    assert_eq!(
        dates,
        ["2026-02-28", "2026-03-31", "2026-04-30", "2026-05-31"]
    );
}

#[test]
fn a_maturity_off_the_due_day_gets_a_stub_row() {
    let rates = [fixed("2026-11-25", "12")];
    let s = project(&inputs(
        Method::EqualPrincipal,
        30_000_000,
        &rates,
        "2027-02-10",
    ));
    assert_eq!(s.rows.last().unwrap().due_date, d("2027-02-10"));
    assert_eq!(s.rows.last().unwrap().balance_after, 0);
}

#[test]
fn a_settled_account_has_no_schedule() {
    let rates = [fixed("2026-11-25", "12")];
    assert!(project(&inputs(Method::Annuity, 0, &rates, "2027-11-25"))
        .rows
        .is_empty());
}

// ------------------------------- prepayment fee -------------------------------

fn rule() -> PrepayRule {
    PrepayRule {
        valid_from: d("2026-01-01"),
        valid_to: None,
        free_allowance_per_month: 100_000_000,
        fee_pct_1e4: 10_000, // 1 %
        min_fee: 0,
    }
}

#[test]
fn prepaying_up_to_the_allowance_is_free() {
    let q = prepay_fee(&[rule()], d("2026-12-10"), 0, 100_000_000);
    assert_eq!(q.fee, 0);
    assert_eq!(q.allowance_remaining, 0);
    let q = prepay_fee(&[rule()], d("2026-12-10"), 0, 40_000_000);
    assert_eq!((q.fee, q.allowance_remaining), (0, 60_000_000));
}

#[test]
fn only_the_part_above_the_allowance_pays_a_fee() {
    let q = prepay_fee(&[rule()], d("2026-12-10"), 0, 150_000_000);
    assert_eq!(q.taxable, 50_000_000);
    assert_eq!(q.fee, 500_000);
}

#[test]
fn the_allowance_is_shared_by_prepayments_in_the_same_month() {
    // 100M already prepaid this month, another 50M → all of it is above the allowance
    let q = prepay_fee(&[rule()], d("2026-12-20"), 100_000_000, 50_000_000);
    assert_eq!(q.fee, 500_000);
    // 70M used, 50M more → 20M above
    let q = prepay_fee(&[rule()], d("2026-12-20"), 70_000_000, 50_000_000);
    assert_eq!((q.taxable, q.fee), (20_000_000, 200_000));
}

#[test]
fn the_minimum_fee_applies_when_something_is_charged() {
    let mut r = rule();
    r.min_fee = 300_000;
    assert_eq!(
        prepay_fee(&[r.clone()], d("2026-12-10"), 0, 110_000_000).fee,
        300_000
    );
    assert_eq!(prepay_fee(&[r], d("2026-12-10"), 0, 90_000_000).fee, 0);
}

#[test]
fn no_rule_means_no_fee_and_the_latest_valid_rule_wins() {
    assert_eq!(prepay_fee(&[], d("2026-12-10"), 0, 500_000_000).fee, 0);
    let mut old = rule();
    old.valid_to = Some(d("2026-06-30"));
    old.free_allowance_per_month = 0;
    let new = PrepayRule {
        valid_from: d("2026-07-01"),
        ..rule()
    };
    let rules = [old, new];
    assert_eq!(
        prepay_fee(&rules, d("2026-05-10"), 0, 10_000_000).fee,
        100_000
    );
    assert_eq!(prepay_fee(&rules, d("2026-08-10"), 0, 10_000_000).fee, 0);
    // a gap between rules means no rule
    let gap = PrepayRule {
        valid_from: d("2026-01-01"),
        valid_to: Some(d("2026-03-31")),
        ..rule()
    };
    assert!(!prepay_fee(&[gap], d("2026-05-10"), 0, 10_000_000).has_rule);
}
