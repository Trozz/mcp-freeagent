//! FreeAgent accounting as MCP tools and prompts: the books of the company whose FreeAgent user
//! signed in, read and written through the FreeAgent API v2.

pub mod api;
pub mod config;
pub mod error;
pub mod oauth;
pub mod prompts;
pub mod service;
pub mod tools;
pub mod uploads;
pub mod verifier;
pub mod view;

pub use service::{BooksService, from_env};

pub const VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
