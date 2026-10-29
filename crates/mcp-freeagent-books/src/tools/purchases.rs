//! Purchases: bills from suppliers, and expenses and mileage claimed by the company's people.

use super::contacts::IdParams;
use super::{
    BooksServer, Caller, Query, category_ref, choice, currency_arg, date_arg, date_range,
    decimal_arg, fields, id_arg, json_result, merge, money_arg, opt_decimal, opt_money, opt_s,
    paging, push, ref_arg, required_date, required_ref, required_text, run, since_arg, text_arg,
    wrap,
};
use crate::error::{self, FaError};
use axum::http::request::Parts;
use base64::Engine as _;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Largest attachment FreeAgent takes.
const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Attachment {
    /// e.g. receipt.pdf
    pub file_name: String,
    /// The file, base64-encoded. Requests to this server are limited to 1 MB in all.
    pub data_base64: String,
    /// application/pdf, image/png, image/jpeg or image/gif. Default: from file_name.
    pub content_type: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BillItem {
    /// Expense category as a nominal code, e.g. 285 (Accommodation and Meals); see list_categories.
    pub category: String,
    /// The line's total including VAT, e.g. 120.00.
    pub total_value: Option<String>,
    /// Or the line's total before VAT.
    pub total_value_ex_tax: Option<String>,
    pub description: Option<String>,
    /// VAT percentage, e.g. 20. Default: from the category.
    pub sales_tax_rate: Option<String>,
    /// An exact VAT amount, overriding the rate.
    pub manual_sales_tax_amount: Option<String>,
    /// TAXABLE, EXEMPT or OUT_OF_SCOPE.
    pub sales_tax_status: Option<String>,
    pub quantity: Option<String>,
    /// -no unit-, Hours, Days, Weeks, Months, Years, Products, Services, Training or Stock.
    pub unit: Option<String>,
    pub project_id: Option<String>,
    /// On update: the id of an existing line to change. Leave out to add a line.
    pub id: Option<String>,
    /// On update with id: true removes that line.
    pub delete: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListBillsParams {
    /// all (default), open, overdue, open_or_overdue, paid, recurring, hire_purchase or cis.
    pub view: Option<String>,
    pub contact_id: Option<String>,
    pub project_id: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub updated_since: Option<String>,
    /// Include each bill's lines.
    pub with_items: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BillFields {
    /// The supplier's invoice number.
    pub reference: Option<String>,
    pub dated_on: Option<String>,
    /// Default (on create): 30 days after dated_on.
    pub due_on: Option<String>,
    pub currency: Option<String>,
    pub comments: Option<String>,
    /// UK/Non-EC (default), EC Goods, EC Services or Reverse Charge.
    pub ec_status: Option<String>,
    pub items: Option<Vec<BillItem>>,
    /// The supplier's invoice, attached to the bill.
    pub attachment: Option<Attachment>,
    /// Any other bill field by FreeAgent's name, e.g. {"recurring": "Monthly"}.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateBillParams {
    /// The supplier.
    pub contact_id: String,
    #[serde(flatten)]
    pub fields: BillFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateBillParams {
    pub id: String,
    pub contact_id: Option<String>,
    #[serde(flatten)]
    pub fields: BillFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListExpensesParams {
    /// recent or recurring. Default: all.
    pub view: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub project_id: Option<String>,
    pub updated_since: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ExpenseFields {
    /// Who paid and is owed it. Default (on create): the signed-in user.
    pub user_id: Option<String>,
    /// Expense category as a nominal code, e.g. 285; see list_categories.
    pub category: Option<String>,
    pub dated_on: Option<String>,
    pub description: Option<String>,
    /// What was spent, including VAT, as a positive amount, e.g. 42.50.
    pub amount: Option<String>,
    /// true when the claimant paid money back to the company instead (a refund).
    pub refund: Option<bool>,
    /// ISO 4217 code when not the company's currency.
    pub currency: Option<String>,
    /// VAT percentage, e.g. 20. Default: from the category.
    pub sales_tax_rate: Option<String>,
    /// An exact reclaimable VAT amount, overriding the rate.
    pub manual_sales_tax_amount: Option<String>,
    pub receipt_reference: Option<String>,
    /// A project to rebill it to.
    pub project_id: Option<String>,
    /// cost, markup or price: how to rebill it to project_id.
    pub rebill_type: Option<String>,
    /// The receipt.
    pub attachment: Option<Attachment>,
    /// Any other expense field by FreeAgent's name.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateExpenseParams {
    #[serde(flatten)]
    pub fields: ExpenseFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateExpenseParams {
    pub id: String,
    #[serde(flatten)]
    pub fields: ExpenseFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MileageParams {
    /// Who drove. Default: the signed-in user.
    pub user_id: Option<String>,
    pub dated_on: String,
    /// Where and why, e.g. "Leeds to Manchester, client meeting".
    pub description: String,
    /// Distance in the company's mileage units (miles unless set to kilometres).
    pub mileage: String,
    /// Car (default), Motorcycle or Bicycle.
    pub vehicle_type: Option<String>,
    /// Petrol (default), Diesel, LPG, Electric, Electric (Home charger) or Electric (Public charger).
    pub engine_type: Option<String>,
    /// From FreeAgent's mileage settings, e.g. "Up to 1400cc"; needed for VAT on fuel.
    pub engine_size: Option<String>,
    /// Reclaim at the HMRC approved rate. Default: true; false only rebills it.
    pub reclaim: Option<bool>,
    /// Whether there is a VAT receipt for the fuel.
    pub have_vat_receipt: Option<bool>,
    pub project_id: Option<String>,
}

/// The file of an attachment argument, checked by its contents.
pub(crate) fn file_of(a: &Attachment) -> error::Result<crate::uploads::File> {
    let data: String = a
        .data_base64
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|_| FaError::invalid("attachment.data_base64 is not valid base64"))?;
    crate::uploads::File::new(
        &required_text("attachment.file_name", &a.file_name)?,
        bytes,
        text_arg("attachment.description", a.description.as_deref())?,
    )
}

/// FreeAgent's attachment object, checked.
pub(crate) fn attachment(a: &Attachment) -> error::Result<Value> {
    let name = required_text("attachment.file_name", &a.file_name)?;
    let data: String = a
        .data_base64
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|_| FaError::invalid("attachment.data_base64 is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(FaError::invalid(
            "attachments must be between 1 byte and 5 MB",
        ));
    }
    let lower = name.to_lowercase();
    let guessed = if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        ""
    };
    let ct = a
        .content_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(guessed);
    // FreeAgent spells PDF application/x-pdf.
    let ct = match ct.to_lowercase().as_str() {
        "application/pdf" | "application/x-pdf" => "application/x-pdf",
        "image/png" | "image/x-png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        _ => {
            return Err(FaError::invalid(
                "attachment must be a PDF, PNG, JPEG or GIF; give content_type or a file_name ending .pdf, .png, .jpg or .gif",
            ));
        }
    };
    Ok(Value::Object(fields(vec![
        ("file_name", Some(Value::from(name))),
        ("content_type", Some(Value::from(ct))),
        ("data", Some(Value::from(data))),
        (
            "description",
            opt_s(text_arg(
                "attachment.description",
                a.description.as_deref(),
            )?),
        ),
    ])))
}

fn bill_items(c: &Caller, items: &[BillItem], updating: bool) -> error::Result<Vec<Value>> {
    if items.len() > 40 {
        return Err(FaError::invalid("a bill has at most 40 lines"));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let n = |f: &str| format!("items[{i}].{f}");
            // On update every line carries url: an existing line's, or "" for a new one.
            let url = match it.id.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(raw) if updating => Some(c.url("bill_items", &id_arg(&n("id"), raw)?)),
                Some(_) => return Err(FaError::invalid(format!("{} is only for update", n("id")))),
                None if it.delete == Some(true) => {
                    return Err(FaError::invalid(format!(
                        "{} needs the line's id",
                        n("delete")
                    )));
                }
                None if updating => Some(String::new()),
                None => None,
            };
            if it.delete == Some(true) {
                return Ok(json!({"url": url, "_destroy": 1}));
            }
            let total = opt_money(&n("total_value"), it.total_value.as_deref())?;
            let ex = opt_money(&n("total_value_ex_tax"), it.total_value_ex_tax.as_deref())?;
            if total.is_none() && ex.is_none() && url.as_deref().is_none_or(str::is_empty) {
                return Err(FaError::invalid(format!(
                    "{} or total_value_ex_tax is required",
                    n("total_value")
                )));
            }
            Ok(Value::Object(fields(vec![
                ("url", url.map(Value::from)),
                (
                    "category",
                    Some(Value::from(category_ref(c, &it.category)?)),
                ),
                ("total_value", opt_s(total)),
                ("total_value_ex_tax", opt_s(ex)),
                (
                    "description",
                    opt_s(text_arg(&n("description"), it.description.as_deref())?),
                ),
                (
                    "sales_tax_rate",
                    opt_s(opt_decimal(
                        &n("sales_tax_rate"),
                        it.sales_tax_rate.as_deref(),
                    )?),
                ),
                (
                    "manual_sales_tax_amount",
                    opt_s(opt_money(
                        &n("manual_sales_tax_amount"),
                        it.manual_sales_tax_amount.as_deref(),
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
                    "quantity",
                    opt_s(opt_decimal(&n("quantity"), it.quantity.as_deref())?),
                ),
                (
                    "unit",
                    choice(
                        &n("unit"),
                        it.unit.as_deref(),
                        &[
                            "-no unit-",
                            "Hours",
                            "Days",
                            "Weeks",
                            "Months",
                            "Years",
                            "Products",
                            "Services",
                            "Training",
                            "Stock",
                        ],
                    )?
                    .map(Value::from),
                ),
                (
                    "project",
                    opt_s(ref_arg(
                        c,
                        &n("project_id"),
                        "projects",
                        it.project_id.as_deref(),
                    )?),
                ),
            ])))
        })
        .collect()
}

fn bill_body(c: &Caller, f: &BillFields, updating: bool) -> error::Result<Map<String, Value>> {
    let typed = fields(vec![
        (
            "reference",
            opt_s(text_arg("reference", f.reference.as_deref())?),
        ),
        (
            "dated_on",
            opt_s(date_arg("dated_on", f.dated_on.as_deref())?),
        ),
        ("due_on", opt_s(date_arg("due_on", f.due_on.as_deref())?)),
        ("currency", opt_s(currency_arg(f.currency.as_deref())?)),
        (
            "comments",
            opt_s(text_arg("comments", f.comments.as_deref())?),
        ),
        (
            "ec_status",
            choice(
                "ec_status",
                f.ec_status.as_deref(),
                &["UK/Non-EC", "EC Goods", "EC Services", "Reverse Charge"],
            )?
            .map(Value::from),
        ),
        (
            "bill_items",
            f.items
                .as_deref()
                .map(|i| bill_items(c, i, updating))
                .transpose()?
                .map(Value::Array),
        ),
        (
            "attachment",
            f.attachment.as_ref().map(attachment).transpose()?,
        ),
    ]);
    merge(f.extra.as_ref(), typed)
}

/// The expense fields given. `amount` is what was spent; FreeAgent stores money paid out to the
/// claimant as negative.
fn expense_body(c: &Caller, f: &ExpenseFields) -> error::Result<Map<String, Value>> {
    let gross = match f.amount.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => {
            let a = money_arg("amount", raw)?;
            if a.starts_with('-') {
                return Err(FaError::invalid(
                    "amount is what was spent, as a positive number; set refund true for money paid back to the company",
                ));
            }
            Some(if f.refund == Some(true) {
                a
            } else {
                format!("-{a}")
            })
        }
        None => None,
    };
    let typed = fields(vec![
        (
            "user",
            opt_s(ref_arg(c, "user_id", "users", f.user_id.as_deref())?),
        ),
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
            "dated_on",
            opt_s(date_arg("dated_on", f.dated_on.as_deref())?),
        ),
        (
            "description",
            opt_s(text_arg("description", f.description.as_deref())?),
        ),
        ("gross_value", opt_s(gross)),
        ("currency", opt_s(currency_arg(f.currency.as_deref())?)),
        (
            "sales_tax_rate",
            opt_s(opt_decimal("sales_tax_rate", f.sales_tax_rate.as_deref())?),
        ),
        (
            "manual_sales_tax_amount",
            opt_s(opt_money(
                "manual_sales_tax_amount",
                f.manual_sales_tax_amount.as_deref(),
            )?),
        ),
        (
            "receipt_reference",
            opt_s(text_arg(
                "receipt_reference",
                f.receipt_reference.as_deref(),
            )?),
        ),
        (
            "rebill_to_project",
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
        (
            "attachment",
            f.attachment.as_ref().map(attachment).transpose()?,
        ),
    ]);
    merge(f.extra.as_ref(), typed)
}

#[tool_router(router = purchases_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "List bills (supplier invoices) with reference, supplier, dates, status, total_value and due_value. view open_or_overdue shows what the company owes.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_bills(
        &self,
        Parameters(p): Parameters<ListBillsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_bills", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &[
                        "all",
                        "open",
                        "overdue",
                        "open_or_overdue",
                        "open_or_overdue_payments",
                        "open_or_overdue_refunds",
                        "paid",
                        "recurring",
                        "hire_purchase",
                        "cis",
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
                q.push(("nested_bill_items", "true".into()));
            }
            Ok(json_result(
                &c.list("bills", &["bills"], "bills", q, pg).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One bill in full, with its lines, VAT and what is paid and due.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_bill(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_bill", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("bill", &["bills", &id], "bill", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Record a bill from a supplier: reference, date, lines with a category and total including VAT, and optionally the supplier's invoice as an attachment. Due 30 days after its date unless due_on is given. Pay it by explaining the bank transaction with paid_bill_id.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_bill(
        &self,
        Parameters(p): Parameters<CreateBillParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_bill", async {
            let c = self.caller(&parts)?;
            let mut body = bill_body(&c, &p.fields, false)?;
            if !body.contains_key("reference") {
                return Err(FaError::invalid(
                    "reference (the supplier's invoice number) is required",
                ));
            }
            let dated = body
                .get("dated_on")
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| FaError::invalid("dated_on is required"))?;
            if body
                .get("bill_items")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
            {
                return Err(FaError::invalid("items must hold at least one line"));
            }
            if !body.contains_key("due_on") {
                let d = chrono::NaiveDate::parse_from_str(&dated, "%Y-%m-%d")
                    .map_err(|_| FaError::invalid("dated_on must be YYYY-MM-DD"))?
                    + chrono::Days::new(30);
                body.insert(
                    "due_on".into(),
                    Value::from(d.format("%Y-%m-%d").to_string()),
                );
            }
            body.insert(
                "contact".into(),
                Value::from(required_ref(&c, "contact_id", "contacts", &p.contact_id)?),
            );
            let out = c
                .write(
                    Method::POST,
                    "create_bill",
                    &["bills"],
                    &[],
                    Some(&wrap("bill", body)),
                    "bill",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change a bill: only the fields given change. items adds lines; an item with id changes that line, and with id and delete true removes it.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn update_bill(
        &self,
        Parameters(p): Parameters<UpdateBillParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_bill", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let mut body = bill_body(&c, &p.fields, true)?;
            if let Some(contact) = ref_arg(&c, "contact_id", "contacts", p.contact_id.as_deref())? {
                body.insert("contact".into(), Value::from(contact));
            }
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_bill",
                    &["bills", &id],
                    &[],
                    Some(&wrap("bill", body)),
                    "bill",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete a bill. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_bill(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_bill", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            c.send(Method::DELETE, "delete_bill", &["bills", &id], &[], None)
                .await?;
            Ok(json_result(&json!({"deleted": c.url("bills", &id)})))
        })
        .await
    }

    #[tool(
        description = "List expense claims and mileage with user, category, date, description and gross_value (negative is money owed to the claimant).",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_expenses(
        &self,
        Parameters(p): Parameters<ListExpensesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_expenses", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice("view", p.view.as_deref(), &["recent", "recurring"])?.map(String::from),
            );
            push(&mut q, "from_date", from);
            push(&mut q, "to_date", to);
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
                &c.list("expenses", &["expenses"], "expenses", q, pg).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One expense or mileage claim in full: user, category, date, amounts, VAT, receipt and rebilling.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_expense(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_expense", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            Ok(json_result(
                &c.one("expense", &["expenses", &id], "expense", &[]).await?,
            ))
        })
        .await
    }

    #[tool(
        description = "Record an expense someone paid personally (out of pocket), owed back to them: category, date, description and amount spent including VAT, optionally with the receipt. Not for costs paid from a company bank account or card: explain those bank transactions instead. For mileage use create_mileage_claim.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_expense(
        &self,
        Parameters(p): Parameters<CreateExpenseParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_expense", async {
            let c = self.caller(&parts)?;
            let mut body = expense_body(&c, &p.fields)?;
            for k in ["category", "dated_on", "description", "gross_value"] {
                if !body.contains_key(k) {
                    let name = if k == "gross_value" { "amount" } else { k };
                    return Err(FaError::invalid(format!("{name} is required")));
                }
            }
            if !body.contains_key("user") {
                body.insert("user".into(), Value::from(self.signed_in_user(&c).await?));
            }
            let out = c
                .write(
                    Method::POST,
                    "create_expense",
                    &["expenses"],
                    &[],
                    Some(&wrap("expense", body)),
                    "expense",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Claim business mileage at HMRC's approved rates: date, journey, distance and vehicle. FreeAgent works out the amount.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_mileage_claim(
        &self,
        Parameters(p): Parameters<MileageParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_mileage_claim", async {
            let c = self.caller(&parts)?;
            let user = match ref_arg(&c, "user_id", "users", p.user_id.as_deref())? {
                Some(u) => u,
                None => self.signed_in_user(&c).await?,
            };
            let body = fields(vec![
                ("user", Some(Value::from(user))),
                ("category", Some(Value::from("Mileage"))),
                (
                    "dated_on",
                    Some(Value::from(required_date("dated_on", &p.dated_on)?)),
                ),
                (
                    "description",
                    Some(Value::from(required_text("description", &p.description)?)),
                ),
                (
                    "mileage",
                    Some(Value::from(decimal_arg("mileage", &p.mileage)?)),
                ),
                (
                    "vehicle_type",
                    Some(Value::from(
                        choice(
                            "vehicle_type",
                            p.vehicle_type.as_deref(),
                            &["Car", "Motorcycle", "Bicycle"],
                        )?
                        .unwrap_or("Car"),
                    )),
                ),
                (
                    "engine_type",
                    choice(
                        "engine_type",
                        p.engine_type.as_deref(),
                        &[
                            "Petrol",
                            "Diesel",
                            "LPG",
                            "Electric",
                            "Electric (Home charger)",
                            "Electric (Public charger)",
                        ],
                    )?
                    .map(Value::from),
                ),
                (
                    "engine_size",
                    opt_s(text_arg("engine_size", p.engine_size.as_deref())?),
                ),
                (
                    "reclaim_mileage",
                    Some(Value::from(if p.reclaim == Some(false) { 0 } else { 1 })),
                ),
                ("have_vat_receipt", p.have_vat_receipt.map(Value::from)),
                (
                    "rebill_to_project",
                    opt_s(ref_arg(
                        &c,
                        "project_id",
                        "projects",
                        p.project_id.as_deref(),
                    )?),
                ),
            ]);
            let out = c
                .write(
                    Method::POST,
                    "create_mileage",
                    &["expenses"],
                    &[],
                    Some(&wrap("expense", body)),
                    "expense",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change an expense: only the fields given change. amount is what was spent, positive.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_expense(
        &self,
        Parameters(p): Parameters<UpdateExpenseParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_expense", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let body = expense_body(&c, &p.fields)?;
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_expense",
                    &["expenses", &id],
                    &[],
                    Some(&wrap("expense", body)),
                    "expense",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete an expense. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_expense(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_expense", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            c.send(
                Method::DELETE,
                "delete_expense",
                &["expenses", &id],
                &[],
                None,
            )
            .await?;
            Ok(json_result(&json!({"deleted": c.url("expenses", &id)})))
        })
        .await
    }
}

impl BooksServer {
    /// The signed-in user's URL.
    pub(crate) async fn signed_in_user(&self, c: &Caller) -> error::Result<String> {
        let me = c.one("users_me", &["users", "me"], "user", &[]).await?;
        me.get("url")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| FaError::Upstream("FreeAgent did not say who is signed in".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(name: &str, data: &str, ct: Option<&str>) -> Attachment {
        Attachment {
            file_name: name.into(),
            data_base64: data.into(),
            content_type: ct.map(String::from),
            description: None,
        }
    }

    #[test]
    fn attachments_are_checked_and_typed_as_freeagent_wants() {
        let a = attachment(&att("Receipt.PDF", "JVBERi0x\n", None)).unwrap();
        assert_eq!(a["content_type"], "application/x-pdf");
        assert_eq!(a["data"], "JVBERi0x");
        assert_eq!(
            attachment(&att("r", "aGk=", Some("image/jpg"))).unwrap()["content_type"],
            "image/jpeg"
        );
        assert!(attachment(&att("r.docx", "aGk=", None)).is_err());
        assert!(attachment(&att("r.pdf", "not base64!", None)).is_err());
        assert!(attachment(&att("r.pdf", "", None)).is_err());
    }
}
