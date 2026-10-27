//! The FreeAgent service's own settings: the OAuth app it signs people in with and which FreeAgent
//! it talks to. The server binary owns the core configuration (URL, OAuth signing key, TLS,
//! logging).

use mcp_server_core::config::EnvReader;
use secrecy::SecretString;
use std::path::PathBuf;
use url::Url;

/// MCP `serverInfo.name` of the FreeAgent service.
pub const SERVICE_NAME: &str = "mcp-freeagent-books";
/// The live FreeAgent API.
pub const PRODUCTION_API_URL: &str = "https://api.freeagent.com/v2";
/// FreeAgent's sandbox, for testing with a sandbox company.
pub const SANDBOX_API_URL: &str = "https://api.sandbox.freeagent.com/v2";

pub const CLIENT_ID_VAR: &str = "FREEAGENT_CLIENT_ID";
pub const CLIENT_SECRET_VAR: &str = "FREEAGENT_CLIENT_SECRET";
pub const ENVIRONMENT_VAR: &str = "FREEAGENT_ENVIRONMENT";
pub const API_URL_VAR: &str = "FREEAGENT_API_URL";
pub const API_VERSION_VAR: &str = "FREEAGENT_API_VERSION";
pub const TOKEN_STORE_VAR: &str = "FREEAGENT_TOKEN_STORE";

pub struct BooksConfig {
    /// The OAuth identifier of the app in the FreeAgent Developer Dashboard.
    pub client_id: String,
    /// The app's OAuth secret.
    pub client_secret: SecretString,
    /// The API base, ending in `/v2`: `approve_app` and `token_endpoint` are below it too.
    pub api_url: Url,
    /// `X-Api-Version` to send, a date such as 2026-09-01, when set.
    pub api_version: Option<String>,
    /// Where the latest refresh token of each sign-in is kept, encrypted, when set.
    pub token_store: Option<PathBuf>,
}

impl BooksConfig {
    pub fn from_env<F: Fn(&str) -> Option<String>>(env: &mut EnvReader<F>) -> Option<Self> {
        let errors_before = env.errors.len();
        let client_id = env.required(CLIENT_ID_VAR);
        if let Some(id) = &client_id
            && !plausible_client_id(id)
        {
            env.errors.push(format!(
                "{CLIENT_ID_VAR} must be the OAuth identifier from the FreeAgent Developer Dashboard (letters, digits, - and _)"
            ));
        }
        let client_secret = env.secret_required(CLIENT_SECRET_VAR);
        let environment = env.optional(ENVIRONMENT_VAR);
        let default_url = match environment.as_deref().map(str::trim) {
            None | Some("") | Some("production") | Some("live") => PRODUCTION_API_URL,
            Some("sandbox") => SANDBOX_API_URL,
            Some(other) => {
                env.errors.push(format!(
                    "{ENVIRONMENT_VAR} must be production or sandbox, got {other:?}"
                ));
                PRODUCTION_API_URL
            }
        };
        let raw_url = env
            .optional(API_URL_VAR)
            .unwrap_or_else(|| default_url.to_string());
        let api_url = match parse_url(API_URL_VAR, &raw_url) {
            Ok(u) => Some(u),
            Err(msg) => {
                env.errors.push(msg);
                None
            }
        };
        let api_version = env
            .optional(API_VERSION_VAR)
            .filter(|v| !v.trim().is_empty());
        if let Some(v) = &api_version
            && chrono::NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d").is_err()
        {
            env.errors.push(format!(
                "{API_VERSION_VAR} must be a date as YYYY-MM-DD, such as 2026-09-01"
            ));
        }
        let token_store = env
            .optional(TOKEN_STORE_VAR)
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from);
        if env.errors.len() != errors_before {
            return None;
        }
        Some(BooksConfig {
            client_id: client_id?,
            client_secret: client_secret?,
            api_url: api_url?,
            api_version: api_version.map(|v| v.trim().to_string()),
            token_store,
        })
    }
}

fn plausible_client_id(id: &str) -> bool {
    (8..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// An API base from variable `name`: https, or http to a loopback address; no credentials, query
/// or fragment. A trailing slash is removed.
pub fn parse_url(name: &str, raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw.trim()).map_err(|e| format!("{name} is not a valid URL: {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{name} must not contain credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{name} must not contain a query or fragment"));
    }
    let ok = (url.scheme() == "https" && url.host_str().is_some())
        || (url.scheme() == "http" && is_loopback(&url));
    if !ok {
        return Err(format!(
            "{name} must use https (http is only allowed for loopback addresses)"
        ));
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url)
}

pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    const APP: &[(&str, &str)] = &[
        (CLIENT_ID_VAR, "test-client-id"),
        (CLIENT_SECRET_VAR, "test-client-secret"),
    ];

    fn load(pairs: &[(&str, &str)]) -> (Option<BooksConfig>, Vec<String>) {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let mut env = EnvReader::new(move |k: &str| map.get(k).cloned());
        let c = BooksConfig::from_env(&mut env);
        (c, env.errors)
    }

    #[test]
    fn defaults_to_the_live_api() {
        let (c, errors) = load(APP);
        assert!(errors.is_empty(), "{errors:?}");
        let c = c.unwrap();
        assert_eq!(c.api_url.as_str(), "https://api.freeagent.com/v2");
        assert_eq!(c.client_id, "test-client-id");
        assert_eq!(c.client_secret.expose_secret(), "test-client-secret");
        assert!(c.api_version.is_none());
        assert!(c.token_store.is_none());
    }

    #[test]
    fn the_sandbox_and_overrides() {
        let (c, errors) = load(&[APP, &[(ENVIRONMENT_VAR, "sandbox")]].concat());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            c.unwrap().api_url.as_str(),
            "https://api.sandbox.freeagent.com/v2"
        );
        let (c, errors) = load(
            &[
                APP,
                &[
                    (API_URL_VAR, "http://127.0.0.1:9999/v2/"),
                    (API_VERSION_VAR, "2026-09-01"),
                    (TOKEN_STORE_VAR, "/data/tokens.json"),
                ],
            ]
            .concat(),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let c = c.unwrap();
        assert_eq!(c.api_url.as_str(), "http://127.0.0.1:9999/v2");
        assert_eq!(c.api_version.as_deref(), Some("2026-09-01"));
        assert_eq!(c.token_store, Some(PathBuf::from("/data/tokens.json")));
    }

    #[test]
    fn every_problem_is_reported() {
        let (c, errors) = load(&[
            (CLIENT_ID_VAR, "bad id!"),
            (ENVIRONMENT_VAR, "staging"),
            (API_URL_VAR, "http://api.freeagent.com/v2"),
            (API_VERSION_VAR, "latest"),
        ]);
        assert!(c.is_none());
        let joined = errors.join("\n");
        for needle in [
            CLIENT_ID_VAR,
            "FREEAGENT_CLIENT_SECRET is required",
            "production or sandbox",
            "must use https",
            "YYYY-MM-DD",
        ] {
            assert!(joined.contains(needle), "{needle}: {joined}");
        }
    }

    #[test]
    fn reads_only_its_own_variables() {
        let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = read.clone();
        let mut env = EnvReader::new(move |k: &str| {
            seen.lock().unwrap().push(k.to_string());
            None
        });
        let _ = BooksConfig::from_env(&mut env);
        for k in read.lock().unwrap().iter() {
            assert!(k.starts_with("FREEAGENT_"), "read {k}");
            assert_ne!(k, "FREEAGENT_SERVICES");
        }
    }
}
