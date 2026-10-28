//! The whole server end to end, built as the binary builds it: a client registers, the person
//! signs in through (fake) FreeAgent's OAuth, and the MCP endpoints then act with their FreeAgent
//! sign-in.
mod support;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use mcp_server_core::app::{Server, SignIn};
use mcp_server_core::service::{HttpSettings, ServiceContext, ServiceEnv};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use support::fake_freeagent::{CLIENT_ID, CLIENT_SECRET, CODE, FakeFreeAgent};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const PUBLIC: &str = "https://books.example.com";
const HOST: &str = "books.example.com";
const CLIENT_REDIRECT: &str = "https://claude.ai/api/mcp/auth_callback";
const VERIFIER: &str = "dBjftJeZ4CVP-mJ92K9fwhzDHvM5HDkyX5Dw1mEtK8k";

async fn app(fake: &FakeFreeAgent) -> axum::Router {
    let api = fake.url.to_string();
    let vars: HashMap<String, String> = [
        ("PUBLIC_URL", PUBLIC),
        ("OAUTH_SIGNING_KEY", "0123456789abcdef0123456789abcdef"),
        ("INSECURE_HTTP", "true"),
        ("FREEAGENT_CLIENT_ID", CLIENT_ID),
        ("FREEAGENT_CLIENT_SECRET", CLIENT_SECRET),
        ("FREEAGENT_API_URL", api.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let mut env = ServiceEnv::new(Box::new(move |k: &str| vars.get(k).cloned()));
    let server = Server::new("mcp-freeagent", "test")
        .env_prefix("FREEAGENT")
        .service("freeagent", mcp_freeagent_books::from_env)
        .sign_in(SignIn::ServiceCredential);
    let loaded = server
        .load(&mut env)
        .unwrap_or_else(|| panic!("config errors: {:?}", env.errors));
    let ctx = ServiceContext {
        http: HttpSettings::from_core(&loaded.core),
        shutdown: CancellationToken::new(),
    };
    server.router(&loaded, &ctx)
}

async fn send(app: &axum::Router, req: Request<Body>) -> Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

async fn body_text(res: Response<Body>) -> String {
    String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

fn param(url: &url::Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

fn form(uri: &str, pairs: &[(&str, &str)]) -> Request<Body> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    Request::post(uri)
        .header("host", HOST)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

/// Signs in through FreeAgent for `resource` and returns an access token.
async fn sign_in(app: &axum::Router, fake: &FakeFreeAgent, resource: &str) -> String {
    let res = send(
        app,
        Request::post("/register")
            .header("host", HOST)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"redirect_uris": [CLIENT_REDIRECT], "client_name": "Claude"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    let reg: Value = serde_json::from_str(&body_text(res).await).unwrap();
    let cid = reg["client_id"].as_str().unwrap().to_string();
    let challenge = mcp_server_core::pkce::s256(VERIFIER);
    let pairs = [
        ("response_type", "code"),
        ("client_id", cid.as_str()),
        ("redirect_uri", CLIENT_REDIRECT),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", "st"),
        ("resource", resource),
    ];
    let q = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let html = body_text(
        send(
            app,
            Request::get(format!("/authorize?{q}"))
                .header("host", HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    assert!(html.contains("Continue to FreeAgent"), "{html}");
    let at = html.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    let csrf = html[at..at + html[at..].find('"').unwrap()].to_string();
    let mut f: Vec<(&str, &str)> = pairs.to_vec();
    f.push(("scope", resource.rsplit('/').next().unwrap()));
    f.push(("csrf", &csrf));
    let res = send(app, form("/authorize", &f)).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let to = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // The person is sent to FreeAgent's approval page for this server's app.
    assert_eq!(to.path(), "/v2/approve_app");
    assert_eq!(param(&to, "client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(
        param(&to, "redirect_uri").as_deref(),
        Some("https://books.example.com/callback")
    );
    let state = param(&to, "state").unwrap();

    // FreeAgent sends them back with a code.
    let cb = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([("code", CODE), ("state", state.as_str())])
        .finish();
    let res = send(
        app,
        Request::get(format!("/callback?{cb}"))
            .header("host", HOST)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "{}",
        body_text(res).await
    );
    let back = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    assert!(back.as_str().starts_with(CLIENT_REDIRECT));
    let code = param(&back, "code").unwrap();
    // The exchange told FreeAgent the same redirect URI.
    let ex = fake.last("POST", "token_endpoint");
    assert!(ex.form.contains(&(
        "redirect_uri".into(),
        "https://books.example.com/callback".into()
    )));

    let res = send(
        app,
        form(
            "/token",
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &cid),
                ("code_verifier", VERIFIER),
                ("redirect_uri", CLIENT_REDIRECT),
            ],
        ),
    )
    .await;
    let tok: Value = serde_json::from_str(&body_text(res).await).unwrap();
    tok["access_token"]
        .as_str()
        .expect("an access token")
        .to_string()
}

fn rpc(path: &str, token: &str, body: Value, session: Option<&str>) -> Request<Body> {
    let mut b = Request::post(path)
        .header("host", HOST)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(s) = session {
        b = b
            .header("mcp-session-id", s)
            .header("mcp-protocol-version", "2025-06-18");
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// The first JSON-RPC message of an SSE (or JSON) response.
async fn message(res: Response<Body>) -> Value {
    let mut body = res.into_body();
    let mut buf = String::new();
    loop {
        if let Ok(v) = serde_json::from_str::<Value>(buf.trim()) {
            return v;
        }
        for line in buf.lines() {
            if let Some(data) = line.strip_prefix("data:")
                && let Ok(v) = serde_json::from_str::<Value>(data.trim())
                && v.get("jsonrpc").is_some()
            {
                return v;
            }
        }
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("a response within 5 s")
            .expect("a message before the body ends")
            .unwrap();
        if let Ok(d) = frame.into_data() {
            buf.push_str(&String::from_utf8_lossy(&d));
        }
    }
}

async fn session(app: &axum::Router, path: &str, token: &str) -> String {
    let res = send(
        app,
        rpc(
            path,
            token,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}}),
            None,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let id = res.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_string();
    let init = message(res).await;
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("FreeAgent")
    );
    let res = send(
        app,
        rpc(
            path,
            token,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            Some(&id),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    id
}

#[tokio::test]
async fn people_sign_in_through_freeagent_and_tools_act_as_them() {
    let fake = FakeFreeAgent::start().await;
    let app = app(&fake).await;
    let token = sign_in(&app, &fake, "https://books.example.com/freeagent").await;
    let sid = session(&app, "/freeagent", &token).await;

    let list = message(
        send(
            &app,
            rpc(
                "/freeagent",
                &token,
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
                Some(&sid),
            ),
        )
        .await,
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"create_invoice") && names.contains(&"explain_bank_transaction"),
        "{names:?}"
    );

    let call = message(
        send(
            &app,
            rpc("/freeagent", &token, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_company", "arguments": {}}}), Some(&sid)),
        )
        .await,
    )
    .await;
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Lee Consulting Ltd"), "{call}");
    // The tool used the access token granted at sign-in: no refresh was needed.
    assert_eq!(fake.lock().refreshes, 0);
    assert_eq!(
        fake.last("GET", "company").authorization.as_deref(),
        Some("Bearer access-0")
    );

    let prompts = message(
        send(
            &app,
            rpc(
                "/freeagent",
                &token,
                json!({"jsonrpc": "2.0", "id": 4, "method": "prompts/list"}),
                Some(&sid),
            ),
        )
        .await,
    )
    .await;
    assert!(prompts["result"]["prompts"].as_array().unwrap().len() >= 12);
    let p = message(
        send(
            &app,
            rpc(
                "/freeagent",
                &token,
                json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/get", "params": {"name": "monthly_bookkeeping", "arguments": {"month": "2026-09"}}}),
                Some(&sid),
            ),
        )
        .await,
    )
    .await;
    let t = p["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(
        t.contains("<month>2026-09</month>") && t.contains("`list_bank_transactions`"),
        "{t}"
    );
}

#[tokio::test]
async fn the_combined_endpoint_prefixes_tools_and_prompts() {
    let fake = FakeFreeAgent::start().await;
    let app = app(&fake).await;
    let token = sign_in(&app, &fake, "https://books.example.com/mcp").await;
    let sid = session(&app, "/mcp", &token).await;
    let list = message(
        send(
            &app,
            rpc(
                "/mcp",
                &token,
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
                Some(&sid),
            ),
        )
        .await,
    )
    .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"freeagent_list_invoices"), "{names:?}");
    let p = message(
        send(
            &app,
            rpc("/mcp", &token, json!({"jsonrpc": "2.0", "id": 3, "method": "prompts/get", "params": {"name": "freeagent_chase_overdue", "arguments": {}}}), Some(&sid)),
        )
        .await,
    )
    .await;
    let t = p["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(t.contains("`freeagent_send_document_email`"), "{t}");
}

#[tokio::test]
async fn without_a_sign_in_the_endpoints_point_at_the_oauth_metadata() {
    let fake = FakeFreeAgent::start().await;
    let app = app(&fake).await;
    let res = send(&app, rpc("/freeagent", "not-a-token", json!({}), None)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let h = res.headers()["www-authenticate"].to_str().unwrap();
    assert!(
        h.contains("/.well-known/oauth-protected-resource/freeagent"),
        "{h}"
    );
    // A FreeAgent token is not accepted in place of signing in.
    let res = send(&app, rpc("/freeagent", "access-0", json!({}), None)).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn declining_at_freeagent_tells_the_client() {
    let fake = FakeFreeAgent::start().await;
    let app = app(&fake).await;
    // Start a sign-in, then come back as FreeAgent does when the person declines.
    let res = send(
        &app,
        Request::post("/register")
            .header("host", HOST)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"redirect_uris": [CLIENT_REDIRECT]}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    let reg: Value = serde_json::from_str(&body_text(res).await).unwrap();
    let cid = reg["client_id"].as_str().unwrap().to_string();
    let challenge = mcp_server_core::pkce::s256(VERIFIER);
    let pairs = [
        ("response_type", "code"),
        ("client_id", cid.as_str()),
        ("redirect_uri", CLIENT_REDIRECT),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", "st"),
        ("resource", "https://books.example.com/freeagent"),
    ];
    let q = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let html = body_text(
        send(
            &app,
            Request::get(format!("/authorize?{q}"))
                .header("host", HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await,
    )
    .await;
    let at = html.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
    let csrf = html[at..at + html[at..].find('"').unwrap()].to_string();
    let mut f: Vec<(&str, &str)> = pairs.to_vec();
    f.push(("scope", "freeagent"));
    f.push(("csrf", &csrf));
    let res = send(&app, form("/authorize", &f)).await;
    let to = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let state = param(&to, "state").unwrap();
    let res = send(
        &app,
        Request::get(format!("/callback?error=access_denied&state={state}"))
            .header("host", HOST)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let back = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    assert_eq!(param(&back, "error").as_deref(), Some("access_denied"));
    assert_eq!(fake.lock().codes_used, 0);
}
