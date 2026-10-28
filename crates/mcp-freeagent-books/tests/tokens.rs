//! FreeAgent access tokens: refreshed from the sealed refresh token, cached, refreshed again on a
//! 401, rotated refresh tokens remembered across restarts, and revoked sign-ins reported.
mod support;

use mcp_freeagent_books::oauth::{TokenError, TokenStore};
use mcp_server_core::credential::Credential;
use reqwest::Method;
use support::fake_freeagent::{CODE, Canned, FakeFreeAgent, REFRESH};
use support::tools::client_with;

fn me() -> Credential {
    Credential::new(REFRESH)
}

#[tokio::test]
async fn the_code_exchange_sends_the_app_credentials_and_redirect_uri() {
    let fake = FakeFreeAgent::start().await;
    let c = client_with(&fake, None);
    let grant = c
        .tokens()
        .app()
        .exchange(CODE, "http://localhost:7771/callback")
        .await
        .unwrap();
    assert_eq!(
        secrecy::ExposeSecret::expose_secret(grant.refresh_token.as_ref().unwrap()),
        REFRESH
    );
    let log = fake.last("POST", "token_endpoint");
    assert!(log.authorization.unwrap().starts_with("Basic "));
    assert!(log.form.contains(&(
        "redirect_uri".into(),
        "http://localhost:7771/callback".into()
    )));
    assert!(
        log.form
            .contains(&("grant_type".into(), "authorization_code".into()))
    );
    // Codes are single use.
    let again = c
        .tokens()
        .app()
        .exchange(CODE, "http://localhost:7771/callback")
        .await;
    assert!(matches!(again, Err(TokenError::Invalid(_))));
}

#[tokio::test]
async fn a_wrong_app_secret_is_the_servers_problem_not_the_persons() {
    let fake = FakeFreeAgent::start().await;
    let mut s = mcp_freeagent_books::api::ApiSettings::new(fake.url.clone());
    s.retry_delay = std::time::Duration::ZERO;
    let http = mcp_freeagent_books::api::http_client(&s).unwrap();
    let app = mcp_freeagent_books::oauth::OAuthApp::new(
        http,
        fake.url.clone(),
        "test-client-id".into(),
        "wrong".to_string().into(),
    );
    assert_eq!(
        app.exchange(CODE, "x").await.err(),
        Some(TokenError::BadClient)
    );
}

#[tokio::test]
async fn access_tokens_are_refreshed_once_and_reused() {
    let fake = FakeFreeAgent::start().await;
    let c = client_with(&fake, None);
    for _ in 0..3 {
        c.get(&me(), "company", &["company"], &[]).await.unwrap();
    }
    assert_eq!(fake.lock().refreshes, 1);
    let calls = fake.api_log();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|l| l.authorization.as_deref() == Some("Bearer access-1"))
    );
}

#[tokio::test]
async fn concurrent_calls_share_one_refresh() {
    let fake = FakeFreeAgent::start().await;
    let c = client_with(&fake, None);
    let calls = (0..8).map(|_| {
        let c = c.clone();
        tokio::spawn(async move { c.get(&me(), "company", &["company"], &[]).await.map(|_| ()) })
    });
    for h in calls {
        h.await.unwrap().unwrap();
    }
    assert_eq!(fake.lock().refreshes, 1);
}

#[tokio::test]
async fn expired_access_tokens_are_refreshed() {
    let fake = FakeFreeAgent::start().await;
    // An access token that is already within the expiry margin.
    fake.lock().expires_in = 60;
    let c = client_with(&fake, None);
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    assert_eq!(fake.lock().refreshes, 2);
}

#[tokio::test]
async fn a_revoked_access_token_is_refreshed_and_the_request_sent_again() {
    let fake = FakeFreeAgent::start().await;
    let c = client_with(&fake, None);
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    fake.lock().access.clear();
    let body = serde_json::json!({"contact": {"organisation_name": "Acme"}});
    c.send(
        &me(),
        Method::POST,
        "create_contact",
        &["contacts"],
        &[],
        Some(&body),
    )
    .await
    .unwrap();
    assert_eq!(fake.lock().refreshes, 2);
    let creates: Vec<_> = fake
        .api_log()
        .into_iter()
        .filter(|l| l.method == "POST")
        .collect();
    assert_eq!(
        creates.len(),
        2,
        "the refused write is sent once more, after the refresh"
    );
}

#[tokio::test]
async fn a_revoked_sign_in_asks_the_person_to_sign_in_again() {
    let fake = FakeFreeAgent::start().await;
    fake.lock().refresh.clear();
    let c = client_with(&fake, None);
    let e = c
        .get(&me(), "company", &["company"], &[])
        .await
        .unwrap_err();
    assert_eq!(e.kind(), "auth");
    assert!(e.to_string().contains("Sign in to FreeAgent again"));
    assert!(
        fake.api_log().is_empty(),
        "nothing is sent without an access token"
    );
}

#[tokio::test]
async fn rotated_refresh_tokens_are_used_and_survive_a_restart() {
    let fake = FakeFreeAgent::start().await;
    fake.lock().rotate = true;
    fake.lock().expires_in = 60;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tokens.json");

    let c = client_with(&fake, Some(TokenStore::open(path.clone()).unwrap()));
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    // refresh-0 was replaced by refresh-1, then refresh-2; only the latest works.
    assert_eq!(fake.lock().refresh, vec!["refresh-2".to_string()]);

    // A restart: a new client reading the same store still reaches FreeAgent with the sealed
    // (original) refresh token.
    let restarted = client_with(&fake, Some(TokenStore::open(path.clone()).unwrap()));
    restarted
        .get(&me(), "company", &["company"], &[])
        .await
        .unwrap();
    assert_eq!(fake.lock().refresh, vec!["refresh-3".to_string()]);

    // Without the store, the original token no longer works.
    let forgetful = client_with(&fake, None);
    assert_eq!(
        forgetful
            .get(&me(), "company", &["company"], &[])
            .await
            .unwrap_err()
            .kind(),
        "auth"
    );
}

#[tokio::test]
async fn api_errors_map_to_kinds() {
    let fake = FakeFreeAgent::start().await;
    let c = client_with(&fake, None);
    fake.on(
        "GET",
        "invoices/1",
        Canned::status(
            404,
            serde_json::json!({"errors": [{"message": "not found"}]}),
        ),
    );
    fake.on(
        "GET",
        "vat_returns",
        Canned::status(
            403,
            serde_json::json!({"errors": {"error": {"message": "Access denied"}}}),
        ),
    );
    fake.on(
        "GET",
        "bills",
        Canned::status(429, serde_json::Value::Null).header("retry-after", "42"),
    );
    fake.on_seq(
        "GET",
        "projects",
        vec![
            Canned::status(503, serde_json::Value::Null),
            Canned::ok(serde_json::json!({"projects": []})),
        ],
    );
    assert_eq!(
        c.get(&me(), "x", &["invoices", "1"], &[])
            .await
            .unwrap_err()
            .kind(),
        "not_found"
    );
    let e = c.get(&me(), "x", &["vat_returns"], &[]).await.unwrap_err();
    assert_eq!(e.kind(), "permission");
    assert!(e.to_string().contains("Access denied"), "{e}");
    let e = c.get(&me(), "x", &["bills"], &[]).await.unwrap_err();
    assert!(e.to_string().contains("42 seconds"), "{e}");
    // A 5xx read is retried once.
    c.get(&me(), "x", &["projects"], &[]).await.unwrap();
}

#[tokio::test]
async fn the_api_version_header_is_sent_when_configured() {
    let fake = FakeFreeAgent::start().await;
    let mut s = mcp_freeagent_books::api::ApiSettings::new(fake.url.clone());
    s.api_version = Some("2026-09-01".into());
    let http = mcp_freeagent_books::api::http_client(&s).unwrap();
    let app = std::sync::Arc::new(mcp_freeagent_books::oauth::OAuthApp::new(
        http.clone(),
        fake.url.clone(),
        "test-client-id".into(),
        "test-client-secret".to_string().into(),
    ));
    let c = mcp_freeagent_books::api::FaClient::new(
        http,
        s,
        mcp_freeagent_books::oauth::Tokens::new(app, None),
    );
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    assert_eq!(
        fake.last("GET", "company").api_version.as_deref(),
        Some("2026-09-01")
    );
}

#[tokio::test]
async fn apps_refused_as_http_basic_fall_back_to_body_credentials() {
    let fake = FakeFreeAgent::start().await;
    fake.lock().body_auth_only = true;
    let c = client_with(&fake, None);
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    let tries: Vec<_> = fake
        .lock()
        .log
        .iter()
        .filter(|l| l.path.ends_with("token_endpoint"))
        .cloned()
        .collect();
    assert_eq!(tries.len(), 2, "Basic, then the body");
    assert!(tries[1].form.iter().any(|(k, _)| k == "client_secret"));
    // Once found, the body is used straight away.
    fake.lock().access.clear();
    c.get(&me(), "company", &["company"], &[]).await.unwrap();
    let tries = fake
        .lock()
        .log
        .iter()
        .filter(|l| l.path.ends_with("token_endpoint"))
        .count();
    assert_eq!(tries, 3);
}
