//! The mcp-freeagent server: FreeAgent tools and prompts on one
//! [`mcp_server_core::app::Server`], each person signing in through FreeAgent's own OAuth.

use mcp_server_core::app::{Server, SignIn};
pub use mcp_server_core::combined::COMBINED;
use mcp_server_core::service::ServiceFactory;

pub const VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub const SERVICE_NAME: &str = "mcp-freeagent";
/// Tracing target prefix: also matches `mcp_freeagent_books` and every other `mcp_freeagent_*`
/// crate.
pub const CRATE_TARGET: &str = "mcp_freeagent";

/// Every service this binary can run, in mount order. Adding a service: a library crate
/// implementing `mcp_server_core::service::Service`, a dependency in Cargo.toml and one line here.
pub const SERVICES: &[(&str, ServiceFactory)] = &[("freeagent", mcp_freeagent_books::from_env)];

/// The server: `FREEAGENT_SERVICES`, `FREEAGENT_COMBINED` and `FREEAGENT_READ_ONLY` choose what
/// runs, and people sign in with their own FreeAgent account through FreeAgent's OAuth, set up by
/// the FreeAgent service.
pub fn server() -> Server {
    Server::new(SERVICE_NAME, VERSION)
        .env_prefix("FREEAGENT")
        .crate_target(CRATE_TARGET)
        .services(SERVICES)
        .sign_in(SignIn::ServiceCredential)
        .icon(
            include_bytes!("../assets/favicon.png"),
            include_str!("../assets/favicon.svg"),
        )
        .docs_url("https://github.com/Trozz/mcp-freeagent#configuration")
}
