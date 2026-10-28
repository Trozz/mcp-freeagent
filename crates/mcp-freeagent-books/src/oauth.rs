//! FreeAgent's OAuth: the app's code exchange and refresh at `token_endpoint`, and each signed-in
//! person's access tokens.
//!
//! People sign in through FreeAgent (see [`crate::verifier`]); the credential sealed into their
//! MCP tokens is their FreeAgent **refresh token**. Tools turn it into an access token here:
//! access tokens (one hour) are cached in memory per person and refreshed when they run out, one
//! refresh at a time per person.
//!
//! FreeAgent may return a new refresh token on each refresh, and does not document whether the
//! old one keeps working. The latest one is therefore remembered for every sign-in, in memory and,
//! when `FREEAGENT_TOKEN_STORE` is set, in a file, so a restart does not lose it. Each entry is
//! encrypted under a key derived from the person's original refresh token, which only their own
//! MCP tokens carry: the file is useless without them.

use crate::error::FaError;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mcp_server_core::credential::Credential;
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

/// An access token is refreshed this long before FreeAgent says it expires.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(120);
/// Sign-ins whose access tokens are remembered at once; idle ones are dropped first.
const MAX_SLOTS: usize = 10_000;
/// Store entries older than this are dropped: an MCP grant lasts 90 days.
const STORE_ENTRY_TTL_SECS: u64 = 100 * 24 * 3600;
const MAX_ERROR_DETAIL: usize = 200;

/// The FreeAgent OAuth app this server signs people in with.
pub struct OAuthApp {
    http: reqwest::Client,
    api_url: Url,
    client_id: String,
    client_secret: SecretString,
    /// Whether FreeAgent refused the app's credentials as HTTP Basic and accepted them in the
    /// form body instead: then they are sent in the body from then on.
    body_auth: AtomicBool,
}

impl std::fmt::Debug for OAuthApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthApp")
            .field("api_url", &self.api_url.as_str())
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// What `token_endpoint` granted.
pub struct TokenGrant {
    pub access_token: SecretString,
    pub expires_in: Duration,
    /// Absent when FreeAgent keeps the refresh token as it was.
    pub refresh_token: Option<SecretString>,
}

/// Why `token_endpoint` granted nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    /// The code or refresh token is not (or no longer) valid, with FreeAgent's reason.
    Invalid(String),
    /// FreeAgent refused the app's own credentials (`FREEAGENT_CLIENT_ID`/`SECRET`).
    BadClient,
    RateLimited(Option<u64>),
    Unavailable(String),
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

impl OAuthApp {
    pub fn new(
        http: reqwest::Client,
        api_url: Url,
        client_id: String,
        client_secret: SecretString,
    ) -> Self {
        OAuthApp {
            http,
            api_url,
            client_id,
            client_secret,
            body_auth: AtomicBool::new(false),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn endpoint(&self, name: &str) -> Url {
        let mut url = self.api_url.clone();
        url.path_segments_mut()
            .expect("an http(s) URL has path segments")
            .pop_if_empty()
            .push(name);
        url
    }

    /// FreeAgent's page where the person signs in and approves this app.
    pub fn approve_url(&self, redirect_uri: &str, state: &str) -> String {
        let mut url = self.endpoint("approve_app");
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state);
        url.into()
    }

    /// Exchanges an authorization code.
    pub async fn exchange(&self, code: &str, redirect_uri: &str) -> Result<TokenGrant, TokenError> {
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }

    /// A new access token for `refresh_token`.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenGrant, TokenError> {
        self.token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenGrant, TokenError> {
        let grant: &'static str = if form[0].1 == "refresh_token" {
            "refresh_token"
        } else {
            "authorization_code"
        };
        // FreeAgent documents HTTP Basic client authentication; some apps are only accepted with
        // the credentials in the body. Basic is tried first, and a Basic refusal once with the
        // body, which is then kept.
        let body_first = self.body_auth.load(Ordering::Relaxed);
        let first = self.token_attempt(grant, form, body_first).await;
        if body_first || !matches!(first, Err(TokenError::BadClient)) {
            return first;
        }
        let second = self.token_attempt(grant, form, true).await;
        match &second {
            Ok(_) => {
                if !self.body_auth.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "FreeAgent refused this app's credentials as HTTP Basic but accepted them in the request body; sending them in the body from now on"
                    );
                }
            }
            Err(TokenError::BadClient) => {}
            Err(_) => tracing::warn!(
                "FreeAgent refused this app's credentials as HTTP Basic (check FREEAGENT_CLIENT_ID and FREEAGENT_CLIENT_SECRET); sent them in the request body instead"
            ),
        }
        second
    }

    async fn token_attempt(
        &self,
        grant: &'static str,
        form: &[(&str, &str)],
        in_body: bool,
    ) -> Result<TokenGrant, TokenError> {
        let started = Instant::now();
        let request = self
            .http
            .post(self.endpoint("token_endpoint"))
            .header(reqwest::header::ACCEPT, "application/json");
        let request = if in_body {
            let mut all: Vec<(&str, &str)> = form.to_vec();
            all.push(("client_id", &self.client_id));
            all.push(("client_secret", self.client_secret.expose_secret()));
            request.form(&all)
        } else {
            request
                .basic_auth(&self.client_id, Some(self.client_secret.expose_secret()))
                .form(form)
        };
        let result = request.send().await;
        let status = result.as_ref().ok().map(|r| r.status().as_u16());
        metrics::counter!("freeagent_token_requests_total", "grant" => grant, "status" => status.map(|s| s.to_string()).unwrap_or_else(|| "error".into()))
            .increment(1);
        tracing::debug!(
            grant,
            status,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "freeagent token request"
        );
        let response = result.map_err(|e| {
            TokenError::Unavailable(if e.is_timeout() {
                "FreeAgent did not answer in time".into()
            } else {
                "FreeAgent could not be reached".into()
            })
        })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            let body: TokenBody = serde_json::from_str(&text).map_err(|_| {
                TokenError::Unavailable("FreeAgent's token answer was not understood".into())
            })?;
            if body.access_token.is_empty() {
                return Err(TokenError::Unavailable(
                    "FreeAgent granted no access token".into(),
                ));
            }
            return Ok(TokenGrant {
                access_token: body.access_token.into(),
                expires_in: Duration::from_secs(body.expires_in.unwrap_or(3600)),
                refresh_token: body
                    .refresh_token
                    .filter(|t| !t.is_empty())
                    .map(SecretString::from),
            });
        }
        match status.as_u16() {
            429 => Err(TokenError::RateLimited(retry_after)),
            // A bad client answers 401 with an HTML body; a bad code or token 400 invalid_grant.
            401 => Err(TokenError::BadClient),
            400 | 403 => {
                let detail = token_error_detail(&text);
                if detail.as_deref() == Some("invalid_client") {
                    Err(TokenError::BadClient)
                } else {
                    Err(TokenError::Invalid(
                        detail.unwrap_or_else(|| "invalid_grant".into()),
                    ))
                }
            }
            code => Err(TokenError::Unavailable(format!(
                "FreeAgent answered HTTP {code}"
            ))),
        }
    }
}

/// `error` (and `error_description`) of an OAuth error body.
fn token_error_detail(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = v.get("error")?.as_str()?;
    let mut out = error.to_string();
    if let Some(d) = v.get("error_description").and_then(|d| d.as_str()) {
        out.push_str(": ");
        out.push_str(d);
    }
    Some(out.chars().take(MAX_ERROR_DETAIL).collect())
}

/// One sign-in's tokens.
struct Slot {
    /// The latest refresh token: the sealed one, or what FreeAgent replaced it with.
    refresh: SecretString,
    access: Option<(SecretString, Instant)>,
}

/// One sign-in's tokens, locked while they are refreshed.
type SharedSlot = Arc<tokio::sync::Mutex<Slot>>;

/// Every signed-in person's access tokens, keyed by the fingerprint of the refresh token sealed
/// into their MCP tokens.
pub struct Tokens {
    app: Arc<OAuthApp>,
    slots: Mutex<HashMap<[u8; 32], (SharedSlot, Instant)>>,
    store: Option<TokenStore>,
}

impl Tokens {
    pub fn new(app: Arc<OAuthApp>, store: Option<TokenStore>) -> Self {
        Tokens {
            app,
            slots: Mutex::new(HashMap::new()),
            store,
        }
    }

    pub fn app(&self) -> &OAuthApp {
        &self.app
    }

    fn slot(&self, credential: &Credential) -> SharedSlot {
        let fp = credential.fingerprint();
        let now = Instant::now();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((slot, used)) = slots.get_mut(&fp) {
            *used = now;
            return slot.clone();
        }
        if slots.len() >= MAX_SLOTS {
            // Drop the least recently used half; their access tokens are fetched again if needed.
            let mut by_age: Vec<([u8; 32], Instant)> =
                slots.iter().map(|(k, (_, used))| (*k, *used)).collect();
            by_age.sort_by_key(|(_, used)| *used);
            for (k, _) in by_age.into_iter().take(MAX_SLOTS / 2) {
                slots.remove(&k);
            }
        }
        let refresh = self
            .store
            .as_ref()
            .and_then(|s| s.latest(credential.expose()))
            .unwrap_or_else(|| credential.expose().to_string().into());
        let slot = Arc::new(tokio::sync::Mutex::new(Slot {
            refresh,
            access: None,
        }));
        slots.insert(fp, (slot.clone(), now));
        slot
    }

    /// Remembers the access token FreeAgent granted with `refresh_token` at sign-in, so the first
    /// tool call does not refresh.
    pub fn seed(&self, refresh_token: &str, grant: &TokenGrant) {
        let credential = Credential::new(refresh_token);
        let slot = self.slot(&credential);
        if let Ok(mut s) = slot.try_lock() {
            s.access = Some((
                grant.access_token.clone(),
                Instant::now() + grant.expires_in.saturating_sub(EXPIRY_MARGIN),
            ));
        };
    }

    /// An access token for the person whose sealed refresh token is `credential`.
    pub async fn access_token(&self, credential: &Credential) -> Result<SecretString, FaError> {
        let slot = self.slot(credential);
        let mut s = slot.lock().await;
        if let Some((token, until)) = &s.access
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        match self.app.refresh(s.refresh.expose_secret()).await {
            Ok(grant) => {
                if let Some(new) = grant.refresh_token.as_ref()
                    && new.expose_secret() != s.refresh.expose_secret()
                {
                    metrics::counter!("freeagent_refresh_token_rotations_total").increment(1);
                    if let Some(store) = &self.store {
                        store.put(credential.expose(), new.expose_secret());
                    }
                    s.refresh = new.clone();
                }
                let token = grant.access_token.clone();
                s.access = Some((
                    grant.access_token,
                    Instant::now() + grant.expires_in.saturating_sub(EXPIRY_MARGIN),
                ));
                Ok(token)
            }
            Err(TokenError::Invalid(_)) => {
                drop(s);
                self.forget(credential);
                Err(FaError::Auth)
            }
            Err(TokenError::BadClient) => {
                tracing::error!(
                    "FreeAgent refused this server's OAuth app: check FREEAGENT_CLIENT_ID and FREEAGENT_CLIENT_SECRET"
                );
                Err(FaError::Internal(
                    "FreeAgent refused this server's OAuth app credentials; the server's operator needs to check FREEAGENT_CLIENT_ID and FREEAGENT_CLIENT_SECRET".into(),
                ))
            }
            Err(TokenError::RateLimited(retry_after_secs)) => {
                Err(FaError::RateLimited { retry_after_secs })
            }
            Err(TokenError::Unavailable(why)) => Err(FaError::Upstream(format!(
                "refreshing the FreeAgent access token failed: {why}"
            ))),
        }
    }

    /// Forgets `token` after FreeAgent refused it, so the next call refreshes.
    pub async fn invalidate(&self, credential: &Credential, token: &SecretString) {
        let slot = self.slot(credential);
        let mut s = slot.lock().await;
        if s.access
            .as_ref()
            .is_some_and(|(t, _)| t.expose_secret() == token.expose_secret())
        {
            s.access = None;
        }
    }

    fn forget(&self, credential: &Credential) {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&credential.fingerprint());
        if let Some(store) = &self.store {
            store.remove(credential.expose());
        }
    }
}

/// The file behind `FREEAGENT_TOKEN_STORE`: for each sign-in whose refresh token FreeAgent
/// replaced, the latest one, encrypted (ChaCha20-Poly1305) under a key derived from the sign-in's
/// original refresh token and indexed by a hash of it.
pub struct TokenStore {
    path: PathBuf,
    entries: Mutex<HashMap<String, StoreEntry>>,
    rng: SystemRandom,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoreEntry {
    sealed: String,
    updated: u64,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    entries: HashMap<String, StoreEntry>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn store_key(original: &str) -> LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, b"mcp-freeagent token store")
        .extract(original.as_bytes());
    let okm = prk
        .expand(&[b"key v1"], &CHACHA20_POLY1305)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    LessSafeKey::new(UnboundKey::from(okm))
}

fn store_index(original: &str) -> String {
    let tag = ring::digest::digest(
        &ring::digest::SHA256,
        format!("mcp-freeagent token store index v1\0{original}").as_bytes(),
    );
    tag.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

const STORE_AAD: &[u8] = b"mcp-freeagent refresh token v1";

impl TokenStore {
    /// Opens the store at `path`, reading it if it exists. A file that cannot be read is an
    /// error; one that does not exist yet is created on the first write.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let entries = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: StoreFile = serde_json::from_slice(&bytes).map_err(|e| {
                    format!("{} is not a FreeAgent token store: {e}", path.display())
                })?;
                let cutoff = now_unix().saturating_sub(STORE_ENTRY_TTL_SECS);
                file.entries
                    .into_iter()
                    .filter(|(_, e)| e.updated >= cutoff)
                    .collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(TokenStore {
            path,
            entries: Mutex::new(entries),
            rng: SystemRandom::new(),
        })
    }

    /// The latest refresh token of the sign-in whose original one is `original`, if it changed.
    pub fn latest(&self, original: &str) -> Option<SecretString> {
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&store_index(original))
            .cloned()?;
        let raw = URL_SAFE_NO_PAD.decode(entry.sealed).ok()?;
        if raw.len() < NONCE_LEN {
            return None;
        }
        let (nonce, rest) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut data = rest.to_vec();
        let plain = store_key(original)
            .open_in_place(nonce, Aad::from(STORE_AAD), &mut data)
            .ok()?;
        std::str::from_utf8(plain)
            .ok()
            .map(|s| SecretString::from(s.to_string()))
    }

    /// Records `latest` as the refresh token of the sign-in whose original one is `original`.
    pub fn put(&self, original: &str, latest: &str) {
        let mut nonce = [0u8; NONCE_LEN];
        if self.rng.fill(&mut nonce).is_err() {
            return;
        }
        let mut data = latest.as_bytes().to_vec();
        if store_key(original)
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(STORE_AAD),
                &mut data,
            )
            .is_err()
        {
            return;
        }
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&data);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(
            store_index(original),
            StoreEntry {
                sealed: URL_SAFE_NO_PAD.encode(sealed),
                updated: now_unix(),
            },
        );
        self.write(&entries);
    }

    fn remove(&self, original: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.remove(&store_index(original)).is_some() {
            self.write(&entries);
        }
    }

    /// Writes the store atomically: a temporary file beside it, then a rename. Only the owner may
    /// read it.
    fn write(&self, entries: &HashMap<String, StoreEntry>) {
        let file = StoreFile {
            version: 1,
            entries: entries.clone(),
        };
        let Ok(json) = serde_json::to_vec(&file) else {
            return;
        };
        let tmp = self.path.with_extension("tmp");
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent()
                && !dir.as_os_str().is_empty()
            {
                std::fs::create_dir_all(dir)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&tmp)?;
            std::io::Write::write_all(&mut f, &json)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = result {
            metrics::counter!("freeagent_token_store_errors_total").increment(1);
            tracing::error!(path = %self.path.display(), error = %e, "writing the FreeAgent token store failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_round_trips_and_is_useless_without_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = TokenStore::open(path.clone()).unwrap();
        assert!(store.latest("original-1").is_none());
        store.put("original-1", "rotated-1");
        assert_eq!(
            store.latest("original-1").unwrap().expose_secret(),
            "rotated-1"
        );
        assert!(store.latest("original-2").is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("rotated-1") && !text.contains("original-1"),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A restart reads it back.
        let again = TokenStore::open(path.clone()).unwrap();
        assert_eq!(
            again.latest("original-1").unwrap().expose_secret(),
            "rotated-1"
        );
        again.remove("original-1");
        assert!(
            TokenStore::open(path)
                .unwrap()
                .latest("original-1")
                .is_none()
        );
    }

    #[test]
    fn a_corrupt_store_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(TokenStore::open(path).is_err());
    }

    #[test]
    fn oauth_error_bodies_are_read() {
        assert_eq!(
            token_error_detail(r#"{"error":"invalid_grant","error_description":"expired"}"#)
                .as_deref(),
            Some("invalid_grant: expired")
        );
        assert_eq!(token_error_detail("HTTP Basic: Access denied."), None);
    }

    #[test]
    fn the_approval_url_names_the_app_and_the_callback() {
        let app = OAuthApp::new(
            {
                mcp_server_core::install_crypto_provider();
                reqwest::Client::new()
            },
            Url::parse("https://api.freeagent.com/v2").unwrap(),
            "client-123".into(),
            "secret".to_string().into(),
        );
        let url = Url::parse(&app.approve_url("http://localhost:7771/callback", "st")).unwrap();
        assert_eq!(url.path(), "/v2/approve_app");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "client-123");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], "http://localhost:7771/callback");
        assert_eq!(q["state"], "st");
        assert!(!format!("{app:?}").contains("secret"));
    }
}
