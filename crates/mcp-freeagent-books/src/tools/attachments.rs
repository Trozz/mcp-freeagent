//! Attaching receipts and invoices: upload links for files the model cannot pass itself, and
//! base64 for clients that can read the file.

use super::{BooksServer, Caller, id_arg, json_result, run, text_arg};
use crate::error::{self, FaError};
use crate::uploads::{self, Dest, File, State, Target};
use crate::view::{self, resource};
use axum::http::request::Parts;
use base64::Engine as _;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UploadLinkParams {
    /// expense, bill or bank_transaction_explanation, to attach the file straight to that item. Leave out, with id, for a receipt or invoice to read first (get_uploaded_file) and attach once the item is made (attach_file with upload_url).
    pub target: Option<String>,
    /// The item's id, with target (create the item first if it does not exist yet).
    pub id: Option<String>,
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
    /// The upload_url of a file the person sent (create_upload_link without a target). Give this, or file_name and data_base64.
    pub upload_url: Option<String>,
    /// e.g. receipt.pdf, with data_base64.
    pub file_name: Option<String>,
    /// The file's bytes, base64-encoded. Only for files you can read yourself (for example with a shell); requests are limited to 1 MB in all.
    pub data_base64: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AttachmentParams {
    /// The attachment's id (or URL), as an expense, bill or bank transaction explanation shows it under attachment.
    pub id: String,
}

/// Largest image handed to the model as an image: base64 makes it about 5 MB, the most a model
/// takes. Larger images, and PDFs, go as an embedded file.
const MAX_IMAGE_BYTES: usize = 3_750_000;

/// A tool result carrying a file for the model to read: an image as an image, anything else as
/// an embedded file with its type.
fn file_result(intro: String, mime: &str, bytes: &[u8], uri: String) -> CallToolResult {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let block = if mime.starts_with("image/") && bytes.len() <= MAX_IMAGE_BYTES {
        ContentBlock::image(data, mime)
    } else {
        ContentBlock::resource(ResourceContents::blob(data, uri).with_mime_type(mime))
    };
    CallToolResult::success(vec![ContentBlock::text(intro), block])
}

/// The common spelling of a content type FreeAgent gives (it calls PDFs `application/x-pdf`).
fn mime_of(content_type: &str) -> String {
    match content_type.trim() {
        "application/x-pdf" | "" => "application/pdf".into(),
        t => t.to_ascii_lowercase(),
    }
}

/// A short description of the item for the upload page: what it is, its date and amount.
fn label(target: Target, id: &str, item: &Value) -> String {
    let s = |k: &str| {
        item.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
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

/// The name of the file an upload would replace: expenses and bills hold one attachment, which
/// a new one replaces. Explanations take several, so nothing is replaced there.
fn replaced(target: Target, item: &Value) -> Option<String> {
    if matches!(target, Target::Explanation) {
        return None;
    }
    let a = item.get("attachment")?;
    Some(
        a.get("file_name")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("a file")
            .to_string(),
    )
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
        description = "Make a link for the person to send a receipt, invoice or other file (PDF, PNG, JPEG or GIF, up to 5 MB): you cannot pass files shared in the chat to a tool yourself. Without target and id, the file is held for you: read it with get_uploaded_file, fill in the expense, bill or explanation from it, then attach it with attach_file and upload_url. With target and id, it is attached to that item at once. Give the person upload_url to open on their phone or computer; it works once, for 30 minutes. With a shell, curl -F file=@<path> <upload_url> works too. Check with get_upload_status.",
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
            let target = text_arg("target", p.target.as_deref())?;
            let id = text_arg("id", p.id.as_deref())?;
            let (target, id) = match (target, id) {
                (None, None) => {
                    let (token, expires) = uploads::create(
                        self.client.clone(),
                        c.credential().clone(),
                        Dest::Held,
                        "a receipt or invoice to read".into(),
                    )?;
                    let url = format!("{base}{}/{token}", uploads::UPLOAD_PATH);
                    let expires: chrono::DateTime<chrono::Utc> = expires.into();
                    return Ok(json_result(&json!({
                        "upload_url": url,
                        "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        "for": "a file for you to read with get_uploaded_file, then attach with attach_file and this upload_url",
                        "tell_the_person": "Open this link and choose the receipt or invoice; I'll read it and fill in the details for you to check. It works once and expires in 30 minutes.",
                        "with_a_shell": format!("curl -F file=@<path to the file> {url}"),
                    })));
                }
                (Some(t), Some(i)) => (Target::parse(&t)?, id_arg("id", &i)?),
                _ => {
                    return Err(FaError::invalid(
                        "give target and id together, or neither for a receipt to read first",
                    ));
                }
            };
            let item = self.attachable(&c, target, &id).await?;
            let (token, expires) = uploads::create(
                self.client.clone(),
                c.credential().clone(),
                Dest::Item {
                    target,
                    id: id.clone(),
                },
                label(target, &id, &item),
            )?;
            let url = format!("{base}{}/{token}", uploads::UPLOAD_PATH);
            let expires: chrono::DateTime<chrono::Utc> = expires.into();
            let mut out = json!({
                "upload_url": url,
                "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "for": {"target": target.root(), "id": id, "item": label(target, &id, &item)},
                "tell_the_person": "Open this link and choose the file to attach. It works once and expires in 30 minutes.",
                "with_a_shell": format!("curl -F file=@<path to the file> {url}"),
            });
            if let Some(name) = replaced(target, &item) {
                out["replaces"] = Value::from(name.clone());
                out["tell_the_person"] = Value::from(format!(
                    "This {} already has a file attached ({name}); uploading replaces it. Open this link and choose the file to attach. It works once and expires in 30 minutes.",
                    target.root().replace('_', " ")
                ));
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Where an upload link from create_upload_link stands: waiting (with the last error, if a try failed), uploading, received (a file held for you to read with get_uploaded_file) or attached.",
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
            let mut out = match &st.dest {
                Dest::Item { target, id } => json!({"target": target.root(), "id": id}),
                Dest::Held => json!({"for": "reading"}),
            };
            match st.state {
                State::Waiting { last_error } => {
                    out["status"] = Value::from("waiting");
                    out["expires_in_minutes"] = Value::from(st.expires_in.as_secs() / 60);
                    if let Some(e) = last_error {
                        out["last_error"] = Value::from(e);
                    }
                }
                State::Uploading => out["status"] = Value::from("uploading"),
                State::Held { file } => {
                    out["status"] = Value::from("received");
                    out["file_name"] = Value::from(file.name.as_str());
                    out["content_type"] = Value::from(file.mime());
                    out["bytes"] = Value::from(file.bytes.len());
                    out["expires_in_minutes"] = Value::from(st.expires_in.as_secs() / 60);
                    out["next"] = Value::from(
                        "read it with get_uploaded_file; attach it with attach_file and this upload_url",
                    );
                }
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
        description = "Read the file the person sent through an upload link made without a target: a photo comes back as an image, a PDF as a file. Use it to fill in an expense, bill or bank explanation (date, supplier, total, VAT), show the person what you read and get their agreement, then attach the file with attach_file and the same upload_url.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    pub async fn get_uploaded_file(
        &self,
        Parameters(p): Parameters<UploadStatusParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_uploaded_file", async {
            let c = self.caller(&parts)?;
            let token = uploads::token_of(&p.upload_url)
                .ok_or_else(|| FaError::invalid("upload_url must be a link create_upload_link returned"))?;
            let file = uploads::held(token, c.credential()).ok_or_else(|| {
                FaError::NotFound("no file is held on that link: it has not been sent yet (get_upload_status), was already attached, or expired".into())
            })?;
            let intro = format!(
                "{} ({}, {} bytes), sent through the upload link.",
                file.name,
                file.mime(),
                file.bytes.len()
            );
            Ok(file_result(intro, file.mime(), &file.bytes, p.upload_url.trim().to_string()))
        })
        .await
    }

    #[tool(
        description = "Read a file already attached in FreeAgent, such as an expense's receipt or a bill's invoice, by the attachment id the item shows: a photo comes back as an image, a PDF as a file. Use it to check what was claimed or to fill in missing details.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_attachment(
        &self,
        Parameters(p): Parameters<AttachmentParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_attachment", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let a = c
                .one("attachment", &["attachments", &id], "attachment", &[])
                .await?;
            let link = a
                .get("content_src")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    FaError::Upstream("FreeAgent gave no file link for this attachment".into())
                })?;
            let bytes = self.client.download(link).await?;
            let mime = mime_of(
                a.get("content_type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            let name = a
                .get("file_name")
                .and_then(Value::as_str)
                .unwrap_or("attachment");
            let intro = format!("{name} ({mime}, {} bytes), attachment {id}.", bytes.len());
            let uri = a
                .get("url")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| c.url("attachments", &id));
            Ok(file_result(intro, &mime, &bytes, uri))
        })
        .await
    }

    #[tool(
        description = "Attach a file to an expense, bill or bank transaction explanation: either one the person sent through an upload link made without a target (upload_url), or one you can read yourself, as base64 (file_name and data_base64; requests are limited to 1 MB, so up to about 700 KB). An expense or bill holds one file, which this replaces.",
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
            let description = text_arg("description", p.description.as_deref())?;
            let (file, token) = match (p.upload_url.as_deref(), p.data_base64.as_deref()) {
                (Some(url), None) => {
                    let token = uploads::token_of(url).ok_or_else(|| {
                        FaError::invalid("upload_url must be a link create_upload_link returned")
                    })?;
                    let mut file = uploads::held(token, c.credential()).ok_or_else(|| {
                        FaError::NotFound("no file is held on that link: it has not been sent yet (get_upload_status), was already attached, or expired".into())
                    })?;
                    if description.is_some() {
                        file.description = description;
                    }
                    (file, Some(token.to_string()))
                }
                (None, Some(b64)) => {
                    let name = p.file_name.as_deref().ok_or_else(|| {
                        FaError::invalid("file_name is required with data_base64")
                    })?;
                    let data: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&data)
                        .map_err(|_| FaError::invalid("data_base64 is not valid base64"))?;
                    (File::new(name, bytes, description)?, None)
                }
                _ => {
                    return Err(FaError::invalid(
                        "give upload_url, or file_name and data_base64, but not both",
                    ));
                }
            };
            let answer = uploads::attach(&self.client, c.credential(), target, &id, &file).await?;
            if let Some(t) = token {
                uploads::mark_attached(&t, target, &id);
            }
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
