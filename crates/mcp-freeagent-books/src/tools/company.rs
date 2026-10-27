//! The company, its users, its tax deadlines, and a snapshot of where the business stands.

use super::{BooksServer, Caller, Query, choice, json_result, run};
use crate::error::{self, FaError};
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListUsersParams {
    /// all, staff, active_staff, advisors or active_advisors. Default: all.
    pub view: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct NoParams {}

/// Most open invoices or bills summed in the snapshot.
const SNAPSHOT_ITEMS: usize = 500;
/// Most bank accounts whose unexplained transactions are counted.
const SNAPSHOT_ACCOUNTS: usize = 10;

/// Count and total of `due_value`, and of those overdue, for open invoices or bills.
fn owed(items: &[Value]) -> Value {
    let mut n = 0;
    let mut total = 0i64;
    let mut overdue_n = 0;
    let mut overdue_total = 0i64;
    let mut currencies: Vec<String> = Vec::new();
    for i in items {
        let due = pence(i.get("due_value")).unwrap_or(0);
        n += 1;
        total += due;
        if i.get("status").and_then(Value::as_str) == Some("Overdue") {
            overdue_n += 1;
            overdue_total += due;
        }
        if let Some(c) = i.get("currency").and_then(Value::as_str)
            && !currencies.iter().any(|x| x == c)
        {
            currencies.push(c.to_string());
        }
    }
    let mut out = json!({
        "count": n,
        "due": money(total),
        "overdue_count": overdue_n,
        "overdue_due": money(overdue_total),
    });
    if currencies.len() > 1 {
        out["note"] = Value::from(format!(
            "amounts mix currencies ({}): due values are in each document's own currency",
            currencies.join(", ")
        ));
    }
    out
}

impl BooksServer {
    async fn snapshot(&self, c: &Caller) -> error::Result<Value> {
        let mut out = Map::new();
        let mut unavailable = Map::new();
        let mut note = |section: &str, e: FaError| {
            unavailable.insert(section.into(), Value::from(format!("{}: {e}", e.kind())));
        };
        match c.one("company", &["company"], "company", &[]).await {
            Ok(co) => {
                let pick = |k: &str| co.get(k).cloned();
                out.insert(
                    "company".into(),
                    view::prune(json!({
                        "name": pick("name"),
                        "type": pick("type"),
                        "currency": pick("currency"),
                        "sales_tax_registration_status": pick("sales_tax_registration_status"),
                        "first_accounting_year_end": pick("first_accounting_year_end"),
                    }))
                    .unwrap_or_default(),
                );
            }
            Err(e @ FaError::Auth) | Err(e @ FaError::RateLimited { .. }) => return Err(e),
            Err(e) => note("company", e),
        }
        let accounts = match c
            .all(
                "bank_accounts",
                &["bank_accounts"],
                "bank_accounts",
                Vec::new(),
                100,
            )
            .await
        {
            Ok((a, _)) => a,
            Err(e) => {
                note("bank_accounts", e);
                Vec::new()
            }
        };
        let mut banks = Vec::new();
        for (i, a) in accounts.iter().enumerate() {
            if a.get("status").and_then(Value::as_str) == Some("hidden") {
                continue;
            }
            let url = a.get("url").and_then(Value::as_str).unwrap_or_default();
            let mut row = json!({
                "id": view::id_of(url),
                "name": a.get("name"),
                "type": a.get("type"),
                "currency": a.get("currency"),
                "current_balance": a.get("current_balance"),
                "latest_activity_date": a.get("latest_activity_date"),
            });
            if i < SNAPSHOT_ACCOUNTS && !url.is_empty() {
                let q: Query = vec![
                    ("bank_account", url.to_string()),
                    ("view", "unexplained".into()),
                    ("per_page", "100".into()),
                ];
                match c.get("bank_transactions", &["bank_transactions"], &q).await {
                    Ok(ans) => {
                        let shown = ans
                            .body
                            .get("bank_transactions")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        row["unexplained_transactions"] =
                            Value::from(ans.total.unwrap_or(shown as u64));
                    }
                    Err(e) => note("unexplained_transactions", e),
                }
            }
            banks.push(view::prune(row).unwrap_or_default());
        }
        out.insert("bank_accounts".into(), Value::Array(banks));
        let open: Query = vec![("view", "open_or_overdue".into())];
        match c
            .all(
                "invoices",
                &["invoices"],
                "invoices",
                open.clone(),
                SNAPSHOT_ITEMS,
            )
            .await
        {
            Ok((items, more)) => {
                let mut v = owed(&items);
                if more {
                    v["truncated"] = Value::from(true);
                }
                out.insert("owed_to_you".into(), v);
            }
            Err(e) => note("owed_to_you", e),
        }
        match c
            .all("bills", &["bills"], "bills", open, SNAPSHOT_ITEMS)
            .await
        {
            Ok((items, more)) => {
                let mut v = owed(&items);
                if more {
                    v["truncated"] = Value::from(true);
                }
                out.insert("you_owe".into(), v);
            }
            Err(e) => note("you_owe", e),
        }
        match c
            .get("tax_timeline", &["company", "tax_timeline"], &[])
            .await
        {
            Ok(ans) => {
                let today = chrono::Local::now()
                    .date_naive()
                    .format("%Y-%m-%d")
                    .to_string();
                let upcoming: Vec<Value> = view::items(&ans.body, "timeline_items")
                    .into_iter()
                    .filter(|i| {
                        i.get("dated_on")
                            .and_then(Value::as_str)
                            .is_some_and(|d| d >= today.as_str())
                    })
                    .take(6)
                    .collect();
                out.insert("upcoming_deadlines".into(), Value::Array(upcoming));
            }
            Err(e) => note("upcoming_deadlines", e),
        }
        if !unavailable.is_empty() {
            out.insert("unavailable".into(), Value::Object(unavailable));
        }
        Ok(Value::Object(out))
    }
}

#[tool_router(router = company_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "The company these books belong to: name, type (limited company, sole trader...), currency, accounting year dates, VAT registration and rates, address and registration number. Call first to know the context.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_company(
        &self,
        Parameters(_p): Parameters<NoParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_company", async {
            let c = self.caller(&parts)?;
            Ok(json_result(
                &c.one("company", &["company"], "company", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "The signed-in FreeAgent user: name, email, role and permission_level (0 none, 1 time, 2 my money, 3 contacts and projects, 4 invoices, estimates and files, 5 bills, 6 banking, 7 tax, accounting and users, 8 full). Tools above their level are refused by FreeAgent.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_current_user(
        &self,
        Parameters(_p): Parameters<NoParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_current_user", async {
            let c = self.caller(&parts)?;
            Ok(json_result(
                &c.one("users_me", &["users", "me"], "user", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "The company's FreeAgent users (directors, employees, accountants) with their ids, roles and permission levels. Needs the tax, accounting and users access level.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_users(
        &self,
        Parameters(p): Parameters<ListUsersParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_users", async {
            let c = self.caller(&parts)?;
            let mut q: Query = Vec::new();
            if let Some(v) = choice(
                "view",
                p.view.as_deref(),
                &[
                    "all",
                    "staff",
                    "active_staff",
                    "advisors",
                    "active_advisors",
                ],
            )? {
                q.push(("view", v.into()));
            }
            let a = c.get("users", &["users"], &q).await?;
            Ok(json_result(
                &json!({"users": view::items(&a.body, "users")}),
            ))
        })
        .await
    }

    #[tool(
        description = "The company's tax timeline: upcoming and recent deadlines for VAT returns, corporation tax, self assessment, payroll and Companies House filings, with amounts due where known.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_tax_timeline(
        &self,
        Parameters(_p): Parameters<NoParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_tax_timeline", async {
            let c = self.caller(&parts)?;
            let a = c
                .get("tax_timeline", &["company", "tax_timeline"], &[])
                .await?;
            Ok(json_result(
                &json!({"timeline_items": view::items(&a.body, "timeline_items")}),
            ))
        })
        .await
    }

    #[tool(
        description = "Where the business stands, in one call: company basics, each bank account's balance and number of unexplained transactions, money owed to you (open and overdue invoices) and owed by you (open and overdue bills), and the next tax deadlines. Sections it could not read are listed under unavailable.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn business_snapshot(
        &self,
        Parameters(_p): Parameters<NoParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("business_snapshot", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.snapshot(&c).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owed_sums_due_values_and_overdue() {
        let items = vec![
            json!({"due_value": "100.00", "status": "Open", "currency": "GBP"}),
            json!({"due_value": "50.5", "status": "Overdue", "currency": "GBP"}),
        ];
        let v = owed(&items);
        assert_eq!(v["count"], 2);
        assert_eq!(v["due"], "150.50");
        assert_eq!(v["overdue_count"], 1);
        assert_eq!(v["overdue_due"], "50.50");
        assert!(v.get("note").is_none());
        let mixed = vec![
            json!({"due_value": "1", "currency": "GBP"}),
            json!({"due_value": "1", "currency": "EUR"}),
        ];
        assert!(owed(&mixed)["note"].as_str().unwrap().contains("EUR"));
    }
}
