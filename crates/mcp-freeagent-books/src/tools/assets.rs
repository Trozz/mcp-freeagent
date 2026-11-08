//! Capital assets: the fixed asset register, its asset types, and a check for purchases that look
//! like assets but were booked as everyday costs.

use super::checks::{accounting_years, parse as parse_date, today, ymd};
use super::{
    BooksServer, Caller, Query, choice, date_range, id_arg, json_result, opt_money, paging, push,
    run,
};
use crate::error;
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Purchases at or above this, including VAT, are checked by default.
const DEFAULT_THRESHOLD_PENCE: i64 = 50_000;
/// Most bills or expenses read for the check.
const CHECK_ITEMS: usize = 2_000;
/// Most items listed in one section; the count covers them all.
const LISTED: usize = 25;

/// Words in a description or category name that suggest equipment the business keeps.
const ASSET_WORDS: &[&str] = &[
    "laptop",
    "laptops",
    "macbook",
    "imac",
    "computer",
    "computers",
    "pc",
    "desktop",
    "monitor",
    "monitors",
    "server",
    "ipad",
    "tablet",
    "iphone",
    "phone",
    "smartphone",
    "camera",
    "cameras",
    "lens",
    "lenses",
    "drone",
    "printer",
    "projector",
    "furniture",
    "desk",
    "desks",
    "chair",
    "chairs",
    "cabinet",
    "vehicle",
    "car",
    "van",
    "motorbike",
    "machinery",
    "machine",
    "equipment",
    "hardware",
    "tools",
];

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListAssetsParams {
    /// all (including disposed), disposed, or disposable (still held and can be sold or written off). Default: FreeAgent's, the assets still held.
    pub view: Option<String>,
    /// Include each asset's history: purchase, depreciation, capital allowances and disposal.
    pub include_history: Option<bool>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct GetAssetParams {
    /// The asset's id, or the URL a tool returned.
    pub id: String,
    /// Include its history (purchase, depreciation, capital allowances, disposal). Default: true.
    pub include_history: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AssetCheckParams {
    /// Start of the period, YYYY-MM-DD. Default: the start of the current accounting year.
    pub from_date: Option<String>,
    /// End of the period, YYYY-MM-DD; with from_date. Default: today.
    pub to_date: Option<String>,
    /// Smallest purchase to check, including VAT, e.g. "500". Default: 500.
    pub threshold: Option<String>,
}

/// The words of `s`, lower case.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The asset words found in `texts`.
fn asset_words(texts: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = texts
        .iter()
        .flat_map(|t| words(t))
        .filter(|w| ASSET_WORDS.contains(&w.as_str()))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The nominal code of a category reference (`.../categories/602-1` is `602-1`).
fn code_of(category: Option<&Value>) -> Option<String> {
    category
        .and_then(Value::as_str)
        .map(|c| view::id_of(c).unwrap_or(c).to_string())
}

/// Whether nominal code `code` is a capital asset purchase: FreeAgent keeps those in the 600s
/// of its balance sheet categories (e.g. 602 Computer Equipment Purchase).
fn is_capital_code(code: &str) -> bool {
    code.split('-')
        .next()
        .and_then(|b| b.parse::<u32>().ok())
        .is_some_and(|n| (600..700).contains(&n))
}

impl BooksServer {
    async fn asset_check(&self, c: &Caller, p: &AssetCheckParams) -> error::Result<Value> {
        let (from, to) = date_range(p.from_date.as_deref(), p.to_date.as_deref())?;
        let threshold = match opt_money("threshold", p.threshold.as_deref())? {
            Some(t) => pence(Some(&Value::from(t)))
                .unwrap_or(DEFAULT_THRESHOLD_PENCE)
                .abs(),
            None => DEFAULT_THRESHOLD_PENCE,
        };
        let company = c.one("company", &["company"], "company", &[]).await?;
        let now = today();
        let (from, to) = match (from, to) {
            (Some(f), Some(t)) => (f, t),
            (Some(f), None) => (f, ymd(now)),
            (None, t) => {
                let end = t.as_deref().and_then(parse_date).unwrap_or(now);
                let start = accounting_years(&company)
                    .into_iter()
                    .find(|(s, e)| *s <= end && end <= *e)
                    .map(|(s, _)| s)
                    .unwrap_or_else(|| end - chrono::Duration::days(364));
                (ymd(start), ymd(end))
            }
        };
        let period: Query = vec![("from_date", from.clone()), ("to_date", to.clone())];
        let mut out = Map::new();
        out.insert(
            "period".into(),
            json!({"from": from, "to": to, "threshold": money(threshold)}),
        );
        let mut todo: Vec<String> = Vec::new();
        let mut unavailable = Map::new();

        // The register.
        match c
            .all(
                "capital_assets",
                &["capital_assets"],
                "capital_assets",
                vec![("view", "all".into())],
                CHECK_ITEMS,
            )
            .await
        {
            Ok((assets, _)) => {
                let held: Vec<&Value> = assets
                    .iter()
                    .filter(|a| a.get("disposed_on").and_then(Value::as_str).is_none())
                    .collect();
                let undepreciated: Vec<Value> = held
                    .iter()
                    .filter(|a| {
                        let method = a
                            .pointer("/depreciation_profile/method")
                            .and_then(Value::as_str);
                        let years = a
                            .get("asset_life_years")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                        method == Some("no_depreciation") || (method.is_none() && years == 0)
                    })
                    .map(|a| {
                        view::prune(json!({
                            "id": a.get("url").and_then(Value::as_str).and_then(view::id_of),
                            "description": a.get("description"),
                            "asset_type": a.get("asset_type"),
                            "purchased_on": a.get("purchased_on"),
                            "depreciation_profile": a.get("depreciation_profile"),
                        }))
                        .unwrap_or_default()
                    })
                    .collect();
                if !undepreciated.is_empty() {
                    todo.push(format!(
                        "Check {} asset(s) with no depreciation: right for land, wrong for most equipment",
                        undepreciated.len()
                    ));
                }
                out.insert(
                    "register".into(),
                    json!({
                        "held": held.len(),
                        "disposed": assets.len() - held.len(),
                        "without_depreciation": undepreciated,
                    }),
                );
            }
            Err(e) => {
                unavailable.insert("register".into(), Value::from(format!("{}: {e}", e.kind())));
            }
        }

        // Purchases in the period that look like equipment but went to an everyday category.
        let cats = self.category_index(c).await.unwrap_or_default();
        let category_name = |code: &str| -> String {
            let base = code.split('-').next().unwrap_or(code);
            cats.get(code)
                .or_else(|| cats.get(base))
                .and_then(|(cat, _)| cat.get("description").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string()
        };
        let mut candidates: Vec<(i64, Value)> = Vec::new();
        let mut consider =
            |kind: &str, item: &Value, line: &Value, value: Option<i64>, description: &str| {
                let Some(value) = value.map(i64::abs).filter(|v| *v >= threshold) else {
                    return;
                };
                if line.get("capital_asset").is_some_and(|v| !v.is_null()) {
                    return;
                }
                let Some(code) = code_of(line.get("category")) else {
                    return;
                };
                if is_capital_code(&code) {
                    return;
                }
                let name = category_name(&code);
                let found = asset_words(&[description, &name]);
                if found.is_empty() {
                    return;
                }
                candidates.push((
                    value,
                    view::prune(json!({
                        "kind": kind,
                        "id": item.get("url").and_then(Value::as_str).and_then(view::id_of),
                        "dated_on": item.get("dated_on"),
                        "contact_name": item.get("contact_name"),
                        "description": description,
                        "category": code,
                        "category_name": name,
                        "value": money(value),
                        "because": found.join(", "),
                    }))
                    .unwrap_or_default(),
                ));
            };
        let mut bill_query = period.clone();
        bill_query.push(("nested_bill_items", "true".into()));
        match c
            .all("bills", &["bills"], "bills", bill_query, CHECK_ITEMS)
            .await
        {
            Ok((bills, _)) => {
                for b in &bills {
                    match b.get("bill_items").and_then(Value::as_array) {
                        Some(lines) if !lines.is_empty() => {
                            for l in lines {
                                let d = l
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let v = pence(l.get("total_value"))
                                    .or_else(|| pence(l.get("gross_value")));
                                consider("bill", b, l, v, d);
                            }
                        }
                        _ => {
                            let d = b
                                .get("comments")
                                .or_else(|| b.get("description"))
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            consider("bill", b, b, pence(b.get("total_value")), d);
                        }
                    }
                }
            }
            Err(e) => {
                unavailable.insert("bills".into(), Value::from(format!("{}: {e}", e.kind())));
            }
        }
        match c
            .all("expenses", &["expenses"], "expenses", period, CHECK_ITEMS)
            .await
        {
            Ok((expenses, _)) => {
                for e in expenses.iter().filter(|e| e.get("mileage").is_none()) {
                    let d = e
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    consider("expense", e, e, pence(e.get("gross_value")), d);
                }
            }
            Err(e) => {
                unavailable.insert("expenses".into(), Value::from(format!("{}: {e}", e.kind())));
            }
        }
        candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
        if !candidates.is_empty() {
            todo.push(format!(
                "Decide whether {} purchase(s) of {} or more are capital assets",
                candidates.len(),
                money(threshold)
            ));
        }
        out.insert(
            "possible_assets".into(),
            json!({
                "count": candidates.len(),
                "items": candidates.into_iter().take(LISTED).map(|(_, v)| v).collect::<Vec<_>>(),
            }),
        );
        out.insert(
            "ready".into(),
            Value::from(todo.is_empty() && unavailable.is_empty()),
        );
        out.insert("to_do".into(), Value::from(todo));
        if !unavailable.is_empty() {
            out.insert("unavailable".into(), Value::Object(unavailable));
        }
        out.insert(
            "note".into(),
            Value::from(
                "Possible assets are found by words such as laptop, camera or furniture in the description or category, so expect some that are everyday costs. Equipment kept and used for more than a year is usually a capital asset: book it to a capital asset category (600s) with a depreciation profile, so it goes on the register and the right capital allowances are claimed. Check with your accountant before re-categorising.",
            ),
        );
        Ok(Value::Object(out))
    }
}

#[tool_router(router = assets_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "The fixed asset register: capital assets with description, asset type, purchase and disposal dates and depreciation profile; with include_history, each one's purchase, depreciation, capital allowance and disposal events. Assets are added by booking a bill, expense or bank payment to a capital asset category.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_capital_assets(
        &self,
        Parameters(p): Parameters<ListAssetsParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_capital_assets", async {
            let c = self.caller(&parts)?;
            let pg = paging(p.page, p.per_page)?;
            let mut q: Query = Vec::new();
            push(
                &mut q,
                "view",
                choice(
                    "view",
                    p.view.as_deref(),
                    &["all", "disposed", "disposable"],
                )?
                .map(String::from),
            );
            if p.include_history == Some(true) {
                q.push(("include_history", "true".into()));
            }
            Ok(json_result(
                &c.list(
                    "capital_assets",
                    &["capital_assets"],
                    "capital_assets",
                    q,
                    pg,
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "One capital asset in full: description, type, purchase and disposal dates, depreciation profile, and (by default) its history of purchase, depreciation, capital allowances and disposal with links to the bill, expense or bank explanation behind each.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn get_capital_asset(
        &self,
        Parameters(p): Parameters<GetAssetParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("get_capital_asset", async {
            let c = self.caller(&parts)?;
            let id = id_arg("id", &p.id)?;
            let mut q: Query = Vec::new();
            if p.include_history != Some(false) {
                q.push(("include_history", "true".into()));
            }
            Ok(json_result(
                &c.one(
                    "capital_asset",
                    &["capital_assets", &id],
                    "capital_asset",
                    &q,
                )
                .await?,
            ))
        })
        .await
    }

    #[tool(
        description = "The capital asset types: FreeAgent's own (Computer Equipment, Fixtures and Fittings, Motor Vehicles, Other Capital Asset) and any the company added. Needs Full Access.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn list_capital_asset_types(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("list_capital_asset_types", async {
            let c = self.caller(&parts)?;
            let (items, more) = c
                .all(
                    "capital_asset_types",
                    &["capital_asset_types"],
                    "capital_asset_types",
                    Vec::new(),
                    500,
                )
                .await?;
            let mut out = json!({"capital_asset_types": items.iter().map(view::resource).collect::<Vec<_>>()});
            if more {
                out["truncated"] = Value::from(true);
            }
            Ok(json_result(&out))
        })
        .await
    }

    #[tool(
        description = "Check the fixed asset register for a period (default: this accounting year): bills and expenses of 500 or more (threshold) that look like equipment (laptops, cameras, furniture, vehicles...) but were booked to an everyday category instead of a capital asset one, and assets held with no depreciation. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn asset_register_check(
        &self,
        Parameters(p): Parameters<AssetCheckParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("asset_register_check", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.asset_check(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equipment_is_found_by_whole_words() {
        assert_eq!(asset_words(&["MacBook Pro 14\"", ""]), vec!["macbook"]);
        assert_eq!(
            asset_words(&["Sony camera and lens", "Equipment"]),
            vec!["camera", "equipment", "lens"]
        );
        // "card", "careful" and "telephone" are not "car" or "phone".
        assert!(asset_words(&["Business card printing, careful", "Telephone"]).is_empty());
    }

    #[test]
    fn capital_codes_are_the_600s() {
        assert!(is_capital_code("602"));
        assert!(is_capital_code("602-1"));
        assert!(!is_capital_code("285"));
        assert!(!is_capital_code("750-1"));
        assert_eq!(
            code_of(Some(&json!(
                "https://api.freeagent.com/v2/categories/602-1"
            )))
            .as_deref(),
            Some("602-1")
        );
    }
}
