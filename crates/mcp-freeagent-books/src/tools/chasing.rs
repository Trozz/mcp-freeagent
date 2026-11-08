//! Credit control: who owes what and for how long, with a reminder drafted for each customer at
//! the stage their oldest invoice has reached. Sending stays with send_document_email, after the
//! person agrees.

use super::checks::{parse as parse_date, today};
use super::{BooksServer, Caller, Query, json_result, push, ref_arg, run};
use crate::error;
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Most overdue invoices read.
const MAX_INVOICES: usize = 1_000;
/// Up to this many days overdue, a friendly nudge.
const FRIENDLY_DAYS: i64 = 7;
/// Up to this many, a firm reminder; after it, a final one.
const FIRM_DAYS: i64 = 30;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ChasePlanParams {
    /// Only this customer: the contact's id or URL, from search_contacts.
    pub contact_id: Option<String>,
}

/// The reminder stage for an invoice `days` overdue.
fn stage(days: i64) -> &'static str {
    if days <= FRIENDLY_DAYS {
        "friendly"
    } else if days <= FIRM_DAYS {
        "firm"
    } else {
        "final"
    }
}

fn symbol(currency: &str) -> String {
    match currency {
        "GBP" => "£".into(),
        "EUR" => "€".into(),
        "USD" => "$".into(),
        "" => String::new(),
        c => format!("{c} "),
    }
}

/// One overdue invoice, as the plan lists it.
struct Overdue {
    id: Option<String>,
    reference: String,
    dated_on: String,
    due_on: String,
    days: i64,
    due: i64,
    currency: String,
    last_emailed: Option<String>,
}

/// A reminder email for `name` about `invoices` (oldest first) at `stage`, signed by `company`.
fn draft(stage: &str, name: &str, company: &str, invoices: &[&Overdue]) -> Value {
    let lines: Vec<String> = invoices
        .iter()
        .map(|i| {
            format!(
                "- Invoice {}: {}{}, due {} ({} days overdue)",
                i.reference,
                symbol(&i.currency),
                money(i.due),
                i.due_on,
                i.days
            )
        })
        .collect();
    let refs: Vec<&str> = invoices.iter().map(|i| i.reference.as_str()).collect();
    let several = invoices.len() > 1;
    let (what, them) = if several {
        (format!("invoices {}", refs.join(", ")), "them")
    } else {
        (format!("invoice {}", refs.join("")), "it")
    };
    let greeting = if name.is_empty() {
        "Hello,".to_string()
    } else {
        format!("Hello {name},")
    };
    let (subject, opening, ask) = match stage {
        "friendly" => (
            format!("Payment reminder: {what}"),
            format!("I hope you are well. Just a quick reminder that {what} {} still unpaid:", if several { "are" } else { "is" }),
            format!("If payment is already on its way, thank you, and please ignore this. If not, I would be grateful if you could arrange it. Do let me know if you need a copy of {them}."),
        ),
        "firm" => (
            format!("Overdue: {what}"),
            format!("Our records show that {what} {} now overdue:", if several { "are" } else { "is" }),
            "Please arrange payment this week. If there is a problem with an invoice, let me know so we can sort it out.".to_string(),
        ),
        _ => (
            format!("Final reminder: {what}"),
            format!("Despite earlier reminders, {what} {} unpaid:", if several { "remain" } else { "remains" }),
            "Please pay within 7 days, or contact me to agree a date for payment.".to_string(),
        ),
    };
    let sign = if company.is_empty() {
        "Kind regards".to_string()
    } else {
        format!("Kind regards,\n{company}")
    };
    json!({
        "subject": subject,
        "body": format!("{greeting}\n\n{opening}\n\n{}\n\n{ask}\n\n{sign}", lines.join("\n")),
    })
}

impl BooksServer {
    async fn chase_plan(&self, c: &Caller, p: &ChasePlanParams) -> error::Result<Value> {
        let mut q: Query = vec![("view", "overdue".into())];
        push(
            &mut q,
            "contact",
            ref_arg(c, "contact_id", "contacts", p.contact_id.as_deref())?,
        );
        let (invoices, more) = c
            .all("invoices", &["invoices"], "invoices", q, MAX_INVOICES)
            .await?;
        let company = c.one("company", &["company"], "company", &[]).await?;
        let company_name = company
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut unavailable = Map::new();

        // FreeAgent's invoice timeline records emails sent; the latest one per reference.
        let mut emailed: BTreeMap<String, String> = BTreeMap::new();
        match c
            .get("invoice_timeline", &["invoices", "timeline"], &[])
            .await
        {
            Ok(a) => {
                for t in view::items(&a.body, "invoice_timeline_items") {
                    let summary = t
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_lowercase();
                    if !(summary.contains("email") || summary.contains("reminder")) {
                        continue;
                    }
                    let (Some(r), Some(d)) = (
                        t.get("reference").and_then(Value::as_str),
                        t.get("dated_on").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let e = emailed.entry(r.to_string()).or_default();
                    if d > e.as_str() {
                        *e = d.to_string();
                    }
                }
            }
            Err(e) => {
                unavailable.insert(
                    "last_emailed".into(),
                    Value::from(format!("{}: {e}", e.kind())),
                );
            }
        }

        let now = today();
        let mut by_contact: BTreeMap<String, (String, Vec<Overdue>)> = BTreeMap::new();
        for i in &invoices {
            let due_on = i
                .get("due_on")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let days = parse_date(&due_on)
                .map(|d| (now - d).num_days())
                .unwrap_or(0);
            let due = pence(i.get("due_value")).unwrap_or(0);
            if days <= 0 || due <= 0 {
                continue;
            }
            let reference = i
                .get("reference")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let contact = i
                .get("contact")
                .and_then(Value::as_str)
                .and_then(view::id_of)
                .unwrap_or_default()
                .to_string();
            let name = i
                .get("contact_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let entry = by_contact
                .entry(contact)
                .or_insert_with(|| (name, Vec::new()));
            entry.1.push(Overdue {
                id: i
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(view::id_of)
                    .map(String::from),
                last_emailed: emailed.get(&reference).cloned(),
                reference,
                dated_on: i
                    .get("dated_on")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                due_on,
                days,
                due,
                currency: i
                    .get("currency")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }

        let mut totals: BTreeMap<String, i64> = BTreeMap::new();
        let mut customers: Vec<(i64, i64, Value)> = Vec::new();
        for (contact, (name, mut list)) in by_contact {
            list.sort_by_key(|i| std::cmp::Reverse(i.days));
            let oldest = list.first().map(|i| i.days).unwrap_or(0);
            let due: i64 = list.iter().map(|i| i.due).sum();
            for i in &list {
                *totals.entry(i.currency.clone()).or_default() += i.due;
            }
            let st = stage(oldest);
            let refs: Vec<&Overdue> = list.iter().collect();
            let last = list.iter().filter_map(|i| i.last_emailed.clone()).max();
            let row = view::prune(json!({
                "contact_id": contact,
                "contact_name": name,
                "invoices": list.iter().map(|i| view::prune(json!({
                    "id": i.id,
                    "reference": i.reference,
                    "dated_on": i.dated_on,
                    "due_on": i.due_on,
                    "days_overdue": i.days,
                    "due": money(i.due),
                    "currency": i.currency,
                    "last_emailed": i.last_emailed,
                })).unwrap_or_default()).collect::<Vec<_>>(),
                "total_due": money(due),
                "oldest_days_overdue": oldest,
                "last_emailed": last,
                "stage": st,
                "draft": draft(st, &name, &company_name, &refs),
                "send_with": list.first().and_then(|i| i.id.clone()).map(|id| format!("send_document_email with document invoice and id {id}, after get_contact for the address")),
            }))
            .unwrap_or_default();
            customers.push((oldest, due, row));
        }
        customers.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut out = json!({
            "as_at": now.format("%Y-%m-%d").to_string(),
            "customers": customers.into_iter().map(|(_, _, v)| v).collect::<Vec<_>>(),
            "totals_by_currency": totals.into_iter().map(|(k, v)| (if k.is_empty() { "unknown".to_string() } else { k }, Value::from(money(v)))).collect::<Map<_, _>>(),
            "stages": format!("friendly up to {FRIENDLY_DAYS} days overdue, firm up to {FIRM_DAYS}, final after"),
            "note": "Drafts are suggestions: show them to the person, change them as they ask, and send only those they agree to. Last emailed comes from FreeAgent's invoice timeline and misses emails sent outside FreeAgent.",
        });
        if more {
            out["truncated"] = Value::from(true);
        }
        if !unavailable.is_empty() {
            out["unavailable"] = Value::Object(unavailable);
        }
        Ok(out)
    }
}

#[tool_router(router = chasing_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "A plan for chasing overdue invoices: each customer with their overdue invoices (days overdue, amount due, when FreeAgent last emailed it), the total, a reminder stage from the oldest invoice (friendly up to 7 days, firm up to 30, final after) and a drafted email. Longest overdue first. Changes and sends nothing: send an agreed draft with send_document_email.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn overdue_chase_plan(
        &self,
        Parameters(p): Parameters<ChasePlanParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("overdue_chase_plan", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.chase_plan(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv(reference: &str, days: i64, due: i64) -> Overdue {
        Overdue {
            id: Some("1".into()),
            reference: reference.into(),
            dated_on: "2026-09-01".into(),
            due_on: "2026-10-01".into(),
            days,
            due,
            currency: "GBP".into(),
            last_emailed: None,
        }
    }

    #[test]
    fn stages_follow_the_oldest_invoice() {
        assert_eq!(stage(1), "friendly");
        assert_eq!(stage(7), "friendly");
        assert_eq!(stage(8), "firm");
        assert_eq!(stage(30), "firm");
        assert_eq!(stage(31), "final");
    }

    #[test]
    fn drafts_name_every_invoice_and_never_threaten() {
        let a = inv("036", 40, 34900);
        let b = inv("041", 12, 12000);
        let d = draft("final", "Acme Ltd", "Lee Consulting Ltd", &[&a, &b]);
        let body = d["body"].as_str().unwrap();
        assert_eq!(d["subject"], "Final reminder: invoices 036, 041");
        assert!(body.starts_with("Hello Acme Ltd,"), "{body}");
        assert!(body.contains("- Invoice 036: £349.00, due 2026-10-01 (40 days overdue)"));
        assert!(body.contains("Despite earlier reminders, invoices 036, 041 remain unpaid:"));
        assert!(body.ends_with("Kind regards,\nLee Consulting Ltd"));
        for word in ["court", "legal", "solicitor", "interest"] {
            assert!(!body.to_lowercase().contains(word), "{word}");
        }
        let one = draft("friendly", "", "", &[&a]);
        assert_eq!(one["subject"], "Payment reminder: invoice 036");
        assert!(one["body"].as_str().unwrap().starts_with("Hello,"));
    }
}
