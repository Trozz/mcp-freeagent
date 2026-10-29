use super::fake_freeagent::{CLIENT_ID, CLIENT_SECRET, FakeFreeAgent, REFRESH};
use axum::http::request::Parts;
use mcp_freeagent_books::api::{ApiSettings, FaClient, http_client};
use mcp_freeagent_books::oauth::{OAuthApp, TokenStore, Tokens};
use mcp_freeagent_books::tools::BooksServer;
use mcp_server_core::credential::Credential;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;

/// A client of the fake, with no delay between a failure and its retry.
pub fn client_with(fake: &FakeFreeAgent, store: Option<TokenStore>) -> Arc<FaClient> {
    let mut s = ApiSettings::new(fake.url.clone());
    s.retry_delay = Duration::ZERO;
    let http = http_client(&s).unwrap();
    let app = Arc::new(OAuthApp::new(
        http.clone(),
        fake.url.clone(),
        CLIENT_ID.into(),
        CLIENT_SECRET.to_string().into(),
    ));
    Arc::new(FaClient::new(http, s, Tokens::new(app, store)))
}

pub fn client(fake: &FakeFreeAgent) -> Arc<FaClient> {
    client_with(fake, None)
}

pub fn server(fake: &FakeFreeAgent) -> BooksServer {
    BooksServer::new(client(fake)).with_public_url("https://books.example.com")
}

/// The request details a tool sees for a person signed in with the fake's refresh token.
pub fn auth() -> Extension<Parts> {
    auth_with(REFRESH)
}

pub fn auth_with(refresh: &str) -> Extension<Parts> {
    let (mut parts, ()) = axum::http::Request::new(()).into_parts();
    parts.extensions.insert(Credential::new(refresh));
    Extension(parts)
}

pub fn no_auth() -> Extension<Parts> {
    let (parts, ()) = axum::http::Request::new(()).into_parts();
    Extension(parts)
}

pub fn params<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("valid tool arguments"))
}

pub fn is_error(r: &CallToolResult) -> bool {
    r.is_error == Some(true)
}

pub fn text(r: &CallToolResult) -> String {
    r.content[0]
        .as_text()
        .expect("first content block is text")
        .text
        .clone()
}

pub fn json(r: &CallToolResult) -> serde_json::Value {
    assert!(!is_error(r), "tool error: {}", text(r));
    serde_json::from_str(&text(r)).expect("tool result is JSON")
}

pub fn error_text(r: &CallToolResult) -> String {
    assert!(is_error(r), "expected a tool error, got: {}", text(r));
    text(r)
}
