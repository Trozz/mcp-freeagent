//! Accounting: categories (the chart of accounts), reports, the general ledger, journals, tax
//! returns and payroll.

use super::{
    BooksServer, MAX_ITEMS, Query, category_ref, choice, date_arg, date_range, fields, id_arg,
    json_result, money_arg, opt_s, paging, push, ref_arg, required_date, required_text, run,
    text_arg, wrap,
};
use crate::error::{self, FaError};
use crate::view::{self, pence};
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

/// The tag on journals this server creates, so they can be told apart (FreeAgent's interface
/// cannot edit tagged journals).
pub const JOURNAL_TAG: &str = "mcp-freeagent";

const CATEGORY_GROUPS: &[(&str, &str)] = &[
    ("income_categories", "income"),
    ("cost_of_sales_categories", "cost_of_sales"),
    ("admin_expenses_categories", "admin_expenses"),
    ("general_categories", "general"),
];

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListCategoriesParams {
    /// Words to find in the name, code or tax reporting name, e.g. "travel", "software" or "285". Leave out for all.
    pub query: Option<String>,
    /// income, cost_of_sales, admin_expenses or general (balance sheet). Default: all.
    pub group: Option<String>,
    /// Include sub-accounts such as each bank account (750-x) and user (900-x).
    pub sub_accounts: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct PeriodParams {
    /// Start, YYYY-MM-DD. The period must be at most 12 months or within one accounting year.
    pub from_date: Option<String>,
    /// End, YYYY-MM-DD. Default: today.
    pub to_date: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ProfitLossParams {
    /// Start, YYYY-MM-DD. Default: the start of the current accounting year.
    pub from_date: Option<String>,
    /// End, YYYY-MM-DD. Default: today.
    pub to_date: Option<String>,
    /// A whole accounting year instead, e.g. 2025/26.
    pub accounting_period: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BalanceSheetParams {
    /// The date, YYYY-MM-DD. Default: today.
    pub as_at_date: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LedgerParams {
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    /// Only this category, by nominal code, e.g. 285 or 750-1.
    pub nominal_code: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListJournalsParams {
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    /// Only journals with this tag.
    pub tag: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct JournalEntry {
    /// The category's nominal code, e.g. 401 or 814.
    pub category: String,
    /// Debit positive, credit negative, e.g. 2500.00 or -2500.00.
    pub debit_value: String,
    pub description: Option<String>,
    /// For user categories (900 to 910): the user.
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateJournalParams {
    pub dated_on: String,
    pub description: String,
    /// The entries; their debit values must add up to zero.
    pub entries: Vec<JournalEntry>,
    /// A tag to find it by. Default: mcp-freeagent.
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TaxReturnsParams {
    /// vat, corporation_tax, self_assessment or final_accounts.
    pub kind: String,
    /// For self_assessment: the user. Default: the signed-in user.
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TaxReturnParams {
    /// vat, corporation_tax, self_assessment or final_accounts.
    pub kind: String,
    /// The period's end date, YYYY-MM-DD, as list_tax_returns gives it.
    pub period_ends_on: String,
    /// For self_assessment: the user. Default: the signed-in user.
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MarkTaxReturnParams {
    /// vat, corporation_tax, self_assessment or final_accounts.
    pub kind: String,
    pub period_ends_on: String,
    /// mark_as_filed, mark_as_unfiled, mark_as_paid or mark_as_unpaid.
    pub action: String,
    /// For marking a VAT or self assessment payment: the payment's due date, as the return's payments list it.
    pub payment_date: Option<String>,
    /// For self_assessment: the user. Default: the signed-in user.
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct PayrollParams {
    /// The tax year by the year it ends: 2026 for April 2025 to April 2026.
    pub year: u32,
    /// A pay period, 0 to 11, for its payslips. Leave out for the year's periods and payments.
    pub period: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaxKind {
    Vat,
    CorporationTax,
    SelfAssessment,
    FinalAccounts,
}

impl TaxKind {
    fn parse(raw: &str) -> error::Result<TaxKind> {
        Ok(
            match choice(
                "kind",
                Some(raw),
                &[
                    "vat",
                    "corporation_tax",
                    "self_assessment",
                    "final_accounts",
                ],
            )? {
                Some("vat") => TaxKind::Vat,
                Some("corporation_tax") => TaxKind::CorporationTax,
                Some("self_assessment") => TaxKind::SelfAssessment,
                _ => TaxKind::FinalAccounts,
            },
        )
    }
    fn collection(self) -> &'static str {
        match self {
            TaxKind::Vat => "vat_returns",
            TaxKind::CorporationTax => "corporation_tax_returns",
            TaxKind::SelfAssessment => "self_assessment_returns",
            TaxKind::FinalAccounts => "final_accounts_reports",
        }
    }
    fn single(self) -> &'static str {
        match self {
            TaxKind::Vat => "vat_return",
            TaxKind::CorporationTax => "corporation_tax_return",
            TaxKind::SelfAssessment => "self_assessment_return",
            TaxKind::FinalAccounts => "final_accounts_report",
        }
    }
}

impl BooksServer {
    /// The path of a tax kind's returns: `/users/<id>/self_assessment_returns` for self
    /// assessment, else the kind's collection.
    async fn tax_base(
        &self,
        c: &super::Caller,
        kind: TaxKind,
        user_id: Option<&str>,
    ) -> error::Result<Vec<String>> {
        if kind != TaxKind::SelfAssessment {
            return Ok(vec![kind.collection().to_string()]);
        }
        let user = match ref_arg(c, "user_id", "users", user_id)? {
            Some(u) => u,
            None => self.signed_in_user(c).await?,
        };
        let id = view::id_of(&user).unwrap_or_default().to_string();
        Ok(vec!["users".into(), id, kind.collection().into()])
    }
}

#[tool_router(router = accounting_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Find categories (the chart of accounts) by words in their name, code or tax reporting name: their nominal_code is what bills, expenses, invoice lines and bank explanations take as category. E.g. query \"software\" or \"travel\".",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_categories(
        &self,
        Parameters(p): Parameters<ListCategoriesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_categories", async {
            let c = self.caller(&parts)?;
            let group = choice(
                "group",
                p.group.as_deref(),
                &["income", "cost_of_sales", "admin_expenses", "general"],
            )?;
            let words: Vec<String> = text_arg("query", p.query.as_deref())?
                .map(|q| {
                    q.to_lowercase()
                        .split_whitespace()
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            let mut q: Query = Vec::new();
            if p.sub_accounts == Some(true) {
                q.push(("sub_accounts", "true".into()));
            }
            let a = c.get("categories", &["categories"], &q).await?;
            let mut out = Vec::new();
            for (key, name) in CATEGORY_GROUPS {
                if group.is_some_and(|g| g != *name) {
                    continue;
                }
                for cat in a
                    .body
                    .get(*key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let hay = [
                        "description",
                        "nominal_code",
                        "tax_reporting_name",
                        "group_description",
                    ]
                    .iter()
                    .filter_map(|k| cat.get(*k).and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .replace('_', " ");
                    if words.iter().all(|w| hay.contains(w.as_str())) {
                        let mut r = view::resource(cat);
                        r["group"] = Value::from(*name);
                        out.push(r);
                    }
                }
            }
            Ok(json_result(&json!({"count": out.len(), "categories": out})))
        })
        .await
    }

    #[tool(
        description = "Profit and loss for a period: income, expenses, operating profit, deductions such as corporation tax and dividends, and retained profit. Default: the current accounting year to date.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_profit_and_loss(
        &self,
        Parameters(p): Parameters<ProfitLossParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_profit_and_loss", async {
            let c = self.caller(&parts)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            match text_arg("accounting_period", p.accounting_period.as_deref())? {
                Some(ap) => {
                    let ok = ap.len() == 7
                        && ap.as_bytes()[4] == b'/'
                        && ap[..4].chars().chain(ap[5..].chars()).all(|c| c.is_ascii_digit());
                    if !ok || from.is_some() || to.is_some() {
                        return Err(FaError::invalid(
                            "accounting_period must be like 2025/26, instead of from_date and to_date",
                        ));
                    }
                    q.push(("accounting_period", ap));
                }
                None => {
                    push(&mut q, "from_date", from);
                    push(&mut q, "to_date", to);
                }
            }
            let a = c
                .get("profit_and_loss", &["accounting", "profit_and_loss", "summary"], &q)
                .await?;
            Ok(json_result(&view::resource(view::unwrap(&a.body, "profit_and_loss_summary"))))
        })
        .await
    }

    #[tool(
        description = "The balance sheet at a date: capital assets, current assets and liabilities, net current assets, total assets and owners' equity (FreeAgent rounds these to whole units).",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_balance_sheet(
        &self,
        Parameters(p): Parameters<BalanceSheetParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_balance_sheet", async {
            let c = self.caller(&parts)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "as_at_date",
                date_arg("as_at_date", p.as_at_date.as_deref())?,
            );
            let a = c
                .get("balance_sheet", &["accounting", "balance_sheet"], &q)
                .await?;
            Ok(json_result(&view::resource(view::unwrap(
                &a.body,
                "balance_sheet",
            ))))
        })
        .await
    }

    #[tool(
        description = "The trial balance for a period: every category with its total. Use display_nominal_code to name categories. A non-zero 999 Suspense or large unexplained balances point at bookkeeping to finish.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_trial_balance(
        &self,
        Parameters(p): Parameters<PeriodParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_trial_balance", async {
            let c = self.caller(&parts)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            let a = c
                .get(
                    "trial_balance",
                    &["accounting", "trial_balance", "summary"],
                    &q,
                )
                .await?;
            Ok(json_result(
                &json!({"trial_balance": view::items(&a.body, "trial_balance_summary")}),
            ))
        })
        .await
    }

    #[tool(
        description = "Cash in and out by month for a period, from the bank accounts, with the closing balance. Historic only. Default: the last 12 months.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_cashflow(
        &self,
        Parameters(p): Parameters<PeriodParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_cashflow", async {
            let c = self.caller(&parts)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let today = chrono::Local::now().date_naive();
            let to = to.unwrap_or_else(|| today.format("%Y-%m-%d").to_string());
            let from = from.unwrap_or_else(|| {
                (today - chrono::Months::new(11))
                    .with_day0_first()
                    .format("%Y-%m-%d")
                    .to_string()
            });
            let a = c
                .get(
                    "cashflow",
                    &["cashflow"],
                    &[("from_date", from), ("to_date", to)],
                )
                .await?;
            Ok(json_result(&view::resource(view::unwrap(
                &a.body, "cashflow",
            ))))
        })
        .await
    }

    #[tool(
        description = "The general ledger: accounting transactions (every posting) for a period, optionally for one category, with debit_value (credit negative) and the item it came from.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_accounting_transactions(
        &self,
        Parameters(p): Parameters<LedgerParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_accounting_transactions", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            if let Some(code) = p.nominal_code.as_deref().filter(|s| !s.trim().is_empty()) {
                q.push(("nominal_code", id_arg("nominal_code", code)?));
            }
            Ok(json_result(
                &c.list(
                    "accounting_transactions",
                    &["accounting", "transactions"],
                    "transactions",
                    q,
                    pg,
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "List journal sets (manual journals), optionally by tag.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_journal_sets(
        &self,
        Parameters(p): Parameters<ListJournalsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_journal_sets", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            push(&mut q, "tag", text_arg("tag", p.tag.as_deref())?);
            Ok(json_result(
                &c.list("journal_sets", &["journal_sets"], "journal_sets", q, pg)
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Post a manual journal: entries whose debit values (credits negative) add up to zero, e.g. a payroll journal or an accrual. Tagged mcp-freeagent unless another tag is given. Only for someone who knows double entry: show the entries and get agreement first.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_journal_set(
        &self,
        Parameters(p): Parameters<CreateJournalParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_journal_set", async {
            let c = self.caller(&parts)?;
            if p.entries.len() < 2 || p.entries.len() > MAX_ITEMS {
                return Err(FaError::invalid(format!(
                    "entries must hold 2 to {MAX_ITEMS} entries"
                )));
            }
            let mut sum = 0i64;
            let mut entries = Vec::new();
            for (i, e) in p.entries.iter().enumerate() {
                let n = |f: &str| format!("entries[{i}].{f}");
                let v = money_arg(&n("debit_value"), &e.debit_value)?;
                sum += pence(Some(&Value::from(v.as_str()))).unwrap_or(0);
                entries.push(Value::Object(fields(vec![
                    (
                        "category",
                        Some(Value::from(category_ref(&c, &e.category)?)),
                    ),
                    ("debit_value", Some(Value::from(v))),
                    (
                        "description",
                        opt_s(text_arg(&n("description"), e.description.as_deref())?),
                    ),
                    (
                        "user",
                        opt_s(ref_arg(&c, &n("user_id"), "users", e.user_id.as_deref())?),
                    ),
                ])));
            }
            if sum != 0 {
                return Err(FaError::invalid(format!(
                    "the entries do not balance: their debit values add up to {}, not 0",
                    view::money(sum)
                )));
            }
            let body = fields(vec![
                (
                    "dated_on",
                    Some(Value::from(required_date("dated_on", &p.dated_on)?)),
                ),
                (
                    "description",
                    Some(Value::from(required_text("description", &p.description)?)),
                ),
                (
                    "tag",
                    Some(Value::from(
                        text_arg("tag", p.tag.as_deref())?.unwrap_or_else(|| JOURNAL_TAG.into()),
                    )),
                ),
                ("journal_entries", Some(Value::Array(entries))),
            ]);
            let out = c
                .write(
                    Method::POST,
                    "create_journal_set",
                    &["journal_sets"],
                    &[],
                    Some(&wrap("journal_set", body)),
                    "journal_set",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "List tax returns of one kind (vat, corporation_tax, self_assessment or final_accounts) with periods, filing due dates, filing status and payments due.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_tax_returns(
        &self,
        Parameters(p): Parameters<TaxReturnsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_tax_returns", async {
            let c = self.caller(&parts)?;
            let kind = TaxKind::parse(&p.kind)?;
            let base = self.tax_base(&c, kind, p.user_id.as_deref()).await?;
            let segs: Vec<&str> = base.iter().map(String::as_str).collect();
            let a = c.get("tax_returns", &segs, &[]).await?;
            Ok(json_result(
                &json!({kind.collection(): view::items(&a.body, kind.collection())}),
            ))
        })
        .await
    }

    #[tool(
        description = "One tax return in full by its period end date; a VAT return includes its box 1 to 9 breakdown and payments. FreeAgent files returns to HMRC from its own interface, not through this server.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_tax_return(
        &self,
        Parameters(p): Parameters<TaxReturnParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_tax_return", async {
            let c = self.caller(&parts)?;
            let kind = TaxKind::parse(&p.kind)?;
            let period = required_date("period_ends_on", &p.period_ends_on)?;
            let mut segs = self.tax_base(&c, kind, p.user_id.as_deref()).await?;
            segs.push(period);
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            Ok(json_result(
                &c.one("tax_return", &segs, kind.single(), &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Record that a tax return was filed or paid outside FreeAgent, or undo that: mark_as_filed, mark_as_unfiled, mark_as_paid or mark_as_unpaid. It does not file anything with HMRC. VAT and self assessment payments need payment_date. Needs full access; confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn mark_tax_return(
        &self,
        Parameters(p): Parameters<MarkTaxReturnParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("mark_tax_return", async {
            let c = self.caller(&parts)?;
            let kind = TaxKind::parse(&p.kind)?;
            let period = required_date("period_ends_on", &p.period_ends_on)?;
            let action = choice(
                "action",
                Some(&p.action),
                &[
                    "mark_as_filed",
                    "mark_as_unfiled",
                    "mark_as_paid",
                    "mark_as_unpaid",
                ],
            )?
            .unwrap_or_default();
            let paying = action.ends_with("paid");
            if paying && kind == TaxKind::FinalAccounts {
                return Err(FaError::invalid("final accounts have no payments to mark"));
            }
            let mut segs = self.tax_base(&c, kind, p.user_id.as_deref()).await?;
            segs.push(period);
            if paying && matches!(kind, TaxKind::Vat | TaxKind::SelfAssessment) {
                let date =
                    date_arg("payment_date", p.payment_date.as_deref())?.ok_or_else(|| {
                        FaError::invalid(
                            "payment_date is required to mark a VAT or self assessment payment",
                        )
                    })?;
                segs.push("payments".into());
                segs.push(date);
            }
            segs.push(action.to_string());
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let out = c
                .write(
                    Method::PUT,
                    "mark_tax_return",
                    &segs,
                    &[],
                    None,
                    kind.single(),
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Payroll for a tax year (UK, read only): its pay periods with filing status and the PAYE payments due; with period, that period's payslips with pay, tax, NI, pensions and student loans.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_payroll(
        &self,
        Parameters(p): Parameters<PayrollParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_payroll", async {
            let c = self.caller(&parts)?;
            if !(2014..=2100).contains(&p.year) {
                return Err(FaError::invalid(
                    "year must be a tax year's end year, such as 2026",
                ));
            }
            let year = p.year.to_string();
            let a = match p.period {
                Some(n) if n <= 11 => {
                    let n = n.to_string();
                    c.get("payroll_period", &["payroll", &year, &n], &[])
                        .await?
                }
                Some(_) => return Err(FaError::invalid("period must be 0 to 11")),
                None => c.get("payroll_year", &["payroll", &year], &[]).await?,
            };
            Ok(json_result(&view::resource(&a.body)))
        })
        .await
    }
}

trait FirstOfMonth {
    fn with_day0_first(self) -> Self;
}

impl FirstOfMonth for chrono::NaiveDate {
    fn with_day0_first(self) -> Self {
        use chrono::Datelike;
        self.with_day(1).unwrap_or(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tax_kinds_name_their_endpoints() {
        let k = TaxKind::parse("VAT").unwrap();
        assert_eq!((k.collection(), k.single()), ("vat_returns", "vat_return"));
        assert_eq!(
            TaxKind::parse("final accounts").unwrap().collection(),
            "final_accounts_reports"
        );
        assert!(TaxKind::parse("payroll").is_err());
    }
}
