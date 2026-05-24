use std::{collections::HashMap, sync::Mutex, time::Instant};

const WINDOW_SECS: u64 = 60;
const MAX_ATTEMPTS: usize = 10;

pub struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true if the request is allowed, false if rate-limited.
    pub fn check(&self, ip: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        let timestamps = hits.entry(ip.to_string()).or_default();

        // Remove entries outside the window
        timestamps.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);

        if timestamps.len() >= MAX_ATTEMPTS {
            return false;
        }

        timestamps.push(now);
        true
    }

    /// Prune stale entries. Call periodically.
    pub fn prune(&self) {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap();
        hits.retain(|_, timestamps| {
            timestamps.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);
            !timestamps.is_empty()
        });
    }
}
