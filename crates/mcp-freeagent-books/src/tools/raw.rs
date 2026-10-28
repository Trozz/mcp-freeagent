//! Any FreeAgent endpoint the other tools do not cover: one read-only tool and one for writes, so
//! a read-only server still allows the first and refuses the second.

use super::{BooksServer, Query, choice, json_result, run};
use crate::error::FaError;
use crate::view;
use axum::http::request::Parts;
use reqwest::Method;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Most query parameters accepted.
const MAX_QUERY: usize = 20;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ApiGetParams {
    /// The endpoint below /v2, e.g. capital_assets, stock_items/12, account_locks, cis_bands or accounting/balance_sheet/opening_balances.
    pub path: String,
    /// Query parameters, e.g. {"view": "all", "page": "2"}.
    pub query: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ApiRequestParams {
    /// POST, PUT or DELETE.
    pub method: String,
    /// The endpoint below /v2, e.g. credit_note_reconciliations or account_locks.
    pub path: String,
    pub query: Option<Map<String, Value>>,
    /// The JSON body FreeAgent's documentation gives for the endpoint, with its root key, e.g. {"price_list_item": {...}}.
    pub body: Option<Value>,
}

fn query_arg(q: Option<&Map<String, Value>>) -> crate::error::Result<Query> {
    let Some(q) = q else {
        return Ok(Vec::new());
    };
    if q.len() > MAX_QUERY {
        return Err(FaError::invalid(format!(
            "query is limited to {MAX_QUERY} parameters"
        )));
    }
    // Query names are static strings in requests: only FreeAgent's known parameter names, plus
    // any short lower-case name, which is leaked once (the set of names is small and bounded).
    q.iter()
        .map(|(k, v)| {
            let ok = !k.is_empty()
                && k.len() <= 40
                && k.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if !ok {
                return Err(FaError::invalid(format!(
                    "query parameter {k:?} is not a FreeAgent parameter name"
                )));
            }
            let v = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => {
                    return Err(FaError::invalid(format!(
                        "query parameter {k} must be a string, number or boolean"
                    )));
                }
            };
            Ok((static_name(k), v))
        })
        .collect()
}

/// A `'static` copy of a query parameter name, interned so each distinct name is allocated once.
fn static_name(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.get(name) {
        return n;
    }
    if names.len() >= 1000 {
        return "unknown";
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

#[tool_router(router = raw_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Read any FreeAgent API endpoint the other tools do not cover, such as capital_assets, stock_items, hire_purchases, properties, account_locks, cis_bands, sales_tax_periods, credit_note_reconciliations, payroll_profiles/2026 or accounting/trial_balance/summary/opening_balances. Returns FreeAgent's JSON with ids added.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn api_get(
        &self,
        Parameters(p): Parameters<ApiGetParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("api_get", async {
            let c = self.caller(&parts)?;
            let segs = self.client.raw_segments(&p.path)?;
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let q = query_arg(p.query.as_ref())?;
            let a = c.get("api_get", &segs, &q).await?;
            let mut out = json!({"body": view::resource(&a.body)});
            if let Some(t) = a.total {
                out["total"] = Value::from(t);
            }
            if a.has_next {
                out["has_next_page"] = Value::from(true);
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Create, change or delete through any FreeAgent API endpoint the other tools do not cover (POST, PUT or DELETE), with the body FreeAgent's API documentation gives. Prefer the specific tools. Show the person exactly what will change and get their agreement first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn api_request(
        &self,
        Parameters(p): Parameters<ApiRequestParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("api_request", async {
            let c = self.caller(&parts)?;
            let method = match choice("method", Some(&p.method), &["POST", "PUT", "DELETE"])? {
                Some("POST") => Method::POST,
                Some("PUT") => Method::PUT,
                _ => Method::DELETE,
            };
            let segs = self.client.raw_segments(&p.path)?;
            let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
            let q = query_arg(p.query.as_ref())?;
            if let Some(b) = &p.body
                && !b.is_object()
            {
                return Err(FaError::invalid("body must be a JSON object"));
            }
            let a = c
                .send(method, "api_request", &segs, &q, p.body.as_ref())
                .await?;
            let mut out = json!({"body": view::resource(&a.body)});
            if let Some(l) = a.location {
                out["location"] = Value::from(l);
            }
            Ok(json_result(&out))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_names_and_scalars() {
        let q: Map<String, Value> =
            serde_json::from_value(json!({"view": "all", "page": 2, "nested": true})).unwrap();
        let mut got = query_arg(Some(&q)).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("nested", "true".into()),
                ("page", "2".into()),
                ("view", "all".into())
            ]
        );
        for bad in [json!({"View": "x"}), json!({"a&b": "x"}), json!({"x": [1]})] {
            let m: Map<String, Value> = serde_json::from_value(bad).unwrap();
            assert!(query_arg(Some(&m)).is_err());
        }
        assert_eq!(static_name("view"), static_name("view"));
    }
}
