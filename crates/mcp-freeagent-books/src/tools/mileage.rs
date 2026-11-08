//! Business mileage against HMRC's approved mileage allowance payments (AMAP): 45p a mile for
//! the first 10,000 business miles in a tax year by car or van, 25p after, 24p by motorcycle and
//! 20p by bicycle. Paying more than that is a taxable benefit.

use super::checks::{today, ymd};
use super::{BooksServer, Caller, json_result, run};
use crate::error::{self, FaError};
use crate::view::{self, money, pence};
use axum::http::request::Parts;
use chrono::{Datelike, NaiveDate};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{ErrorData as McpError, tool, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Business miles a year at the higher car rate, in hundredths of a mile.
const CAR_THRESHOLD: i64 = 10_000 * 100;
/// Pence a mile.
const CAR_HIGHER: i64 = 45;
const CAR_LOWER: i64 = 25;
const MOTORCYCLE: i64 = 24;
const BICYCLE: i64 = 20;
/// Within this many miles of the threshold, say so.
const NEAR_MILES: i64 = 1_000;
/// Most expenses read for a tax year.
const MAX_EXPENSES: usize = 5_000;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MileageTrackerParams {
    /// The UK tax year (6 April to 5 April), e.g. 2026/27, or the year it starts, 2026. Default: the current one.
    pub tax_year: Option<String>,
    /// Only this person: their user id or URL, from list_users.
    pub user_id: Option<String>,
}

/// The tax year starting on 6 April of the year given as `2026/27`, `2026-27` or `2026`.
fn tax_year(raw: Option<&str>, today: NaiveDate) -> error::Result<i32> {
    let Some(t) = raw.map(str::trim).filter(|t| !t.is_empty()) else {
        let april6 = NaiveDate::from_ymd_opt(today.year(), 4, 6).unwrap_or(today);
        return Ok(if today >= april6 {
            today.year()
        } else {
            today.year() - 1
        });
    };
    let bad = || FaError::invalid(format!("tax_year must be like 2026/27 or 2026, got {t:?}"));
    let (start, end) = match t.split_once(['/', '-']) {
        Some((s, e)) => (s, Some(e)),
        None => (t, None),
    };
    let start: i32 = start.parse().map_err(|_| bad())?;
    if !(2000..=2100).contains(&start) {
        return Err(bad());
    }
    if let Some(e) = end {
        let e: i32 = e.parse().map_err(|_| bad())?;
        let ok = if e < 100 {
            e == (start + 1) % 100
        } else {
            e == start + 1
        };
        if !ok {
            return Err(bad());
        }
    }
    Ok(start)
}

/// One mileage claim, as the tracker reads it.
#[derive(Debug, Clone)]
struct Claim {
    id: Option<String>,
    dated_on: String,
    description: String,
    /// Hundredths of a mile.
    miles: i64,
    /// What was claimed back, in pence; `None` when the claim only rebills.
    claimed: Option<i64>,
}

/// What the claims of one vehicle type come to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Assessed {
    miles: i64,
    at_higher: i64,
    at_lower: i64,
    claimed: i64,
    allowance: i64,
    /// Claims paid above the approved rate: (claim, claimed, allowed).
    over: Vec<(usize, i64, i64)>,
    /// The date the car threshold was passed.
    passed_on: Option<String>,
}

/// Pence for `miles` hundredths at `rate` pence a mile, to the nearest penny.
fn at(miles: i64, rate: i64) -> i64 {
    (miles * rate + 50) / 100
}

/// Assesses claims (in date order) of one vehicle type against the approved rates.
fn assess(vehicle: &str, claims: &[Claim]) -> Assessed {
    let mut a = Assessed::default();
    for (n, c) in claims.iter().enumerate() {
        let allowed = match vehicle {
            "Motorcycle" => at(c.miles, MOTORCYCLE),
            "Bicycle" => at(c.miles, BICYCLE),
            _ => {
                let left = (CAR_THRESHOLD - a.miles).max(0);
                let higher = c.miles.min(left);
                let lower = c.miles - higher;
                a.at_higher += higher;
                a.at_lower += lower;
                if lower > 0 && a.passed_on.is_none() {
                    a.passed_on = Some(c.dated_on.clone());
                }
                at(higher, CAR_HIGHER) + at(lower, CAR_LOWER)
            }
        };
        a.miles += c.miles;
        if let Some(claimed) = c.claimed {
            a.claimed += claimed;
            a.allowance += allowed;
            if claimed > allowed + 1 {
                a.over.push((n, claimed, allowed));
            }
        }
    }
    a
}

/// Miles in hundredths as a decimal string.
fn miles(h: i64) -> String {
    let s = money(h);
    s.strip_suffix(".00").map(String::from).unwrap_or(s)
}

impl BooksServer {
    async fn mileage(&self, c: &Caller, p: &MileageTrackerParams) -> error::Result<Value> {
        let year = tax_year(p.tax_year.as_deref(), today())?;
        let from = NaiveDate::from_ymd_opt(year, 4, 6).unwrap_or_default();
        let to = NaiveDate::from_ymd_opt(year + 1, 4, 5).unwrap_or_default();
        let only = match p
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            Some(u) => Some(super::id_arg("user_id", u)?),
            None => None,
        };
        let (expenses, more) = c
            .all(
                "expenses",
                &["expenses"],
                "expenses",
                vec![("from_date", ymd(from)), ("to_date", ymd(to))],
                MAX_EXPENSES,
            )
            .await?;
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        if let Ok((users, _)) = c.all("users", &["users"], "users", Vec::new(), 500).await {
            for u in users {
                if let Some(id) = u.get("url").and_then(Value::as_str).and_then(view::id_of) {
                    names.insert(id.to_string(), view::contact_name(&u));
                }
            }
        }
        // user -> vehicle -> claims
        let mut by_user: BTreeMap<String, BTreeMap<String, Vec<Claim>>> = BTreeMap::new();
        for e in &expenses {
            let Some(m) = e
                .get("mileage")
                .filter(|v| !v.is_null())
                .and_then(|v| pence(Some(v)))
            else {
                continue;
            };
            let user = e
                .get("user")
                .and_then(Value::as_str)
                .and_then(view::id_of)
                .unwrap_or_default()
                .to_string();
            if only.as_ref().is_some_and(|o| *o != user) {
                continue;
            }
            let vehicle = e
                .get("vehicle_type")
                .and_then(Value::as_str)
                .filter(|v| matches!(*v, "Motorcycle" | "Bicycle"))
                .unwrap_or("Car")
                .to_string();
            let reclaimed = match e.get("reclaim_mileage") {
                Some(Value::Number(n)) => n.as_i64() != Some(0),
                Some(Value::String(s)) => s != "0",
                Some(Value::Bool(b)) => *b,
                _ => true,
            };
            let gross = pence(e.get("gross_value")).map(i64::abs).unwrap_or(0);
            by_user
                .entry(user)
                .or_default()
                .entry(vehicle)
                .or_default()
                .push(Claim {
                    id: e
                        .get("url")
                        .and_then(Value::as_str)
                        .and_then(view::id_of)
                        .map(String::from),
                    dated_on: e
                        .get("dated_on")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    description: e
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    miles: m.abs(),
                    claimed: (reclaimed && gross > 0).then_some(gross),
                });
        }

        let mut people = Vec::new();
        for (user, vehicles) in by_user {
            let mut row = Map::new();
            row.insert("user_id".into(), Value::from(user.clone()));
            if let Some(n) = names.get(&user) {
                row.insert("name".into(), Value::from(n.clone()));
            }
            let mut flags: Vec<String> = Vec::new();
            for (vehicle, mut claims) in vehicles {
                claims.sort_by(|a, b| a.dated_on.cmp(&b.dated_on));
                let a = assess(&vehicle, &claims);
                let mut v = json!({
                    "claims": claims.len(),
                    "miles": miles(a.miles),
                    "claimed": money(a.claimed),
                    "approved_amount": money(a.allowance),
                });
                if vehicle == "Car" {
                    let left = (CAR_THRESHOLD - a.miles).max(0);
                    v["miles_at_45p"] = Value::from(miles(a.at_higher));
                    v["miles_at_25p"] = Value::from(miles(a.at_lower));
                    v["miles_left_at_45p"] = Value::from(miles(left));
                    if let Some(d) = &a.passed_on {
                        flags.push(format!(
                            "passed 10,000 business miles by car on {d}: every mile after that is at 25p"
                        ));
                    } else if left > 0 && left <= NEAR_MILES * 100 {
                        flags.push(format!(
                            "{} business miles left at 45p this tax year; after that HMRC's rate is 25p",
                            miles(left)
                        ));
                    }
                }
                for (n, claimed, allowed) in &a.over {
                    let c = &claims[*n];
                    flags.push(format!(
                        "{} {:?} ({}, {} miles): claimed {}, the approved rate gives {}; the extra {} is taxable unless corrected",
                        c.dated_on,
                        c.description,
                        c.id.as_deref().map(|i| format!("expense {i}")).unwrap_or_default(),
                        miles(c.miles),
                        money(*claimed),
                        money(*allowed),
                        money(claimed - allowed)
                    ));
                }
                row.insert(vehicle.to_lowercase(), v);
            }
            if !flags.is_empty() {
                row.insert("flags".into(), Value::from(flags));
            }
            people.push(Value::Object(row));
        }
        let mut out = json!({
            "tax_year": format!("{year}/{:02}", (year + 1) % 100),
            "from": ymd(from),
            "to": ymd(to),
            "people": people,
            "rates": "HMRC approved mileage rates: car or van 45p a mile for the first 10,000 business miles in the tax year and 25p after; motorcycle 24p; bicycle 20p.",
            "note": "Read from FreeAgent's mileage claims; journeys not claimed there are not counted. The 10,000 miles are counted across all cars and vans a person uses for work. If the company records distances in kilometres, convert before comparing.",
        });
        if more {
            out["truncated"] = Value::from(true);
        }
        if to < today() {
            out["complete"] = Value::from(true);
        }
        Ok(out)
    }
}

#[tool_router(router = mileage_router, vis = "pub(crate)")]
impl BooksServer {
    #[tool(
        description = "Business mileage claimed in a UK tax year (6 April to 5 April; default: the current one), per person and vehicle: miles, amount claimed and HMRC's approved amount; for cars, miles at 45p and 25p and miles left before the 10,000-mile threshold; and flags for passing the threshold or claims above the approved rate. Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    pub async fn mileage_tracker(
        &self,
        Parameters(p): Parameters<MileageTrackerParams>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        run("mileage_tracker", async {
            let c = self.caller(&parts)?;
            Ok(json_result(&self.mileage(&c, &p).await?))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(date: &str, m: i64, claimed: Option<i64>) -> Claim {
        Claim {
            id: None,
            dated_on: date.into(),
            description: "trip".into(),
            miles: m * 100,
            claimed,
        }
    }

    #[test]
    fn tax_years_start_on_6_april() {
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        assert_eq!(tax_year(None, d(2026, 4, 5)).unwrap(), 2025);
        assert_eq!(tax_year(None, d(2026, 4, 6)).unwrap(), 2026);
        assert_eq!(tax_year(Some("2025/26"), d(2026, 1, 1)).unwrap(), 2025);
        assert_eq!(tax_year(Some("2025-2026"), d(2026, 1, 1)).unwrap(), 2025);
        assert_eq!(tax_year(Some("2024"), d(2026, 1, 1)).unwrap(), 2024);
        for bad in ["2025/27", "26/27", "last year", "1999"] {
            assert!(tax_year(Some(bad), d(2026, 1, 1)).is_err(), "{bad}");
        }
    }

    #[test]
    fn car_miles_drop_to_25p_after_10000() {
        let claims = vec![
            claim("2026-05-01", 9_900, Some(445_500)),
            // Straddles: 100 at 45p and 200 at 25p is 95.00.
            claim("2026-09-01", 300, Some(9_500)),
            // All after the threshold, but claimed at 45p.
            claim("2026-10-01", 100, Some(4_500)),
            // Rebilled only: counted as miles, not money.
            claim("2026-11-01", 50, None),
        ];
        let a = assess("Car", &claims);
        assert_eq!(a.miles, 10_350 * 100);
        assert_eq!(a.at_higher, 10_000 * 100);
        assert_eq!(a.at_lower, 350 * 100);
        assert_eq!(a.passed_on.as_deref(), Some("2026-09-01"));
        assert_eq!(a.claimed, 445_500 + 9_500 + 4_500);
        assert_eq!(a.allowance, 445_500 + 9_500 + 2_500);
        assert_eq!(a.over, vec![(2, 4_500, 2_500)]);
    }

    #[test]
    fn motorcycles_and_bicycles_have_flat_rates() {
        let a = assess("Motorcycle", &[claim("2026-05-01", 100, Some(2_400))]);
        assert_eq!((a.allowance, a.over.len()), (2_400, 0));
        let b = assess("Bicycle", &[claim("2026-05-01", 10, Some(450))]);
        assert_eq!(b.allowance, 200);
        assert_eq!(b.over, vec![(0, 450, 200)]);
        assert_eq!(miles(1_250), "12.50");
        assert_eq!(miles(10_000), "100");
    }
}
