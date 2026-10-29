//! Banking: accounts, transactions, explaining (categorising) transactions, approving FreeAgent's
//! guessed explanations, statement uploads and bank feeds.

use super::contacts::IdParams;
use super::purchases::{Attachment, file_of};
use super::{
    BooksServer, Caller, MAX_ITEMS, Query, category_ref, choice, date_arg, date_range, fields,
    id_arg, json_result, merge, money_arg, opt_decimal, opt_money, opt_s, paging, push, ref_arg,
    required_date, required_ref, required_text, run, since_arg, text_arg, wrap,
};
use crate::error::{self, FaError};
use crate::uploads::{File, Target};
use crate::view::{self, pence};
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Most explanations approved in one call.
const MAX_APPROVALS: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListBankAccountsParams {
    /// standard_bank_accounts, credit_card_accounts or paypal_accounts. Default: all.
    pub view: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListBankTransactionsParams {
    /// The bank account (list_bank_accounts).
    pub bank_account_id: String,
    /// all (default), unexplained, explained, manual, imported or marked_for_review (explained by FreeAgent's guess, awaiting approval).
    pub view: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub updated_since: Option<String>,
    /// Only the transactions of the latest statement upload.
    pub last_uploaded: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListExplanationsParams {
    pub bank_account_id: String,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub updated_since: Option<String>,
    /// Only explanations FreeAgent guessed that await approval.
    pub marked_for_review_only: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ExplanationFields {
    /// An ordinary payment or receipt: its category as a nominal code, e.g. 285 or 001; see list_categories.
    pub category: Option<String>,
    /// A customer paying an invoice (or a credit note refunded): the invoice's id.
    pub paid_invoice_id: Option<String>,
    /// Paying a supplier's bill (or a bill refund): the bill's id.
    pub paid_bill_id: Option<String>,
    /// Money moved to or from another of the company's own accounts: that bank account's id.
    pub transfer_bank_account_id: Option<String>,
    /// Money paid to or received from a director or employee (expenses repaid, salary, director's loan): the user's id. Add category for a specific kind, e.g. 902 net salary.
    pub paid_user_id: Option<String>,
    /// Amount explained: money out negative, money in positive. Default (on create): the transaction's unexplained amount.
    pub gross_value: Option<String>,
    /// For a foreign-currency invoice or bill: the amount in its currency.
    pub foreign_currency_value: Option<String>,
    /// Default (on create): the transaction's date.
    pub dated_on: Option<String>,
    pub description: Option<String>,
    /// VAT percentage, e.g. 20. Default: from the category.
    pub sales_tax_rate: Option<String>,
    /// TAXABLE, EXEMPT or OUT_OF_SCOPE.
    pub sales_tax_status: Option<String>,
    /// UK/Non-EC, EC Goods, EC Services, Reverse Charge or EC VAT MOSS.
    pub ec_status: Option<String>,
    pub receipt_reference: Option<String>,
    /// A project to rebill the cost to.
    pub project_id: Option<String>,
    /// cost, markup or price.
    pub rebill_type: Option<String>,
    /// The receipt or invoice for it.
    pub attachment: Option<Attachment>,
    /// Any other explanation field by FreeAgent's name, e.g. {"stock_item": ..., "capital_asset": ...}.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ExplainParams {
    /// The transaction to explain (list_bank_transactions with view unexplained).
    pub bank_transaction_id: String,
    #[serde(flatten)]
    pub fields: ExplanationFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateExplanationParams {
    pub id: String,
    /// false approves an explanation FreeAgent guessed.
    pub marked_for_review: Option<bool>,
    #[serde(flatten)]
    pub fields: ExplanationFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ApproveParams {
    pub bank_account_id: String,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    /// The explanations to approve, by id. Without them, the ones awaiting approval are listed and nothing changes, unless approve_all is true.
    pub explanation_ids: Option<Vec<String>>,
    /// Approve every explanation awaiting approval in the account and dates (up to 200).
    pub approve_all: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct StatementLine {
    pub dated_on: String,
    /// Money out negative, money in positive.
    pub amount: String,
    pub description: String,
    /// The bank's own transaction id, if known, so it is not imported twice.
    pub fitid: Option<String>,
    /// CREDIT, DEBIT, INT, DIV, FEE, SRVCHG, DEP, ATM, POS, XFER, CHECK, PAYMENT, CASH, DIRECTDEP, DIRECTDEBIT, REPEATPMT or OTHER (default).
    pub transaction_type: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UploadStatementParams {
    pub bank_account_id: String,
    /// The statement's lines, up to 100. Upload whole days: FreeAgent skips lines that match one already there by date, amount and description.
    pub lines: Vec<StatementLine>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct NoArgs {}

const EC_STATUSES: &[&str] = &[
    "UK/Non-EC",
    "EC Goods",
    "EC Services",
    "Reverse Charge",
    "EC VAT MOSS",
];

fn explanation_body(c: &Caller, f: &ExplanationFields) -> error::Result<Map<String, Value>> {
    let typed = fields(vec![
        (
            "category",
            f.category
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|r| category_ref(c, r))
                .transpose()?
                .map(Value::from),
        ),
        (
            "paid_invoice",
            opt_s(ref_arg(
                c,
                "paid_invoice_id",
                "invoices",
                f.paid_invoice_id.as_deref(),
            )?),
        ),
        (
            "paid_bill",
            opt_s(ref_arg(
                c,
                "paid_bill_id",
                "bills",
                f.paid_bill_id.as_deref(),
            )?),
        ),
        (
            "transfer_bank_account",
            opt_s(ref_arg(
                c,
                "transfer_bank_account_id",
                "bank_accounts",
                f.transfer_bank_account_id.as_deref(),
            )?),
        ),
        (
            "paid_user",
            opt_s(ref_arg(
                c,
                "paid_user_id",
                "users",
                f.paid_user_id.as_deref(),
            )?),
        ),
        (
            "gross_value",
            opt_s(opt_money("gross_value", f.gross_value.as_deref())?),
        ),
        (
            "foreign_currency_value",
            opt_s(opt_money(
                "foreign_currency_value",
                f.foreign_currency_value.as_deref(),
            )?),
        ),
        (
            "dated_on",
            opt_s(date_arg("dated_on", f.dated_on.as_deref())?),
        ),
        (
            "description",
            opt_s(text_arg("description", f.description.as_deref())?),
        ),
        (
            "sales_tax_rate",
            opt_s(opt_decimal("sales_tax_rate", f.sales_tax_rate.as_deref())?),
        ),
        (
            "sales_tax_status",
            choice(
                "sales_tax_status",
                f.sales_tax_status.as_deref(),
                &["TAXABLE", "EXEMPT", "OUT_OF_SCOPE"],
            )?
            .map(Value::from),
        ),
        (
            "ec_status",
            choice("ec_status", f.ec_status.as_deref(), EC_STATUSES)?.map(Value::from),
        ),
        (
            "receipt_reference",
            opt_s(text_arg(
                "receipt_reference",
                f.receipt_reference.as_deref(),
            )?),
        ),
        (
            "project",
            opt_s(ref_arg(
                c,
                "project_id",
                "projects",
                f.project_id.as_deref(),
            )?),
        ),
        (
            "rebill_type",
            choice(
                "rebill_type",
                f.rebill_type.as_deref(),
                &["cost", "markup", "price"],
            )?
            .map(Value::from),
        ),
    ]);
    // An attachment goes through the explanation's attachments endpoint after the write: the
    // single attachment field is withdrawn from FreeAgent's API.
    merge(f.extra.as_ref(), typed)
}

/// Attaches `file`, if any, to explanation `out` just written, and notes the outcome in `out`.
/// A failure is reported, not raised: the explanation itself was saved.
async fn attach_after(c: &Caller, out: &mut Value, file: Option<File>) {
    let Some(file) = file else { return };
    let Some(id) = out.get("id").and_then(Value::as_str).map(String::from) else {
        out["attachment_error"] = Value::from(
            "FreeAgent did not return the explanation's id, so the file was not attached; use attach_file",
        );
        return;
    };
    match crate::uploads::attach(
        &c.client_ref(),
        c.credential(),
        Target::Explanation,
        &id,
        &file,
    )
    .await
    {
        Ok(_) => out["attached"] = Value::from(file.name),
        Err(e) => {
            out["attachment_error"] = Value::from(format!("{}: {e}", e.kind()));
            out["note"] = Value::from(
                "the explanation was saved but the file was not attached; do not explain the transaction again, attach the file with attach_file or create_upload_link",
            );
        }
    }
}

/// Checks that an explanation says what the money was, in exactly one way.
fn check_target(body: &Map<String, Value>) -> error::Result<()> {
    let targets: Vec<&str> = [
        "paid_invoice",
        "paid_bill",
        "transfer_bank_account",
        "paid_user",
    ]
    .into_iter()
    .filter(|k| body.contains_key(*k))
    .collect();
    let has_category = body.contains_key("category");
    let ok = match targets.as_slice() {
        [] => {
            has_category
                || body.keys().any(|k| {
                    ["stock_item", "disposed_asset", "capital_asset"].contains(&k.as_str())
                })
        }
        ["paid_user"] => true,
        [_] => !has_category,
        _ => false,
    };
    if !ok {
        return Err(FaError::invalid(
            "say what the money was in one way: category, paid_invoice_id, paid_bill_id, transfer_bank_account_id, or paid_user_id (optionally with category)",
        ));
    }
    Ok(())
}

#[tool_router(router = banking_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "The company's bank, credit card and PayPal accounts with id, name, type, currency and FreeAgent's current_balance (from explained transactions, not the bank's live balance).",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_bank_accounts(
        &self,
        Parameters(p): Parameters<ListBankAccountsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_bank_accounts", async {
            let c = self.caller(&parts)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &[
                        "standard_bank_accounts",
                        "credit_card_accounts",
                        "paypal_accounts",
                    ],
                )?
                .map(String::from),
            );
            let (items, _) = c
                .all("bank_accounts", &["bank_accounts"], "bank_accounts", q, 200)
                .await?;
            Ok(json_result(
                &json!({"bank_accounts": items.iter().map(view::resource).collect::<Vec<_>>()}),
            ))
        })
        .await
    }

    #[tool(
        description = "One bank account in full: type, currency, balance, account details and latest activity.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_bank_account(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_bank_account", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("bank_account", &["bank_accounts", &id], "bank_account", &[])
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "List one bank account's transactions with date, amount (out negative), description, unexplained_amount and their explanations. view unexplained shows what still needs explaining; marked_for_review what FreeAgent guessed.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_bank_transactions(
        &self,
        Parameters(p): Parameters<ListBankTransactionsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_bank_transactions", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = vec![(
                "bank_account",
                required_ref(&c, "bank_account_id", "bank_accounts", &p.bank_account_id)?,
            )];
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &[
                        "all",
                        "unexplained",
                        "explained",
                        "manual",
                        "imported",
                        "marked_for_review",
                    ],
                )?
                .map(String::from),
            );
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            if p.last_uploaded == Some(true) {
                q.push(("last_uploaded", "true".into()));
            }
            Ok(json_result(
                &c.list(
                    "bank_transactions",
                    &["bank_transactions"],
                    "bank_transactions",
                    q,
                    pg,
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One bank transaction in full, with its explanations.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_bank_transaction(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_bank_transaction", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one(
                    "bank_transaction",
                    &["bank_transactions", &id],
                    "bank_transaction",
                    &[],
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "List the explanations of one bank account's transactions: what each payment or receipt was (category, invoice, bill, transfer or user), with VAT.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_bank_transaction_explanations(
        &self,
        Parameters(p): Parameters<ListExplanationsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_bank_transaction_explanations", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = vec![(
                "bank_account",
                required_ref(&c, "bank_account_id", "bank_accounts", &p.bank_account_id)?,
            )];
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            let mut out = c
                .list(
                    "bank_transaction_explanations",
                    &["bank_transaction_explanations"],
                    "bank_transaction_explanations",
                    q,
                    pg,
                )
                .await?;
            if p.marked_for_review_only == Some(true)
                && let Some(items) = out.get_mut("items").and_then(Value::as_array_mut)
            {
                items.retain(|e| e.get("marked_for_review").and_then(Value::as_bool) == Some(true));
                let n = items.len();
                out["count"] = Value::from(n);
                if let Value::Object(m) = &mut out {
                    m.remove("total");
                }
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Explain (categorise) a bank transaction: say what the money was with category (an ordinary cost or income), paid_invoice_id (a customer paying), paid_bill_id (paying a supplier), transfer_bank_account_id (between the company's accounts) or paid_user_id (to or from a director or employee). Amount and date default to the transaction's; a part explanation leaves the rest unexplained. Show the person your explanation and get their agreement first.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn explain_bank_transaction(
        &self,
        Parameters(p): Parameters<ExplainParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("explain_bank_transaction", async {
            let c = self.caller(&parts)?;
            let tx_id = id_arg("bank_transaction_id", &p.bank_transaction_id)?;
            let mut body = explanation_body(&c, &p.fields)?;
            check_target(&body)?;
            let file = p.fields.attachment.as_ref().map(file_of).transpose()?;
            let tx = c.one("bank_transaction", &["bank_transactions", &tx_id], "bank_transaction", &[]).await?;
            let unexplained = pence(tx.get("unexplained_amount"));
            if unexplained == Some(0) {
                return Err(FaError::invalid(format!(
                    "bank transaction {tx_id} is already fully explained; change its explanation with update_bank_transaction_explanation"
                )));
            }
            if !body.contains_key("gross_value") {
                let amount = tx
                    .get("unexplained_amount")
                    .or_else(|| tx.get("amount"))
                    .cloned()
                    .ok_or_else(|| FaError::invalid("give gross_value: the transaction has no amount"))?;
                body.insert("gross_value".into(), amount);
            } else if let (Some(given), Some(left)) = (pence(body.get("gross_value")), unexplained) {
                if given.signum() != left.signum() {
                    return Err(FaError::invalid(format!(
                        "gross_value must have the transaction's sign: money out is negative, money in positive (unexplained amount {})",
                        view::money(left)
                    )));
                }
                if given.abs() > left.abs() {
                    return Err(FaError::invalid(format!(
                        "gross_value is more than the unexplained amount, {}",
                        view::money(left)
                    )));
                }
            }
            if !body.contains_key("dated_on")
                && let Some(d) = tx.get("dated_on")
            {
                body.insert("dated_on".into(), d.clone());
            }
            if !body.contains_key("description")
                && body.contains_key("category")
                && let Some(d) = tx.get("description")
            {
                body.insert("description".into(), d.clone());
            }
            body.insert("bank_transaction".into(), Value::from(c.url("bank_transactions", &tx_id)));
            let mut out = c
                .write(
                    Method::POST,
                    "create_explanation",
                    &["bank_transaction_explanations"],
                    &[],
                    Some(&wrap("bank_transaction_explanation", body)),
                    "bank_transaction_explanation",
                )
                .await?;
            attach_after(&c, &mut out, file).await;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change a bank transaction explanation: only the fields given change. marked_for_review false approves one FreeAgent guessed.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_bank_transaction_explanation(
        &self,
        Parameters(p): Parameters<UpdateExplanationParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_bank_transaction_explanation", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let mut body = explanation_body(&c, &p.fields)?;
            if let Some(m) = p.marked_for_review {
                body.insert("marked_for_review".into(), Value::from(m));
            }
            let file = p.fields.attachment.as_ref().map(file_of).transpose()?;
            if body.is_empty() {
                let Some(file) = file else {
                    return Err(FaError::invalid("give at least one field to change"));
                };
                let a = crate::uploads::attach(&c.client_ref(), c.credential(), Target::Explanation, &id, &file).await?;
                return Ok(json_result(&json!({"attached": file.name, "to": c.url("bank_transaction_explanations", &id), "result": view::resource(&a)})));
            }
            let mut out = c
                .write(
                    Method::PUT,
                    "update_explanation",
                    &["bank_transaction_explanations", &id],
                    &[],
                    Some(&wrap("bank_transaction_explanation", body)),
                    "bank_transaction_explanation",
                )
                .await?;
            attach_after(&c, &mut out, file).await;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete a bank transaction explanation, leaving the transaction unexplained again. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_bank_transaction_explanation(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_bank_transaction_explanation", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            c.send(
                Method::DELETE,
                "delete_explanation",
                &["bank_transaction_explanations", &id],
                &[],
                None,
            )
            .await?;
            Ok(json_result(
                &json!({"deleted": c.url("bank_transaction_explanations", &id)}),
            ))
        })
        .await
    }

    #[tool(
        description = "Approve explanations FreeAgent guessed (marked for review) in one bank account. Without explanation_ids or approve_all it only lists them, so you can show the person; then approve the ones they agree with.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn approve_explanations(
        &self,
        Parameters(p): Parameters<ApproveParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("approve_explanations", async {
            let c = self.caller(&parts)?;
            let account = required_ref(&c, "bank_account_id", "bank_accounts", &p.bank_account_id)?;
            let ids: Vec<String> = p
                .explanation_ids
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|r| id_arg("explanation_ids", r))
                .collect::<error::Result<_>>()?;
            if ids.len() > MAX_APPROVALS {
                return Err(FaError::invalid(format!("explanation_ids is limited to {MAX_APPROVALS}")));
            }
            let targets: Vec<String> = if !ids.is_empty() {
                ids
            } else {
                let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
                let mut q: Query = vec![("bank_account", account)];
                push(&mut q, "from_date", from);
                push(&mut q, "to_date", to);
                let (all, more) = c
                    .all("bank_transaction_explanations", &["bank_transaction_explanations"], "bank_transaction_explanations", q, 2000)
                    .await?;
                let pending: Vec<Value> = all
                    .iter()
                    .filter(|e| e.get("marked_for_review").and_then(Value::as_bool) == Some(true))
                    .map(view::resource)
                    .collect();
                if p.approve_all != Some(true) {
                    let mut out = json!({
                        "awaiting_approval": pending,
                        "note": "nothing was changed: approve with explanation_ids, or approve_all true",
                    });
                    if more {
                        out["truncated"] = Value::from(true);
                    }
                    return Ok(json_result(&out));
                }
                pending
                    .iter()
                    .filter_map(|e| e.get("id").and_then(Value::as_str).map(String::from))
                    .take(MAX_APPROVALS)
                    .collect()
            };
            let body = json!({"bank_transaction_explanation": {"marked_for_review": false}});
            let mut approved = Vec::new();
            let mut failed = Vec::new();
            for id in &targets {
                match c
                    .send(Method::PUT, "approve_explanation", &["bank_transaction_explanations", id], &[], Some(&body))
                    .await
                {
                    Ok(_) => approved.push(Value::from(id.as_str())),
                    Err(e @ FaError::RateLimited { .. }) | Err(e @ FaError::Auth) => {
                        failed.push(json!({"id": id, "error": format!("{}: {e}", e.kind())}));
                        break;
                    }
                    Err(e) => failed.push(json!({"id": id, "error": format!("{}: {e}", e.kind())})),
                }
            }
            let mut out = json!({"approved": approved});
            if !failed.is_empty() {
                out["failed"] = Value::Array(failed);
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Add transactions to a bank account from statement lines, e.g. for an account without a bank feed. Then list_bank_transactions with last_uploaded true to check what was imported.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn upload_bank_statement(
        &self,
        Parameters(p): Parameters<UploadStatementParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("upload_bank_statement", async {
            let c = self.caller(&parts)?;
            let account = required_ref(&c, "bank_account_id", "bank_accounts", &p.bank_account_id)?;
            if p.lines.is_empty() || p.lines.len() > MAX_ITEMS {
                return Err(FaError::invalid(format!("lines must hold 1 to {MAX_ITEMS} statement lines")));
            }
            let lines: Vec<Value> = p
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let n = |f: &str| format!("lines[{i}].{f}");
                    Ok(Value::Object(fields(vec![
                        ("dated_on", Some(Value::from(required_date(&n("dated_on"), &l.dated_on)?))),
                        ("amount", Some(Value::from(money_arg(&n("amount"), &l.amount)?))),
                        ("description", Some(Value::from(required_text(&n("description"), &l.description)?))),
                        ("fitid", opt_s(text_arg(&n("fitid"), l.fitid.as_deref())?)),
                        (
                            "transaction_type",
                            choice(
                                &n("transaction_type"),
                                l.transaction_type.as_deref(),
                                &["CREDIT", "DEBIT", "INT", "DIV", "FEE", "SRVCHG", "DEP", "ATM", "POS", "XFER", "CHECK", "PAYMENT", "CASH", "DIRECTDEP", "DIRECTDEBIT", "REPEATPMT", "OTHER"],
                            )?
                            .map(Value::from),
                        ),
                    ])))
                })
                .collect::<error::Result<_>>()?;
            let n = lines.len();
            c.send(
                Method::POST,
                "upload_statement",
                &["bank_transactions", "statement"],
                &[("bank_account", account)],
                Some(&json!({"statement": lines})),
            )
            .await?;
            Ok(json_result(&json!({
                "uploaded_lines": n,
                "note": "FreeAgent skips lines it already has; list_bank_transactions with last_uploaded true shows what was imported",
            })))
        })
        .await
    }

    #[tool(
        description = "The company's bank feeds: which accounts are connected to their bank, the feed state, and when its authorisation (SCA) expires and must be renewed in FreeAgent.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_bank_feeds(
        &self,
        Parameters(_p): Parameters<NoArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_bank_feeds", async {
            let c = self.caller(&parts)?;
            let a = c.get("bank_feeds", &["bank_feeds"], &[]).await?;
            Ok(json_result(
                &json!({"bank_feeds": view::items(&a.body, "bank_feeds")}),
            ))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(keys: &[&str]) -> Map<String, Value> {
        keys.iter()
            .map(|k| (k.to_string(), Value::from("x")))
            .collect()
    }

    #[test]
    fn an_explanation_says_what_the_money_was_once() {
        assert!(check_target(&body(&["category"])).is_ok());
        assert!(check_target(&body(&["paid_invoice"])).is_ok());
        assert!(check_target(&body(&["paid_user", "category"])).is_ok());
        assert!(check_target(&body(&["transfer_bank_account"])).is_ok());
        assert!(check_target(&body(&[])).is_err());
        assert!(check_target(&body(&["paid_invoice", "category"])).is_err());
        assert!(check_target(&body(&["paid_invoice", "paid_bill"])).is_err());
        assert!(check_target(&body(&["capital_asset"])).is_ok());
    }
}
