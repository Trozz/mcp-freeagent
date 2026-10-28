//! Shapes FreeAgent answers into the compact JSON the tools return: envelopes unwrapped, empty
//! values dropped, an `id` beside every resource `url`, and pages labelled with the next page.

use serde_json::{Map, Value, json};

/// Removes nulls, empty strings and empty arrays or objects, recursively. `None` when nothing is
/// left. `false` and `0` are kept: they carry meaning in accounts.
pub fn prune(v: Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        Value::Array(items) => {
            let items: Vec<Value> = items.into_iter().filter_map(prune).collect();
            (!items.is_empty()).then_some(Value::Array(items))
        }
        Value::Object(map) => {
            let map: Map<String, Value> = map
                .into_iter()
                .filter_map(|(k, v)| prune(v).map(|v| (k, v)))
                .collect();
            (!map.is_empty()).then_some(Value::Object(map))
        }
        other => Some(other),
    }
}

/// The last path segment of a FreeAgent resource URL: its id.
pub fn id_of(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
}

/// Adds `id` beside the `url` of every object that has one and no `id` yet, recursively.
fn add_ids(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if !map.contains_key("id")
                && let Some(id) = map.get("url").and_then(Value::as_str).and_then(id_of)
            {
                let id = id.to_string();
                map.insert("id".into(), Value::from(id));
            }
            map.values_mut().for_each(add_ids);
        }
        Value::Array(items) => items.iter_mut().for_each(add_ids),
        _ => {}
    }
}

/// One resource for output: ids added, empty values dropped.
pub fn resource(v: &Value) -> Value {
    let mut v = v.clone();
    add_ids(&mut v);
    prune(v).unwrap_or_else(|| json!({}))
}

/// The value under `key` of an answer (`{"invoice": {...}}` gives the invoice); the answer itself
/// when it has no such key.
pub fn unwrap<'a>(answer: &'a Value, key: &str) -> &'a Value {
    answer.get(key).unwrap_or(answer)
}

/// A page of a list answer: `items` under `key`, with `count`, `total` when FreeAgent gave it,
/// `page`, and `next_page` while there are more.
pub fn page(items: Vec<Value>, page: u32, total: Option<u64>, has_next: bool) -> Value {
    let mut out = Map::new();
    out.insert("count".into(), Value::from(items.len()));
    if let Some(t) = total {
        out.insert("total".into(), Value::from(t));
    }
    out.insert("page".into(), Value::from(page));
    if has_next {
        out.insert("next_page".into(), Value::from(page + 1));
    }
    out.insert("items".into(), Value::Array(items));
    Value::Object(out)
}

/// The items of a list answer under `key`, shaped.
pub fn items(answer: &Value, key: &str) -> Vec<Value> {
    answer
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().map(resource).collect())
        .unwrap_or_default()
}

/// A display name for a contact: its organisation, else first and last name.
pub fn contact_name(c: &Value) -> String {
    if let Some(org) = c
        .get("organisation_name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return org.to_string();
    }
    [c.get("first_name"), c.get("last_name")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A FreeAgent money value (a decimal string, sometimes a number) as hundredths, for sums.
pub fn pence(v: Option<&Value>) -> Option<i64> {
    let s = match v? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let negative = s.starts_with('-');
    let s = s.trim_start_matches(['-', '+']);
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut frac2: String = frac.chars().take(2).collect();
    while frac2.len() < 2 {
        frac2.push('0');
    }
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut value = whole
        .checked_mul(100)?
        .checked_add(frac2.parse::<i64>().ok()?)?;
    // Round half away from zero on the third decimal.
    if frac.chars().nth(2).is_some_and(|c| c >= '5') {
        value += 1;
    }
    Some(if negative { -value } else { value })
}

/// Hundredths as a decimal string, e.g. `-1234` is `"-12.34"`.
pub fn money(pence: i64) -> String {
    let sign = if pence < 0 { "-" } else { "" };
    let p = pence.unsigned_abs();
    format!("{sign}{}.{:02}", p / 100, p % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_added_beside_urls_everywhere() {
        let v = json!({
            "url": "https://api.freeagent.com/v2/invoices/12",
            "contact": "https://api.freeagent.com/v2/contacts/2",
            "comments": "",
            "invoice_items": [{"url": "https://api.freeagent.com/v2/invoice_items/7", "description": "x"}],
            "paid_value": "0.0",
            "is_interim": false,
            "nothing": null
        });
        let r = resource(&v);
        assert_eq!(r["id"], "12");
        assert_eq!(r["invoice_items"][0]["id"], "7");
        assert_eq!(r["paid_value"], "0.0");
        assert_eq!(r["is_interim"], false);
        assert!(r.get("comments").is_none() && r.get("nothing").is_none());
    }

    #[test]
    fn pages_say_whether_there_is_more() {
        let p = page(vec![json!({"a": 1})], 2, Some(30), true);
        assert_eq!(p["count"], 1);
        assert_eq!(p["total"], 30);
        assert_eq!(p["next_page"], 3);
        let p = page(vec![], 1, None, false);
        assert!(p.get("next_page").is_none() && p.get("total").is_none());
    }

    #[test]
    fn contact_names() {
        assert_eq!(
            contact_name(&json!({"organisation_name": "Acme Ltd", "first_name": "A"})),
            "Acme Ltd"
        );
        assert_eq!(
            contact_name(&json!({"first_name": "Ann", "last_name": "Lee"})),
            "Ann Lee"
        );
        assert_eq!(
            contact_name(&json!({"organisation_name": " ", "last_name": "Lee"})),
            "Lee"
        );
    }

    #[test]
    fn money_is_summed_exactly() {
        assert_eq!(pence(Some(&json!("200.0"))), Some(20000));
        assert_eq!(pence(Some(&json!("-730.5"))), Some(-73050));
        assert_eq!(pence(Some(&json!("12.345"))), Some(1235));
        assert_eq!(pence(Some(&json!(15))), Some(1500));
        assert_eq!(pence(Some(&json!(".5"))), Some(50));
        assert_eq!(pence(Some(&json!("abc"))), None);
        assert_eq!(pence(None), None);
        assert_eq!(money(-1234), "-12.34");
        assert_eq!(money(5), "0.05");
        assert_eq!(
            id_of("https://api.freeagent.com/v2/categories/750-1"),
            Some("750-1")
        );
    }
}
