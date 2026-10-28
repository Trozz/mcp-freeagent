#[derive(Debug, thiserror::Error)]
pub enum FaError {
    /// FreeAgent no longer accepts this sign-in: the refresh token was revoked, expired or
    /// replaced, or access to the app was withdrawn.
    #[error(
        "FreeAgent no longer accepts this connection's sign-in (it may have been revoked or expired). Sign in to FreeAgent again: reconnect the connector, or run /mcp in Claude Code"
    )]
    Auth,
    /// Signed in, but the FreeAgent user's access level does not allow this.
    #[error("{0}")]
    Permission(String),
    #[error("this request carried no FreeAgent sign-in; connect again")]
    MissingCredential,
    /// A complete message: tool errors are already prefixed with the kind, `not_found: `.
    #[error("{0}")]
    NotFound(String),
    #[error("{}", rate_limited_message(*.retry_after_secs))]
    RateLimited {
        /// Seconds FreeAgent asked us to wait, when it said.
        retry_after_secs: Option<u64>,
    },
    #[error("{0}")]
    Validation(String),
    #[error("FreeAgent error: {0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

fn rate_limited_message(retry_after_secs: Option<u64>) -> String {
    let base = "the FreeAgent API rate limit (120 requests per minute and 3600 per hour per user) is used up";
    match retry_after_secs {
        Some(s) => format!("{base}; try again in {s} seconds"),
        None => format!("{base}; try again in a minute"),
    }
}

impl FaError {
    pub fn kind(&self) -> &'static str {
        match self {
            FaError::Auth | FaError::MissingCredential => "auth",
            FaError::Permission(_) => "permission",
            FaError::NotFound(_) => "not_found",
            FaError::RateLimited { .. } => "rate_limited",
            FaError::Validation(_) => "validation",
            FaError::Upstream(_) => "upstream",
            FaError::Internal(_) => "internal",
        }
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        FaError::Validation(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, FaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        for (e, kind) in [
            (FaError::Auth, "auth"),
            (FaError::MissingCredential, "auth"),
            (FaError::Permission("x".into()), "permission"),
            (FaError::NotFound("x".into()), "not_found"),
            (
                FaError::RateLimited {
                    retry_after_secs: None,
                },
                "rate_limited",
            ),
            (FaError::invalid("x"), "validation"),
            (FaError::Upstream("x".into()), "upstream"),
            (FaError::Internal("x".into()), "internal"),
        ] {
            assert_eq!(e.kind(), kind);
        }
    }

    #[test]
    fn messages_say_what_to_do() {
        assert!(
            FaError::Auth
                .to_string()
                .contains("Sign in to FreeAgent again")
        );
        let m = FaError::RateLimited {
            retry_after_secs: Some(30),
        }
        .to_string();
        assert!(m.contains("120 requests per minute"), "{m}");
        assert!(m.contains("30 seconds"), "{m}");
    }
}
