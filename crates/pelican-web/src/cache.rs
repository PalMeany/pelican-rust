//! Process-local TTL cache standing in for Laravel's `cache()->remember()` on
//! Wings lookups (server details, node statistics).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

static STORE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn get(key: &str) -> Option<Value> {
    let mut m = STORE.lock().unwrap_or_else(|p| p.into_inner());
    match m.get(key) {
        Some((exp, v)) if *exp > Instant::now() => Some(v.clone()),
        Some(_) => {
            m.remove(key);
            None
        }
        None => None,
    }
}

pub fn put(key: &str, ttl: Duration, v: Value) {
    let mut m = STORE.lock().unwrap_or_else(|p| p.into_inner());
    if m.len() > 10_000 {
        let now = Instant::now();
        m.retain(|_, (exp, _)| *exp > now);
    }
    m.insert(key.to_string(), (Instant::now() + ttl, v));
}

pub fn forget(key: &str) {
    STORE.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
}

/// `cache()->remember($key, $ttl, $f)`.
pub async fn remember<F, Fut>(key: &str, ttl: Duration, f: F) -> Value
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Value>,
{
    if let Some(v) = get(key) {
        return v;
    }
    let v = f().await;
    put(key, ttl, v.clone());
    v
}
