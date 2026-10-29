use crate::api::{ApiSettings, FaClient, http_client};
use crate::config::BooksConfig;
use crate::oauth::{OAuthApp, TokenStore, Tokens};
use crate::tools::BooksServer;
use mcp_server_core::combined::DynHandler;
use mcp_server_core::credential::CredentialVerifier;
use mcp_server_core::service::{Service, ServiceContext, ServiceEnv, mcp_service};
use std::sync::Arc;

/// The FreeAgent service, mounted by the server at `/freeagent`.
pub struct BooksService {
    client: Arc<FaClient>,
    /// `PUBLIC_URL`, learnt when the service is mounted, for upload links.
    public_url: std::sync::OnceLock<String>,
}

impl BooksService {
    pub fn new(client: Arc<FaClient>) -> Self {
        Self {
            client,
            public_url: std::sync::OnceLock::new(),
        }
    }

    fn server(&self) -> BooksServer {
        let server = BooksServer::new(self.client.clone());
        match self.public_url.get() {
            Some(url) => server.with_public_url(url.clone()),
            None => server,
        }
    }
}

impl Service for BooksService {
    fn name(&self) -> &'static str {
        "freeagent"
    }

    fn tool_names(&self) -> Vec<String> {
        self.server().tool_names()
    }

    fn router(self: Arc<Self>, ctx: &ServiceContext) -> axum::Router {
        let _ = self.public_url.set(ctx.http.public_url.clone());
        tracing::info!(
            api = %self.client.base(),
            client_id = %self.client.tokens().app().client_id(),
            "freeagent settings"
        );
        mcp_service(ctx, move || Ok(self.server()))
    }

    fn credential_verifier(&self) -> Option<Arc<dyn CredentialVerifier>> {
        Some(crate::verifier::verifier(self.client.clone()))
    }

    fn handler(&self) -> DynHandler {
        DynHandler::new(self.server())
    }
}

/// Builds the FreeAgent client from its parts: the OAuth app, the token cache and the API.
pub fn client(cfg: BooksConfig, store: Option<TokenStore>) -> Result<FaClient, String> {
    let mut settings = ApiSettings::new(cfg.api_url.clone());
    settings.api_version = cfg.api_version;
    let http = http_client(&settings).map_err(|e| e.to_string())?;
    let app = Arc::new(OAuthApp::new(
        http.clone(),
        cfg.api_url,
        cfg.client_id,
        cfg.client_secret,
    ));
    Ok(FaClient::new(http, settings, Tokens::new(app, store)))
}

/// Reads the FreeAgent configuration and builds the service. Errors go to `env.errors`.
pub fn from_env(env: &mut ServiceEnv) -> Option<Arc<dyn Service>> {
    let mut cfg = BooksConfig::from_env(env)?;
    let store = match cfg.token_store.take() {
        Some(path) => match TokenStore::open(path) {
            Ok(s) => Some(s),
            Err(e) => {
                env.errors
                    .push(format!("{}: {e}", crate::config::TOKEN_STORE_VAR));
                return None;
            }
        },
        None => None,
    };
    match client(cfg, store) {
        Ok(c) => Some(Arc::new(BooksService::new(Arc::new(c)))),
        Err(e) => {
            env.errors
                .push(format!("FreeAgent client setup failed: {e}"));
            None
        }
    }
}
