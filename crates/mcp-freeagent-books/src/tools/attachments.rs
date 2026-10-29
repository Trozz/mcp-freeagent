//! Attaching receipts and invoices: upload links for files the model cannot pass itself, and
//! base64 for clients that can read the file.

use super::{BooksServer, Caller, id_arg, json_result, run, text_arg};
use crate::error::{self, FaError};
use crate::uploads::{self, File, State, Target};
use crate::view::{self, resource};
use axum::http::request::Parts;
use base64::Engine as _;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UploadLinkParams {
    /// expense, bill or bank_transaction_explanation.
    pub target: String,
    /// The item's id (create it first if it does not exist yet).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UploadStatusParams {
    /// The upload_url create_upload_link returned.
    pub upload_url: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AttachFileParams {
    /// expense, bill or bank_transaction_explanation.
    pub target: String,
    /// The item's id.
    pub id: String,
    /// e.g. receipt.pdf
    pub file_name: String,
    /// The file's bytes, base64-encoded. Only for files you can read yourself (for example with a shell); requests are limited to 1 MB in all.
    pub data_base64: String,
    pub description: Option<String>,
}

/// A short description of the item for the upload page: what it is, its date and amount.
fn label(target: Target, id: &str, item: &Value) -> String {
    let s = |k: &str| {
        item.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let what = match target {
        Target::Expense => s("description").map(String::from),
        Target::Bill => s("reference").map(|r| format!("bill {r}")),
        Target::Explanation => s("description").map(String::from),
    }
    .unwrap_or_else(|| format!("{} {id}", target.root().replace('_', " ")));
    let amount = ["gross_value", "total_value"]
        .iter()
        .find_map(|k| item.get(*k))
        .and_then(|v| view::pence(Some(v)))
        .map(|p| view::money(p.abs()));
    let mut out: String = what.chars().take(120).collect();
    if let Some(d) = s("dated_on") {
        out.push_str(&format!(", {d}"));
    }
    if let Some(a) = amount {
        out.push_str(&format!(", {a}"));
        if let Some(c) = s("currency") {
            out.push_str(&format!(" {c}"));
        }
    }
    out
}

impl BooksServer {
    /// The item, checked to exist and readable by the caller.
    async fn attachable(&self, c: &Caller, target: Target, id: &str) -> error::Result<Value> {
        let endpoint: &'static str = match target {
            Target::Expense => "expense",
            Target::Bill => "bill",
            Target::Explanation => "bank_transaction_explanation",
        };
        c.one(endpoint, &[target.collection(), id], target.root(), &[])
            .await
    }
}

#[tool_router(router = attachments_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Make a link for attaching a receipt, invoice or other file (PDF, PNG, JPEG or GIF, up to 5 MB) to an expense, bill or bank transaction explanation. Use it whenever the person has a file to attach: you cannot pass files shared in the chat to a tool yourself. Give the person upload_url to open on their phone or computer and pick the file; it works once, for 30 minutes. In a client with a shell, the file can be sent with curl -F file=@<path> <upload_url> instead. Then check with get_upload_status.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn create_upload_link(
        &self,
        Parameters(p): Parameters<UploadLinkParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("create_upload_link", async {
            let c = self.caller(&parts)?;
            let base = self.public_url().ok_or_else(|| {
                FaError::Internal("this server does not know its public address, so it cannot make upload links".into())
            })?;
            let target = Target::parse(&p.target)?;
            let id = id_arg("id", &p.id)?;
            let item = self.attachable(&c, target, &id).await?;
            let (token, expires) = uploads::create(
                self.client.clone(),
                c.credential().clone(),
                target,
                &id,
                label(target, &id, &item),
            )?;
            let url = format!("{base}{}/{token}", uploads::UPLOAD_PATH);
            let expires: chrono::DateTime<chrono::Utc> = expires.into();
            Ok(json_result(&json!({
                "upload_url": url,
                "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "for": {"target": target.root(), "id": id, "item": label(target, &id, &item)},
                "tell_the_person": "Open this link and choose the file to attach. It works once and expires in 30 minutes.",
                "with_a_shell": format!("curl -F file=@<path to the file> {url}"),
            })))
        })
        .await
    }

    #[tool(
        description = "Whether a file has been attached through an upload link from create_upload_link: waiting (with the last error, if a try failed), uploading or attached.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    pub async fn get_upload_status(
        &self,
        Parameters(p): Parameters<UploadStatusParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_upload_status", async {
            let c = self.caller(&parts)?;
            let token = uploads::token_of(&p.upload_url)
                .ok_or_else(|| FaError::invalid("upload_url must be a link create_upload_link returned"))?;
            let st = uploads::status(token, c.credential()).ok_or_else(|| {
                FaError::NotFound("no such upload link: it expired or the server restarted; make a new one with create_upload_link".into())
            })?;
            let mut out = json!({"target": st.target.root(), "id": st.id});
            match st.state {
                State::Waiting { last_error } => {
                    out["status"] = Value::from("waiting");
                    out["expires_in_minutes"] = Value::from(st.expires_in.as_secs() / 60);
                    if let Some(e) = last_error {
                        out["last_error"] = Value::from(e);
                    }
                }
                State::Uploading => out["status"] = Value::from("uploading"),
                State::Attached { file_name, bytes } => {
                    out["status"] = Value::from("attached");
                    out["file_name"] = Value::from(file_name);
                    out["bytes"] = Value::from(bytes);
                }
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Attach a file you can read yourself (for example with a shell) to an expense, bill or bank transaction explanation, as base64. Requests are limited to 1 MB, so this suits files up to about 700 KB. For a file the person shared in the chat, or anything larger, use create_upload_link.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn attach_file(
        &self,
        Parameters(p): Parameters<AttachFileParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("attach_file", async {
            let c = self.caller(&parts)?;
            let target = Target::parse(&p.target)?;
            let id = id_arg("id", &p.id)?;
            let data: String = p
                .data_base64
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&data)
                .map_err(|_| FaError::invalid("data_base64 is not valid base64"))?;
            let file = File::new(
                &p.file_name,
                bytes,
                text_arg("description", p.description.as_deref())?,
            )?;
            let answer = uploads::attach(&self.client, c.credential(), target, &id, &file).await?;
            Ok(json_result(&json!({
                "attached": file.name,
                "to": {"target": target.root(), "id": id},
                "result": resource(&answer),
            })))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_say_what_the_item_is() {
        let e = json!({"description": "Train to Leeds", "dated_on": "2026-10-01", "gross_value": "-42.50", "currency": "GBP"});
        assert_eq!(
            label(Target::Expense, "5", &e),
            "Train to Leeds, 2026-10-01, 42.50 GBP"
        );
        let b = json!({"reference": "INV-9", "total_value": "120.0"});
        assert_eq!(label(Target::Bill, "3", &b), "bill INV-9, 120.00");
        assert_eq!(
            label(Target::Explanation, "7", &json!({})),
            "bank transaction explanation 7"
        );
    }
}
