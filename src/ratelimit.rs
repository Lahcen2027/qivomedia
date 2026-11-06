//! Small in-memory sliding-window rate limiter (per process). Good for a single
//! server; move to Redis/Postgres when running several instances.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::http::HeaderMap;

fn store() -> &'static Mutex<HashMap<String, Vec<Instant>>> {
    static L: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

fn prune(m: &mut HashMap<String, Vec<Instant>>, window: Duration) {
    let now = Instant::now();
    m.retain(|_, v| {
        v.retain(|t| now.duration_since(*t) < window.max(Duration::from_secs(3600)));
        !v.is_empty()
    });
}

fn count(v: &[Instant], window: Duration) -> usize {
    let now = Instant::now();
    v.iter().filter(|t| now.duration_since(**t) < window).count()
}

/// Record a hit and return whether it is within `max` per `window` for `key`.
pub fn allow(key: &str, max: usize, window: Duration) -> bool {
    let mut m = store().lock().unwrap();
    prune(&mut m, window);
    let hits = m.entry(key.to_string()).or_default();
    if count(hits, window) >= max {
        return false;
    }
    hits.push(Instant::now());
    true
}

/// Has `key` reached `max` hits in `window`? (Does not record a hit.)
pub fn exceeded(key: &str, max: usize, window: Duration) -> bool {
    let m = store().lock().unwrap();
    m.get(key).map(|v| count(v, window) >= max).unwrap_or(false)
}

/// Record a hit without checking (e.g. a failed login).
pub fn hit(key: &str) {
    store().lock().unwrap().entry(key.to_string()).or_default().push(Instant::now());
}

pub fn reset(key: &str) {
    store().lock().unwrap().remove(key);
}

/// Client IP as seen by our reverse proxy. Caddy appends the address it actually saw
/// as the *last* X-Forwarded-For entry; earlier entries can be forged by the client,
/// so only the rightmost one is trusted.
pub fn client_ip(h: &HeaderMap) -> String {
    h.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into())
}

pub const MIN: Duration = Duration::from_secs(60);
pub const HOUR: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window() {
        for _ in 0..3 {
            assert!(allow("rl:test:a", 3, MIN));
        }
        assert!(!allow("rl:test:a", 3, MIN));
        assert!(allow("rl:test:b", 3, MIN));
        hit("rl:test:c");
        hit("rl:test:c");
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 203.0.113.9".parse().unwrap());
        assert_eq!(client_ip(&h), "203.0.113.9", "forged leading entries are ignored");
        assert!(exceeded("rl:test:c", 2, MIN));
        reset("rl:test:c");
        assert!(!exceeded("rl:test:c", 2, MIN));
    }
}
