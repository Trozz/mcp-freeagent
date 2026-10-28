//! Contacts (customers and suppliers) and the notes kept on contacts and projects.

use super::{
    BooksServer, Query, choice, fields, id_arg, json_result, merge, opt_s, paging, push,
    required_text, run, since_arg, text_arg, wrap,
};
use crate::error::{self, FaError};
use crate::view::{self, contact_name, resource};
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Most contacts read when searching by name.
const MAX_SEARCHED: usize = 3000;

const VIEWS: &[&str] = &[
    "all",
    "active",
    "clients",
    "suppliers",
    "active_projects",
    "completed_projects",
    "open_clients",
    "open_suppliers",
    "hidden",
];

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SearchContactsParams {
    /// Words to find in the name, organisation or email, e.g. "acme" or "jane@". Leave out to list.
    pub query: Option<String>,
    /// all, active (default), clients, suppliers, active_projects, completed_projects, open_clients, open_suppliers or hidden.
    pub view: Option<String>,
    /// name (default), created_at or updated_at; prefix - for newest first, e.g. -updated_at.
    pub sort: Option<String>,
    /// Only contacts changed since, as YYYY-MM-DD or an ISO 8601 date-time.
    pub updated_since: Option<String>,
    pub page: Option<u32>,
    /// 1 to 100. Default: 25.
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct IdParams {
    /// The id, or the URL a tool returned.
    pub id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ContactFields {
    /// Company name; give this or a person's name.
    pub organisation_name: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    /// Where invoices are emailed, when not email.
    pub billing_email: Option<String>,
    pub phone_number: Option<String>,
    pub mobile: Option<String>,
    pub address1: Option<String>,
    pub address2: Option<String>,
    pub address3: Option<String>,
    pub town: Option<String>,
    pub region: Option<String>,
    pub postcode: Option<String>,
    /// Country name as FreeAgent spells it, e.g. United Kingdom.
    pub country: Option<String>,
    /// Payment terms for this contact's invoices, in days.
    pub default_payment_terms_in_days: Option<u32>,
    /// Auto (default), Always or Never: whether invoices to them charge VAT.
    pub charge_sales_tax: Option<String>,
    /// Their VAT registration number.
    pub sales_tax_registration_number: Option<String>,
    /// Any other contact field by FreeAgent's name, e.g. {"locale": "fr", "contact_name_on_invoices": true}.
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateContactParams {
    #[serde(flatten)]
    pub fields: ContactFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateContactParams {
    /// The contact's id.
    pub id: String,
    /// Active or Hidden.
    pub status: Option<String>,
    #[serde(flatten)]
    pub fields: ContactFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListNotesParams {
    /// The contact whose notes to list. Give this or project_id.
    pub contact_id: Option<String>,
    /// The project whose notes to list.
    pub project_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateNoteParams {
    /// The contact to add the note to. Give this or project_id.
    pub contact_id: Option<String>,
    /// The project to add the note to.
    pub project_id: Option<String>,
    /// The note's text.
    pub note: String,
}

/// The contact fields given, for a create or an update.
fn contact_body(
    f: &ContactFields,
    status: Option<&'static str>,
) -> error::Result<Map<String, Value>> {
    let t = |name: &str, v: &Option<String>| -> error::Result<Option<Value>> {
        Ok(opt_s(text_arg(name, v.as_deref())?))
    };
    let charge = choice(
        "charge_sales_tax",
        f.charge_sales_tax.as_deref(),
        &["Auto", "Always", "Never"],
    )?;
    let typed = fields(vec![
        (
            "organisation_name",
            t("organisation_name", &f.organisation_name)?,
        ),
        ("first_name", t("first_name", &f.first_name)?),
        ("last_name", t("last_name", &f.last_name)?),
        ("email", t("email", &f.email)?),
        ("billing_email", t("billing_email", &f.billing_email)?),
        ("phone_number", t("phone_number", &f.phone_number)?),
        ("mobile", t("mobile", &f.mobile)?),
        ("address1", t("address1", &f.address1)?),
        ("address2", t("address2", &f.address2)?),
        ("address3", t("address3", &f.address3)?),
        ("town", t("town", &f.town)?),
        ("region", t("region", &f.region)?),
        ("postcode", t("postcode", &f.postcode)?),
        ("country", t("country", &f.country)?),
        (
            "default_payment_terms_in_days",
            f.default_payment_terms_in_days.map(Value::from),
        ),
        ("charge_sales_tax", charge.map(Value::from)),
        (
            "sales_tax_registration_number",
            t(
                "sales_tax_registration_number",
                &f.sales_tax_registration_number,
            )?,
        ),
        ("status", status.map(Value::from)),
    ]);
    merge(f.extra.as_ref(), typed)
}

/// Whether contact `c` matches every word of `query` in its names or emails.
fn matches(c: &Value, words: &[String]) -> bool {
    let hay = [
        "organisation_name",
        "first_name",
        "last_name",
        "email",
        "billing_email",
    ]
    .iter()
    .filter_map(|k| c.get(*k).and_then(Value::as_str))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase();
    words.iter().all(|w| hay.contains(w.as_str()))
}

/// A contact as a list shows it: its fields with `name` first.
fn listed(c: &Value) -> Value {
    let mut r = resource(c);
    if let Value::Object(m) = &mut r {
        m.insert("name".into(), Value::from(contact_name(c)));
    }
    r
}

/// `contact=<url>` or `project=<url>` for notes: exactly one.
fn note_parent(
    c: &super::Caller,
    contact: Option<&str>,
    project: Option<&str>,
) -> error::Result<(&'static str, String)> {
    let contact = contact.map(str::trim).filter(|s| !s.is_empty());
    let project = project.map(str::trim).filter(|s| !s.is_empty());
    match (contact, project) {
        (Some(id), None) => Ok(("contact", c.url("contacts", &id_arg("contact_id", id)?))),
        (None, Some(id)) => Ok(("project", c.url("projects", &id_arg("project_id", id)?))),
        _ => Err(FaError::invalid(
            "give exactly one of contact_id and project_id",
        )),
    }
}

#[tool_router(router = contacts_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Find contacts (customers and suppliers). With query, searches names, organisations and emails for every word given (FreeAgent has no search of its own, so this reads up to 3000 contacts); without it, lists a page. Each result has its id, name, email and account_balance.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn search_contacts(
        &self,
        Parameters(p): Parameters<SearchContactsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("search_contacts", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(&mut q, "view", choice("view", p.view.as_deref(), VIEWS)?.map(String::from));
            if let Some(sort) = text_arg("sort", p.sort.as_deref())? {
                let bare = sort.trim_start_matches('-');
                if !["name", "created_at", "updated_at"].contains(&bare) {
                    return Err(FaError::invalid(
                        "sort must be name, created_at or updated_at, with - in front for descending",
                    ));
                }
                q.push(("sort", sort));
            }
            push(&mut q, "updated_since", since_arg(p.updated_since.as_deref())?);
            let Some(query) = text_arg("query", p.query.as_deref())? else {
                q.push(("page", pg.page.to_string()));
                q.push(("per_page", pg.per_page.to_string()));
                let a = c.get("contacts", &["contacts"], &q).await?;
                let items = a
                    .body
                    .get("contacts")
                    .and_then(Value::as_array)
                    .map(|v| v.iter().map(listed).collect())
                    .unwrap_or_default();
                return Ok(json_result(&view::page(items, pg.page, a.total, a.has_next)));
            };
            let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
            let (all, more) = c
                .all("contacts", &["contacts"], "contacts", q, MAX_SEARCHED)
                .await?;
            let found: Vec<Value> = all.iter().filter(|x| matches(x, &words)).collect::<Vec<_>>()
                .into_iter()
                .map(listed)
                .collect();
            let total = found.len();
            let start = ((pg.page - 1) * pg.per_page) as usize;
            let shown: Vec<Value> = found.into_iter().skip(start).take(pg.per_page as usize).collect();
            let has_next = start + shown.len() < total;
            let mut out = view::page(shown, pg.page, Some(total as u64), has_next);
            if more {
                out["note"] = Value::from(format!(
                    "only the first {MAX_SEARCHED} contacts were searched; narrow with view or updated_since"
                ));
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "One contact in full: names, emails, phones, address, payment terms, VAT settings, account_balance and active projects count.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_contact(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_contact", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let a = c.get("contact", &["contacts", &id], &[]).await?;
            Ok(json_result(&listed(view::unwrap(&a.body, "contact"))))
        })
        .await
    }

    #[tool(
        description = "Create a contact (customer or supplier). Give organisation_name, or first_name and last_name. Search first so you do not create a duplicate. Returns the new contact with its id.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_contact(
        &self,
        Parameters(p): Parameters<CreateContactParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_contact", async {
            let c = self.caller(&parts)?;
            let body = contact_body(&p.fields, None)?;
            let named = body.contains_key("organisation_name")
                || (body.contains_key("first_name") && body.contains_key("last_name"));
            if !named {
                return Err(FaError::invalid(
                    "give organisation_name, or both first_name and last_name",
                ));
            }
            let out = c
                .write(
                    Method::POST,
                    "create_contact",
                    &["contacts"],
                    &[],
                    Some(&wrap("contact", body)),
                    "contact",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Change a contact: only the fields given change. status Hidden hides it from lists.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn update_contact(
        &self,
        Parameters(p): Parameters<UpdateContactParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("update_contact", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let status = choice("status", p.status.as_deref(), &["Active", "Hidden"])?;
            let body = contact_body(&p.fields, status)?;
            if body.is_empty() {
                return Err(FaError::invalid("give at least one field to change"));
            }
            let out = c
                .write(
                    Method::PUT,
                    "update_contact",
                    &["contacts", &id],
                    &[],
                    Some(&wrap("contact", body)),
                    "contact",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Delete a contact. FreeAgent refuses when the contact has invoices, bills or projects; hide it with update_contact status Hidden instead. Confirm with the person first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn delete_contact(
        &self,
        Parameters(p): Parameters<IdParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("delete_contact", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            c.send(
                Method::DELETE,
                "delete_contact",
                &["contacts", &id],
                &[],
                None,
            )
            .await?;
            Ok(json_result(&json!({"deleted": c.url("contacts", &id)})))
        })
        .await
    }

    #[tool(
        description = "The notes kept on one contact or project, newest first, with author and dates.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_notes(
        &self,
        Parameters(p): Parameters<ListNotesParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_notes", async {
            let c = self.caller(&parts)?;
            let (k, url) = note_parent(&c, p.contact_id.as_deref(), p.project_id.as_deref())?;
            let a = c.get("notes", &["notes"], &[(k, url)]).await?;
            Ok(json_result(
                &json!({"notes": view::items(&a.body, "notes")}),
            ))
        })
        .await
    }

    #[tool(
        description = "Add a note to a contact or project, e.g. a record of a call or a payment promise.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_note(
        &self,
        Parameters(p): Parameters<CreateNoteParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_note", async {
            let c = self.caller(&parts)?;
            let (k, url) = note_parent(&c, p.contact_id.as_deref(), p.project_id.as_deref())?;
            let note = required_text("note", &p.note)?;
            let body = json!({"note": {"note": note}});
            let out = c
                .write(
                    Method::POST,
                    "create_note",
                    &["notes"],
                    &[(k, url)],
                    Some(&body),
                    "note",
                )
                .await?;
            Ok(json_result(&out))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_every_word_in_names_and_emails() {
        let c = json!({"organisation_name": "Acme Widgets Ltd", "email": "Jane@acme.example"});
        let w = |q: &str| {
            q.split_whitespace()
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        };
        assert!(matches(&c, &w("acme")));
        assert!(matches(&c, &w("widgets jane@")));
        assert!(!matches(&c, &w("acme bolts")));
    }

    #[test]
    fn contact_bodies_hold_only_what_was_given() {
        let f = ContactFields {
            organisation_name: Some(" Acme ".into()),
            charge_sales_tax: Some("always".into()),
            default_payment_terms_in_days: Some(14),
            ..Default::default()
        };
        let b = contact_body(&f, None).unwrap();
        assert_eq!(
            Value::Object(b),
            json!({"organisation_name": "Acme", "charge_sales_tax": "Always", "default_payment_terms_in_days": 14})
        );
        let bad = ContactFields {
            charge_sales_tax: Some("sometimes".into()),
            ..Default::default()
        };
        assert!(contact_body(&bad, None).is_err());
    }
}
