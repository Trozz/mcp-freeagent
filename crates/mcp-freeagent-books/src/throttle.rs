//! Keeps each signed-in person under FreeAgent's limit of 120 requests a minute: a request that
//! would go over waits for the oldest one in the window to age out, instead of failing with a
//! 429. Reviews that read many pages (month_end_check, asset_register_check) are the usual cause.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Requests a minute each person may send. FreeAgent allows 120; the margin covers other apps
/// using the same FreeAgent user.
pub const REQUESTS_PER_MINUTE: u32 = 110;
const WINDOW: Duration = Duration::from_secs(60);
/// Above this many people tracked, idle ones are dropped.
const PRUNE_ABOVE: usize = 1_000;

/// A sliding one-minute window of request times per person (by credential fingerprint).
pub struct Throttle {
    per_minute: u32,
    sent: Mutex<HashMap<[u8; 32], VecDeque<Instant>>>,
}

impl Throttle {
    /// `per_minute` 0 turns the throttle off.
    pub fn new(per_minute: u32) -> Self {
        Throttle {
            per_minute,
            sent: Mutex::new(HashMap::new()),
        }
    }

    /// Waits until `key` may send another request, and counts it.
    pub async fn acquire(&self, key: [u8; 32]) {
        if self.per_minute == 0 {
            return;
        }
        loop {
            match self.try_at(key, Instant::now()) {
                None => return,
                Some(wait) => {
                    metrics::counter!("freeagent_throttled_total").increment(1);
                    tracing::debug!(wait_ms = wait.as_millis() as u64, "throttling");
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Counts a request from `key` at `now` and returns `None`, or, when the window is full,
    /// returns how long to wait and counts nothing.
    fn try_at(&self, key: [u8; 32], now: Instant) -> Option<Duration> {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        if sent.len() > PRUNE_ABOVE {
            sent.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < WINDOW));
        }
        let q = sent.entry(key).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
            q.pop_front();
        }
        if q.len() < self.per_minute as usize {
            q.push_back(now);
            return None;
        }
        let oldest = *q.front()?;
        Some((oldest + WINDOW).saturating_duration_since(now) + Duration::from_millis(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_window_waits_for_the_oldest_request_to_age_out() {
        let t = Throttle::new(3);
        let start = Instant::now();
        let a = [1u8; 32];
        for i in 0..3 {
            assert_eq!(t.try_at(a, start + Duration::from_secs(i)), None);
        }
        let wait = t.try_at(a, start + Duration::from_secs(10)).unwrap();
        assert!(
            wait > Duration::from_secs(49) && wait < Duration::from_secs(51),
            "{wait:?}"
        );
        // Someone else is not held up.
        assert_eq!(t.try_at([2u8; 32], start + Duration::from_secs(10)), None);
        // A minute after the first request there is room again.
        assert_eq!(t.try_at(a, start + Duration::from_secs(60)), None);
        assert!(t.try_at(a, start + Duration::from_secs(60)).is_some());
    }

    #[tokio::test]
    async fn zero_turns_it_off() {
        let t = Throttle::new(0);
        for _ in 0..1_000 {
            t.acquire([0u8; 32]).await;
        }
    }
}
