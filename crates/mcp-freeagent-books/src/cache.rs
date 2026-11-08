//! A short-lived cache of FreeAgent's category list (the chart of accounts) for each signed-in
//! person. Categories rarely change, but are read by every search for a nominal code and by the
//! reviews, so one read serves them for a few minutes.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a category list is kept.
pub const CATEGORIES_TTL: Duration = Duration::from_secs(10 * 60);
/// Above this many entries, expired ones are dropped.
const PRUNE_ABOVE: usize = 1_000;

/// (credential fingerprint, with sub-accounts).
type Key = ([u8; 32], bool);

/// Category lists by person, with when they were read.
#[derive(Default)]
pub struct CategoryCache {
    entries: Mutex<HashMap<Key, (Instant, Value)>>,
}

impl CategoryCache {
    /// The list kept for `key`, unless it has expired.
    pub fn get(&self, key: [u8; 32], sub_accounts: bool) -> Option<Value> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&(key, sub_accounts))
            .filter(|(at, _)| at.elapsed() < CATEGORIES_TTL)
            .map(|(_, v)| v.clone())
    }

    pub fn put(&self, key: [u8; 32], sub_accounts: bool, v: Value) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() > PRUNE_ABOVE {
            entries.retain(|_, (at, _)| at.elapsed() < CATEGORIES_TTL);
        }
        entries.insert((key, sub_accounts), (Instant::now(), v));
    }

    /// Forgets `key`'s lists, after a write that may change categories.
    pub fn forget(&self, key: [u8; 32]) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(k, _), _| *k != key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lists_are_kept_per_person_and_forgotten_on_request() {
        let c = CategoryCache::default();
        c.put([1; 32], false, json!({"a": 1}));
        assert_eq!(c.get([1; 32], false), Some(json!({"a": 1})));
        assert_eq!(c.get([1; 32], true), None);
        assert_eq!(c.get([2; 32], false), None);
        c.forget([1; 32]);
        assert_eq!(c.get([1; 32], false), None);
    }
}
