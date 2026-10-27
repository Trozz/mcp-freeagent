//! An in-memory FreeAgent API shaped like the real one: `token_endpoint` with HTTP Basic client
//! authentication, authorization codes, access tokens that expire and refresh tokens that can be
//! rotated or revoked; bearer authentication on everything else; canned resources that tests set
//! per method and path; and a log of every request for assertions.
#![allow(dead_code)]

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use url::Url;

pub const CLIENT_ID: &str = "test-client-id";
pub const CLIENT_SECRET: &str = "test-client-secret";
/// The authorization code the fake grants for.
pub const CODE: &str = "good-code";
/// The refresh token the code grants.
pub const REFRESH: &str = "refresh-0";

#[derive(Debug, Clone)]
pub struct Logged {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub authorization: Option<String>,
    pub api_version: Option<String>,
    pub body: Option<Value>,
    pub form: Vec<(String, String)>,
}

impl Logged {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A canned answer.
#[derive(Debug, Clone)]
pub struct Canned {
    pub status: u16,
    pub body: Value,
    pub headers: Vec<(&'static str, String)>,
}

impl Canned {
    pub fn ok(body: Value) -> Self {
        Canned {
            status: 200,
            body,
            headers: Vec::new(),
        }
    }
    pub fn status(status: u16, body: Value) -> Self {
        Canned {
            status,
            body,
            headers: Vec::new(),
        }
    }
    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

#[derive(Default)]
pub struct FakeState {
    pub log: Vec<Logged>,
    /// Answers by "METHOD /v2/path"; a list is served in order, the last one repeatedly.
    pub canned: HashMap<String, Vec<Canned>>,
    /// Valid access tokens.
    pub access: Vec<String>,
    /// Valid refresh tokens.
    pub refresh: Vec<String>,
    /// Whether a refresh replaces the refresh token (and the old one stops working).
    pub rotate: bool,
    /// Refreshes made.
    pub refreshes: usize,
    pub codes_used: usize,
    /// expires_in the fake grants.
    pub expires_in: u64,
    /// Accept the app's credentials only in the form body, refusing HTTP Basic.
    pub body_auth_only: bool,
}

#[derive(Clone)]
pub struct FakeFreeAgent {
    pub url: Url,
    pub state: Arc<Mutex<FakeState>>,
}

impl FakeFreeAgent {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState {
            refresh: vec![REFRESH.to_string()],
            expires_in: 3600,
            ..Default::default()
        }));
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let fake = FakeFreeAgent {
            url: Url::parse(&format!("http://{addr}/v2")).unwrap(),
            state,
        };
        fake.defaults();
        fake
    }

    pub fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    /// The URL of resource `path` on the fake, as FreeAgent writes references.
    pub fn res(&self, path: &str) -> String {
        format!("{}/{path}", self.url)
    }

    /// Serves `answer` for `method` `path` (below /v2).
    pub fn on(&self, method: &str, path: &str, answer: Canned) {
        self.lock()
            .canned
            .insert(format!("{method} /v2/{path}"), vec![answer]);
    }

    /// Serves `answers` in order for `method` `path`.
    pub fn on_seq(&self, method: &str, path: &str, answers: Vec<Canned>) {
        self.lock()
            .canned
            .insert(format!("{method} /v2/{path}"), answers);
    }

    /// Requests other than to the token endpoint.
    pub fn api_log(&self) -> Vec<Logged> {
        self.lock()
            .log
            .iter()
            .filter(|l| !l.path.ends_with("/token_endpoint"))
            .cloned()
            .collect()
    }

    /// The last request to `method` `path`.
    pub fn last(&self, method: &str, path: &str) -> Logged {
        let full = format!("/v2/{path}");
        self.lock()
            .log
            .iter()
            .rev()
            .find(|l| l.method == method && l.path == full)
            .cloned()
            .unwrap_or_else(|| panic!("no {method} {full} was made"))
    }

    fn defaults(&self) {
        let user = self.res("users/7");
        self.on(
            "GET",
            "users/me",
            Canned::ok(json!({"user": {"url": user, "first_name": "Ann", "last_name": "Lee", "email": "ann@example.com", "permission_level": 8}})),
        );
        self.on(
            "GET",
            "company",
            Canned::ok(json!({"company": {"url": self.res("company"), "name": "Lee Consulting Ltd", "subdomain": "leeconsulting", "type": "UkLimitedCompany", "currency": "GBP", "sales_tax_registration_status": "Registered"}})),
        );
    }
}

fn basic_ok(headers: &HeaderMap) -> bool {
    let want = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(want.as_str())
}

fn json_response(status: u16, body: &Value, headers: &[(&'static str, String)]) -> Response {
    let mut res = (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    )
        .into_response();
    for (k, v) in headers {
        res.headers_mut().insert(*k, v.parse().unwrap());
    }
    res
}

async fn handle(
    State(state): State<Arc<Mutex<FakeState>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let query: Vec<(String, String)> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let form: Vec<(String, String)> = if headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"))
    {
        url::form_urlencoded::parse(&body).into_owned().collect()
    } else {
        Vec::new()
    };
    let json_body: Option<Value> = serde_json::from_slice(&body).ok();
    let mut st = state.lock().unwrap();
    st.log.push(Logged {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        api_version: headers
            .get("x-api-version")
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        body: json_body.clone(),
        form: form.clone(),
    });
    if headers.get("user-agent").is_none() {
        return json_response(
            400,
            &json!({"errors": [{"message": "A user-agent must be specified"}]}),
            &[],
        );
    }

    if path == "/v2/token_endpoint" {
        let f = |k: &str| form.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let body_ok = f("client_id").as_deref() == Some(CLIENT_ID)
            && f("client_secret").as_deref() == Some(CLIENT_SECRET);
        let client_ok = if st.body_auth_only {
            body_ok && headers.get("authorization").is_none()
        } else {
            basic_ok(&headers)
        };
        if !client_ok {
            return (StatusCode::UNAUTHORIZED, "HTTP Basic: Access denied.\n").into_response();
        }
        let expires_in = st.expires_in;
        match f("grant_type").as_deref() {
            Some("authorization_code") => {
                if f("code").as_deref() != Some(CODE) || st.codes_used > 0 {
                    return json_response(400, &json!({"error": "invalid_grant"}), &[]);
                }
                st.codes_used += 1;
                let access = "access-0".to_string();
                st.access.push(access.clone());
                return json_response(
                    200,
                    &json!({"access_token": access, "token_type": "bearer", "expires_in": expires_in, "refresh_token": REFRESH, "refresh_token_expires_in": 631151957}),
                    &[],
                );
            }
            Some("refresh_token") => {
                let given = f("refresh_token").unwrap_or_default();
                if !st.refresh.contains(&given) {
                    return json_response(400, &json!({"error": "invalid_grant"}), &[]);
                }
                st.refreshes += 1;
                let n = st.refreshes;
                let access = format!("access-{n}");
                st.access.push(access.clone());
                let refresh = if st.rotate {
                    let new = format!("refresh-{n}");
                    st.refresh.retain(|r| r != &given);
                    st.refresh.push(new.clone());
                    new
                } else {
                    given
                };
                return json_response(
                    200,
                    &json!({"access_token": access, "token_type": "bearer", "expires_in": expires_in, "refresh_token": refresh, "refresh_token_expires_in": 631151957}),
                    &[],
                );
            }
            _ => return json_response(400, &json!({"error": "unsupported_grant_type"}), &[]),
        }
    }

    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    if !st.access.contains(&token) {
        return json_response(
            401,
            &json!({"errors": {"error": {"message": "Access token not recognised"}}}),
            &[],
        );
    }
    let key = format!("{method} {path}");
    if let Some(answers) = st.canned.get_mut(&key) {
        let a = if answers.len() > 1 {
            answers.remove(0)
        } else {
            answers[0].clone()
        };
        return json_response(a.status, &a.body, &a.headers);
    }
    // Writes echo their body back by default, as FreeAgent returns what it created.
    if method != Method::GET {
        let mut echoed = json_body.unwrap_or(Value::Null);
        if let Some(obj) = echoed.as_object_mut()
            && let Some((_, inner)) = obj.iter_mut().next()
            && let Some(inner) = inner.as_object_mut()
        {
            inner.insert("url".into(), Value::from(format!("http://fake{path}/99")));
        }
        let status = if method == Method::POST { 201 } else { 200 };
        return json_response(status, &echoed, &[]);
    }
    json_response(404, &json!({"errors": [{"message": "not found"}]}), &[])
}
