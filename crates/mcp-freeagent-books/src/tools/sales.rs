//! Sales: invoices, estimates, credit notes and recurring invoices, their emails and PDFs, and the
//! price list.

use super::contacts::IdParams;
use super::{
    BooksServer, Caller, MAX_ITEMS, Query, category_ref, choice, currency_arg, date_arg, fields,
    id_arg, json_result, merge, money_arg, opt_decimal, opt_s, paging, push, ref_arg, required_ref,
    required_text, run, since_arg, text_arg, wrap,
};
use crate::error::{self, FaError};
use crate::view;
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const ITEM_TYPES: &[&str] = &[
    "Hours", "Days", "Weeks", "Months", "Years", "Products", "Services", "Training", "Expenses",
    "Comment", "Bills", "Discount", "Credit", "VAT", "Stock",
];
const EC_STATUSES: &[&str] = &[
    "UK/Non-EC",
    "EC Goods",
    "EC Services",
    "Reverse Charge",
    "EC VAT MOSS",
];

/// The kinds of sales document that share emails, PDFs and line items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Doc {
    Invoice,
    Estimate,
    CreditNote,
}

impl Doc {
    fn parse(raw: &str) -> error::Result<Doc> {
        match choice(
            "document",
            Some(raw),
            &["invoice", "estimate", "credit_note"],
        )? {
            Some("invoice") => Ok(Doc::Invoice),
            Some("estimate") => Ok(Doc::Estimate),
            _ => Ok(Doc::CreditNote),
        }
    }
    fn collection(self) -> &'static str {
        match self {
            Doc::Invoice => "invoices",
            Doc::Estimate => "estimates",
            Doc::CreditNote => "credit_notes",
        }
    }
    fn root(self) -> &'static str {
        match self {
            Doc::Invoice => "invoice",
            Doc::Estimate => "estimate",
            Doc::CreditNote => "credit_note",
        }
    }
    fn items_key(self) -> &'static str {
        match self {
            Doc::Invoice => "invoice_items",
            Doc::Estimate => "estimate_items",
            Doc::CreditNote => "credit_note_items",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LineItem {
    /// What was sold, as it reads on the document.
    pub description: String,
    /// Hours, Days, Weeks, Months, Years, Products, Services, Training, Expenses, Comment, Bills, Discount, Credit, VAT or Stock; leave out for no unit.
    pub item_type: Option<String>,
    /// How many, e.g. 7.5. Default: 1.
    pub quantity: Option<String>,
    /// Price per unit before VAT, e.g. 80.
    pub price: Option<String>,
    /// VAT percentage, e.g. 20, 5 or 0. Default: from the contact's charge_sales_tax setting.
    pub sales_tax_rate: Option<String>,
    /// TAXABLE, EXEMPT or OUT_OF_SCOPE.
    pub sales_tax_status: Option<String>,
    /// Income category as a nominal code, e.g. 001 (Sales). Default: the company's sales category.
    pub category: Option<String>,
    /// On update: the id of an existing item to change. Leave out to add a new item.
    pub id: Option<String>,
    /// On update with id: true removes that item.
    pub delete: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListInvoicesParams {
    /// all (default), recent_open_or_overdue, open, overdue, open_or_overdue, draft, paid, scheduled_to_email, thank_you_emails, reminder_emails, or last_N_months such as last_3_months.
    pub view: Option<String>,
    pub contact_id: Option<String>,
    pub project_id: Option<String>,
    pub updated_since: Option<String>,
    /// created_at (default) or updated_at; prefix - for newest first.
    pub sort: Option<String>,
    /// Include each invoice's line items.
    pub with_items: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct InvoiceFields {
    /// The invoice date. Default (on create): today.
    pub dated_on: Option<String>,
    /// Days until due; 0 means due on receipt. Default (on create): the contact's payment terms, else 30.
    pub payment_terms_in_days: Option<u32>,
    /// A fixed due date instead of payment terms.
    pub due_on: Option<String>,
    /// Invoice number. Default: the next in sequence.
    pub reference: Option<String>,
    /// ISO 4217 code. Default: the company's currency.
    pub currency: Option<String>,
    pub project_id: Option<String>,
    /// Text shown on the invoice, such as payment details or thanks.
    pub comments: Option<String>,
    pub po_reference: Option<String>,
    pub discount_percent: Option<String>,
    /// Add the project's unbilled time: billed_grouped_by_single_timeslip, billed_grouped_by_timeslip, billed_grouped_by_timeslip_task or billed_grouped_by_timeslip_date. Needs project_id.
    pub include_timeslips: Option<String>,
    /// Add the project's rebillable expenses: billed_grouped_by_single_expense or billed_grouped_by_expense. Needs project_id.
    pub include_expenses: Option<String>,
    /// UK/Non-EC (default), EC Goods, EC Services, Reverse Charge or EC VAT MOSS.
    pub ec_status: Option<String>,
    /// Line items.
    pub items: Option<Vec<LineItem>>,
    /// Any other invoice field by FreeAgent's name, e.g. {"payment_methods": {"stripe": true}}.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateInvoiceParams {
    /// The customer.
    pub contact_id: String,
    #[serde(flatten)]
    pub fields: InvoiceFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateInvoiceParams {
    pub id: String,
    pub contact_id: Option<String>,
    #[serde(flatten)]
    pub fields: InvoiceFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct InvoiceActionParams {
    pub id: String,
    /// mark_as_sent (also re-opens a cancelled invoice), mark_as_draft, mark_as_scheduled (to be emailed by FreeAgent), mark_as_cancelled (write off an overdue invoice), convert_to_credit_note (a draft negative invoice) or duplicate (a new draft copy dated today).
    pub action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListEstimatesParams {
    /// all (default), recent, draft, non_draft, sent, approved, rejected or invoiced.
    pub view: Option<String>,
    pub contact_id: Option<String>,
    pub project_id: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub updated_since: Option<String>,
    pub with_items: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateEstimateParams {
    pub contact_id: String,
    /// Estimate (default), Quote or Proposal.
    pub estimate_type: Option<String>,
    /// Its number, e.g. EST-012.
    pub reference: String,
    /// Default: today.
    pub dated_on: Option<String>,
    /// Default: the company's currency.
    pub currency: Option<String>,
    pub project_id: Option<String>,
    pub notes: Option<String>,
    pub discount_percent: Option<String>,
    pub ec_status: Option<String>,
    pub items: Vec<LineItem>,
    /// Any other estimate field by FreeAgent's name.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct EstimateActionParams {
    pub id: String,
    /// mark_as_sent, mark_as_draft, mark_as_approved, mark_as_rejected, convert_to_invoice or duplicate.
    pub action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListCreditNotesParams {
    /// all (default), recent_open_or_overdue, open, overdue, open_or_overdue, draft, refunded or last_N_months.
    pub view: Option<String>,
    pub contact_id: Option<String>,
    pub project_id: Option<String>,
    pub updated_since: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateCreditNoteParams {
    pub contact_id: String,
    /// Default: today.
    pub dated_on: Option<String>,
    /// Default: 0 (due now).
    pub payment_terms_in_days: Option<u32>,
    pub reference: Option<String>,
    pub currency: Option<String>,
    pub project_id: Option<String>,
    pub comments: Option<String>,
    /// The items credited, with positive prices.
    pub items: Vec<LineItem>,
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListRecurringParams {
    /// draft, active or inactive. Default: all.
    pub view: Option<String>,
    pub contact_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SendEmailParams {
    /// invoice, estimate or credit_note.
    pub document: String,
    pub id: String,
    /// Recipient email addresses, comma-separated. Default: the contact's billing or main email.
    pub to: Option<String>,
    /// The sender: a verified address from list_email_addresses. Default: FreeAgent's.
    pub from: Option<String>,
    pub subject: Option<String>,
    /// The message. FreeAgent adds the link to view and pay.
    pub body: Option<String>,
    /// Send FreeAgent's own email template instead of subject and body.
    pub use_template: Option<bool>,
    /// Also send a copy to the sender. Default: true.
    pub email_to_sender: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocumentParams {
    /// invoice, estimate or credit_note.
    pub document: String,
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListPriceListParams {
    /// created_at, code or updated_at.
    pub sort: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct NoArgs {}

/// FreeAgent's line items for `doc` from the tool's.
fn line_items(
    c: &Caller,
    doc: Doc,
    items: &[LineItem],
    updating: bool,
) -> error::Result<Vec<Value>> {
    if items.len() > MAX_ITEMS {
        return Err(FaError::invalid(format!("items is limited to {MAX_ITEMS}")));
    }
    let mut types: Vec<&'static str> = ITEM_TYPES.to_vec();
    if doc == Doc::Estimate {
        types.push("-no unit-");
    }
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let n = |f: &str| format!("items[{i}].{f}");
            if let Some(raw) = it.id.as_deref().filter(|s| !s.trim().is_empty()) {
                if !updating {
                    return Err(FaError::invalid(format!("{} is only for update", n("id"))));
                }
                let id = id_arg(&n("id"), raw)?;
                if it.delete == Some(true) {
                    return Ok(json!({"id": id, "_destroy": 1}));
                }
            } else if it.delete == Some(true) {
                return Err(FaError::invalid(format!(
                    "{} needs the item's id",
                    n("delete")
                )));
            }
            let item_type = choice(&n("item_type"), it.item_type.as_deref(), &types)?;
            let price = it
                .price
                .as_deref()
                .map(|p| money_arg(&n("price"), p))
                .transpose()?;
            if price.is_none() && it.id.is_none() && item_type != Some("Comment") {
                return Err(FaError::invalid(format!("{} is required", n("price"))));
            }
            let mut m = fields(vec![
                (
                    "id",
                    it.id
                        .as_deref()
                        .map(|r| id_arg(&n("id"), r))
                        .transpose()?
                        .map(Value::from),
                ),
                (
                    "description",
                    Some(Value::from(required_text(
                        &n("description"),
                        &it.description,
                    )?)),
                ),
                ("item_type", item_type.map(Value::from)),
                (
                    "quantity",
                    Some(Value::from(
                        opt_decimal(&n("quantity"), it.quantity.as_deref())?
                            .unwrap_or_else(|| "1".into()),
                    )),
                ),
                ("price", opt_s(price)),
                (
                    "sales_tax_rate",
                    opt_s(opt_decimal(
                        &n("sales_tax_rate"),
                        it.sales_tax_rate.as_deref(),
                    )?),
                ),
                (
                    "sales_tax_status",
                    choice(
                        &n("sales_tax_status"),
                        it.sales_tax_status.as_deref(),
                        &["TAXABLE", "EXEMPT", "OUT_OF_SCOPE"],
                    )?
                    .map(Value::from),
                ),
                (
                    "category",
                    it.category
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .map(|r| category_ref(c, r))
                        .transpose()?
                        .map(Value::from),
                ),
            ]);
            if doc == Doc::Estimate && !m.contains_key("item_type") {
                m.insert("item_type".into(), Value::from("-no unit-"));
            }
            Ok(Value::Object(m))
        })
        .collect()
}

fn today() -> String {
    chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

fn invoice_body(
    c: &Caller,
    f: &InvoiceFields,
    doc: Doc,
    updating: bool,
) -> error::Result<Map<String, Value>> {
    let typed = fields(vec![
        (
            "dated_on",
            opt_s(date_arg("dated_on", f.dated_on.as_deref())?),
        ),
        (
            "payment_terms_in_days",
            f.payment_terms_in_days.map(Value::from),
        ),
        ("due_on", opt_s(date_arg("due_on", f.due_on.as_deref())?)),
        (
            "reference",
            opt_s(text_arg("reference", f.reference.as_deref())?),
        ),
        ("currency", opt_s(currency_arg(f.currency.as_deref())?)),
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
            "comments",
            opt_s(text_arg("comments", f.comments.as_deref())?),
        ),
        (
            "po_reference",
            opt_s(text_arg("po_reference", f.po_reference.as_deref())?),
        ),
        (
            "discount_percent",
            opt_s(opt_decimal(
                "discount_percent",
                f.discount_percent.as_deref(),
            )?),
        ),
        (
            "include_timeslips",
            choice(
                "include_timeslips",
                f.include_timeslips.as_deref(),
                &[
                    "billed_grouped_by_single_timeslip",
                    "billed_grouped_by_timeslip",
                    "billed_grouped_by_timeslip_task",
                    "billed_grouped_by_timeslip_date",
                ],
            )?
            .map(Value::from),
        ),
        (
            "include_expenses",
            choice(
                "include_expenses",
                f.include_expenses.as_deref(),
                &[
                    "billed_grouped_by_single_expense",
                    "billed_grouped_by_expense",
                ],
            )?
            .map(Value::from),
        ),
        (
            "ec_status",
            choice("ec_status", f.ec_status.as_deref(), EC_STATUSES)?.map(Value::from),
        ),
        (
            doc.items_key(),
            f.items
                .as_deref()
                .map(|items| line_items(c, doc, items, updating))
                .transpose()?
                .map(Value::Array),
        ),
    ]);
    if (typed.contains_key("include_timeslips") || typed.contains_key("include_expenses"))
        && !typed.contains_key("project")
        && !updating
    {
        return Err(FaError::invalid(
            "include_timeslips and include_expenses need project_id",
        ));
    }
    merge(f.extra.as_ref(), typed)
}

fn view_arg(v: Option<&str>, fixed: &[&'static str]) -> error::Result<Option<String>> {
    let Some(t) = v.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if let Some(n) = t
        .strip_prefix("last_")
        .and_then(|r| r.strip_suffix("_months"))
        && n.parse::<u32>().is_ok_and(|n| (1..=120).contains(&n))
    {
        return Ok(Some(t.to_string()));
    }
    Ok(choice("view", Some(t), fixed)?.map(String::from))
}

fn sort_arg(v: Option<&str>) -> error::Result<Option<String>> {
    let Some(t) = text_arg("sort", v)? else {
        return Ok(None);
    };
    if !["created_at", "updated_at"].contains(&t.trim_start_matches('-')) {
        return Err(FaError::invalid(
            "sort must be created_at or updated_at, with - in front for newest first",
        ));
    }
    Ok(Some(t))
}

impl BooksServer {
    /// The contact's payment terms, else 30.
    async fn payment_terms(&self, c: &Caller, contact: &str) -> u32 {
        let id = view::id_of(contact).unwrap_or_default().to_string();
        c.one("contact", &["contacts", &id], "contact", &[])
            .await
            .ok()
            .and_then(|v| {
                v.get("default_payment_terms_in_days")
                    .and_then(Value::as_u64)
            })
            .map(|d| d as u32)
            .unwrap_or(30)
    }

    async fn company_currency(&self, c: &Caller) -> Option<Value> {
        c.one("company", &["company"], "company", &[])
            .await
            .ok()?
            .get("currency")
            .cloned()
    }
}

#[tool_router(router = sales_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "List invoices with reference, contact, dates, status (Draft, Open, Overdue, Paid...), total_value and due_value. view open_or_overdue shows what customers owe; overdue what is late.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_invoices(
        &self,
        Parameters(p): Parameters<ListInvoicesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_invoices", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                view_arg(
                    p.view.as_deref(),
                    &[
                        "all",
                        "recent_open_or_overdue",
                        "open",
                        "overdue",
                        "open_or_overdue",
                        "draft",
                        "paid",
                        "scheduled_to_email",
                        "thank_you_emails",
                        "reminder_emails",
                    ],
                )?,
            );
            push(
                &mut q,
                "contact",
                ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())?,
            );
            push(
                &mut q,
                "project",
                ref_arg(&c, "project_id", "projects", p.project_id.as_deref())?,
            );
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            push(&mut q, "sort", sort_arg(p.sort.as_deref())?);
            if p.with_items == Some(true) {
                q.push(("nested_invoice_items", "true".into()));
            }
            Ok(json_result(
                &c.list("invoices", &["invoices"], "invoices", q, pg).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One invoice in full, with its line items, totals, VAT, what is paid and due, and its payment link.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_invoice(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_invoice", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("invoice", &["invoices", &id], "invoice", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Create a draft invoice for a customer from line items, and/or a project's unbilled time and expenses (include_timeslips, include_expenses). Dated today and on the contact's payment terms unless given. It stays a draft until invoice_action mark_as_sent or send_document_email. Returns it with its id and totals.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_invoice(
        &self,
        Parameters(p): Parameters<CreateInvoiceParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_invoice", async {
            let c = self.caller(&parts)?;
            let contact = required_ref(&c, "contact_id", "contacts", &p.contact_id)?;
            let mut body = invoice_body(&c, &p.fields, Doc::Invoice, false)?;
            let has_items = body
                .get("invoice_items")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
            if !has_items
                && !body.contains_key("include_timeslips")
                && !body.contains_key("include_expenses")
            {
                return Err(FaError::invalid(
                    "give items, or include_timeslips or include_expenses with project_id",
                ));
            }
            body.entry("dated_on")
                .or_insert_with(|| Value::from(today()));
            if !body.contains_key("payment_terms_in_days") {
                let terms = self.payment_terms(&c, &contact).await;
                body.insert("payment_terms_in_days".into(), Value::from(terms));
            }
            body.insert("contact".into(), Value::from(contact));
            let out = c
                .write(
                    Method::POST,
                    "create_invoice",
                    &["invoices"],
                    &[],
                    Some(&wrap("invoice", body)),
                    "invoice",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change an invoice: only the fields given change. items adds new lines; an item with id changes that line, and with id and delete true removes it. Sent invoices may be locked; mark_as_draft first.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn update_invoice(
        &self,
        Parameters(p): Parameters<UpdateInvoiceParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_invoice", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let mut body = invoice_body(&c, &p.fields, Doc::Invoice, true)?;
            if let Some(contact) = ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())? {
                body.insert("contact".into(), Value::from(contact));
            }
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_invoice",
                    &["invoices", &id],
                    &[],
                    Some(&wrap("invoice", body)),
                    "invoice",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete a draft invoice. Invoices that were sent are refused: cancel them with invoice_action mark_as_cancelled, or issue a credit note. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_invoice(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_invoice", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let inv = c.one("invoice", &["invoices", &id], "invoice", &[]).await?;
            let status = inv.get("status").and_then(Value::as_str).unwrap_or("");
            if status != "Draft" {
                return Err(FaError::invalid(format!(
                    "invoice {id} is {status}, not Draft: only drafts are deleted here. Cancel it with invoice_action mark_as_cancelled or credit it with create_credit_note"
                )));
            }
            c.send(Method::DELETE, "delete_invoice", &["invoices", &id], &[], None).await?;
            Ok(json_result(&json!({"deleted": c.url("invoices", &id), "reference": inv.get("reference")})))
        })
        .await
    }

    #[tool(
        description = "Move an invoice through its life: mark_as_sent (without emailing), mark_as_draft, mark_as_scheduled, mark_as_cancelled (write off as unpaid), convert_to_credit_note, or duplicate it as a new draft. To email it, use send_document_email. Record a payment by explaining the bank transaction with paid_invoice_id.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn invoice_action(
        &self,
        Parameters(p): Parameters<InvoiceActionParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("invoice_action", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let action = choice(
                "action",
                Some(&p.action),
                &[
                    "mark_as_sent",
                    "mark_as_draft",
                    "mark_as_scheduled",
                    "mark_as_cancelled",
                    "convert_to_credit_note",
                    "duplicate",
                ],
            )?
            .unwrap_or_default();
            let out = if action == "duplicate" {
                c.write(
                    Method::POST,
                    "invoice_duplicate",
                    &["invoices", &id, "duplicate"],
                    &[],
                    None,
                    "invoice",
                )
                .await?
            } else {
                let key = if action == "convert_to_credit_note" {
                    "credit_note"
                } else {
                    "invoice"
                };
                c.write(
                    Method::PUT,
                    "invoice_transition",
                    &["invoices", &id, "transitions", action],
                    &[],
                    None,
                    key,
                )
                .await?
            };
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "List estimates and quotes with reference, contact, status and totals.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_estimates(
        &self,
        Parameters(p): Parameters<ListEstimatesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_estimates", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = super::date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &[
                        "all",
                        "recent",
                        "draft",
                        "non_draft",
                        "sent",
                        "approved",
                        "rejected",
                        "invoiced",
                    ],
                )?
                .map(String::from),
            );
            push(
                &mut q,
                "contact",
                ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())?,
            );
            push(
                &mut q,
                "project",
                ref_arg(&c, "project_id", "projects", p.project_id.as_deref())?,
            );
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            if p.with_items == Some(true) {
                q.push(("nested_estimate_items", "true".into()));
            }
            Ok(json_result(
                &c.list("estimates", &["estimates"], "estimates", q, pg)
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One estimate or quote in full, with its items.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_estimate(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_estimate", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("estimate", &["estimates", &id], "estimate", &[])
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Create a draft estimate, quote or proposal for a contact from line items. Send it with send_document_email; when accepted, estimate_action convert_to_invoice.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_estimate(
        &self,
        Parameters(p): Parameters<CreateEstimateParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_estimate", async {
            let c = self.caller(&parts)?;
            if p.items.is_empty() {
                return Err(FaError::invalid("items must hold at least one line"));
            }
            let currency = match currency_arg(p.currency.as_deref())? {
                Some(cur) => Some(Value::from(cur)),
                None => self.company_currency(&c).await,
            };
            let typed = fields(vec![
                (
                    "contact",
                    Some(Value::from(required_ref(
                        &c,
                        "contact_id",
                        "contacts",
                        &p.contact_id,
                    )?)),
                ),
                (
                    "estimate_type",
                    Some(Value::from(
                        choice(
                            "estimate_type",
                            p.estimate_type.as_deref(),
                            &["Estimate", "Quote", "Proposal"],
                        )?
                        .unwrap_or("Estimate"),
                    )),
                ),
                (
                    "reference",
                    Some(Value::from(required_text("reference", &p.reference)?)),
                ),
                (
                    "dated_on",
                    Some(Value::from(
                        date_arg("dated_on", p.dated_on.as_deref())?.unwrap_or_else(today),
                    )),
                ),
                ("currency", currency),
                ("status", Some(Value::from("Draft"))),
                (
                    "project",
                    opt_s(ref_arg(
                        &c,
                        "project_id",
                        "projects",
                        p.project_id.as_deref(),
                    )?),
                ),
                ("notes", opt_s(text_arg("notes", p.notes.as_deref())?)),
                (
                    "discount_percent",
                    opt_s(opt_decimal(
                        "discount_percent",
                        p.discount_percent.as_deref(),
                    )?),
                ),
                (
                    "ec_status",
                    choice("ec_status", p.ec_status.as_deref(), EC_STATUSES)?.map(Value::from),
                ),
                (
                    "estimate_items",
                    Some(Value::Array(line_items(
                        &c,
                        Doc::Estimate,
                        &p.items,
                        false,
                    )?)),
                ),
            ]);
            let body = merge(p.extra.as_ref(), typed)?;
            let out = c
                .write(
                    Method::POST,
                    "create_estimate",
                    &["estimates"],
                    &[],
                    Some(&wrap("estimate", body)),
                    "estimate",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Move an estimate through its life: mark_as_sent, mark_as_draft, mark_as_approved, mark_as_rejected, convert_to_invoice (makes a draft invoice from it), or duplicate it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn estimate_action(
        &self,
        Parameters(p): Parameters<EstimateActionParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("estimate_action", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let action = choice(
                "action",
                Some(&p.action),
                &[
                    "mark_as_sent",
                    "mark_as_draft",
                    "mark_as_approved",
                    "mark_as_rejected",
                    "convert_to_invoice",
                    "duplicate",
                ],
            )?
            .unwrap_or_default();
            let out = if action == "duplicate" {
                c.write(
                    Method::POST,
                    "estimate_duplicate",
                    &["estimates", &id, "duplicate"],
                    &[],
                    None,
                    "estimate",
                )
                .await?
            } else {
                c.write(
                    Method::PUT,
                    "estimate_transition",
                    &["estimates", &id, "transitions", action],
                    &[],
                    None,
                    "estimate",
                )
                .await?
            };
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "List credit notes with reference, contact, status and values.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_credit_notes(
        &self,
        Parameters(p): Parameters<ListCreditNotesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_credit_notes", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                view_arg(
                    p.view.as_deref(),
                    &[
                        "all",
                        "recent_open_or_overdue",
                        "open",
                        "overdue",
                        "open_or_overdue",
                        "draft",
                        "refunded",
                    ],
                )?,
            );
            push(
                &mut q,
                "contact",
                ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())?,
            );
            push(
                &mut q,
                "project",
                ref_arg(&c, "project_id", "projects", p.project_id.as_deref())?,
            );
            push(
                &mut q,
                "updated_since",
                since_arg(p.updated_since.as_deref())?,
            );
            Ok(json_result(
                &c.list("credit_notes", &["credit_notes"], "credit_notes", q, pg)
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One credit note in full, with its items.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_credit_note(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_credit_note", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("credit_note", &["credit_notes", &id], "credit_note", &[])
                    .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Create a draft credit note for a customer, e.g. to credit part of an invoice. Prices are positive. Apply it to an invoice or refund it in FreeAgent, or with api_request on credit_note_reconciliations.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_credit_note(
        &self,
        Parameters(p): Parameters<CreateCreditNoteParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_credit_note", async {
            let c = self.caller(&parts)?;
            if p.items.is_empty() {
                return Err(FaError::invalid("items must hold at least one line"));
            }
            let typed = fields(vec![
                (
                    "contact",
                    Some(Value::from(required_ref(
                        &c,
                        "contact_id",
                        "contacts",
                        &p.contact_id,
                    )?)),
                ),
                (
                    "dated_on",
                    Some(Value::from(
                        date_arg("dated_on", p.dated_on.as_deref())?.unwrap_or_else(today),
                    )),
                ),
                (
                    "payment_terms_in_days",
                    Some(Value::from(p.payment_terms_in_days.unwrap_or(0))),
                ),
                (
                    "reference",
                    opt_s(text_arg("reference", p.reference.as_deref())?),
                ),
                ("currency", opt_s(currency_arg(p.currency.as_deref())?)),
                (
                    "project",
                    opt_s(ref_arg(
                        &c,
                        "project_id",
                        "projects",
                        p.project_id.as_deref(),
                    )?),
                ),
                (
                    "comments",
                    opt_s(text_arg("comments", p.comments.as_deref())?),
                ),
                (
                    "credit_note_items",
                    Some(Value::Array(line_items(
                        &c,
                        Doc::CreditNote,
                        &p.items,
                        false,
                    )?)),
                ),
            ]);
            let body = merge(p.extra.as_ref(), typed)?;
            let out = c
                .write(
                    Method::POST,
                    "create_credit_note",
                    &["credit_notes"],
                    &[],
                    Some(&wrap("credit_note", body)),
                    "credit_note",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "List recurring invoices (the profiles FreeAgent invoices from on a schedule) with frequency, next_recurs_on and status.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_recurring_invoices(
        &self,
        Parameters(p): Parameters<ListRecurringParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_recurring_invoices", async {
            let c = self.caller(&parts)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice("view", p.view.as_deref(), &["draft", "active", "inactive"])?
                    .map(String::from),
            );
            push(
                &mut q,
                "contact",
                ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())?,
            );
            let (items, more) = c
                .all(
                    "recurring_invoices",
                    &["recurring_invoices"],
                    "recurring_invoices",
                    q,
                    500,
                )
                .await?;
            let mut out =
                json!({"recurring_invoices": items.iter().map(view::resource).collect::<Vec<_>>()});
            if more {
                out["truncated"] = Value::from(true);
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Email an invoice, estimate or credit note to the customer through FreeAgent, with a link to view (and pay) it; a draft invoice is marked as sent. Give subject and body, or use_template for FreeAgent's own wording. Show the person the email and get their agreement first.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn send_document_email(
        &self,
        Parameters(p): Parameters<SendEmailParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("send_document_email", async {
            let c = self.caller(&parts)?;
            let doc = Doc::parse(&p.document)?;
            let id = id_arg("id", &p.id)?;
            let email = if p.use_template == Some(true) {
                json!({"use_template": true})
            } else {
                let mut m = fields(vec![
                    ("to", opt_s(text_arg("to", p.to.as_deref())?)),
                    ("from", opt_s(text_arg("from", p.from.as_deref())?)),
                    (
                        "subject",
                        Some(Value::from(required_text(
                            "subject",
                            p.subject.as_deref().unwrap_or(""),
                        )?)),
                    ),
                    (
                        "body",
                        Some(Value::from(required_text(
                            "body",
                            p.body.as_deref().unwrap_or(""),
                        )?)),
                    ),
                    ("email_to_sender", p.email_to_sender.map(Value::from)),
                ]);
                if !m.contains_key("to") {
                    let d = c
                        .one("document", &[doc.collection(), &id], doc.root(), &[])
                        .await?;
                    let contact = d
                        .get("contact")
                        .and_then(Value::as_str)
                        .and_then(view::id_of)
                        .ok_or_else(|| FaError::invalid("the document has no contact; give to"))?
                        .to_string();
                    let ct = c
                        .one("contact", &["contacts", &contact], "contact", &[])
                        .await?;
                    let to = ["billing_email", "email"]
                        .iter()
                        .find_map(|k| ct.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
                        .ok_or_else(|| {
                            FaError::invalid("the contact has no email address; give to")
                        })?;
                    m.insert("to".into(), Value::from(to));
                }
                Value::Object(m)
            };
            let body = json!({doc.root(): {"email": email}});
            let endpoint: &'static str = match doc {
                Doc::Invoice => "invoice_email",
                Doc::Estimate => "estimate_email",
                Doc::CreditNote => "credit_note_email",
            };
            c.send(
                Method::POST,
                endpoint,
                &[doc.collection(), &id, "send_email"],
                &[],
                Some(&body),
            )
            .await?;
            Ok(json_result(
                &json!({"sent": c.url(doc.collection(), &id), "email": email}),
            ))
        })
        .await
    }

    #[tool(
        description = "The PDF of an invoice, estimate or credit note, as an embedded application/pdf resource, e.g. to attach or save.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_document_pdf(
        &self,
        Parameters(p): Parameters<DocumentParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_document_pdf", async {
            let c = self.caller(&parts)?;
            let doc = Doc::parse(&p.document)?;
            let id = id_arg("id", &p.id)?;
            let endpoint: &'static str = match doc {
                Doc::Invoice => "invoice_pdf",
                Doc::Estimate => "estimate_pdf",
                Doc::CreditNote => "credit_note_pdf",
            };
            let a = c
                .get(endpoint, &[doc.collection(), &id, "pdf"], &[])
                .await?;
            let content: String = a
                .body
                .pointer("/pdf/content")
                .and_then(Value::as_str)
                .ok_or_else(|| FaError::Upstream("FreeAgent sent no PDF content".into()))?
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let uri = format!("{}/pdf", c.url(doc.collection(), &id));
            Ok(CallToolResult::success(vec![
                ContentBlock::text(format!("The PDF of {} {id}.", doc.root().replace('_', " "))),
                ContentBlock::resource(
                    ResourceContents::blob(content, uri).with_mime_type("application/pdf"),
                ),
            ]))
        })
        .await
    }

    #[tool(
        description = "The price list: saved products and services with code, description, price, unit and VAT, to reuse as invoice lines.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_price_list_items(
        &self,
        Parameters(p): Parameters<ListPriceListParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_price_list_items", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "sort",
                choice(
                    "sort",
                    p.sort.as_deref(),
                    &["created_at", "code", "updated_at"],
                )?
                .map(String::from),
            );
            Ok(json_result(
                &c.list(
                    "price_list_items",
                    &["price_list_items"],
                    "price_list_items",
                    q,
                    pg,
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "The verified email addresses FreeAgent can send invoices and estimates from, for send_document_email's from.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_email_addresses(
        &self,
        Parameters(_p): Parameters<NoArgs>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_email_addresses", async {
            let c = self.caller(&parts)?;
            let a = c.get("email_addresses", &["email_addresses"], &[]).await?;
            Ok(json_result(&json!({"email_addresses": a.body.get("email_addresses").cloned().unwrap_or_default()})))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_include_last_n_months() {
        assert_eq!(
            view_arg(Some("last_3_months"), &["all"])
                .unwrap()
                .as_deref(),
            Some("last_3_months")
        );
        assert_eq!(
            view_arg(Some("open or overdue"), &["open_or_overdue"])
                .unwrap()
                .as_deref(),
            Some("open_or_overdue")
        );
        assert!(view_arg(Some("last_x_months"), &["all"]).is_err());
        assert!(view_arg(Some("last_0_months"), &["all"]).is_err());
    }

    #[test]
    fn documents_name_their_endpoints() {
        let d = Doc::parse("credit note").unwrap();
        assert_eq!(
            (d.collection(), d.root(), d.items_key()),
            ("credit_notes", "credit_note", "credit_note_items")
        );
        assert!(Doc::parse("bill").is_err());
    }
}
