//! Thin client for the FreeAgent API v2: the caller's access token (from [`crate::oauth::Tokens`]),
//! no redirects, timeouts, one retry of reads after a transport error or a 5xx, one retry with a
//! fresh access token after a 401, status mapping, paging headers and metrics. Writes are never
//! retried after they reach FreeAgent, so a slow create cannot run twice.

use crate::error::{FaError, Result};
use crate::oauth::Tokens;
use mcp_server_core::credential::Credential;
use mcp_server_core::observability::content_logging_enabled;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use secrecy::ExposeSecret;
use serde_json::Value;
use std::time::{Duration, Instant};
use url::Url;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const RETRY_DELAY: Duration = Duration::from_millis(250);
/// Largest response body read. PDFs come base64-encoded in JSON, so this is generous.
pub const MAX_BODY_BYTES: usize = 24 * 1024 * 1024;
const MAX_ERROR_DETAIL: usize = 400;
/// Longest raw API path accepted.
const MAX_PATH_CHARS: usize = 200;

pub struct ApiSettings {
    /// The API base, ending in `/v2`.
    pub base: Url,
    /// `X-Api-Version` to send, when set.
    pub api_version: Option<String>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub retry_delay: Duration,
    /// Always log response bodies at debug level. Without it, bodies are logged only while
    /// `DEBUG_UNSAFE_LOG_CONTENT` is on.
    pub log_bodies: bool,
}

impl ApiSettings {
    pub fn new(base: Url) -> Self {
        ApiSettings {
            base,
            api_version: None,
            connect_timeout: CONNECT_TIMEOUT,
            request_timeout: REQUEST_TIMEOUT,
            retry_delay: RETRY_DELAY,
            log_bodies: false,
        }
    }
}

/// The HTTP client every FreeAgent request goes through: no redirects, timeouts, this server's
/// user agent (FreeAgent requires one).
pub fn http_client(settings: &ApiSettings) -> Result<reqwest::Client> {
    mcp_server_core::install_crypto_provider();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(settings.connect_timeout)
        .timeout(settings.request_timeout)
        .user_agent(format!(
            "mcp-freeagent/{} (+https://github.com/Trozz/mcp-freeagent)",
            crate::VERSION
        ))
        .build()
        .map_err(|e| FaError::Internal(format!("building the HTTP client failed: {e}")))
}

/// A FreeAgent answer: its JSON body and what its headers say about paging.
#[derive(Debug, Clone)]
pub struct Answer {
    pub body: Value,
    /// `X-Total-Count`: how many items the whole list has.
    pub total: Option<u64>,
    /// Whether the `Link` header names a next page.
    pub has_next: bool,
    /// `Location` of a created resource.
    pub location: Option<String>,
}

pub struct FaClient {
    http: reqwest::Client,
    settings: ApiSettings,
    tokens: Tokens,
}

fn status_class(status: Option<StatusCode>) -> &'static str {
    match status.map(|s| s.as_u16()) {
        Some(200..=299) => "2xx",
        Some(300..=399) => "3xx",
        Some(400..=499) => "4xx",
        Some(500..=599) => "5xx",
        _ => "error",
    }
}

/// A short, single-line message from a FreeAgent error body. Bodies come as
/// `{"errors":[{"message":"..."}]}`, `{"errors":{"error":{"message":"..."}}}`,
/// `{"errors":{"error":[{"message":"..."}]}}` or plain text.
pub fn error_detail(body: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    match serde_json::from_str::<Value>(body) {
        Ok(v) => collect_messages(v.get("errors").unwrap_or(&v), &mut parts),
        Err(_) => {
            let t = body.trim();
            if !t.is_empty() && !t.starts_with('<') {
                parts.push(t.to_string());
            }
        }
    }
    let joined = parts.join("; ");
    let joined = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    if joined.chars().count() > MAX_ERROR_DETAIL {
        let cut: String = joined.chars().take(MAX_ERROR_DETAIL).collect();
        return Some(format!("{cut}..."));
    }
    Some(joined)
}

fn collect_messages(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| collect_messages(i, out)),
        Value::Object(map) => {
            if let Some(m) = map.get("message").and_then(Value::as_str) {
                out.push(m.to_string());
            } else {
                map.values().for_each(|i| collect_messages(i, out));
            }
        }
        _ => {}
    }
}

fn map_status(
    method: &Method,
    path: &str,
    status: StatusCode,
    body: &str,
    retry_after_secs: Option<u64>,
) -> FaError {
    let detail = error_detail(body);
    let with_detail = |prefix: String| match &detail {
        Some(d) => format!("{prefix}: {d}"),
        None => prefix,
    };
    match status.as_u16() {
        401 => FaError::Auth,
        403 => FaError::Permission(with_detail(format!(
            "FreeAgent refused {method} {path} (HTTP 403): the signed-in user's access level does not allow it, or the feature does not apply to this company"
        ))),
        404 => FaError::NotFound(with_detail(format!("nothing found at {path}"))),
        429 => FaError::RateLimited { retry_after_secs },
        400 | 406 | 409 | 422 => FaError::Validation(with_detail(format!(
            "FreeAgent rejected {method} {path} (HTTP {})",
            status.as_u16()
        ))),
        300..=399 => FaError::Upstream(format!(
            "{method} {path} returned an unexpected redirect (HTTP {})",
            status.as_u16()
        )),
        code => FaError::Upstream(with_detail(format!("{method} {path} returned HTTP {code}"))),
    }
}

fn has_next(headers: &HeaderMap) -> bool {
    headers
        .get_all(reqwest::header::LINK)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|part| part.contains("rel=\"next\"") || part.contains("rel=next"))
}

impl FaClient {
    pub fn new(http: reqwest::Client, settings: ApiSettings, tokens: Tokens) -> Self {
        FaClient {
            http,
            settings,
            tokens,
        }
    }

    pub fn base(&self) -> &Url {
        &self.settings.base
    }

    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    fn log_bodies(&self) -> bool {
        self.settings.log_bodies || content_logging_enabled()
    }

    /// `base` with `segments` appended (each percent-encoded) and `query` added.
    pub fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Url {
        let mut url = self.settings.base.clone();
        url.path_segments_mut()
            .expect("an http(s) base URL has path segments")
            .pop_if_empty()
            .extend(segments);
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        url
    }

    /// The URL FreeAgent uses to name resource `id` of `collection`, e.g. `contacts/2`: how
    /// resources refer to each other in request bodies and filters.
    pub fn resource_url(&self, collection: &str, id: &str) -> String {
        self.url(&[collection, id], &[]).into()
    }

    /// The path segments below the API base of a raw path given by a person or model: a path
    /// such as `invoices/12`, `/v2/invoices/12` or a full URL on this server's FreeAgent host.
    pub fn raw_segments(&self, raw: &str) -> Result<Vec<String>> {
        let raw = raw.trim();
        let base = &self.settings.base;
        let path = if let Ok(u) = Url::parse(raw) {
            if u.origin() != base.origin() {
                return Err(FaError::invalid(format!(
                    "path must be on this server's FreeAgent API ({}), not another host",
                    base.origin().ascii_serialization()
                )));
            }
            if u.query().is_some() {
                return Err(FaError::invalid(
                    "put query parameters in query, not in path",
                ));
            }
            u.path().to_string()
        } else {
            raw.to_string()
        };
        if path.contains('?') || path.contains('#') {
            return Err(FaError::invalid(
                "put query parameters in query, not in path",
            ));
        }
        let base_path = base.path().trim_end_matches('/');
        let rest = path
            .strip_prefix(base_path)
            .filter(|r| r.is_empty() || r.starts_with('/'))
            .unwrap_or(&path);
        let rest = rest.strip_prefix("/v2/").unwrap_or(rest);
        let segments: Vec<String> = rest
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let ok = !segments.is_empty()
            && rest.chars().count() <= MAX_PATH_CHARS
            && segments.iter().all(|s| {
                s != "."
                    && s != ".."
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
            && !matches!(segments[0].as_str(), "token_endpoint" | "approve_app");
        if !ok {
            return Err(FaError::invalid(
                "path must be a FreeAgent API path such as invoices/123 or accounting/profit_and_loss/summary",
            ));
        }
        Ok(segments)
    }

    /// GET a JSON resource. `endpoint` is a fixed label for metrics and logs.
    pub async fn get(
        &self,
        key: &Credential,
        endpoint: &'static str,
        segments: &[&str],
        query: &[(&str, String)],
    ) -> Result<Answer> {
        self.send(key, Method::GET, endpoint, segments, query, None)
            .await
    }

    /// Sends a request with the caller's access token.
    pub async fn send(
        &self,
        key: &Credential,
        method: Method,
        endpoint: &'static str,
        segments: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Answer> {
        let url = self.url(segments, query);
        let path = url.path().to_string();
        let retryable = method == Method::GET;
        let mut retried = false;
        let mut reauthorised = false;
        loop {
            let token = self.tokens.access_token(key).await?;
            let started = Instant::now();
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .bearer_auth(token.expose_secret())
                .header(reqwest::header::ACCEPT, "application/json");
            if let Some(v) = &self.settings.api_version {
                request = request.header("X-Api-Version", v);
            }
            if let Some(body) = body {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.to_string());
            }
            let result = request.send().await;
            let status = result.as_ref().ok().map(|r| r.status());
            metrics::counter!("freeagent_requests_total", "endpoint" => endpoint, "status_class" => status_class(status))
                .increment(1);
            metrics::histogram!("freeagent_request_duration_seconds", "endpoint" => endpoint)
                .record(started.elapsed().as_secs_f64());
            tracing::debug!(
                endpoint,
                method = %method,
                path = %path,
                status = status.map(|s| s.as_u16()),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "freeagent request"
            );
            let response = match result {
                Ok(r) => r,
                Err(e) if retryable && !retried => {
                    tracing::debug!(endpoint, error = %describe(&e), "retrying");
                    retried = true;
                    tokio::time::sleep(self.settings.retry_delay).await;
                    continue;
                }
                Err(e) => {
                    let what = if e.is_timeout() {
                        "timed out"
                    } else {
                        "failed"
                    };
                    return Err(FaError::Upstream(format!(
                        "{method} {path} {what}: {}",
                        describe(&e)
                    )));
                }
            };
            let status = response.status();
            // An access token can be revoked or expire early: refresh once and send again. The
            // request was refused before FreeAgent acted on it, so this is safe for writes too.
            if status == StatusCode::UNAUTHORIZED && !reauthorised {
                reauthorised = true;
                self.tokens.invalidate(key, &token).await;
                continue;
            }
            if status.is_server_error() && retryable && !retried {
                tracing::debug!(endpoint, status = status.as_u16(), "retrying");
                retried = true;
                tokio::time::sleep(self.settings.retry_delay).await;
                continue;
            }
            let headers = response.headers().clone();
            let retry_after_secs = headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let text = read_body(response, &path).await?;
            if self.log_bodies() {
                tracing::debug!(endpoint, path = %path, status = status.as_u16(), body = %text, "freeagent response");
            }
            if !status.is_success() {
                if status == StatusCode::TOO_MANY_REQUESTS {
                    metrics::counter!("freeagent_rate_limited_total").increment(1);
                }
                return Err(map_status(&method, &path, status, &text, retry_after_secs));
            }
            let body = if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).map_err(|e| {
                    FaError::Upstream(format!(
                        "{method} {path} returned invalid JSON ({e}); check FREEAGENT_API_URL points at the FreeAgent API"
                    ))
                })?
            };
            return Ok(Answer {
                body,
                total: headers
                    .get("x-total-count")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok()),
                has_next: has_next(&headers),
                location: headers
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            });
        }
    }
}

fn describe(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    // reqwest includes the URL, which carries query values; keep only the kind of failure.
    msg.split(" for url ").next().unwrap_or(&msg).to_string()
}

async fn read_body(mut response: reqwest::Response, path: &str) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|n| n as usize > MAX_BODY_BYTES)
    {
        return Err(FaError::Upstream(format!(
            "{path} answered with more than {} MB",
            MAX_BODY_BYTES / (1024 * 1024)
        )));
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| FaError::Upstream(format!("reading {path} failed: {}", describe(&e))))?
    {
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(FaError::Upstream(format!(
                "{path} answered with more than {} MB",
                MAX_BODY_BYTES / (1024 * 1024)
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::OAuthApp;
    use std::sync::Arc;

    fn client() -> FaClient {
        let base = Url::parse("https://api.freeagent.com/v2").unwrap();
        let app = Arc::new(OAuthApp::new(
            {
                mcp_server_core::install_crypto_provider();
                reqwest::Client::new()
            },
            base.clone(),
            "id".into(),
            "secret".to_string().into(),
        ));
        FaClient::new(
            {
                mcp_server_core::install_crypto_provider();
                reqwest::Client::new()
            },
            ApiSettings::new(base),
            Tokens::new(app, None),
        )
    }

    #[test]
    fn error_bodies_in_every_shape() {
        assert_eq!(
            error_detail(r#"{"errors":[{"message":"a"},{"message":"b"}]}"#).as_deref(),
            Some("a; b")
        );
        assert_eq!(
            error_detail(r#"{"errors":{"error":{"message":"Access token not recognised"}}}"#)
                .as_deref(),
            Some("Access token not recognised")
        );
        assert_eq!(
            error_detail(r#"{"errors":{"error":[{"message":"x"}]}}"#).as_deref(),
            Some("x")
        );
        assert_eq!(
            error_detail("You must not exceed 120 requests per 60 seconds").as_deref(),
            Some("You must not exceed 120 requests per 60 seconds")
        );
        assert_eq!(error_detail("<html>oops</html>"), None);
        assert_eq!(error_detail(""), None);
    }

    #[test]
    fn statuses_map_to_kinds() {
        let m = Method::GET;
        assert_eq!(
            map_status(&m, "/v2/x", StatusCode::UNAUTHORIZED, "", None).kind(),
            "auth"
        );
        assert_eq!(
            map_status(&m, "/v2/x", StatusCode::FORBIDDEN, "", None).kind(),
            "permission"
        );
        assert_eq!(
            map_status(&m, "/v2/x", StatusCode::NOT_FOUND, "", None).kind(),
            "not_found"
        );
        let e = map_status(
            &Method::POST,
            "/v2/invoices",
            StatusCode::UNPROCESSABLE_ENTITY,
            r#"{"errors":[{"message":"Contact can't be blank"}]}"#,
            None,
        );
        assert_eq!(e.kind(), "validation");
        assert!(e.to_string().contains("Contact can't be blank"));
        assert!(matches!(
            map_status(&m, "/v2/x", StatusCode::TOO_MANY_REQUESTS, "", Some(60)),
            FaError::RateLimited {
                retry_after_secs: Some(60)
            }
        ));
        assert_eq!(
            map_status(&m, "/v2/x", StatusCode::BAD_GATEWAY, "", None).kind(),
            "upstream"
        );
    }

    #[test]
    fn link_headers_name_the_next_page() {
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            "<https://api.freeagent.com/v2/invoices?page=1>; rel=\"prev\", <https://api.freeagent.com/v2/invoices?page=3>; rel=\"next\"".parse().unwrap(),
        );
        assert!(has_next(&h));
        h.insert(
            reqwest::header::LINK,
            "<https://api.freeagent.com/v2/invoices?page=1>; rel=\"first\""
                .parse()
                .unwrap(),
        );
        assert!(!has_next(&h));
    }

    #[test]
    fn resource_urls_and_raw_paths() {
        let c = client();
        assert_eq!(
            c.resource_url("contacts", "2"),
            "https://api.freeagent.com/v2/contacts/2"
        );
        for (raw, want) in [
            ("invoices/12", vec!["invoices", "12"]),
            ("/v2/invoices/12", vec!["invoices", "12"]),
            (
                "https://api.freeagent.com/v2/invoices/12",
                vec!["invoices", "12"],
            ),
            (
                "accounting/profit_and_loss/summary",
                vec!["accounting", "profit_and_loss", "summary"],
            ),
            ("/company", vec!["company"]),
        ] {
            assert_eq!(c.raw_segments(raw).unwrap(), want, "{raw}");
        }
        for bad in [
            "",
            "/",
            "https://evil.example.com/v2/invoices",
            "invoices/../token_endpoint",
            "token_endpoint",
            "approve_app",
            "invoices?view=all",
            "invoices/1 2",
            "invoices/%2e%2e",
        ] {
            assert_eq!(
                c.raw_segments(bad).unwrap_err().kind(),
                "validation",
                "{bad}"
            );
        }
    }
}
