//! Signing people in through FreeAgent's own OAuth: the sign-in page sends them to FreeAgent to
//! approve this app, and their FreeAgent refresh token is sealed into their MCP tokens.

use crate::api::FaClient;
use crate::oauth::TokenError;
use mcp_server_core::credential::{Credential, CredentialVerifier};
use mcp_server_core::upstream::{
    Exchange, ExchangeFuture, UpstreamGrant, UpstreamSignIn, UpstreamVerifier,
};
use serde_json::Value;
use std::sync::Arc;

/// The label people see on the sign-in page. Part of the sign-in version: changing it signs
/// everyone out.
pub const LABEL: &str = "FreeAgent account";

pub struct FreeAgentSignIn {
    client: Arc<FaClient>,
}

impl FreeAgentSignIn {
    pub fn new(client: Arc<FaClient>) -> Self {
        FreeAgentSignIn { client }
    }

    /// Who signed in, from `/users/me` and `/company`: `freeagent:<company subdomain>:user:<id>`,
    /// stable across sign-ins. `None` when FreeAgent does not say.
    async fn subject(&self, credential: &Credential) -> Option<String> {
        let me = self
            .client
            .get(credential, "users_me", &["users", "me"], &[])
            .await
            .ok()?;
        let user_url = me.body.pointer("/user/url").and_then(Value::as_str)?;
        let user_id = user_url.rsplit('/').next().filter(|s| !s.is_empty())?;
        let company = self
            .client
            .get(credential, "company", &["company"], &[])
            .await
            .ok();
        let subdomain = company
            .as_ref()
            .and_then(|c| c.body.pointer("/company/subdomain"))
            .and_then(Value::as_str)
            .filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        let subject = match subdomain {
            Some(s) => format!("freeagent:{s}:user:{user_id}"),
            None => format!("freeagent:user:{user_id}"),
        };
        (subject.len() <= 120
            && subject
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_')))
        .then_some(subject)
    }
}

impl UpstreamSignIn for FreeAgentSignIn {
    fn provider(&self) -> &str {
        "FreeAgent"
    }

    fn authorize_url(&self, redirect_uri: &str, state: &str) -> String {
        self.client.tokens().app().approve_url(redirect_uri, state)
    }

    fn exchange<'a>(&'a self, code: &'a str, redirect_uri: &'a str) -> ExchangeFuture<'a> {
        Box::pin(async move {
            let grant = match self
                .client
                .tokens()
                .app()
                .exchange(code, redirect_uri)
                .await
            {
                Ok(g) => g,
                Err(e) => return verdict(e),
            };
            let Some(refresh) = grant.refresh_token.as_ref() else {
                return Exchange::Unavailable("FreeAgent granted no refresh token".into());
            };
            let refresh = secrecy::ExposeSecret::expose_secret(refresh).to_string();
            self.client.tokens().seed(&refresh, &grant);
            let credential = Credential::new(refresh.clone());
            let subject = self.subject(&credential).await;
            let out = UpstreamGrant::new(refresh);
            tracing::info!(
                subject = subject.as_deref().unwrap_or("unknown"),
                "signed in to FreeAgent"
            );
            Exchange::Granted(match subject {
                Some(s) => out.with_subject(s),
                None => out,
            })
        })
    }
}

/// How a failed code exchange is reported on the sign-in page.
fn verdict(e: TokenError) -> Exchange {
    match e {
        TokenError::Invalid(why) => Exchange::Refused(format!("{why}.")),
        TokenError::BadClient => {
            tracing::error!(
                "FreeAgent refused this server's OAuth app: check FREEAGENT_CLIENT_ID and FREEAGENT_CLIENT_SECRET"
            );
            Exchange::Unavailable(
                "FreeAgent refused this server's app credentials; ask the server's operator to check them".into(),
            )
        }
        TokenError::RateLimited(_) => {
            Exchange::Unavailable("FreeAgent is limiting requests; try again in a minute".into())
        }
        TokenError::Unavailable(why) => Exchange::Unavailable(why),
    }
}

/// The verifier the service offers: people sign in through FreeAgent. Switching FreeAgent
/// environment or OAuth app signs everyone out.
pub fn verifier(client: Arc<FaClient>) -> Arc<dyn CredentialVerifier> {
    let generation = format!(
        "{}\0{}",
        client.base().as_str(),
        client.tokens().app().client_id()
    );
    Arc::new(
        UpstreamVerifier::new(LABEL, FreeAgentSignIn::new(client))
            .with_help("Sign in with the FreeAgent login of the company whose books you want to use. Tools can do what your FreeAgent access level allows.")
            .with_generation(generation.into_bytes()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_codes_say_why_and_app_problems_blame_the_server() {
        assert!(matches!(
            verdict(TokenError::Invalid("invalid_grant".into())),
            Exchange::Refused(m) if m == "invalid_grant."
        ));
        assert!(matches!(
            verdict(TokenError::BadClient),
            Exchange::Unavailable(m) if m.contains("operator")
        ));
        assert!(matches!(
            verdict(TokenError::RateLimited(Some(5))),
            Exchange::Unavailable(_)
        ));
    }
}
