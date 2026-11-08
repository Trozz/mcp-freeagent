//! Upload links: how a person attaches a receipt or invoice to an expense, bill or bank
//! explanation when the MCP client cannot hand the file to a tool.
//!
//! A model sees files people share with it but cannot pass their bytes as tool arguments, and
//! requests to the MCP endpoints are capped at 1 MB. So `create_upload_link` makes a single-use
//! link, `PUBLIC_URL/upload/<token>`, for one FreeAgent item. The person opens it and picks the
//! file (or a client with a shell sends it with `curl -F file=@receipt.pdf <link>`), and the
//! server attaches it in FreeAgent as the person who made the link.
//!
//! A link can also be made for no item: the server then holds the file for [`LINK_TTL`] so the
//! model can read it (`read_upload`), fill in an expense, bill or explanation from it, and attach
//! it there (`attach_file` with the link).
//!
//! Links live in this process's memory: they last [`LINK_TTL`], work once, and are lost on a
//! restart. The token is 128 random bits and is the only thing that authorises the upload, so the
//! page is served without caching or referrers and the token is never logged.

use crate::api::FaClient;
use crate::error::{FaError, Result};
use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, Path};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use mcp_server_core::credential::Credential;
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Where upload links are served, below `PUBLIC_URL`.
pub const UPLOAD_PATH: &str = "/upload";
/// How long a link can be used.
pub const LINK_TTL: Duration = Duration::from_secs(30 * 60);
/// Largest file FreeAgent takes.
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
/// Links remembered at once; expired ones are dropped first.
const MAX_LINKS: usize = 5_000;
/// Bytes of uploaded files held for reading at once, across every link.
const MAX_HELD_BYTES: usize = 100 * 1024 * 1024;
/// Uploads sent on to FreeAgent at once.
const MAX_RUNNING: usize = 8;
/// The API version whose explanation attachments endpoint is used.
pub const ATTACHMENTS_API_VERSION: &str = "2026-09-01";

/// What a file is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Expense,
    Bill,
    Explanation,
}

impl Target {
    pub fn parse(raw: &str) -> Result<Target> {
        let norm: String = raw
            .trim()
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '_'))
            .flat_map(char::to_lowercase)
            .collect();
        match norm.as_str() {
            "expense" | "expenses" | "mileage" => Ok(Target::Expense),
            "bill" | "bills" => Ok(Target::Bill),
            "banktransactionexplanation"
            | "explanation"
            | "banktransaction"
            | "banktransactionexplanations" => Ok(Target::Explanation),
            _ => Err(FaError::invalid(
                "target must be expense, bill or bank_transaction_explanation",
            )),
        }
    }

    pub fn collection(self) -> &'static str {
        match self {
            Target::Expense => "expenses",
            Target::Bill => "bills",
            Target::Explanation => "bank_transaction_explanations",
        }
    }

    pub fn root(self) -> &'static str {
        match self {
            Target::Expense => "expense",
            Target::Bill => "bill",
            Target::Explanation => "bank_transaction_explanation",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Target::Expense => "expense",
            Target::Bill => "bill",
            Target::Explanation => "bank transaction explanation",
        }
    }
}

/// A file to attach, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    /// As FreeAgent spells it: `application/x-pdf`, `image/png`, `image/jpeg` or `image/gif`.
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
    pub description: Option<String>,
}

/// The content type of a file from its first bytes, as FreeAgent spells it.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF") {
        Some("application/x-pdf")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

impl File {
    /// A file FreeAgent accepts: a PDF, PNG, JPEG or GIF (told by its contents) of 1 byte to 5 MB.
    pub fn new(name: &str, bytes: Vec<u8>, description: Option<String>) -> Result<File> {
        if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
            return Err(FaError::invalid("files must be between 1 byte and 5 MB"));
        }
        let content_type = sniff(&bytes).ok_or_else(|| {
            FaError::invalid("FreeAgent takes PDF, PNG, JPEG or GIF files only; this file is none of those (a photo saved as HEIC can be shared as JPEG instead)")
        })?;
        let ext = match content_type {
            "application/x-pdf" => "pdf",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => "gif",
        };
        let clean: String = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
        let name = match clean.trim() {
            "" => format!("attachment.{ext}"),
            n => n.to_string(),
        };
        Ok(File {
            name,
            content_type,
            bytes,
            description,
        })
    }

    /// The content type as the rest of the world spells it (`application/pdf`, not FreeAgent's
    /// `application/x-pdf`).
    pub fn mime(&self) -> &'static str {
        if self.content_type == "application/x-pdf" {
            "application/pdf"
        } else {
            self.content_type
        }
    }

    fn json(&self) -> Value {
        let mut v = json!({
            "file_name": self.name,
            "content_type": self.content_type,
            "data": base64::engine::general_purpose::STANDARD.encode(&self.bytes),
        });
        if let Some(d) = &self.description {
            v["description"] = Value::from(d.as_str());
        }
        v
    }
}

/// Attaches `file` to FreeAgent item `id` of `target`, as the person whose sign-in is
/// `credential`. Expenses and bills take it as their attachment; bank transaction explanations
/// through their attachments endpoint, which is not affected by the old single-attachment field
/// being withdrawn.
pub async fn attach(
    client: &FaClient,
    credential: &Credential,
    target: Target,
    id: &str,
    file: &File,
) -> Result<Value> {
    let a = match target {
        Target::Explanation => {
            let body = json!({"attachments": [file.json()]});
            client
                .send_versioned(
                    credential,
                    Method::POST,
                    "attach_explanation",
                    &[target.collection(), id, "attachments"],
                    &[],
                    Some(&body),
                    Some(ATTACHMENTS_API_VERSION),
                )
                .await?
        }
        Target::Expense | Target::Bill => {
            let body = json!({target.root(): {"attachment": file.json()}});
            let endpoint = if target == Target::Expense {
                "attach_expense"
            } else {
                "attach_bill"
            };
            client
                .send(
                    credential,
                    Method::PUT,
                    endpoint,
                    &[target.collection(), id],
                    &[],
                    Some(&body),
                )
                .await?
        }
    };
    metrics::counter!("freeagent_attachments_total", "target" => target.root()).increment(1);
    Ok(a.body)
}

/// Where an uploaded file goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// Attached at once to this item.
    Item { target: Target, id: String },
    /// Held for the model to read and attach.
    Held,
}

/// Where a link stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Waiting {
        last_error: Option<String>,
    },
    Uploading,
    /// Uploaded to a link for no item and held for the model.
    Held {
        file: File,
    },
    Attached {
        file_name: String,
        bytes: usize,
    },
}

struct Link {
    client: Arc<FaClient>,
    credential: Credential,
    dest: Dest,
    label: String,
    created: SystemTime,
    expires: Instant,
    state: State,
}

/// What a tool can say about a link.
#[derive(Debug, Clone)]
pub struct Status {
    pub dest: Dest,
    pub state: State,
    pub expires_in: Duration,
}

static LINKS: LazyLock<Mutex<HashMap<String, Link>>> = LazyLock::new(Default::default);
static RUNNING: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_RUNNING)));

fn links() -> std::sync::MutexGuard<'static, HashMap<String, Link>> {
    LINKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_token() -> Result<String> {
    use ring::rand::SecureRandom as _;
    let mut raw = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| FaError::Internal("no randomness for the upload link".into()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
}

/// Makes a link for a file going to `dest`, described to the person as `label`. Returns its
/// token and when it expires.
pub fn create(
    client: Arc<FaClient>,
    credential: Credential,
    dest: Dest,
    label: String,
) -> Result<(String, SystemTime)> {
    let token = new_token()?;
    let now = Instant::now();
    let mut all = links();
    if all.len() >= MAX_LINKS {
        all.retain(|_, l| l.expires > now);
        if all.len() >= MAX_LINKS {
            return Err(FaError::Upstream(
                "too many upload links are open on this server; try again in a few minutes".into(),
            ));
        }
    }
    let created = SystemTime::now();
    all.insert(
        token.clone(),
        Link {
            client,
            credential,
            dest,
            label,
            created,
            expires: now + LINK_TTL,
            state: State::Waiting { last_error: None },
        },
    );
    Ok((token, created + LINK_TTL))
}

/// The status of link `token`, if it exists and belongs to `credential`'s sign-in.
pub fn status(token: &str, credential: &Credential) -> Option<Status> {
    let all = links();
    let l = all.get(token)?;
    if l.credential.fingerprint() != credential.fingerprint() {
        return None;
    }
    Some(Status {
        dest: l.dest.clone(),
        state: l.state.clone(),
        expires_in: l.expires.saturating_duration_since(Instant::now()),
    })
}

/// The file held on link `token`, if it has one, has not expired and belongs to `credential`'s
/// sign-in.
pub fn held(token: &str, credential: &Credential) -> Option<File> {
    let all = links();
    let l = all.get(token)?;
    if l.credential.fingerprint() != credential.fingerprint() || l.expires <= Instant::now() {
        return None;
    }
    match &l.state {
        State::Held { file } => Some(file.clone()),
        _ => None,
    }
}

/// Records that the file held on link `token` was attached to item `id` of `target`, and lets
/// it go.
pub fn mark_attached(token: &str, target: Target, id: &str) {
    if let Some(l) = links().get_mut(token)
        && let State::Held { file } = &l.state
    {
        l.state = State::Attached {
            file_name: file.name.clone(),
            bytes: file.bytes.len(),
        };
        l.dest = Dest::Item {
            target,
            id: id.to_string(),
        };
    }
}

fn held_bytes(all: &HashMap<String, Link>) -> usize {
    all.values()
        .map(|l| match &l.state {
            State::Held { file } => file.bytes.len(),
            _ => 0,
        })
        .sum()
}

/// The token of a link given as the token or as its URL.
pub fn token_of(raw: &str) -> Option<&str> {
    let t = raw.trim().trim_end_matches('/').rsplit('/').next()?;
    (t.len() == 22
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then_some(t)
}

/// The upload page and endpoint, added to the server with `Server::wrap_router`. They are
/// outside the bearer guard: the link's token is what authorises an upload.
pub fn routes(app: Router) -> Router {
    app.route(
        &format!("{UPLOAD_PATH}/{{token}}"),
        get(page)
            .post(upload)
            .layer(DefaultBodyLimit::max(MAX_FILE_BYTES + 256 * 1024)),
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn with_headers(status: StatusCode, html: String) -> Response {
    let mut res = (status, Html(html)).into_response();
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    res
}

const STYLE: &str = "body{font-family:system-ui;max-width:28rem;margin:3rem auto;padding:0 1rem;line-height:1.4}input[type=file]{margin:1rem 0;width:100%}button{padding:.6rem 1.2rem}small{color:#555}.ok{color:#0a7}.err{color:#b00}";

fn page_html(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><style>{STYLE}</style></head><body><h1>{}</h1>{body}</body></html>",
        escape(title),
        escape(title)
    )
}

/// Answers a browser with a page and anything else (curl) with one line of text.
fn answer(headers: &HeaderMap, status: StatusCode, title: &str, message: &str) -> Response {
    let wants_html = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if wants_html {
        let class = if status.is_success() { "ok" } else { "err" };
        with_headers(
            status,
            page_html(
                title,
                &format!("<p class=\"{class}\">{}</p>", escape(message)),
            ),
        )
    } else {
        let mut res = (status, format!("{message}\n")).into_response();
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        res
    }
}

const GONE: &str =
    "This upload link has expired, was already used or does not exist. Ask for a new one.";

async fn page(Path(token): Path<String>, headers: HeaderMap) -> Response {
    let found = {
        let mut all = links();
        let now = Instant::now();
        all.retain(|_, l| l.expires > now);
        all.get(&token)
            .map(|l| (l.label.clone(), l.dest.clone(), l.state.clone(), l.expires))
    };
    let Some((label, dest, state, expires)) = found else {
        return answer(&headers, StatusCode::NOT_FOUND, "Upload link", GONE);
    };
    match (&state, &dest) {
        (State::Attached { file_name, .. }, Dest::Item { target, .. }) => {
            return answer(
                &headers,
                StatusCode::OK,
                "Already attached",
                &format!(
                    "{file_name} is attached to the {} in FreeAgent.",
                    target.noun()
                ),
            );
        }
        (State::Held { file }, _) => {
            return answer(
                &headers,
                StatusCode::OK,
                "Already received",
                &format!("{} has been received. Go back to your chat.", file.name),
            );
        }
        _ => {}
    }
    let minutes = expires.saturating_duration_since(Instant::now()).as_secs() / 60;
    let retry = match state {
        State::Waiting {
            last_error: Some(e),
        } => format!("<p class=\"err\">The last try failed: {}</p>", escape(&e)),
        _ => String::new(),
    };
    let (title, intro, button) = match &dest {
        Dest::Item { target, .. } => (
            "Attach a file",
            format!(
                "<p>Attach a receipt or invoice to this {} in FreeAgent:</p><p><strong>{}</strong></p>",
                target.noun(),
                escape(&label)
            ),
            "Attach",
        ),
        Dest::Held => (
            "Send a receipt",
            "<p>Choose a receipt or invoice. Claude will read it and fill in the details for you to check before anything is saved in FreeAgent.</p>".to_string(),
            "Send",
        ),
    };
    let body = format!(
        "{intro}{retry}\
<form method=\"post\" enctype=\"multipart/form-data\"><input type=\"file\" name=\"file\" accept=\"application/pdf,image/png,image/jpeg,image/gif\" required>\
<button type=\"submit\">{button}</button></form><p><small>PDF, PNG, JPEG or GIF, up to 5 MB. This link works once and expires in about {minutes} minutes.</small></p>"
    );
    with_headers(StatusCode::OK, page_html(title, &body))
}

async fn upload(Path(token): Path<String>, headers: HeaderMap, mut form: Multipart) -> Response {
    // Claim the link before reading the file, so two uploads cannot both attach.
    let claimed = {
        let mut all = links();
        match all.get_mut(&token) {
            Some(l) if l.expires > Instant::now() => match l.state {
                State::Waiting { .. } => {
                    l.state = State::Uploading;
                    Some((l.client.clone(), l.credential.clone(), l.dest.clone()))
                }
                _ => None,
            },
            _ => None,
        }
    };
    let Some((client, credential, dest)) = claimed else {
        return answer(&headers, StatusCode::NOT_FOUND, "Upload link", GONE);
    };
    let release = |error: String| {
        if let Some(l) = links().get_mut(&token) {
            l.state = State::Waiting {
                last_error: Some(error),
            };
        }
    };
    let mut file: Option<(String, Vec<u8>)> = None;
    loop {
        match form.next_field().await {
            Ok(Some(field)) => {
                let is_file = field.name() == Some("file") || field.file_name().is_some();
                if !is_file {
                    continue;
                }
                let name = field.file_name().unwrap_or_default().to_string();
                match field.bytes().await {
                    Ok(b) => {
                        file = Some((name, b.to_vec()));
                        break;
                    }
                    Err(_) => {
                        let msg = "The file could not be read; it may be larger than 5 MB.";
                        release(msg.into());
                        return answer(
                            &headers,
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "Upload failed",
                            msg,
                        );
                    }
                }
            }
            Ok(None) => break,
            Err(_) => {
                let msg = "The upload could not be read; it may be larger than 5 MB.";
                release(msg.into());
                return answer(&headers, StatusCode::BAD_REQUEST, "Upload failed", msg);
            }
        }
    }
    let Some((name, bytes)) = file else {
        let msg = "No file was sent: send it as the form field named file.";
        release(msg.into());
        return answer(&headers, StatusCode::BAD_REQUEST, "Upload failed", msg);
    };
    let file = match File::new(&name, bytes, None) {
        Ok(f) => f,
        Err(e) => {
            let msg = e.to_string();
            release(msg.clone());
            return answer(
                &headers,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Upload failed",
                &msg,
            );
        }
    };
    let (target, id) = match dest {
        Dest::Item { target, id } => (target, id),
        Dest::Held => {
            let mut all = links();
            if held_bytes(&all) + file.bytes.len() > MAX_HELD_BYTES {
                drop(all);
                let msg =
                    "The server is holding too many files just now; try again in a few minutes.";
                release(msg.into());
                return answer(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Upload failed",
                    msg,
                );
            }
            let name = file.name.clone();
            if let Some(l) = all.get_mut(&token) {
                l.state = State::Held { file };
                // The model has the link's full time from now to read and attach the file.
                l.expires = Instant::now() + LINK_TTL;
            }
            tracing::info!("file received through an upload link for reading");
            return answer(
                &headers,
                StatusCode::OK,
                "Received",
                &format!("{name} has been received. Go back to your chat: Claude can read it now."),
            );
        }
    };
    let Ok(_permit) = RUNNING.clone().acquire_owned().await else {
        release("the server is shutting down".into());
        return answer(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            "Upload failed",
            "Try again shortly.",
        );
    };
    match attach(&client, &credential, target, &id, &file).await {
        Ok(_) => {
            if let Some(l) = links().get_mut(&token) {
                l.state = State::Attached {
                    file_name: file.name.clone(),
                    bytes: file.bytes.len(),
                };
            }
            tracing::info!(
                target = target.root(),
                "file attached through an upload link"
            );
            answer(
                &headers,
                StatusCode::OK,
                "Attached",
                &format!(
                    "{} is attached to the {} in FreeAgent. You can close this page.",
                    file.name,
                    target.noun()
                ),
            )
        }
        Err(e) => {
            tracing::warn!(
                target = target.root(),
                kind = e.kind(),
                "attaching an uploaded file failed"
            );
            let msg = format!("FreeAgent did not take the file: {e}");
            release(msg.clone());
            answer(&headers, StatusCode::BAD_GATEWAY, "Upload failed", &msg)
        }
    }
}

/// When a link was made, for tests and diagnostics.
pub fn created_at(token: &str) -> Option<SystemTime> {
    links().get(token).map(|l| l.created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_typed_by_their_contents() {
        assert_eq!(sniff(b"%PDF-1.7"), Some("application/x-pdf"));
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 0x0D]), Some("image/png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff(b"PK\x03\x04"), None);
        let f = File::new("C:\\photos\\receipt.jpg", vec![0xFF, 0xD8, 0xFF, 1], None).unwrap();
        assert_eq!(
            (f.name.as_str(), f.content_type),
            ("receipt.jpg", "image/jpeg")
        );
        assert_eq!(
            File::new("", b"%PDF-1".to_vec(), None).unwrap().name,
            "attachment.pdf"
        );
        assert!(File::new("x.heic", b"....ftypheic".to_vec(), None).is_err());
        assert!(File::new("x.pdf", Vec::new(), None).is_err());
        assert!(File::new("x.pdf", vec![b'%'; MAX_FILE_BYTES + 1], None).is_err());
    }

    #[test]
    fn targets_and_tokens() {
        assert_eq!(
            Target::parse("Bank transaction explanation").unwrap(),
            Target::Explanation
        );
        assert_eq!(Target::parse("expenses").unwrap().collection(), "expenses");
        assert!(Target::parse("invoice").is_err());
        let t = new_token().unwrap();
        assert_eq!(t.len(), 22);
        assert_eq!(
            token_of(&format!("https://x.example.com/upload/{t}")),
            Some(t.as_str())
        );
        assert_eq!(token_of(&t), Some(t.as_str()));
        assert_eq!(token_of("https://x.example.com/upload/short"), None);
    }
}
