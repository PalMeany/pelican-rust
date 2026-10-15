//! Process-local TTL cache standing in for Laravel's `cache()->remember()` on
//! Wings lookups (server details, node statistics).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

static STORE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Keys [`remember_swr`] is refreshing in the background.
static REFRESHING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// How long past its TTL [`remember_swr`] still shows a value while fetching a new one.
const STALE_GRACE: Duration = Duration::from_secs(300);

/// The value under `key` and whether it is past its TTL (but within [`STALE_GRACE`]).
fn lookup(key: &str) -> Option<(Value, bool)> {
    let m = STORE.lock().unwrap_or_else(|p| p.into_inner());
    let (exp, v) = m.get(key)?;
    let now = Instant::now();
    if *exp > now {
        Some((v.clone(), false))
    } else if now.duration_since(*exp) < STALE_GRACE {
        Some((v.clone(), true))
    } else {
        None
    }
}

pub fn get(key: &str) -> Option<Value> {
    lookup(key).filter(|(_, stale)| !stale).map(|(v, _)| v)
}

pub fn put(key: &str, ttl: Duration, v: Value) {
    let mut m = STORE.lock().unwrap_or_else(|p| p.into_inner());
    if m.len() > 10_000 {
        let now = Instant::now();
        m.retain(|_, (exp, _)| *exp + STALE_GRACE > now);
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

/// [`remember`] for answers from Wings that pages show: past its TTL a value is still returned
/// at once while a fresh one is fetched in the background, so a page waits for Wings (and for
/// an unreachable node's timeouts) only when nothing recent is known.
pub async fn remember_swr<F, Fut>(key: &str, ttl: Duration, f: F) -> Value
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Value> + Send + 'static,
{
    match lookup(key) {
        Some((v, false)) => v,
        Some((v, true)) => {
            let first = REFRESHING.lock().unwrap_or_else(|p| p.into_inner()).insert(key.to_string());
            if first {
                let fut = f();
                let key = key.to_string();
                tokio::spawn(async move {
                    let fresh = fut.await;
                    put(&key, ttl, fresh);
                    REFRESHING.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
                });
            }
            v
        }
        None => {
            let v = f().await;
            put(key, ttl, v.clone());
            v
        }
    }
}
