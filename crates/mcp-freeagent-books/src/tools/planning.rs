//! Planning from the books: how much tax to keep back, whether a dividend can be paid and its
//! paperwork, a year-end pack for the accountant, and one period against the same one a year
//! earlier.

// Amounts in pence are grouped as pounds and pence: 50_000_00 is 50,000.00.
#![allow(clippy::inconsistent_digit_grouping)]

use super::checks::{LoanReviewParams, accounting_years, parse, people, pounds, today, ymd};
use super::{
    BooksServer, Caller, date_arg, date_range, json_result, money_arg, required_date, run, text_arg,
};
use crate::error::{self, FaError};
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use chrono::{Months, NaiveDate};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Corporation tax rates from 1 April 2023, in percent.
const SMALL_RATE: i128 = 19;
const MAIN_RATE: i128 = 25;
/// The limits for a 12-month period, in pence: 19% up to the lower, 25% from the upper.
const LOWER_LIMIT: i128 = 50_000_00;
const UPPER_LIMIT: i128 = 250_000_00;
/// Most associated companies accepted.
const MAX_ASSOCIATED: u32 = 100;
/// Most shareholders on one set of dividend paperwork.
const MAX_SHAREHOLDERS: usize = 50;
/// Most rows listed in one section of the accountant's pack.
const PACK_ROWS: usize = 200;
/// Most journals listed in the pack.
const PACK_JOURNALS: usize = 30;
/// Journals of at least this many pence are named in the pack's questions.
const LARGE_JOURNAL: i64 = 1_000_00;
/// Bank charges above this many pence in a year are queried.
const LARGE_BANK_CHARGES: i64 = 500_00;
/// Most categories listed by compare_periods.
const MOVERS: usize = 25;
/// Most expenses or bills read for the pack.
const PACK_ITEMS: usize = 2_000;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TaxSetAsideParams {
    /// The date to work to, YYYY-MM-DD. Default: today.
    pub as_at_date: Option<String>,
    /// How many other companies are associated with this one (under common control). They share the corporation tax limits. Default: 0.
    pub associated_companies: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Shareholder {
    /// The shareholder's name, as on the register of members.
    pub name: String,
    /// How many shares they hold.
    pub shares: u64,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DividendCheckParams {
    /// The dividend being considered, in total, e.g. "5000.00". Leave out to see the most that could be paid.
    pub amount: Option<String>,
    /// The date to check at, YYYY-MM-DD. Default: today.
    pub as_at_date: Option<String>,
    /// Companies associated with this one, for the corporation tax estimate. Default: 0.
    pub associated_companies: Option<u32>,
    /// Who holds the shares, to split the dividend. FreeAgent keeps no share register.
    pub shareholders: Option<Vec<Shareholder>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DividendPaperworkParams {
    /// The date the directors declare the dividend (the board meeting), YYYY-MM-DD.
    pub declared_on: String,
    /// The date it is paid, YYYY-MM-DD. Default: the declaration date.
    pub paid_on: Option<String>,
    /// The dividend in total, e.g. "5000.00". Give this or per_share.
    pub total: Option<String>,
    /// The dividend per share, up to four decimals, e.g. "50.00". Give this or total.
    pub per_share: Option<String>,
    /// Every shareholder of the class and their shares.
    pub shareholders: Vec<Shareholder>,
    /// The share class. Default: Ordinary.
    pub share_class: Option<String>,
    /// interim (declared by the directors, the usual case) or final (recommended by the directors for the shareholders to approve). Default: interim.
    pub kind: Option<String>,
    /// The directors at the meeting, by name. Default: a blank line to fill in.
    pub directors_present: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AccountantPackParams {
    /// The accounting year's end date, YYYY-MM-DD. Default: the last year that has ended.
    pub year_end: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ComparePeriodsParams {
    /// Start of the period, YYYY-MM-DD. Default: the start of the current accounting year.
    pub from_date: Option<String>,
    /// End of the period, YYYY-MM-DD. Default: today.
    pub to_date: Option<String>,
    /// Start of the period to compare with. Default: from_date a year earlier.
    pub compare_from_date: Option<String>,
    /// End of the period to compare with. Default: to_date a year earlier.
    pub compare_to_date: Option<String>,
}

/// Corporation tax in pence on `profit` pence for an accounting period of `days` days shared
/// with `associated` associated companies, and how it was worked out: 19% up to the lower limit,
/// 25% from the upper, and 25% less marginal relief (3/200 of the gap to the upper limit)
/// between. The limits are cut for a period shorter than a year and shared between associated
/// companies. Rates and limits are those from 1 April 2023.
pub(crate) fn corporation_tax(profit: i64, days: i64, associated: u32) -> (i64, &'static str) {
    if profit <= 0 {
        return (0, "no taxable profit");
    }
    let days = i128::from(days.clamp(1, 365));
    let share = 365 * (1 + i128::from(associated));
    let lower = LOWER_LIMIT * days / share;
    let upper = UPPER_LIMIT * days / share;
    let p = i128::from(profit);
    let (tax, how) = if p <= lower {
        (p * SMALL_RATE / 100, "small profits rate, 19%")
    } else if p >= upper {
        (p * MAIN_RATE / 100, "main rate, 25%")
    } else {
        (
            p * MAIN_RATE / 100 - (upper - p) * 3 / 200,
            "main rate, 25%, less marginal relief",
        )
    };
    (tax as i64, how)
}

/// The corporation tax limits in pence for a period of `days` days.
fn limits(days: i64, associated: u32) -> (i64, i64) {
    let days = i128::from(days.clamp(1, 365));
    let share = 365 * (1 + i128::from(associated));
    (
        (LOWER_LIMIT * days / share) as i64,
        (UPPER_LIMIT * days / share) as i64,
    )
}

/// `part` as a percentage of `whole`, to one decimal place.
fn percent(part: i64, whole: i64) -> Option<String> {
    if whole == 0 {
        return None;
    }
    let tenths = (i128::from(part) * 1000 / i128::from(whole)) as i64;
    let sign = if tenths < 0 { "-" } else { "" };
    let t = tenths.unsigned_abs();
    Some(format!("{sign}{}.{}", t / 10, t % 10))
}

/// The accounting year holding `at`, from the company's periods; else the year to `at`.
fn year_holding(company: &Value, at: NaiveDate) -> (NaiveDate, NaiveDate) {
    let years = accounting_years(company);
    if let Some(y) = years.iter().find(|(s, e)| *s <= at && at <= *e) {
        return *y;
    }
    // After the last period FreeAgent lists: the year after it, of the same length.
    if let Some((s, e)) = years.iter().max_by_key(|(_, e)| *e).copied()
        && e < at
    {
        let mut s = s;
        let mut e = e;
        while e < at {
            s = e.succ_opt().unwrap_or(e);
            e = e.checked_add_months(Months::new(12)).unwrap_or(at);
        }
        return (s, e);
    }
    let start = at
        .checked_sub_months(Months::new(12))
        .unwrap_or(at)
        .succ_opt()
        .unwrap_or(at);
    (
        start,
        start
            .checked_add_months(Months::new(12))
            .unwrap_or(at)
            .pred_opt()
            .unwrap_or(at),
    )
}

fn days(from: NaiveDate, to: NaiveDate) -> i64 {
    (to - from).num_days() + 1
}

/// The words of `name`, split on anything not a letter or digit.
fn has_word(name: &str, word: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

/// What the company owes on balance sheet accounts whose names match `is`, in pence (a credit
/// balance is owed; a debit balance, such as a VAT refund due, comes out negative).
fn owed_on(bs: &Value, is: impl Fn(&str) -> bool) -> Option<i64> {
    let mut found = false;
    let mut total = 0;
    for section in ["current_assets", "current_liabilities"] {
        for a in bs
            .pointer(&format!("/{section}/accounts"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = a.get("name").and_then(Value::as_str).unwrap_or_default();
            if is(name) {
                found = true;
                total -= pounds(a.get("total_debit_value")) * 100;
            }
        }
    }
    found.then_some(total)
}

fn is_corporation_tax(name: &str) -> bool {
    name.to_lowercase().contains("corporation tax")
}

fn is_vat(name: &str) -> bool {
    has_word(name, "VAT")
}

fn is_paye(name: &str) -> bool {
    has_word(name, "PAYE") || name.to_lowercase().contains("national insurance")
}

/// A money value of the profit and loss summary, in pence.
fn pl(v: &Value, key: &str) -> Option<i64> {
    pence(v.get(key))
}

/// Whether the profit and loss summary already deducts corporation tax.
fn tax_booked(pl_summary: &Value) -> bool {
    pl_summary
        .get("less")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|l| {
            l.get("title")
                .and_then(Value::as_str)
                .is_some_and(is_corporation_tax)
                && pence(l.get("total")).unwrap_or(0) != 0
        })
}

/// The base of a nominal code: `285` of `285` or `750-1`.
fn code_base(code: &str) -> Option<u32> {
    code.split('-').next()?.trim().parse().ok()
}

/// Which part of the profit and loss account a nominal code is in.
fn pl_group(base: u32) -> Option<&'static str> {
    match base {
        0..=99 => Some("income"),
        100..=199 => Some("cost_of_sales"),
        200..=599 => Some("expenses"),
        _ => None,
    }
}

/// A trial balance row's code, name and total in pence (debit positive).
fn tb_row(r: &Value) -> Option<(String, String, i64)> {
    let code = r
        .get("display_nominal_code")
        .or_else(|| r.get("nominal_code"))
        .and_then(Value::as_str)?
        .to_string();
    let name = r
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((code, name, pence(r.get("total")).unwrap_or(0)))
}

/// A decimal with up to four places as ten-thousandths.
fn ten_thousandths(name: &str, v: &str) -> error::Result<i64> {
    let t = super::decimal_arg(name, v)?;
    let negative = t.starts_with('-');
    let body = t.trim_start_matches('-');
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    let mut f = frac.to_string();
    while f.len() < 4 {
        f.push('0');
    }
    let n = whole.parse::<i64>().unwrap_or(0) * 10_000 + f.parse::<i64>().unwrap_or(0);
    if negative || n == 0 {
        return Err(FaError::invalid(format!("{name} must be more than zero")));
    }
    Ok(n)
}

/// Ten-thousandths of a pound as a decimal string with at least two places.
fn rate(n: i64) -> String {
    let s = format!("{}.{:04}", n / 10_000, n % 10_000);
    let trimmed = s.trim_end_matches('0');
    let (whole, frac) = trimmed.split_once('.').unwrap_or((trimmed, ""));
    format!("{whole}.{frac:0<2}")
}

/// Shareholders checked: names given, shares above zero, at most [`MAX_SHAREHOLDERS`].
fn holders(list: &[Shareholder]) -> error::Result<Vec<(String, u64)>> {
    if list.is_empty() || list.len() > MAX_SHAREHOLDERS {
        return Err(FaError::invalid(format!(
            "shareholders must list 1 to {MAX_SHAREHOLDERS} shareholders"
        )));
    }
    list.iter()
        .map(|h| {
            let name = super::required_text("shareholders.name", &h.name)?;
            if h.shares == 0 {
                return Err(FaError::invalid(format!(
                    "{name} must hold at least one share"
                )));
            }
            Ok((name, h.shares))
        })
        .collect()
}

/// `total` pence split by shares, any odd pence to the largest holding, so it adds up exactly.
fn split(total: i64, holders: &[(String, u64)]) -> Vec<i64> {
    let shares: i128 = holders.iter().map(|(_, s)| i128::from(*s)).sum();
    let mut out: Vec<i64> = holders
        .iter()
        .map(|(_, s)| (i128::from(total) * i128::from(*s) / shares) as i64)
        .collect();
    let left = total - out.iter().sum::<i64>();
    if let Some(i) = (0..holders.len()).max_by_key(|i| holders[*i].1) {
        out[i] += left;
    }
    out
}

fn assoc(v: Option<u32>) -> error::Result<u32> {
    let a = v.unwrap_or(0);
    if a > MAX_ASSOCIATED {
        return Err(FaError::invalid(format!(
            "associated_companies must be at most {MAX_ASSOCIATED}"
        )));
    }
    Ok(a)
}

fn at_date(v: Option<&str>) -> error::Result<NaiveDate> {
    Ok(date_arg("as_at_date", v)?
        .and_then(|d| parse(&d))
        .unwrap_or_else(today))
}

/// A category in compare_periods: name, group, this period and the earlier one, in pence.
type Compared = (String, &'static str, i64, i64);

/// The figures behind [`BooksServer::set_aside`], in pence.
struct SetAside {
    report: Value,
    profit_to_date: i64,
    tax_this_year: i64,
    tax_booked: bool,
    keep_back: i64,
    bank: i64,
    pl: Value,
    bs: Value,
}

impl BooksServer {
    async fn bank_total(&self, c: &Caller) -> error::Result<(i64, Vec<Value>)> {
        let (accounts, _) = c
            .all(
                "bank_accounts",
                &["bank_accounts"],
                "bank_accounts",
                Vec::new(),
                100,
            )
            .await?;
        let mut total = 0;
        let mut rows = Vec::new();
        for a in accounts {
            if a.get("status").and_then(Value::as_str) == Some("hidden") {
                continue;
            }
            let b = pence(a.get("current_balance")).unwrap_or(0);
            total += b;
            rows.push(json!({"name": a.get("name"), "balance": money(b)}));
        }
        Ok((total, rows))
    }

    async fn profit_and_loss(
        &self,
        c: &Caller,
        from: NaiveDate,
        to: NaiveDate,
    ) -> error::Result<Value> {
        let a = c
            .get(
                "profit_and_loss",
                &["accounting", "profit_and_loss", "summary"],
                &[("from_date", ymd(from)), ("to_date", ymd(to))],
            )
            .await?;
        Ok(view::unwrap(&a.body, "profit_and_loss_summary").clone())
    }

    async fn trial_balance(
        &self,
        c: &Caller,
        from: NaiveDate,
        to: NaiveDate,
    ) -> error::Result<Vec<Value>> {
        let a = c
            .get(
                "trial_balance",
                &["accounting", "trial_balance", "summary"],
                &[("from_date", ymd(from)), ("to_date", ymd(to))],
            )
            .await?;
        Ok(view::items(&a.body, "trial_balance_summary"))
    }

    async fn set_aside(
        &self,
        c: &Caller,
        at: NaiveDate,
        associated: u32,
    ) -> error::Result<SetAside> {
        let company = c.one("company", &["company"], "company", &[]).await?;
        let (start, end) = year_holding(&company, at);
        let to = at.min(end);
        let period_days = days(start, end);
        let elapsed = days(start, to).max(1);
        let mut unavailable = Map::new();
        let pl_summary = self.profit_and_loss(c, start, to).await?;
        let bs = self.balance_sheet(c, to).await?;
        let profit = pl(&pl_summary, "operating_profit").unwrap_or(0);
        let booked = tax_booked(&pl_summary);
        let (tax, how) = corporation_tax(profit, period_days, associated);
        let projected =
            i64::try_from(i128::from(profit) * i128::from(period_days) / i128::from(elapsed))
                .unwrap_or(i64::MAX);
        let (projected_tax, _) = corporation_tax(projected, period_days, associated);
        let (lower, upper) = limits(period_days, associated);
        let ct_owed = owed_on(&bs, is_corporation_tax).unwrap_or(0);
        let vat_owed = owed_on(&bs, is_vat);
        let paye_owed = owed_on(&bs, is_paye).unwrap_or(0);

        let registered = company
            .get("sales_tax_registration_status")
            .and_then(Value::as_str)
            == Some("Registered");
        let mut next_vat = Value::Null;
        if registered {
            match c.get("vat_returns", &["vat_returns"], &[]).await {
                Ok(a) => {
                    if let Some(r) = view::items(&a.body, "vat_returns")
                        .into_iter()
                        .filter(|r| r.get("filing_status").and_then(Value::as_str) != Some("filed"))
                        .min_by_key(|r| {
                            r.get("period_ends_on")
                                .and_then(Value::as_str)
                                .unwrap_or("9999")
                                .to_string()
                        })
                    {
                        next_vat = view::prune(json!({
                            "period_ends_on": r.get("period_ends_on"),
                            "filing_due_on": r.get("filing_due_on"),
                            "payments": r.get("payments"),
                        }))
                        .unwrap_or_default();
                    }
                }
                Err(e) => {
                    unavailable.insert(
                        "vat_return".into(),
                        Value::from(format!("{}: {e}", e.kind())),
                    );
                }
            }
        }
        let (bank, bank_rows) = match self.bank_total(c).await {
            Ok(b) => b,
            Err(e) => {
                unavailable.insert(
                    "bank_accounts".into(),
                    Value::from(format!("{}: {e}", e.kind())),
                );
                (0, Vec::new())
            }
        };
        let this_year = if booked { 0 } else { tax };
        let keep_back =
            this_year + ct_owed.max(0) + vat_owed.unwrap_or(0).max(0) + paye_owed.max(0);
        let mut report = json!({
            "as_at": ymd(to),
            "accounting_year": {"from": ymd(start), "to": ymd(end), "days_gone": elapsed, "days": period_days},
            "profit_to_date": money(profit),
            "corporation_tax": {
                "estimate_on_profit_to_date": money(tax),
                "worked_out_as": how,
                "effective_rate_percent": percent(tax, profit),
                "full_year_if_profit_continues": {"profit": money(projected), "tax": money(projected_tax)},
                "limits": {"lower": money(lower), "upper": money(upper), "associated_companies": associated},
                "already_in_profit_and_loss": booked,
                "owed_on_balance_sheet": money(ct_owed),
            },
            "vat": {
                "owed_on_balance_sheet": vat_owed.map(money),
                "next_return": next_vat,
            },
            "paye_and_ni_owed": money(paye_owed),
            "keep_back": {
                "corporation_tax_this_year": money(this_year),
                "corporation_tax_owed": money(ct_owed.max(0)),
                "vat": money(vat_owed.unwrap_or(0).max(0)),
                "paye_and_ni": money(paye_owed.max(0)),
                "total": money(keep_back),
            },
            "bank_balances": {"total": money(bank), "accounts": bank_rows},
            "left_after_keeping_back": money(bank - keep_back),
            "note": "Estimates. Corporation tax is worked out on the operating profit in the books, with no adjustment for disallowed costs, depreciation against capital allowances, losses brought forward or tax already paid in instalments; VAT, PAYE and corporation tax owed are FreeAgent's balance sheet figures to the nearest pound. Check with your accountant before relying on them.",
        });
        if !unavailable.is_empty() {
            report["unavailable"] = Value::Object(unavailable);
        }
        Ok(SetAside {
            report: view::prune(report).unwrap_or_default(),
            profit_to_date: profit,
            tax_this_year: this_year,
            tax_booked: booked,
            keep_back,
            bank,
            pl: pl_summary,
            bs,
        })
    }

    async fn dividend(&self, c: &Caller, p: &DividendCheckParams) -> error::Result<Value> {
        let at = at_date(p.as_at_date.as_deref())?;
        let amount = p
            .amount
            .as_deref()
            .map(|a| money_arg("amount", a))
            .transpose()?;
        let amount = amount.as_deref().and_then(|a| pence(Some(&Value::from(a))));
        if amount.is_some_and(|a| a <= 0) {
            return Err(FaError::invalid("amount must be more than zero"));
        }
        let holders = p.shareholders.as_deref().map(holders).transpose()?;
        let s = self
            .set_aside(c, at, assoc(p.associated_companies)?)
            .await?;

        // Reserves: the profit and loss account carried forward, after dividends already paid.
        let (reserves, source) = match pl(&s.pl, "retained_profit_carried_forward") {
            Some(r) => (
                Some(r),
                "retained_profit_carried_forward in the profit and loss summary",
            ),
            None => match (
                pl(&s.pl, "retained_profit_brought_forward"),
                pl(&s.pl, "retained_profit"),
            ) {
                (Some(b), Some(r)) => (
                    Some(b + r),
                    "retained profit brought forward plus this year's",
                ),
                _ => (None, "not found"),
            },
        };
        let distributable = reserves.map(|r| r - s.tax_this_year);
        let cash = s.bank - s.keep_back;
        let most = distributable.map(|d| d.min(cash).max(0));

        let mut flags: Vec<String> = Vec::new();
        match (amount, distributable) {
            (Some(a), Some(d)) if a > d => flags.push(format!(
                "{} is more than the {} of profits available: a dividend above distributable reserves is unlawful and the shareholders may have to repay it",
                money(a), money(d.max(0))
            )),
            (_, None) => flags.push("retained profits were not found in FreeAgent's profit and loss summary: work out distributable reserves with your accountant before paying a dividend".into()),
            _ => {}
        }
        if let Some(a) = amount
            && a > cash
        {
            flags.push(format!(
                "{} is more than the {} of cash left after keeping back tax",
                money(a),
                money(cash.max(0))
            ));
        }
        if !s.tax_booked && s.tax_this_year > 0 {
            flags.push(format!(
                "about {} of corporation tax on this year's profit is not in the books yet and has been taken off the profits available",
                money(s.tax_this_year)
            ));
        }

        let bs_people = people(&s.bs);
        let loans = self
            .loan_review(
                c,
                &LoanReviewParams {
                    as_at_date: Some(ymd(at)),
                },
            )
            .await
            .ok();
        let s455_of = |name: &str| -> Option<Value> {
            loans
                .as_ref()?
                .get("directors")?
                .as_array()?
                .iter()
                .find(|d| {
                    d.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                })?
                .get("s455")
                .cloned()
        };
        let mut split_rows = Vec::new();
        if let Some(h) = &holders {
            let total_shares: u64 = h.iter().map(|(_, s)| *s).sum();
            let each = split(amount.or(most).unwrap_or(0), h);
            for ((name, shares), part) in h.iter().zip(each) {
                let mut row = json!({"name": name, "shares": shares, "dividend": money(part)});
                if let Some(person) = bs_people
                    .values()
                    .find(|q| q.name.eq_ignore_ascii_case(name))
                {
                    let loan = person.loan * 100;
                    row["loan_account_now"] = Value::from(money(loan));
                    row["loan_account_if_credited"] = Value::from(money(loan - part));
                    if loan > 0 {
                        let mut note = format!(
                            "crediting the dividend to {name}'s loan account instead of paying it out would {} the {} they owe",
                            if part >= loan { "clear" } else { "reduce" },
                            money(loan)
                        );
                        if let Some(s455) = s455_of(name) {
                            if let Some(by) = s455.get("repay_by").and_then(Value::as_str) {
                                note.push_str(&format!(
                                    "; to count against s455 it must be declared and credited by {by}"
                                ));
                            }
                            row["s455"] = s455;
                        }
                        flags.push(note);
                    }
                }
                split_rows.push(row);
            }
            if total_shares == 0 {
                split_rows.clear();
            }
        }

        let mut out = json!({
            "as_at": ymd(at),
            "profits": {
                "retained_profits": reserves.map(money),
                "source": source,
                "corporation_tax_not_yet_booked": money(s.tax_this_year),
                "available_for_dividends": distributable.map(money),
                "profit_this_year_to_date": money(s.profit_to_date),
            },
            "cash": {
                "bank": money(s.bank),
                "keep_back_for_tax": money(s.keep_back),
                "available": money(cash),
            },
            "most_that_could_be_paid": most.map(money),
            "amount": amount.map(money),
            "ok": amount.is_some_and(|a| distributable.is_some_and(|d| a <= d) && a <= cash),
            "shareholders": split_rows,
            "flags": flags,
            "next_steps": "Hold a board meeting and minute the decision, give each shareholder a dividend voucher (draft_dividend_paperwork drafts both), then record the payment: explain the bank payment to the shareholder with the Dividend category, or for a dividend credited to a director's loan account, a journal debiting dividends and crediting the loan account.",
            "note": "Estimates from FreeAgent's figures. Distributable reserves are what the last accounts and the books since show; your accountant should confirm them, especially if the last accounts have not been finalised.",
        });
        if holders.is_none() {
            let names: Vec<&str> = bs_people
                .values()
                .map(|p| p.name.as_str())
                .filter(|n| !n.is_empty())
                .collect();
            out["shareholders"] = json!({
                "note": "FreeAgent keeps no share register: give shareholders with their shares to split the dividend.",
                "directors_in_the_books": names,
            });
        }
        if let Some(u) = s.report.get("unavailable") {
            out["unavailable"] = u.clone();
        }
        Ok(view::prune(out).unwrap_or_default())
    }

    async fn paperwork(&self, c: &Caller, p: &DividendPaperworkParams) -> error::Result<Value> {
        let declared = required_date("declared_on", &p.declared_on)?;
        let paid = date_arg("paid_on", p.paid_on.as_deref())?.unwrap_or_else(|| declared.clone());
        let holders = holders(&p.shareholders)?;
        let class =
            text_arg("share_class", p.share_class.as_deref())?.unwrap_or_else(|| "Ordinary".into());
        let kind =
            super::choice("kind", p.kind.as_deref(), &["interim", "final"])?.unwrap_or("interim");
        let total_shares: u64 = holders.iter().map(|(_, s)| *s).sum();
        let (per_share, amounts) = match (p.total.as_deref(), p.per_share.as_deref()) {
            (Some(t), None) => {
                let total = pence(Some(&Value::from(money_arg("total", t)?))).unwrap_or(0);
                if total <= 0 {
                    return Err(FaError::invalid("total must be more than zero"));
                }
                let per = (i128::from(total) * 100 / i128::from(total_shares)) as i64;
                (per, split(total, &holders))
            }
            (None, Some(r)) => {
                let per = ten_thousandths("per_share", r)?;
                let each = holders
                    .iter()
                    .map(|(_, s)| ((i128::from(per) * i128::from(*s) + 50) / 100) as i64)
                    .collect();
                (per, each)
            }
            _ => return Err(FaError::invalid("give total or per_share, not both")),
        };
        let total: i64 = amounts.iter().sum();
        let company = c.one("company", &["company"], "company", &[]).await?;
        let field = |k: &str| {
            company
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let name = field("name").unwrap_or("[company name]").to_string();
        let number = field("company_registration_number")
            .unwrap_or("[company number]")
            .to_string();
        let address: Vec<&str> = [
            "address1", "address2", "address3", "town", "region", "postcode",
        ]
        .iter()
        .filter_map(|k| field(k))
        .collect();
        let address = if address.is_empty() {
            "[registered office]".to_string()
        } else {
            address.join(", ")
        };
        let present = match p.directors_present.as_deref() {
            Some(d) if !d.is_empty() => d
                .iter()
                .map(|n| n.trim())
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            _ => "[names of the directors present]".into(),
        };
        let per = rate(per_share);
        let resolution = if kind == "interim" {
            format!(
                "It was resolved that an interim dividend of £{per} per {class} share be paid on {paid} to the shareholders on the register of members at the close of business on {declared}, £{} in all.",
                money(total)
            )
        } else {
            format!(
                "It was resolved to recommend to the shareholders a final dividend of £{per} per {class} share, £{} in all, to be paid on {paid} to the shareholders on the register of members at the close of business on {declared}, subject to their approval by ordinary resolution.",
                money(total)
            )
        };
        let minute = format!(
            "# {name}\n\nCompany number {number}\n\nRegistered office: {address}\n\n## Minutes of a meeting of the board of directors held on {declared}\n\nPresent: {present}\n\n1. The directors reviewed the company's management accounts to {declared} and were satisfied that the company has enough profits available for distribution, within Part 23 of the Companies Act 2006, to pay the dividend below and still meet its debts as they fall due.\n2. {resolution}\n3. There being no other business, the meeting closed.\n\nSigned: ______________________ Director\n\nDate: {declared}\n"
        );
        let vouchers: Vec<Value> = holders
            .iter()
            .zip(&amounts)
            .enumerate()
            .map(|(i, ((holder, shares), amount))| {
                json!({
                    "shareholder": holder,
                    "amount": money(*amount),
                    "text": format!(
                        "# Dividend voucher\n\n{name}, company number {number}\n\nRegistered office: {address}\n\nVoucher number: {}\n\nDate paid: {paid}\n\nShareholder: {holder}\n\nShares held: {shares} {class} shares\n\n{} dividend declared on {declared} at £{per} per share\n\nAmount paid: £{}\n\nSigned: ______________________ Director, for and on behalf of {name}\n",
                        i + 1,
                        if kind == "interim" { "Interim" } else { "Final" },
                        money(*amount)
                    ),
                })
            })
            .collect();
        Ok(json!({
            "total": money(total),
            "per_share": per,
            "board_minute": minute,
            "vouchers": vouchers,
            "note": "Drafts for you to check, sign and keep with the company's records; they are not stored in FreeAgent. Check dividend_check first: a dividend needs enough distributable profits. Give each shareholder their voucher.",
        }))
    }

    async fn pack(&self, c: &Caller, p: &AccountantPackParams) -> error::Result<Value> {
        let company = c.one("company", &["company"], "company", &[]).await?;
        let years = accounting_years(&company);
        let now = today();
        let (start, end, ended) = match date_arg("year_end", p.year_end.as_deref())? {
            Some(d) => {
                let e = parse(&d).unwrap_or(now);
                let y = years
                    .iter()
                    .find(|(_, ye)| *ye == e)
                    .copied()
                    .ok_or_else(|| {
                        let known: Vec<String> = years.iter().map(|(_, e)| ymd(*e)).collect();
                        FaError::invalid(format!(
                            "year_end must be the end of an accounting year: {}",
                            known.join(", ")
                        ))
                    })?;
                (y.0, y.1, y.1 < now)
            }
            None => match years
                .iter()
                .filter(|(_, e)| *e < now)
                .max_by_key(|(_, e)| *e)
            {
                Some((s, e)) => (*s, *e, true),
                None => {
                    let (s, e) = year_holding(&company, now);
                    (s, e, false)
                }
            },
        };
        let to = end.min(now);
        let mut out = Map::new();
        let mut questions: Vec<String> = Vec::new();
        let mut unavailable = Map::new();
        let mut note = |section: &str, e: FaError| {
            unavailable.insert(section.into(), Value::from(format!("{}: {e}", e.kind())));
        };
        out.insert(
            "company".into(),
            view::prune(json!({
                "name": company.get("name"),
                "company_registration_number": company.get("company_registration_number"),
                "vat_registration_status": company.get("sales_tax_registration_status"),
            }))
            .unwrap_or_default(),
        );
        out.insert(
            "year".into(),
            json!({"from": ymd(start), "to": ymd(end), "ended": ended}),
        );
        if !ended {
            questions.push(format!(
                "This accounting year ends on {}: the figures are to {} only",
                ymd(end),
                ymd(to)
            ));
        }

        match self.profit_and_loss(c, start, to).await {
            Ok(v) => {
                if ended && !tax_booked(&v) && pl(&v, "operating_profit").unwrap_or(0) > 0 {
                    questions.push("Corporation tax for the year is not in the books yet: will you work it out and post it?".into());
                }
                out.insert(
                    "profit_and_loss".into(),
                    view::prune(json!({
                        "income": v.get("income"),
                        "expenses": v.get("expenses"),
                        "operating_profit": v.get("operating_profit"),
                        "less": v.get("less"),
                        "retained_profit": v.get("retained_profit"),
                        "retained_profit_brought_forward": v.get("retained_profit_brought_forward"),
                        "retained_profit_carried_forward": v.get("retained_profit_carried_forward"),
                    }))
                    .unwrap_or_default(),
                );
            }
            Err(e) => note("profit_and_loss", e),
        }

        match self.balance_sheet(c, to).await {
            Ok(bs) => {
                let mut headlines = Map::new();
                let mut accounts = Vec::new();
                if let Some(o) = bs.as_object() {
                    for (k, v) in o {
                        match v {
                            Value::Object(section) => {
                                let mut s = Map::new();
                                for (sk, sv) in section {
                                    if sk == "accounts" {
                                        for a in sv.as_array().into_iter().flatten() {
                                            accounts.push(json!({
                                                "section": k,
                                                "code": a.get("nominal_code"),
                                                "name": a.get("name"),
                                                "debit": a.get("total_debit_value"),
                                            }));
                                        }
                                    } else if !sv.is_object() && !sv.is_array() {
                                        s.insert(sk.clone(), sv.clone());
                                    }
                                }
                                if !s.is_empty() {
                                    headlines.insert(k.clone(), Value::Object(s));
                                }
                            }
                            Value::Array(_) => {}
                            other => {
                                headlines.insert(k.clone(), other.clone());
                            }
                        }
                    }
                }
                accounts.truncate(PACK_ROWS);
                out.insert(
                    "balance_sheet".into(),
                    json!({"as_at": ymd(to), "headlines": headlines, "accounts": accounts}),
                );
            }
            Err(e) => note("balance_sheet", e),
        }

        match self.trial_balance(c, start, to).await {
            Ok(rows) => {
                let mut list = Vec::new();
                let mut suspense = 0;
                let mut bank_charges = 0;
                for (code, name, total) in rows.iter().filter_map(tb_row) {
                    if total == 0 {
                        continue;
                    }
                    let base = code_base(&code);
                    if base == Some(999) {
                        suspense += total;
                    }
                    if name.to_lowercase().contains("bank charge") {
                        bank_charges += total;
                    }
                    match base.and_then(pl_group) {
                        Some("income") if total > 0 => questions.push(format!(
                            "{code} {name} has a debit balance of {}: income should be a credit, so something may be misposted",
                            money(total)
                        )),
                        Some("cost_of_sales" | "expenses") if total < 0 => questions.push(format!(
                            "{code} {name} has a credit balance of {}: a cost should be a debit, so something may be misposted",
                            money(-total)
                        )),
                        _ => {}
                    }
                    list.push(json!({"code": code, "name": name, "total": money(total)}));
                }
                if suspense != 0 {
                    questions.push(format!(
                        "{} is in 999 Suspense: what should it be?",
                        money(suspense)
                    ));
                }
                if bank_charges > LARGE_BANK_CHARGES {
                    questions.push(format!(
                        "Bank charges come to {} for the year: is anything in them, such as a balance correction, really something else?",
                        money(bank_charges)
                    ));
                }
                let truncated = list.len() > PACK_ROWS;
                list.truncate(PACK_ROWS);
                let mut tb = json!({"rows": list, "suspense": money(suspense)});
                if truncated {
                    tb["truncated"] = Value::from(true);
                }
                out.insert("trial_balance".into(), tb);
            }
            Err(e) => note("trial_balance", e),
        }

        match self
            .loan_review(
                c,
                &LoanReviewParams {
                    as_at_date: Some(ymd(to)),
                },
            )
            .await
        {
            Ok(v) => {
                for d in v
                    .get("directors")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let who = d
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("A director");
                    for f in d
                        .get("flags")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(f) = f.as_str() {
                            questions.push(format!("{who}: {f}"));
                        }
                    }
                }
                out.insert(
                    "directors_loans".into(),
                    v.get("directors").cloned().unwrap_or_default(),
                );
            }
            Err(e) => note("directors_loans", e),
        }

        let period = vec![("from_date", ymd(start)), ("to_date", ymd(to))];
        match c
            .all(
                "journal_sets",
                &["journal_sets"],
                "journal_sets",
                period.clone(),
                PACK_ITEMS,
            )
            .await
        {
            Ok((items, _)) => {
                let mut rows = Vec::new();
                for j in &items {
                    let size: i64 = j
                        .get("journal_entries")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|e| pence(e.get("debit_value")))
                        .filter(|v| *v > 0)
                        .sum();
                    let desc = j
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("(no description)");
                    let date = j.get("dated_on").and_then(Value::as_str).unwrap_or("");
                    if size >= LARGE_JOURNAL {
                        questions.push(format!(
                            "Journal \"{desc}\" on {date} moves {}: is it right, and is there evidence for it?",
                            money(size)
                        ));
                    }
                    rows.push(
                        view::prune(json!({
                            "id": j.get("url").and_then(Value::as_str).and_then(view::id_of),
                            "dated_on": j.get("dated_on"),
                            "description": j.get("description"),
                            "tag": j.get("tag"),
                            "debits": (size > 0).then(|| money(size)),
                        }))
                        .unwrap_or_default(),
                    );
                }
                let count = rows.len();
                rows.truncate(PACK_JOURNALS);
                out.insert("journals".into(), json!({"count": count, "items": rows}));
            }
            Err(e) => note("journals", e),
        }

        let mut open = Map::new();
        match c
            .all(
                "bank_accounts",
                &["bank_accounts"],
                "bank_accounts",
                Vec::new(),
                100,
            )
            .await
        {
            Ok((accounts, _)) => {
                let n: u64 = accounts
                    .iter()
                    .filter(|a| a.get("status").and_then(Value::as_str) != Some("hidden"))
                    .filter_map(|a| {
                        a.get("unexplained_transaction_count")
                            .and_then(Value::as_u64)
                    })
                    .sum();
                if n > 0 {
                    questions.push(format!(
                        "{n} bank transaction(s) are still unexplained (all dates)"
                    ));
                }
                open.insert("unexplained_bank_transactions_now".into(), Value::from(n));
            }
            Err(e) => note("bank_accounts", e),
        }
        match c
            .all(
                "expenses",
                &["expenses"],
                "expenses",
                period.clone(),
                PACK_ITEMS,
            )
            .await
        {
            Ok((items, _)) => {
                let missing: Vec<&Value> = items
                    .iter()
                    .filter(|e| e.get("attachment").is_none() && e.get("mileage").is_none())
                    .collect();
                let vat: i64 = missing
                    .iter()
                    .filter_map(|e| pence(e.get("sales_tax_value")))
                    .sum();
                if !missing.is_empty() {
                    questions.push(format!(
                        "{} expense(s) in the year have no receipt, reclaiming {} of VAT",
                        missing.len(),
                        money(vat.abs())
                    ));
                }
                open.insert(
                    "expenses_without_receipts".into(),
                    json!({"count": missing.len(), "vat_reclaimed": money(vat.abs())}),
                );
            }
            Err(e) => note("expenses", e),
        }
        match c
            .all("bills", &["bills"], "bills", period, PACK_ITEMS)
            .await
        {
            Ok((items, _)) => {
                let n = items
                    .iter()
                    .filter(|b| b.get("attachment").is_none())
                    .count();
                open.insert("bills_without_invoice".into(), Value::from(n));
            }
            Err(e) => note("bills", e),
        }
        out.insert("open_items".into(), Value::Object(open));
        out.insert("questions_for_accountant".into(), Value::from(questions));
        if !unavailable.is_empty() {
            out.insert("unavailable".into(), Value::Object(unavailable));
        }
        out.insert(
            "note".into(),
            Value::from("A summary from FreeAgent to send your accountant with the questions; the balance sheet is to the nearest pound. Bank, receipt and bill counts are FreeAgent's own and the questions are prompts to check, not findings."),
        );
        Ok(Value::Object(out))
    }

    async fn compare(&self, c: &Caller, p: &ComparePeriodsParams) -> error::Result<Value> {
        let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
        let (cf, ct) = date_range(p.compare_from_date.as_deref(), p.compare_to_date.as_deref())?;
        let now = today();
        let to = to.and_then(|d| parse(&d)).unwrap_or(now);
        let from = match from.and_then(|d| parse(&d)) {
            Some(f) => f,
            None => {
                let company = c.one("company", &["company"], "company", &[]).await?;
                year_holding(&company, to).0
            }
        };
        if from > to {
            return Err(FaError::invalid("from_date must not be after to_date"));
        }
        let back = |d: NaiveDate| d.checked_sub_months(Months::new(12)).unwrap_or(d);
        let (cf, ct) = match (cf.and_then(|d| parse(&d)), ct.and_then(|d| parse(&d))) {
            (Some(f), Some(t)) => (f, t),
            (None, None) => (back(from), back(to)),
            _ => {
                return Err(FaError::invalid(
                    "give compare_from_date and compare_to_date together, or neither",
                ));
            }
        };
        let this = self.trial_balance(c, from, to).await?;
        let last = self.trial_balance(c, cf, ct).await?;
        // Shown the natural way round: income as a positive credit, costs as a positive debit.
        let mut rows: std::collections::BTreeMap<String, Compared> =
            std::collections::BTreeMap::new();
        for (which, list) in [(0, &this), (1, &last)] {
            for (code, name, total) in list.iter().filter_map(tb_row) {
                let Some(group) = code_base(&code).and_then(pl_group) else {
                    continue;
                };
                let shown = if group == "income" { -total } else { total };
                let r = rows.entry(code).or_insert((name, group, 0, 0));
                if which == 0 {
                    r.2 += shown;
                } else {
                    r.3 += shown;
                }
            }
        }
        let mut totals: std::collections::BTreeMap<&str, (i64, i64)> =
            std::collections::BTreeMap::new();
        for (_, group, a, b) in rows.values() {
            let t = totals.entry(group).or_default();
            t.0 += a;
            t.1 += b;
        }
        let get = |g: &str| totals.get(g).copied().unwrap_or_default();
        let (inc, cos, exp) = (get("income"), get("cost_of_sales"), get("expenses"));
        let profit = (inc.0 - cos.0 - exp.0, inc.1 - cos.1 - exp.1);
        let line = |(a, b): (i64, i64)| json!({"this": money(a), "last": money(b), "change": money(a - b), "change_percent": percent(a - b, b)});
        let mut movers: Vec<(&String, &Compared)> =
            rows.iter().filter(|(_, r)| r.2 != r.3).collect();
        movers.sort_by_key(|(_, r)| std::cmp::Reverse((r.2 - r.3).abs()));
        let compared = rows.len();
        let listed: Vec<Value> = movers
            .iter()
            .take(MOVERS)
            .map(|(code, (name, group, a, b))| {
                view::prune(json!({
                    "code": code,
                    "name": name,
                    "group": group,
                    "this": money(*a),
                    "last": money(*b),
                    "change": money(a - b),
                    "change_percent": percent(a - b, *b),
                }))
                .unwrap_or_default()
            })
            .collect();
        Ok(json!({
            "this": {"from": ymd(from), "to": ymd(to)},
            "last": {"from": ymd(cf), "to": ymd(ct)},
            "totals": {
                "income": line(inc),
                "cost_of_sales": line(cos),
                "expenses": line(exp),
                "profit": line(profit),
            },
            "biggest_changes": listed,
            "categories_compared": compared,
            "categories_changed": movers.len(),
            "note": "From FreeAgent's trial balance: income and costs as positive figures, profit before tax and dividends. change_percent is against the earlier period.",
        }))
    }
}

#[tool_router(router = planning_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "How much to keep back for tax, at a date (default today): an estimate of corporation tax on this accounting year's profit so far (19% up to 50,000, 25% from 250,000, marginal relief between, limits cut for short years and shared with associated companies) and if the profit continues for the year, plus corporation tax, VAT and PAYE already owed on the balance sheet and the next VAT return; then the bank balances and what is left after keeping it back. Estimates only. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn tax_set_aside(
        &self,
        Parameters(p): Parameters<TaxSetAsideParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("tax_set_aside", async {
            let c = self.caller(&parts)?;
            let at = at_date(p.as_at_date.as_deref())?;
            let s = self
                .set_aside(&c, at, assoc(p.associated_companies)?)
                .await?;
            Ok(json_result(&s.report))
        })
        .await
    }

    #[tool(
        description = "Whether a dividend can be paid, at a date (default today): retained profits from the profit and loss account less corporation tax not yet booked (distributable reserves), the cash left after keeping back tax, the most that could be paid, and for an amount, whether it fits. With shareholders and their shares, each one's part and the effect on their director's loan account if it is credited there, with the s455 deadline. Flags an amount above the profits available. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn dividend_check(
        &self,
        Parameters(p): Parameters<DividendCheckParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("dividend_check", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.dividend(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "Draft the paperwork for a dividend: a board minute declaring it (interim, or a final dividend recommended to the shareholders) and a dividend voucher for each shareholder, with the company's name, number and registered office from FreeAgent. Give total or per_share and every shareholder with their shares. Returns text to check and sign; stores nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn draft_dividend_paperwork(
        &self,
        Parameters(p): Parameters<DividendPaperworkParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("draft_dividend_paperwork", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.paperwork(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "A year-end pack for the company's accountant, in one call, for an accounting year (default the last one that ended): profit and loss and balance sheet headlines, the trial balance, each director's loan account with s455, journals posted in the year, open items (unexplained bank transactions, expenses without receipts, bills without invoices, suspense) and a list of questions for the accountant, such as misposted-looking balances and large journals. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn accountant_pack(
        &self,
        Parameters(p): Parameters<AccountantPackParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("accountant_pack", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.pack(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "Profit and loss for a period against the same period a year earlier (default: this accounting year to date against the same dates last year): income, cost of sales, expenses and profit with the change, and the categories that changed most, by amount and percentage. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn compare_periods(
        &self,
        Parameters(p): Parameters<ComparePeriodsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("compare_periods", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.compare(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corporation_tax_follows_the_rates_and_marginal_relief() {
        // HMRC's worked figures for a 12-month period with no associated companies.
        assert_eq!(
            corporation_tax(100_000_00, 365, 0),
            (22_750_00, "main rate, 25%, less marginal relief")
        );
        assert_eq!(corporation_tax(40_000_00, 365, 0).0, 7_600_00);
        assert_eq!(corporation_tax(300_000_00, 365, 0).0, 75_000_00);
        assert_eq!(corporation_tax(50_000_00, 365, 0).0, 9_500_00);
        assert_eq!(corporation_tax(250_000_00, 366, 0).0, 62_500_00);
        assert_eq!(corporation_tax(-5_00, 365, 0).0, 0);
        // One associated company halves the limits: 40,000 is then in the marginal band.
        assert_eq!(limits(365, 1), (25_000_00, 125_000_00));
        assert_eq!(
            corporation_tax(40_000_00, 365, 1).0,
            10_000_00 - 85_000_00 * 3 / 200
        );
        // A six-month period halves them too.
        assert_eq!(limits(182, 0).0, 50_000_00 * 182 / 365);
    }

    #[test]
    fn helpers() {
        assert_eq!(percent(25, 200).as_deref(), Some("12.5"));
        assert_eq!(percent(-1, 3).as_deref(), Some("-33.3"));
        assert_eq!(percent(1, 0), None);
        assert_eq!(rate(500_000), "50.00");
        assert_eq!(rate(12_345), "1.2345");
        assert_eq!(ten_thousandths("x", "0.5").unwrap(), 5_000);
        assert!(ten_thousandths("x", "0").is_err());
        let h = vec![("A".to_string(), 1), ("B".to_string(), 2)];
        assert_eq!(split(100_00, &h), vec![33_33, 66_67]);
        assert!(is_vat("VAT") && is_vat("VAT Liability") && !is_vat("Private healthcare"));
        assert!(is_paye("PAYE/NI") && is_corporation_tax("Corporation Tax"));
        assert_eq!(pl_group(code_base("285").unwrap()), Some("expenses"));
        assert_eq!(pl_group(code_base("750-1").unwrap()), None);
        assert_eq!(pl_group(code_base("001").unwrap()), Some("income"));
    }

    #[test]
    fn years_carry_on_after_the_last_listed_period() {
        let c = json!({"annual_accounting_periods": [
            {"starts_on": "2024-10-01", "ends_on": "2025-09-30"}
        ]});
        let d = |s: &str| parse(s).unwrap();
        assert_eq!(
            year_holding(&c, d("2025-01-01")),
            (d("2024-10-01"), d("2025-09-30"))
        );
        assert_eq!(
            year_holding(&c, d("2026-11-08")),
            (d("2026-10-01"), d("2027-09-30"))
        );
    }
}
