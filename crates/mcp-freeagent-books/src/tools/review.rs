//! Reviews that help get the books right: a check for likely mistakes, explanations suggested
//! from how the same payee was explained before, and finding what a receipt belongs to.

use super::{
    BooksServer, Caller, Query, date_range, id_arg, json_result, money_arg, opt_money, run,
    text_arg,
};
use crate::error::{self, FaError};
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use chrono::{Duration, NaiveDate};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};

/// Most items of one kind read for a review.
const REVIEW_ITEMS: usize = 2_000;
/// Most items listed in one section; the count covers them all.
const LISTED: usize = 25;
/// Most journal sets read in full for the journal check.
const JOURNALS_READ: usize = 40;
/// Days apart that two bills or expenses for the same amount count as possible duplicates.
const BILL_DUPLICATE_DAYS: i64 = 7;
const EXPENSE_DUPLICATE_DAYS: i64 = 2;
/// Bank charges at or above this are unusual (in pence).
const LARGE_BANK_CHARGE: i64 = 10_000;
/// A cost described as a correction or adjustment at or above this is checked (in pence).
const LARGE_CORRECTION: i64 = 25_000;
/// Journals at or above this are listed (in pence), unless the person gives another amount.
const LARGE_JOURNAL: i64 = 100_000;

/// Category or description words for things that normally carry no VAT to reclaim: insurance,
/// finance costs, wages, fares, postage stamps, donations and government fees.
const NO_VAT_WORDS: &[&str] = &[
    "insurance",
    "bank charge",
    "bank/finance",
    "finance charge",
    "interest",
    "salary",
    "salaries",
    "wages",
    "pension",
    "donation",
    "charitable",
    "train",
    "rail",
    "flight",
    "airfare",
    "tfl",
    "oyster",
    "bus fare",
    "stamps",
    "royal mail",
    "companies house",
    "hmrc",
    "dvla",
    "council tax",
    "business rates",
];

/// Words in a cost's description that suggest it is a correction rather than a purchase.
const CORRECTION_WORDS: &[&str] = &[
    "correction",
    "adjust",
    "balance",
    "reversal",
    "reverse",
    "write off",
    "write-off",
    "error",
];

/// Words dropped from bank descriptions before payees are compared: payment types and filler
/// that banks add to every line.
const NOISE: &[&str] = &[
    "card",
    "payment",
    "payments",
    "pos",
    "dd",
    "so",
    "bgc",
    "bp",
    "fpi",
    "fpo",
    "tfr",
    "trf",
    "transfer",
    "ref",
    "reference",
    "to",
    "from",
    "gb",
    "gbr",
    "uk",
    "ltd",
    "limited",
    "plc",
    "www",
    "com",
    "co",
    "the",
    "and",
    "visa",
    "debit",
    "credit",
    "contactless",
    "online",
    "direct",
    "faster",
    "via",
    "mobile",
    "apple",
    "pay",
    "gbp",
];

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct HealthParams {
    /// Start of the period, YYYY-MM-DD. Default: the start of the current accounting year.
    pub from_date: Option<String>,
    /// End of the period, YYYY-MM-DD. Default: today.
    pub to_date: Option<String>,
    /// Journals at or above this amount are listed, e.g. "1000". Default: 1000.
    pub large_journal: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SuggestParams {
    /// One bank account (list_bank_accounts). Default: every account that is not hidden.
    pub bank_account_id: Option<String>,
    /// How far back to learn from, in months, 1 to 36. Default: 12.
    pub history_months: Option<u32>,
    /// Most unexplained transactions to suggest for, 1 to 100. Default: 50.
    pub max_transactions: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MatchReceiptParams {
    /// The receipt's total, e.g. "42.50" (sign does not matter).
    pub amount: String,
    /// The date on the receipt, YYYY-MM-DD.
    pub date: String,
    /// The shop or supplier on the receipt, to rank matches.
    pub supplier: Option<String>,
    /// Days either side of the date to look, 0 to 30. Default: 5 (card payments can clear days later).
    pub days: Option<u32>,
    /// How far the amount may differ, e.g. "1.00" for a foreign-currency card payment. Default: 0.
    pub tolerance: Option<String>,
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn parse(d: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn id(v: &Value) -> Option<String> {
    v.get("url")
        .and_then(Value::as_str)
        .and_then(view::id_of)
        .map(String::from)
}

/// The nominal code of a category URL (`.../categories/285` is `285`).
fn code_of(v: &Value) -> Option<String> {
    v.get("category")
        .and_then(Value::as_str)
        .and_then(view::id_of)
        .map(String::from)
}

/// Days between two YYYY-MM-DD dates; `None` if either is not a date.
fn days_apart(a: &str, b: &str) -> Option<i64> {
    Some((parse(a)? - parse(b)?).num_days().abs())
}

/// A bank description reduced to the payee: lower case, without digits, references, payment
/// types and punctuation, at most three words. "CARD PAYMENT TO TESCO STORES 2231 ON 12/03" and
/// "TESCO STORES 4410" are both "tesco stores".
fn payee(description: &str) -> String {
    description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !w.chars().any(|c| c.is_ascii_digit()))
        .filter(|w| !NOISE.contains(w))
        .filter(|w| !matches!(*w, "on" | "at" | "for" | "of"))
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first of `words` that `hay` mentions, ignoring case.
fn mentions<'a>(hay: &str, words: &[&'a str]) -> Option<&'a str> {
    let hay = hay.to_lowercase();
    words.iter().find(|w| hay.contains(**w)).copied()
}

/// Category names by nominal code, from FreeAgent's category list.
async fn category_names(c: &Caller) -> error::Result<HashMap<String, String>> {
    let a = c.get("categories", &["categories"], &[]).await?;
    let mut out = HashMap::new();
    if let Some(groups) = a.body.as_object() {
        for cats in groups.values().filter_map(Value::as_array) {
            for cat in cats {
                if let (Some(code), Some(name)) = (
                    cat.get("nominal_code").and_then(Value::as_str),
                    cat.get("description").and_then(Value::as_str),
                ) {
                    out.insert(code.to_string(), name.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// The bank accounts to look at: `one` if given, else every account that is not hidden. Pairs of
/// (id, name).
async fn bank_accounts(c: &Caller, one: Option<&str>) -> error::Result<Vec<(String, String)>> {
    if let Some(raw) = one {
        let id = id_arg("bank_account_id", raw)?;
        let a = c
            .one("bank_account", &["bank_accounts", &id], "bank_account", &[])
            .await?;
        return Ok(vec![(id, str_of(&a, "name").to_string())]);
    }
    let (accounts, _) = c
        .all(
            "bank_accounts",
            &["bank_accounts"],
            "bank_accounts",
            Vec::new(),
            100,
        )
        .await?;
    Ok(accounts
        .iter()
        .filter(|a| str_of(a, "status") != "hidden")
        .filter_map(|a| Some((id(a)?, str_of(a, "name").to_string())))
        .collect())
}

/// Every explanation in `account` between the dates.
async fn explanations(
    c: &Caller,
    account: &str,
    from: &str,
    to: &str,
) -> error::Result<(Vec<Value>, bool)> {
    c.all(
        "bank_transaction_explanations",
        &["bank_transaction_explanations"],
        "bank_transaction_explanations",
        vec![
            ("bank_account", c.url("bank_accounts", account)),
            ("from_date", from.to_string()),
            ("to_date", to.to_string()),
        ],
        REVIEW_ITEMS,
    )
    .await
}

/// A section of a review: how many were found and the first [`LISTED`].
fn section(items: Vec<Value>) -> Value {
    let count = items.len();
    let mut v =
        json!({"count": count, "items": items.into_iter().take(LISTED).collect::<Vec<_>>()});
    if count > LISTED {
        v["listed"] = Value::from(LISTED);
    }
    v
}

/// Pairs of `items` with the same `key` dated at most `days` apart, each pair once.
fn duplicates(
    items: &[Value],
    key: impl Fn(&Value) -> Option<String>,
    days: i64,
) -> Vec<(&Value, &Value)> {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for i in items {
        if let Some(k) = key(i) {
            groups.entry(k).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for group in groups.values() {
        for (n, a) in group.iter().enumerate() {
            for b in &group[n + 1..] {
                if days_apart(str_of(a, "dated_on"), str_of(b, "dated_on"))
                    .is_some_and(|d| d <= days)
                {
                    out.push((*a, *b));
                }
            }
        }
    }
    out
}

/// What an explanation said the money was, as the arguments `explain_bank_transaction` takes,
/// with a label. `None` for invoice and bill payments, which only fit the one invoice or bill.
fn explained_as(e: &Value) -> Option<(Map<String, Value>, String)> {
    let mut args = Map::new();
    let label;
    if let Some(acc) = e
        .get("transfer_bank_account")
        .and_then(Value::as_str)
        .and_then(view::id_of)
    {
        args.insert("transfer_bank_account_id".into(), Value::from(acc));
        label = format!("transfer:{acc}");
    } else if let Some(user) = e
        .get("paid_user")
        .and_then(Value::as_str)
        .and_then(view::id_of)
    {
        args.insert("paid_user_id".into(), Value::from(user));
        let cat = code_of(e);
        if let Some(cat) = &cat {
            args.insert("category".into(), Value::from(cat.as_str()));
        }
        label = format!("user:{user}:{}", cat.unwrap_or_default());
    } else if e.get("paid_invoice").is_some() || e.get("paid_bill").is_some() {
        return None;
    } else {
        let cat = code_of(e)?;
        args.insert("category".into(), Value::from(cat.as_str()));
        for k in ["sales_tax_rate", "sales_tax_status", "ec_status"] {
            if let Some(v) = e.get(k).filter(|v| !v.is_null()) {
                let v = match v {
                    Value::Number(n) => Value::from(n.to_string()),
                    other => other.clone(),
                };
                args.insert(k.into(), v);
            }
        }
        label = format!(
            "category:{cat}:{}:{}",
            args.get("sales_tax_rate")
                .and_then(Value::as_str)
                .unwrap_or(""),
            args.get("ec_status").and_then(Value::as_str).unwrap_or("")
        );
    }
    Some((args, label))
}

/// The suggestion for a payee from its past explanations (same direction of money only): the
/// most common one, with how often and when it was last used, and a confidence.
fn suggest(history: &[&Value], names: &HashMap<String, String>) -> Option<Value> {
    let mut tally: BTreeMap<String, (usize, String, Map<String, Value>)> = BTreeMap::new();
    let mut matched_bills = 0usize;
    for e in history {
        match explained_as(e) {
            Some((args, label)) => {
                let date = str_of(e, "dated_on").to_string();
                let t = tally.entry(label).or_insert((0, String::new(), args));
                t.0 += 1;
                if date > t.1 {
                    t.1 = date;
                }
            }
            None => matched_bills += 1,
        }
    }
    let total: usize = tally.values().map(|t| t.0).sum::<usize>() + matched_bills;
    let (_, (n, last, args)) = tally
        .iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then(a.1.1.cmp(&b.1.1)))?;
    let share = *n as f64 / total as f64;
    let confidence = if *n >= 3 && share >= 0.8 {
        "high"
    } else if share >= 0.6 {
        "medium"
    } else {
        "low"
    };
    let mut v = json!({
        "explain_with": args,
        "based_on": format!("{n} of {total} past explanation(s) of this payee"),
        "last_used": last,
        "confidence": confidence,
    });
    if let Some(name) = args
        .get("category")
        .and_then(Value::as_str)
        .and_then(|c| names.get(c))
    {
        v["category_name"] = Value::from(name.as_str());
    }
    if matched_bills > 0 {
        v["note"] = Value::from(format!(
            "{matched_bills} past payment(s) of this payee were matched to an invoice or bill: check for an open one first"
        ));
    }
    Some(v)
}

impl BooksServer {
    async fn health(&self, c: &Caller, p: &HealthParams) -> error::Result<Value> {
        let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
        let to = to.unwrap_or_else(|| ymd(today()));
        let from = match from {
            Some(f) => f,
            None => {
                let company = c.one("company", &["company"], "company", &[]).await?;
                let end = parse(&to).unwrap_or_else(today);
                super::checks::accounting_years(&company)
                    .into_iter()
                    .find(|(s, e)| *s <= end && end <= *e)
                    .map(|(s, _)| ymd(s))
                    .unwrap_or_else(|| ymd(end - Duration::days(365)))
            }
        };
        let large_journal = match opt_money("large_journal", p.large_journal.as_deref())? {
            Some(v) => pence(Some(&Value::from(v))).unwrap_or(LARGE_JOURNAL).abs(),
            None => LARGE_JOURNAL,
        };
        let period: Query = vec![("from_date", from.clone()), ("to_date", to.clone())];
        let mut out = Map::new();
        let mut findings: Vec<String> = Vec::new();
        let mut unavailable = Map::new();
        let mut note = |section: &str, e: FaError| {
            unavailable.insert(section.into(), Value::from(format!("{}: {e}", e.kind())));
        };
        out.insert("period".into(), json!({"from": from, "to": to}));
        let names = category_names(c).await.unwrap_or_default();
        let name_of = |code: &Option<String>| -> String {
            code.as_ref()
                .map(|c| names.get(c).cloned().unwrap_or_else(|| c.clone()))
                .unwrap_or_default()
        };

        // Bills: the same supplier and total within a week.
        match c
            .all("bills", &["bills"], "bills", period.clone(), REVIEW_ITEMS)
            .await
        {
            Ok((bills, _)) => {
                let pairs = duplicates(
                    &bills,
                    |b| {
                        Some(format!(
                            "{}|{}",
                            str_of(b, "contact"),
                            pence(b.get("total_value"))?
                        ))
                    },
                    BILL_DUPLICATE_DAYS,
                );
                let rows: Vec<Value> = pairs
                    .iter()
                    .map(|(a, b)| {
                        json!({
                            "bill_ids": [id(a), id(b)],
                            "contact_name": a.get("contact_name"),
                            "total_value": a.get("total_value"),
                            "dated_on": [a.get("dated_on"), b.get("dated_on")],
                            "references": [a.get("reference"), b.get("reference")],
                        })
                    })
                    .collect();
                if !rows.is_empty() {
                    findings.push(format!("{} pair(s) of bills may be duplicates", rows.len()));
                }
                out.insert("possible_duplicate_bills".into(), section(rows));
            }
            Err(e) => note("possible_duplicate_bills", e),
        }

        // Expenses: the same claimant and amount within two days; and VAT on things that have none.
        match c
            .all(
                "expenses",
                &["expenses"],
                "expenses",
                period.clone(),
                REVIEW_ITEMS,
            )
            .await
        {
            Ok((expenses, _)) => {
                let costs: Vec<Value> = expenses
                    .iter()
                    .filter(|e| e.get("mileage").is_none())
                    .cloned()
                    .collect();
                let pairs = duplicates(
                    &costs,
                    |e| {
                        Some(format!(
                            "{}|{}",
                            str_of(e, "user"),
                            pence(e.get("gross_value"))?
                        ))
                    },
                    EXPENSE_DUPLICATE_DAYS,
                );
                let rows: Vec<Value> = pairs
                    .iter()
                    .map(|(a, b)| {
                        json!({
                            "expense_ids": [id(a), id(b)],
                            "gross_value": a.get("gross_value"),
                            "dated_on": [a.get("dated_on"), b.get("dated_on")],
                            "descriptions": [a.get("description"), b.get("description")],
                        })
                    })
                    .collect();
                if !rows.is_empty() {
                    findings.push(format!(
                        "{} pair(s) of expenses may be duplicates",
                        rows.len()
                    ));
                }
                out.insert("possible_duplicate_expenses".into(), section(rows));
                let vat: Vec<Value> = costs
                    .iter()
                    .filter(|e| pence(e.get("sales_tax_value")).is_some_and(|v| v != 0))
                    .filter_map(|e| {
                        let cat = name_of(&code_of(e));
                        let why =
                            mentions(&format!("{cat} {}", str_of(e, "description")), NO_VAT_WORDS)?;
                        Some(json!({
                            "kind": "expense",
                            "id": id(e),
                            "dated_on": e.get("dated_on"),
                            "description": e.get("description"),
                            "category": cat,
                            "gross_value": e.get("gross_value"),
                            "vat": e.get("sales_tax_value"),
                            "why": format!("\"{why}\" usually carries no VAT to reclaim"),
                        }))
                    })
                    .collect();
                out.insert("vat_on_items_without_vat".into(), json!(vat));
            }
            Err(e) => note("possible_duplicate_expenses", e),
        }

        // Bank explanations: payments to directors booked as costs, large bank charges, large
        // corrections booked as costs, and VAT on things that have none.
        let users = match c.get("users", &["users"], &[]).await {
            Ok(a) => a
                .body
                .get("users")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            Err(e) => {
                note("director_payments_as_costs", e);
                Vec::new()
            }
        };
        let directors: Vec<(String, String)> = users
            .iter()
            .filter(|u| {
                let role = str_of(u, "role").to_lowercase();
                role.contains("director") || role.contains("owner") || role.is_empty()
            })
            .filter_map(|u| {
                let last = str_of(u, "last_name").trim().to_lowercase();
                (last.len() >= 3).then(|| {
                    (
                        last,
                        format!("{} {}", str_of(u, "first_name"), str_of(u, "last_name"))
                            .trim()
                            .to_string(),
                    )
                })
            })
            .collect();
        let mut to_directors = Vec::new();
        let mut charges = Vec::new();
        let mut corrections = Vec::new();
        let mut vat = Vec::new();
        let mut truncated = false;
        match bank_accounts(c, None).await {
            Ok(accounts) => {
                for (acc, acc_name) in &accounts {
                    let (items, more) = match explanations(c, acc, &from, &to).await {
                        Ok(r) => r,
                        Err(e) => {
                            note("bank_explanations", e);
                            break;
                        }
                    };
                    truncated |= more;
                    for e in &items {
                        let Some(code) = code_of(e) else { continue };
                        if e.get("paid_user").is_some()
                            || e.get("transfer_bank_account").is_some()
                            || e.get("paid_bill").is_some()
                            || e.get("paid_invoice").is_some()
                        {
                            continue;
                        }
                        let gross = pence(e.get("gross_value")).unwrap_or(0);
                        let cat = names.get(&code).cloned().unwrap_or_else(|| code.clone());
                        let desc = str_of(e, "description");
                        let row = |why: String| {
                            json!({
                                "kind": "bank_transaction_explanation",
                                "id": id(e),
                                "bank_account": acc_name,
                                "dated_on": e.get("dated_on"),
                                "description": desc,
                                "category": format!("{code} {cat}"),
                                "gross_value": e.get("gross_value"),
                                "why": why,
                            })
                        };
                        // Codes below 900 are income and costs; 900s are balance sheet accounts.
                        let is_cost = code.len() == 3 && code.as_str() < "900" && gross < 0;
                        // Whole words only: a director called Lee is not in "Leeds".
                        let lower = desc.to_lowercase();
                        let words: Vec<&str> =
                            lower.split(|c: char| !c.is_alphanumeric()).collect();
                        if is_cost
                            && let Some((_, name)) = directors
                                .iter()
                                .find(|(last, _)| words.contains(&last.as_str()))
                        {
                            to_directors.push(row(format!(
                                "a payment that names {name} is booked as a cost; if it was salary, expenses repaid or a loan, explain it with paid_user_id"
                            )));
                        }
                        let cat_lower = cat.to_lowercase();
                        if gross <= -LARGE_BANK_CHARGE
                            && cat_lower.contains("bank")
                            && cat_lower.contains("charge")
                        {
                            charges.push(row(format!(
                                "{} is a large bank charge; check it is not a transfer, a loan or a correction",
                                money(-gross)
                            )));
                        }
                        if is_cost
                            && gross <= -LARGE_CORRECTION
                            && let Some(w) = mentions(desc, CORRECTION_WORDS)
                        {
                            corrections.push(row(format!(
                                "described as a \"{w}\" but booked as a cost of {}: corrections usually belong in a journal or the account they correct",
                                money(-gross)
                            )));
                        }
                        if pence(e.get("sales_tax_value")).is_some_and(|v| v != 0)
                            && let Some(w) = mentions(&format!("{cat} {desc}"), NO_VAT_WORDS)
                        {
                            let mut r = row(format!("\"{w}\" usually carries no VAT to reclaim"));
                            r["vat"] = e.get("sales_tax_value").cloned().unwrap_or_default();
                            vat.push(r);
                        }
                    }
                }
            }
            Err(e) => note("bank_explanations", e),
        }
        if !to_directors.is_empty() {
            findings.push(format!(
                "{} payment(s) naming a director are booked as costs",
                to_directors.len()
            ));
        }
        if !charges.is_empty() {
            findings.push(format!("{} unusually large bank charge(s)", charges.len()));
        }
        if !corrections.is_empty() {
            findings.push(format!(
                "{} correction(s) or adjustment(s) booked as costs",
                corrections.len()
            ));
        }
        out.insert("director_payments_as_costs".into(), section(to_directors));
        out.insert("large_bank_charges".into(), section(charges));
        out.insert("corrections_booked_as_costs".into(), section(corrections));
        let mut all_vat = out
            .remove("vat_on_items_without_vat")
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        all_vat.extend(vat);
        if !all_vat.is_empty() {
            findings.push(format!(
                "{} item(s) reclaim VAT on something that usually has none",
                all_vat.len()
            ));
        }
        out.insert("vat_on_items_without_vat".into(), section(all_vat));

        // Journals: large or round amounts, or ones that move money in or out of a director's
        // loan, suspense or a bank account with no bank transaction behind them.
        match c
            .all(
                "journal_sets",
                &["journal_sets"],
                "journal_sets",
                period.clone(),
                REVIEW_ITEMS,
            )
            .await
        {
            Ok((sets, more)) => {
                let mut rows = Vec::new();
                for (n, set) in sets.iter().enumerate() {
                    let full;
                    let set = if set.get("journal_entries").is_some() {
                        set
                    } else if n < JOURNALS_READ {
                        let Some(sid) = id(set) else { continue };
                        full = match c
                            .one("journal_set", &["journal_sets", &sid], "journal_set", &[])
                            .await
                        {
                            Ok(v) => v,
                            Err(e) => {
                                note("journals_to_check", e);
                                break;
                            }
                        };
                        &full
                    } else {
                        truncated = true;
                        continue;
                    };
                    let entries = set
                        .get("journal_entries")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let total: i64 = entries
                        .iter()
                        .filter_map(|e| pence(e.get("debit_value")))
                        .filter(|v| *v > 0)
                        .sum();
                    let mut why: Vec<String> = Vec::new();
                    if total >= large_journal {
                        why.push(format!("moves {}", money(total)));
                    }
                    if total >= 50_000 && total % 10_000 == 0 {
                        why.push("a round amount".into());
                    }
                    let codes: Vec<String> = entries.iter().filter_map(code_of).collect();
                    for (base, what) in [
                        ("907", "a director's loan account"),
                        ("999", "suspense"),
                        ("750", "a bank account"),
                    ] {
                        if codes.iter().any(|c| c.split('-').next() == Some(base)) {
                            why.push(format!("posts to {what}"));
                        }
                    }
                    if why.is_empty() {
                        continue;
                    }
                    rows.push(json!({
                        "id": id(set),
                        "dated_on": set.get("dated_on"),
                        "description": set.get("description"),
                        "tag": set.get("tag"),
                        "total": money(total),
                        "entries": entries.iter().take(6).map(|e| json!({
                            "category": code_of(e).map(|c| format!("{c} {}", names.get(&c).map(String::as_str).unwrap_or(""))),
                            "debit_value": e.get("debit_value"),
                            "description": e.get("description"),
                        })).collect::<Vec<_>>(),
                        "why": why.join("; "),
                    }));
                }
                truncated |= more;
                if !rows.is_empty() {
                    findings.push(format!("{} journal(s) worth a second look", rows.len()));
                }
                out.insert("journals_to_check".into(), section(rows));
            }
            Err(e) => note("journals_to_check", e),
        }

        out.insert(
            "clean".into(),
            Value::from(findings.is_empty() && unavailable.is_empty()),
        );
        out.insert("findings".into(), Value::from(findings));
        if truncated {
            out.insert(
                "truncated".into(),
                Value::from(
                    "there was more than one review reads; check a shorter period for the rest",
                ),
            );
        }
        if !unavailable.is_empty() {
            out.insert("unavailable".into(), Value::Object(unavailable));
        }
        out.insert(
            "note".into(),
            Value::from("These are things worth a second look, not proof of a mistake: check each one before changing anything."),
        );
        Ok(Value::Object(out))
    }

    async fn suggestions(&self, c: &Caller, p: &SuggestParams) -> error::Result<Value> {
        let months = p.history_months.unwrap_or(12);
        if !(1..=36).contains(&months) {
            return Err(FaError::invalid("history_months must be between 1 and 36"));
        }
        let max = p.max_transactions.unwrap_or(50);
        if !(1..=100).contains(&max) {
            return Err(FaError::invalid(
                "max_transactions must be between 1 and 100",
            ));
        }
        let to = today();
        let from = to - Duration::days(i64::from(months) * 31);
        let names = category_names(c).await.unwrap_or_default();
        let accounts = bank_accounts(c, p.bank_account_id.as_deref()).await?;
        let mut rows = Vec::new();
        let mut without = 0usize;
        let mut left = max as usize;
        let mut more_left = false;
        for (acc, acc_name) in &accounts {
            if left == 0 {
                more_left = true;
                break;
            }
            let (txs, more) = c
                .all(
                    "bank_transactions",
                    &["bank_transactions"],
                    "bank_transactions",
                    vec![
                        ("bank_account", c.url("bank_accounts", acc)),
                        ("view", "unexplained".into()),
                    ],
                    left,
                )
                .await?;
            more_left |= more;
            if txs.is_empty() {
                continue;
            }
            left -= txs.len();
            let (history, _) = explanations(c, acc, &ymd(from), &ymd(to)).await?;
            let mut by_payee: HashMap<(String, bool), Vec<&Value>> = HashMap::new();
            for e in &history {
                if e.get("marked_for_review").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let key = payee(str_of(e, "description"));
                if key.is_empty() {
                    continue;
                }
                let out = pence(e.get("gross_value")).unwrap_or(0) < 0;
                by_payee.entry((key, out)).or_default().push(e);
            }
            for t in &txs {
                let amount = t.get("unexplained_amount").or_else(|| t.get("amount"));
                let out = pence(amount).unwrap_or(0) < 0;
                let key = payee(str_of(t, "description"));
                let mut row = json!({
                    "bank_transaction_id": id(t),
                    "bank_account": acc_name,
                    "dated_on": t.get("dated_on"),
                    "description": t.get("description"),
                    "unexplained_amount": amount,
                    "payee": key,
                });
                match by_payee.get(&(key, out)).and_then(|h| suggest(h, &names)) {
                    Some(mut s) => {
                        if let Some(args) = s.get_mut("explain_with").and_then(Value::as_object_mut)
                        {
                            args.insert(
                                "bank_transaction_id".into(),
                                row["bank_transaction_id"].clone(),
                            );
                        }
                        row["suggestion"] = s;
                    }
                    None => {
                        without += 1;
                        row["suggestion"] = Value::Null;
                    }
                }
                rows.push(row);
            }
        }
        let with = rows.len() - without;
        let mut out = json!({
            "learned_from": {"from": ymd(from), "to": ymd(to)},
            "with_suggestion": with,
            "without_history": without,
            "transactions": rows,
            "next": "Show the person the suggestions; explain the ones they agree with in one call to explain_bank_transactions, passing each explain_with. For transactions without history, suggest an explanation yourself (an open invoice or bill, a transfer, a director, or a category).",
        });
        if more_left {
            out["more_unexplained"] = Value::from(true);
        }
        Ok(view::prune(out).unwrap_or_default())
    }

    async fn receipt_matches(&self, c: &Caller, p: &MatchReceiptParams) -> error::Result<Value> {
        let amount = pence(Some(&Value::from(money_arg("amount", &p.amount)?)))
            .ok_or_else(|| FaError::invalid("amount must be a number"))?
            .abs();
        let date = super::required_date("date", &p.date)?;
        let day = parse(&date).unwrap_or_else(today);
        let days = p.days.unwrap_or(5);
        if days > 30 {
            return Err(FaError::invalid("days must be between 0 and 30"));
        }
        let tolerance = match opt_money("tolerance", p.tolerance.as_deref())? {
            Some(t) => pence(Some(&Value::from(t))).unwrap_or(0).abs(),
            None => 0,
        };
        let supplier = text_arg("supplier", p.supplier.as_deref())?.map(|s| payee(&s));
        let from = ymd(day - Duration::days(i64::from(days)));
        let to = ymd(day + Duration::days(i64::from(days)));
        let fits =
            |v: Option<&Value>| pence(v).is_some_and(|x| (x.abs() - amount).abs() <= tolerance);
        let words_match = |text: &str| {
            supplier.as_ref().is_some_and(|s| {
                let t = payee(text);
                !s.is_empty() && s.split(' ').any(|w| w.len() > 2 && t.contains(w))
            })
        };
        let mut candidates: Vec<(i64, Value)> = Vec::new();
        let rank = |dated_on: &str, text: &str, exact: bool| {
            let d = days_apart(dated_on, &date).unwrap_or(99);
            d * 10 - if words_match(text) { 25 } else { 0 } + if exact { 0 } else { 5 }
        };
        let period: Query = vec![("from_date", from.clone()), ("to_date", to.clone())];

        for (acc, acc_name) in bank_accounts(c, None).await? {
            let (txs, _) = c
                .all(
                    "bank_transactions",
                    &["bank_transactions"],
                    "bank_transactions",
                    vec![
                        ("bank_account", c.url("bank_accounts", &acc)),
                        ("from_date", from.clone()),
                        ("to_date", to.clone()),
                    ],
                    REVIEW_ITEMS,
                )
                .await?;
            for t in txs.iter().filter(|t| fits(t.get("amount"))) {
                let exact = pence(t.get("amount")).map(i64::abs) == Some(amount);
                let r = rank(str_of(t, "dated_on"), str_of(t, "description"), exact);
                let explained: Vec<&Value> = t
                    .get("bank_transaction_explanations")
                    .and_then(Value::as_array)
                    .map(|v| v.iter().collect())
                    .unwrap_or_default();
                let unexplained = pence(t.get("unexplained_amount")).unwrap_or(0) != 0;
                let base = json!({
                    "bank_account": acc_name,
                    "dated_on": t.get("dated_on"),
                    "description": t.get("description"),
                    "amount": t.get("amount"),
                });
                if unexplained || explained.is_empty() {
                    let mut v = base.clone();
                    v["kind"] = Value::from("unexplained bank transaction");
                    v["bank_transaction_id"] = Value::from(id(t));
                    v["next"] = Value::from(
                        "explain it with explain_bank_transaction, passing the receipt as attachment {upload_url}, or explain it and then attach_file with target bank_transaction_explanation",
                    );
                    candidates.push((r, v));
                }
                for e in explained {
                    let mut v = base.clone();
                    v["kind"] = Value::from("bank transaction explanation");
                    v["category"] = Value::from(code_of(e));
                    let eid = id(e);
                    if e.get("attachment").is_some() {
                        v["already_has_a_file"] = Value::from(true);
                    }
                    v["attach_with"] = json!({"target": "bank_transaction_explanation", "id": eid});
                    candidates.push((r + if e.get("attachment").is_some() { 50 } else { 0 }, v));
                }
            }
        }
        let (expenses, _) = c
            .all(
                "expenses",
                &["expenses"],
                "expenses",
                period.clone(),
                REVIEW_ITEMS,
            )
            .await?;
        for e in expenses.iter().filter(|e| fits(e.get("gross_value"))) {
            let exact = pence(e.get("gross_value")).map(i64::abs) == Some(amount);
            let mut v = json!({
                "kind": "expense",
                "dated_on": e.get("dated_on"),
                "description": e.get("description"),
                "gross_value": e.get("gross_value"),
                "attach_with": {"target": "expense", "id": id(e)},
            });
            let has = e.get("attachment").is_some();
            if has {
                v["already_has_a_file"] = Value::from(true);
            }
            let r = rank(str_of(e, "dated_on"), str_of(e, "description"), exact);
            candidates.push((r + if has { 50 } else { 0 }, v));
        }
        let (bills, _) = c
            .all("bills", &["bills"], "bills", period, REVIEW_ITEMS)
            .await?;
        for b in bills.iter().filter(|b| fits(b.get("total_value"))) {
            let exact = pence(b.get("total_value")).map(i64::abs) == Some(amount);
            let text = format!("{} {}", str_of(b, "contact_name"), str_of(b, "reference"));
            let mut v = json!({
                "kind": "bill",
                "dated_on": b.get("dated_on"),
                "contact_name": b.get("contact_name"),
                "reference": b.get("reference"),
                "total_value": b.get("total_value"),
                "attach_with": {"target": "bill", "id": id(b)},
            });
            let has = b.get("attachment").is_some();
            if has {
                v["already_has_a_file"] = Value::from(true);
            }
            let r = rank(str_of(b, "dated_on"), &text, exact);
            candidates.push((r + if has { 50 } else { 0 }, v));
        }
        candidates.sort_by_key(|(r, _)| *r);
        let found = candidates.len();
        let list: Vec<Value> = candidates
            .into_iter()
            .take(LISTED)
            .filter_map(|(_, v)| view::prune(v))
            .collect();
        Ok(json!({
            "looked_for": {"amount": money(amount), "from": from, "to": to},
            "found": found,
            "candidates": list,
            "next": if found == 0 {
                "Nothing matches: offer to record it as a new expense (create_expense) or bill (create_bill) and attach the receipt with attach_file."
            } else {
                "Best match first. Confirm with the person, then attach_file with the candidate's attach_with and the upload_url. An unexplained bank transaction needs explaining first."
            },
        }))
    }
}

#[tool_router(router = review_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Look for likely mistakes in the books over a period (default: the accounting year to date): bills or expenses that may be duplicates, VAT reclaimed on things that usually have none (insurance, fares, bank charges, wages), payments naming a director booked as costs, unusually large bank charges, corrections booked as costs, and journals that are large, round or post to a director's loan, suspense or a bank account. Each finding says why. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn books_health_check(
        &self,
        Parameters(p): Parameters<HealthParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("books_health_check", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.health(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "Suggest explanations for unexplained bank transactions from how the same payee was explained before (default: the last 12 months, every account): for each, the category with VAT, transfer or director it usually was, how often, when last, and a confidence, as explain_with arguments ready for explain_bank_transactions. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn suggest_explanations(
        &self,
        Parameters(p): Parameters<SuggestParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("suggest_explanations", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.suggestions(&c, &p).await?))
        })
        .await
    }

    #[tool(
        description = "Find what a receipt or invoice belongs to, from its total and date (and supplier, to rank): bank transactions, their explanations, expenses and bills for that amount within a few days, best first, each with the attach_file target and id to attach it to. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn match_receipt(
        &self,
        Parameters(p): Parameters<MatchReceiptParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("match_receipt", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.receipt_matches(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payees_ignore_references_and_payment_types() {
        assert_eq!(
            payee("CARD PAYMENT TO TESCO STORES 2231 ON 12/03"),
            "tesco stores"
        );
        assert_eq!(payee("TESCO STORES 4410"), "tesco stores");
        assert_eq!(
            payee("DD Adobe Systems Software Ireland"),
            "adobe systems software"
        );
        assert_eq!(payee("12345 / 99"), "");
    }

    #[test]
    fn suggestions_take_the_most_common_explanation() {
        let names = HashMap::from([("285".to_string(), "Software".to_string())]);
        let a = json!({"category": "https://x/v2/categories/285", "sales_tax_rate": "20.0", "dated_on": "2026-01-01", "gross_value": "-10"});
        let b = json!({"category": "https://x/v2/categories/285", "sales_tax_rate": "20.0", "dated_on": "2026-03-01", "gross_value": "-10"});
        let c = json!({"category": "https://x/v2/categories/285", "sales_tax_rate": "20.0", "dated_on": "2026-02-01", "gross_value": "-10"});
        let d = json!({"paid_bill": "https://x/v2/bills/9", "dated_on": "2026-04-01"});
        let s = suggest(&[&a, &b, &c], &names).unwrap();
        assert_eq!(s["confidence"], "high");
        assert_eq!(s["last_used"], "2026-03-01");
        assert_eq!(s["category_name"], "Software");
        assert_eq!(
            s["explain_with"],
            json!({"category": "285", "sales_tax_rate": "20.0"})
        );
        let s = suggest(&[&a, &d], &names).unwrap();
        assert_eq!(s["confidence"], "low");
        assert!(s["note"].as_str().unwrap().contains("invoice or bill"));
        assert!(suggest(&[&d], &names).is_none());
    }

    #[test]
    fn duplicates_are_paired_within_the_window() {
        let items = vec![
            json!({"contact": "c1", "total_value": "60.0", "dated_on": "2026-01-01"}),
            json!({"contact": "c1", "total_value": "60.00", "dated_on": "2026-01-05"}),
            json!({"contact": "c1", "total_value": "60.0", "dated_on": "2026-02-20"}),
            json!({"contact": "c2", "total_value": "60.0", "dated_on": "2026-01-02"}),
        ];
        let pairs = duplicates(
            &items,
            |b| {
                Some(format!(
                    "{}|{}",
                    str_of(b, "contact"),
                    pence(b.get("total_value"))?
                ))
            },
            7,
        );
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn mentions_finds_words_without_regard_to_case() {
        assert_eq!(
            mentions("Motor INSURANCE renewal", NO_VAT_WORDS),
            Some("insurance")
        );
        assert_eq!(
            mentions("Account Balance correction", CORRECTION_WORDS),
            Some("correction")
        );
        assert_eq!(mentions("Adobe", NO_VAT_WORDS), None);
    }
}
