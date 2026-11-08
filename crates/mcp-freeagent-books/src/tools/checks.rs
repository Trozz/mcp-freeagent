//! Reviews that read across the books: what to tidy before a month end or VAT return, and where
//! each director's loan account stands.

use super::{BooksServer, Caller, Query, date_range, json_result, run};
use crate::error::{self, FaError};
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use chrono::{Datelike, Months, NaiveDate};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Most expenses, bills or invoices read for one check.
const CHECK_ITEMS: usize = 1_000;
/// Most items listed in one section of a check; the count covers them all.
const LISTED: usize = 25;
/// Draft invoices older than this many days are flagged.
const STALE_DRAFT_DAYS: i64 = 30;
/// The s455 charge on a director's loan still owed nine months after the year end, for loans
/// made on or after 6 April 2022.
const S455_RATE_PERCENT: i64 = 3375; // 33.75%, in hundredths of a percent
/// A director's loan above this at any time in a tax year can be a taxable benefit in kind.
const BIK_THRESHOLD_POUNDS: i64 = 10_000;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MonthEndParams {
    /// Start of the period to check, YYYY-MM-DD. Default: the next VAT return's period, or last month for a company not registered for VAT.
    pub from_date: Option<String>,
    /// End of the period, YYYY-MM-DD; with from_date.
    pub to_date: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LoanReviewParams {
    /// The date to review at, YYYY-MM-DD. Default: today.
    pub as_at_date: Option<String>,
}

pub(crate) fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

pub(crate) fn parse(d: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
}

pub(crate) fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// The first and last day of the month before `d`.
fn last_month(d: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first_this = d.with_day(1).unwrap_or(d);
    let last = first_this.pred_opt().unwrap_or(first_this);
    (last.with_day(1).unwrap_or(last), last)
}

/// The company's accounting years as (start, end), from `annual_accounting_periods`.
pub(crate) fn accounting_years(company: &Value) -> Vec<(NaiveDate, NaiveDate)> {
    company
        .get("annual_accounting_periods")
        .and_then(Value::as_array)
        .map(|ps| {
            ps.iter()
                .filter_map(|p| {
                    let s = parse(p.get("starts_on")?.as_str()?)?;
                    let e = parse(p.get("ends_on")?.as_str()?)?;
                    Some((s, e))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A whole-pound value from a report (FreeAgent's balance sheet rounds to whole units).
pub(crate) fn pounds(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .map(|f| f.round() as i64)
            .unwrap_or(0),
        _ => 0,
    }
}

/// One person's accounts with the company, from the balance sheet, in whole pounds. Debit
/// (positive) means the person owes the company.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Person {
    pub(crate) name: String,
    /// 907-n Director Loan Account.
    pub(crate) loan: i64,
    /// 905-n Expense Account: negative when the company owes them expenses.
    pub(crate) expenses: i64,
    /// 902-n Salary and Bonuses: negative when salary is owed to them.
    pub(crate) salary: i64,
}

/// Every person with a loan, expense or salary account on balance sheet `bs`, by their account
/// suffix (907-1 is person 1).
pub(crate) fn people(bs: &Value) -> BTreeMap<String, Person> {
    let mut out: BTreeMap<String, Person> = BTreeMap::new();
    for section in ["current_assets", "current_liabilities"] {
        let Some(accounts) = bs
            .pointer(&format!("/{section}/accounts"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for a in accounts {
            let Some((base, n)) = a
                .get("nominal_code")
                .and_then(Value::as_str)
                .and_then(|c| c.split_once('-'))
            else {
                continue;
            };
            if !matches!(base, "907" | "905" | "902") {
                continue;
            }
            let p = out.entry(n.to_string()).or_default();
            if let Some(name) = a
                .get("name")
                .and_then(Value::as_str)
                .and_then(|s| s.split_once(": "))
                .map(|(_, n)| n.trim())
                && (p.name.is_empty() || base == "907")
            {
                p.name = name.to_string();
            }
            let v = pounds(a.get("total_debit_value"));
            match base {
                "907" => p.loan += v,
                "905" => p.expenses += v,
                _ => p.salary += v,
            }
        }
    }
    out
}

/// The s455 charge on `outstanding` whole pounds, as a decimal string.
fn s455(outstanding: i64) -> String {
    money(outstanding * 100 * S455_RATE_PERCENT / 10_000)
}

impl BooksServer {
    pub(crate) async fn balance_sheet(&self, c: &Caller, as_at: NaiveDate) -> error::Result<Value> {
        let a = c
            .get(
                "balance_sheet",
                &["accounting", "balance_sheet"],
                &[("as_at_date", ymd(as_at))],
            )
            .await?;
        Ok(view::unwrap(&a.body, "balance_sheet").clone())
    }

    async fn month_end(&self, c: &Caller, p: &MonthEndParams) -> error::Result<Value> {
        let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
        let mut out = Map::new();
        let mut todo: Vec<String> = Vec::new();
        let mut unavailable = Map::new();
        let mut note = |section: &str, e: FaError| {
            unavailable.insert(section.into(), Value::from(format!("{}: {e}", e.kind())));
        };
        let company = c.one("company", &["company"], "company", &[]).await?;
        let registered = company
            .get("sales_tax_registration_status")
            .and_then(Value::as_str)
            == Some("Registered");

        // The period: as given, else the next VAT return's, else last month.
        let mut vat_return: Option<Value> = None;
        if registered {
            match c.get("vat_returns", &["vat_returns"], &[]).await {
                Ok(a) => {
                    let returns = view::items(&a.body, "vat_returns");
                    vat_return = match (&from, &to) {
                        (Some(_), Some(t)) => returns
                            .iter()
                            .find(|r| r.get("period_ends_on").and_then(Value::as_str) == Some(t))
                            .cloned(),
                        _ => returns
                            .iter()
                            .filter(|r| {
                                r.get("filing_status").and_then(Value::as_str) != Some("filed")
                            })
                            .min_by_key(|r| {
                                r.get("period_ends_on")
                                    .and_then(Value::as_str)
                                    .unwrap_or("9999")
                                    .to_string()
                            })
                            .cloned(),
                    };
                }
                Err(e) => note("vat_return", e),
            }
        }
        let (from, to, basis) = match (from, to) {
            (Some(f), Some(t)) => (f, t, "as asked"),
            (None, None) => match vat_return.as_ref().and_then(|r| {
                Some((
                    r.get("period_starts_on")?.as_str()?.to_string(),
                    r.get("period_ends_on")?.as_str()?.to_string(),
                ))
            }) {
                Some((f, t)) => (f, t, "the next VAT return"),
                None => {
                    let (f, t) = last_month(today());
                    (ymd(f), ymd(t), "last month")
                }
            },
            _ => {
                return Err(FaError::invalid(
                    "give from_date and to_date together, or neither",
                ));
            }
        };
        out.insert(
            "period".into(),
            json!({"from": from, "to": to, "basis": basis}),
        );
        if let Some(r) = &vat_return {
            out.insert(
                "vat_return".into(),
                view::prune(json!({
                    "period_ends_on": r.get("period_ends_on"),
                    "filing_due_on": r.get("filing_due_on"),
                    "filing_status": r.get("filing_status"),
                    "payments": r.get("payments"),
                }))
                .unwrap_or_default(),
            );
        }
        let period: Query = vec![("from_date", from.clone()), ("to_date", to.clone())];

        // Bank: FreeAgent counts unexplained transactions and guesses awaiting approval itself.
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
                let mut rows = Vec::new();
                for a in &accounts {
                    if a.get("status").and_then(Value::as_str) == Some("hidden") {
                        continue;
                    }
                    let n = a
                        .get("unexplained_transaction_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let r = a
                        .get("marked_for_review_count")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if n == 0 && r == 0 {
                        continue;
                    }
                    let name = a
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("a bank account");
                    if n > 0 {
                        todo.push(format!("Explain {n} transaction(s) in {name}"));
                    }
                    if r > 0 {
                        todo.push(format!(
                            "Approve or correct {r} guessed explanation(s) in {name}"
                        ));
                    }
                    rows.push(json!({
                        "id": a.get("url").and_then(Value::as_str).and_then(view::id_of),
                        "name": name,
                        "unexplained_transactions": n,
                        "explanations_to_approve": r,
                    }));
                }
                out.insert("bank_accounts_to_finish".into(), Value::Array(rows));
            }
            Err(e) => note("bank_accounts_to_finish", e),
        }

        // Expenses dated in the period with no receipt (mileage needs none).
        match c
            .all(
                "expenses",
                &["expenses"],
                "expenses",
                period.clone(),
                CHECK_ITEMS,
            )
            .await
        {
            Ok((items, more)) => {
                let missing: Vec<&Value> = items
                    .iter()
                    .filter(|e| e.get("attachment").is_none() && e.get("mileage").is_none())
                    .collect();
                let vat: i64 = missing
                    .iter()
                    .filter_map(|e| pence(e.get("sales_tax_value")))
                    .sum();
                if !missing.is_empty() {
                    todo.push(format!(
                        "Add receipts to {} expense(s){}",
                        missing.len(),
                        if vat != 0 {
                            format!(", which reclaim {} of VAT between them", money(vat.abs()))
                        } else {
                            String::new()
                        }
                    ));
                }
                let mut v = json!({
                    "count": missing.len(),
                    "vat_reclaimed_without_receipt": money(vat.abs()),
                    "items": missing.iter().take(LISTED).map(|e| view::prune(json!({
                        "id": e.get("url").and_then(Value::as_str).and_then(view::id_of),
                        "dated_on": e.get("dated_on"),
                        "description": e.get("description"),
                        "gross_value": e.get("gross_value"),
                        "user": e.get("user"),
                    })).unwrap_or_default()).collect::<Vec<_>>(),
                });
                if more {
                    v["truncated"] = Value::from(true);
                }
                out.insert("expenses_without_receipts".into(), v);
            }
            Err(e) => note("expenses_without_receipts", e),
        }

        // Bills dated in the period with no invoice attached.
        match c
            .all("bills", &["bills"], "bills", period.clone(), CHECK_ITEMS)
            .await
        {
            Ok((items, more)) => {
                let missing: Vec<&Value> = items
                    .iter()
                    .filter(|b| b.get("attachment").is_none())
                    .collect();
                if !missing.is_empty() {
                    todo.push(format!(
                        "Attach the supplier's invoice to {} bill(s)",
                        missing.len()
                    ));
                }
                let mut v = json!({
                    "count": missing.len(),
                    "items": missing.iter().take(LISTED).map(|b| view::prune(json!({
                        "id": b.get("url").and_then(Value::as_str).and_then(view::id_of),
                        "reference": b.get("reference"),
                        "contact_name": b.get("contact_name"),
                        "dated_on": b.get("dated_on"),
                        "total_value": b.get("total_value"),
                    })).unwrap_or_default()).collect::<Vec<_>>(),
                });
                if more {
                    v["truncated"] = Value::from(true);
                }
                out.insert("bills_without_invoice".into(), v);
            }
            Err(e) => note("bills_without_invoice", e),
        }

        // Draft invoices: in the period they belong in the return; old ones are usually forgotten.
        let stale_before = ymd(today() - chrono::Duration::days(STALE_DRAFT_DAYS));
        match c
            .all(
                "invoices",
                &["invoices"],
                "invoices",
                vec![("view", "draft".into())],
                CHECK_ITEMS,
            )
            .await
        {
            Ok((items, _)) => {
                let date = |i: &Value| {
                    i.get("dated_on")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                let in_period: Vec<&Value> = items
                    .iter()
                    .filter(|i| {
                        let d = date(i);
                        d >= from && d <= to
                    })
                    .collect();
                let stale: Vec<&Value> = items.iter().filter(|i| date(i) < stale_before).collect();
                if !in_period.is_empty() {
                    todo.push(format!(
                        "Send or delete {} draft invoice(s) dated in the period",
                        in_period.len()
                    ));
                }
                if !stale.is_empty() {
                    todo.push(format!(
                        "Review {} draft invoice(s) more than {STALE_DRAFT_DAYS} days old",
                        stale.len()
                    ));
                }
                let row = |i: &&Value| {
                    view::prune(json!({
                        "id": i.get("url").and_then(Value::as_str).and_then(view::id_of),
                        "reference": i.get("reference"),
                        "contact_name": i.get("contact_name"),
                        "dated_on": i.get("dated_on"),
                        "total_value": i.get("total_value"),
                    }))
                    .unwrap_or_default()
                };
                out.insert(
                    "draft_invoices".into(),
                    json!({
                        "in_period": in_period.iter().map(row).collect::<Vec<_>>(),
                        "older_than_30_days": {
                            "count": stale.len(),
                            "items": stale.iter().take(LISTED).map(row).collect::<Vec<_>>(),
                        },
                    }),
                );
            }
            Err(e) => note("draft_invoices", e),
        }

        // Overdue each way.
        for (key, endpoint, label) in [
            ("overdue_invoices", "invoices", "invoice"),
            ("overdue_bills", "bills", "bill"),
        ] {
            match c
                .all(
                    endpoint,
                    &[endpoint],
                    endpoint,
                    vec![("view", "overdue".into())],
                    CHECK_ITEMS,
                )
                .await
            {
                Ok((items, _)) => {
                    let due: i64 = items.iter().filter_map(|i| pence(i.get("due_value"))).sum();
                    if !items.is_empty() {
                        todo.push(if label == "invoice" {
                            format!(
                                "Chase {} overdue invoice(s), {} in all",
                                items.len(),
                                money(due)
                            )
                        } else {
                            format!(
                                "Pay or check {} overdue bill(s), {} in all",
                                items.len(),
                                money(due)
                            )
                        });
                    }
                    out.insert(key.into(), json!({"count": items.len(), "due": money(due)}));
                }
                Err(e) => note(key, e),
            }
        }

        // Suspense, over the accounting year to the period's end.
        let to_date = parse(&to).unwrap_or_else(today);
        let year_start = accounting_years(&company)
            .into_iter()
            .find(|(s, e)| *s <= to_date && to_date <= *e)
            .map(|(s, _)| s)
            .unwrap_or_else(|| to_date.with_ordinal(1).unwrap_or(to_date));
        match c
            .get(
                "trial_balance",
                &["accounting", "trial_balance", "summary"],
                &[("from_date", ymd(year_start)), ("to_date", to.clone())],
            )
            .await
        {
            Ok(a) => {
                let suspense: i64 = view::items(&a.body, "trial_balance_summary")
                    .iter()
                    .filter(|r| {
                        r.get("nominal_code").and_then(Value::as_str) == Some("999")
                            || r.get("display_nominal_code").and_then(Value::as_str) == Some("999")
                    })
                    .filter_map(|r| pence(r.get("total")))
                    .sum();
                if suspense != 0 {
                    todo.push(format!("Clear {} from 999 Suspense", money(suspense)));
                }
                out.insert("suspense".into(), Value::from(money(suspense)));
            }
            Err(e) => note("suspense", e),
        }

        // Directors' loan accounts overdrawn at the period's end.
        match self.balance_sheet(c, to_date).await {
            Ok(bs) => {
                let overdrawn: Vec<Value> = people(&bs)
                    .values()
                    .filter(|p| p.loan > 0)
                    .map(|p| json!({"name": p.name, "owes_company": money(p.loan * 100)}))
                    .collect();
                if !overdrawn.is_empty() {
                    todo.push(format!(
                        "Review {} overdrawn director's loan account(s) with director_loan_review",
                        overdrawn.len()
                    ));
                }
                out.insert("directors_loans_overdrawn".into(), Value::Array(overdrawn));
            }
            Err(e) => note("directors_loans_overdrawn", e),
        }

        out.insert(
            "ready".into(),
            Value::from(todo.is_empty() && unavailable.is_empty()),
        );
        out.insert("to_do".into(), Value::from(todo));
        if !unavailable.is_empty() {
            out.insert("unavailable".into(), Value::Object(unavailable));
        }
        Ok(Value::Object(out))
    }

    pub(crate) async fn loan_review(
        &self,
        c: &Caller,
        p: &LoanReviewParams,
    ) -> error::Result<Value> {
        let at = match super::date_arg("as_at_date", p.as_at_date.as_deref())? {
            Some(d) => parse(&d).unwrap_or_else(today),
            None => today(),
        };
        let company = c.one("company", &["company"], "company", &[]).await?;
        let now = people(&self.balance_sheet(c, at).await?);
        // The last accounting year ended on or before `at`, and its s455 deadline: tax is due on
        // what is still owed nine months after the year end.
        let year_end = accounting_years(&company)
            .into_iter()
            .map(|(_, e)| e)
            .filter(|e| *e < at)
            .max();
        let (at_year_end, at_deadline, deadline) = match year_end {
            Some(ye) => {
                let deadline = ye.checked_add_months(Months::new(9)).unwrap_or(ye);
                let ye_people = people(&self.balance_sheet(c, ye).await?);
                let dl_people = if deadline < at {
                    Some(people(&self.balance_sheet(c, deadline).await?))
                } else {
                    None
                };
                (Some((ye, ye_people)), dl_people, Some(deadline))
            }
            None => (None, None, None),
        };
        let mut directors = Vec::new();
        let mut keys: Vec<&String> = now.keys().collect();
        if let Some((_, ye)) = &at_year_end {
            keys.extend(ye.keys());
        }
        keys.sort();
        keys.dedup();
        for k in keys {
            let cur = now.get(k).cloned().unwrap_or_default();
            let ye = at_year_end.as_ref().and_then(|(_, m)| m.get(k)).cloned();
            let name = if cur.name.is_empty() {
                ye.as_ref().map(|p| p.name.clone()).unwrap_or_default()
            } else {
                cur.name.clone()
            };
            if cur.loan == 0
                && cur.expenses == 0
                && cur.salary == 0
                && ye.as_ref().is_none_or(|p| p.loan == 0)
            {
                continue;
            }
            let net = cur.loan + cur.expenses + cur.salary;
            let mut d = json!({
                "name": name,
                "loan_account": money(cur.loan * 100),
                "loan_position": if cur.loan > 0 { "owes the company" } else if cur.loan < 0 { "owed by the company" } else { "clear" },
                "expenses_owed_to_them": money((-cur.expenses).max(0) * 100),
                "salary_owed_to_them": money((-cur.salary).max(0) * 100),
                "net": money(net * 100),
                "net_position": if net > 0 { "owes the company" } else if net < 0 { "owed by the company" } else { "clear" },
            });
            let mut flags: Vec<String> = Vec::new();
            if let (Some((ye_date, _)), Some(ye_p), Some(dl)) = (&at_year_end, &ye, deadline) {
                d["at_year_end"] =
                    json!({"date": ymd(*ye_date), "loan_account": money(ye_p.loan * 100)});
                if ye_p.loan > 0 {
                    let by_deadline = at_deadline
                        .as_ref()
                        .map(|m| m.get(k).map(|p| p.loan).unwrap_or(0))
                        .unwrap_or(cur.loan);
                    let outstanding = ye_p.loan.min(by_deadline.max(0));
                    let passed = dl < at;
                    d["s455"] = json!({
                        "year_end": ymd(*ye_date),
                        "repay_by": ymd(dl),
                        "deadline_passed": passed,
                        "owed_at_year_end": money(ye_p.loan * 100),
                        "still_owed": money(outstanding * 100),
                        "estimated_charge": s455(outstanding),
                    });
                    if outstanding > 0 {
                        flags.push(if passed {
                            format!(
                                "{} of the loan owed at {} was still owed on {}: s455 tax of about {} is due with that year's corporation tax, and is repaid by HMRC once the loan is cleared",
                                money(outstanding * 100), ymd(*ye_date), ymd(dl), s455(outstanding)
                            )
                        } else {
                            format!(
                                "repay {} by {} to avoid s455 tax of about {}",
                                money(outstanding * 100), ymd(dl), s455(outstanding)
                            )
                        });
                    }
                }
            }
            if cur.loan > BIK_THRESHOLD_POUNDS {
                flags.push(format!(
                    "the loan is over {}: unless interest is charged at HMRC's official rate it is a taxable benefit in kind, reported on a P11D",
                    money(BIK_THRESHOLD_POUNDS * 100)
                ));
            }
            if cur.loan > 0 && cur.expenses < 0 {
                flags.push(format!(
                    "{} of expenses owed to them could be set against the loan with a journal (Dr 905, Cr 907)",
                    money((-cur.expenses).min(cur.loan) * 100)
                ));
            }
            if !flags.is_empty() {
                d["flags"] = Value::from(flags);
            }
            directors.push(d);
        }
        Ok(json!({
            "as_at": ymd(at),
            "directors": directors,
            "note": "Balances are FreeAgent's balance sheet figures to the nearest pound; positive loan_account means the person owes the company. s455 and benefit-in-kind figures are estimates: repayments soon followed by new borrowing may not count, so check with your accountant before acting.",
        }))
    }
}

#[tool_router(router = checks_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "What to finish before closing a month or filing a VAT return, in one call: the period (default: the next VAT return's, else last month), unexplained bank transactions and guesses to approve, expenses without receipts (with the VAT they reclaim), bills without the supplier's invoice, draft invoices in the period or over 30 days old, overdue invoices and bills, the 999 Suspense balance and overdrawn directors' loan accounts, with a to_do list. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn month_end_check(
        &self,
        Parameters(p): Parameters<MonthEndParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("month_end_check", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.month_end(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "Each director's account with the company: loan account (positive: they owe the company), expenses and salary the company owes them, and the net; the loan at the last year end with the s455 deadline (nine months after it) and an estimate of the s455 tax on what was still owed; and a flag for loans over 10,000 (benefit in kind). Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn director_loan_review(
        &self,
        Parameters(p): Parameters<LoanReviewParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("director_loan_review", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.loan_review(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_and_rates() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        assert_eq!(
            last_month(d),
            (
                NaiveDate::from_ymd_opt(2025, 12, 1).unwrap(),
                NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()
            )
        );
        assert_eq!(s455(1779), "600.41");
        assert_eq!(s455(10_000), "3375.00");
        assert_eq!(pounds(Some(&json!(11142))), 11142);
        assert_eq!(pounds(Some(&json!("-1275.4"))), -1275);
    }

    #[test]
    fn people_are_read_from_the_balance_sheet() {
        let bs = json!({
            "current_assets": {"accounts": [
                {"name": "Director Loan Account: Ann Lee", "nominal_code": "907-1", "total_debit_value": 1779},
                {"name": "Bank Account: Main", "nominal_code": "750-1", "total_debit_value": 900}
            ]},
            "current_liabilities": {"accounts": [
                {"name": "Expense Account: Ann Lee", "nominal_code": "905-1", "total_debit_value": -2301},
                {"name": "Salary and Bonuses: Ann Lee", "nominal_code": "902-1", "total_debit_value": -26},
                {"name": "Director Loan Account: Bob Hay", "nominal_code": "907-2", "total_debit_value": -6599}
            ]}
        });
        let p = people(&bs);
        assert_eq!(
            p["1"],
            Person {
                name: "Ann Lee".into(),
                loan: 1779,
                expenses: -2301,
                salary: -26
            }
        );
        assert_eq!(p["2"].loan, -6599);
        assert_eq!(p.len(), 2);
    }
}
