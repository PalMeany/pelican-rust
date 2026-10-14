//! `Illuminate\Cache\RateLimiter` + `ThrottleRequests` semantics over an
//! in-process store (ADR-0009).

use std::collections::HashMap;

use parking_lot::Mutex;

#[derive(Default)]
pub struct RateLimiter {
    inner: Mutex<HashMap<String, Entry>>,
    /// `CACHE_STORE=array`: Laravel's array store lives for one request, so
    /// counters never carry over (every call starts from an empty store).
    ephemeral: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    value: i64,
    expires_at: i64,
}

/// Result of attempting a throttled request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Throttle {
    /// Allowed; `remaining` for the `X-RateLimit-Remaining` header.
    Allowed { limit: u32, remaining: i64 },
    /// Too many attempts: `retry_after` seconds, `reset_at` unix time.
    Exceeded { limit: u32, retry_after: i64, reset_at: i64 },
}

impl RateLimiter {
    pub fn new(cache_store: &str) -> Self {
        RateLimiter { inner: Mutex::default(), ephemeral: cache_store == "array" }
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, HashMap<String, Entry>> {
        let mut map = self.inner.lock();
        if self.ephemeral {
            map.clear();
        }
        map
    }

    fn get(map: &mut HashMap<String, Entry>, key: &str, now: i64) -> Option<i64> {
        match map.get(key) {
            Some(e) if e.expires_at > now => Some(e.value),
            Some(_) => {
                map.remove(key);
                None
            }
            None => None,
        }
    }

    /// `tooManyAttempts` then `hit` — exactly what ThrottleRequests does
    /// before passing the request on. `now` is unix seconds.
    pub fn attempt(&self, key: &str, max: u32, decay_seconds: i64, now: i64) -> Throttle {
        let mut map = self.lock();
        if map.len() > 10_000 {
            map.retain(|_, e| e.expires_at > now);
        }
        let timer_key = format!("{key}:timer");
        let attempts = Self::get(&mut map, key, now).unwrap_or(0);
        if attempts >= i64::from(max) {
            if let Some(available_at) = Self::get(&mut map, &timer_key, now) {
                let retry_after = (available_at - now).max(0);
                return Throttle::Exceeded { limit: max, retry_after, reset_at: now + retry_after };
            }
            map.remove(key);
        }
        // hit(): add timer (if absent), add counter 0 (if absent), increment.
        if Self::get(&mut map, &timer_key, now).is_none() {
            map.insert(timer_key, Entry { value: now + decay_seconds, expires_at: now + decay_seconds });
        }
        let hits = match map.get_mut(key) {
            Some(e) if e.expires_at > now => {
                e.value += 1;
                e.value
            }
            _ => {
                map.insert(key.to_string(), Entry { value: 1, expires_at: now + decay_seconds });
                1
            }
        };
        Throttle::Allowed { limit: max, remaining: i64::from(max) - hits }
    }

    /// `RateLimiter::tooManyAttempts`: Some(available_in) when blocked; resets
    /// the counter if the window expired.
    pub fn too_many_attempts(&self, key: &str, max: u32, now: i64) -> Option<i64> {
        let mut map = self.lock();
        let attempts = Self::get(&mut map, key, now).unwrap_or(0);
        if attempts >= i64::from(max) {
            let timer_key = format!("{key}:timer");
            if let Some(available_at) = Self::get(&mut map, &timer_key, now) {
                return Some((available_at - now).max(0));
            }
            map.remove(key);
        }
        None
    }

    /// `RateLimiter::hit($key, $decay)`.
    pub fn hit(&self, key: &str, decay_seconds: i64, now: i64) -> i64 {
        let mut map = self.lock();
        let timer_key = format!("{key}:timer");
        if Self::get(&mut map, &timer_key, now).is_none() {
            map.insert(timer_key, Entry { value: now + decay_seconds, expires_at: now + decay_seconds });
        }
        match map.get_mut(key) {
            Some(e) if e.expires_at > now => {
                e.value += 1;
                e.value
            }
            _ => {
                map.insert(key.to_string(), Entry { value: 1, expires_at: now + decay_seconds });
                1
            }
        }
    }

    /// Current hits for a key (no side effects).
    pub fn attempts(&self, key: &str, now: i64) -> i64 {
        let mut map = self.lock();
        Self::get(&mut map, key, now).unwrap_or(0)
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_resets() {
        let rl = RateLimiter::default();
        assert_eq!(rl.attempt("k", 2, 60, 1000), Throttle::Allowed { limit: 2, remaining: 1 });
        assert_eq!(rl.attempt("k", 2, 60, 1001), Throttle::Allowed { limit: 2, remaining: 0 });
        assert_eq!(rl.attempt("k", 2, 60, 1002), Throttle::Exceeded { limit: 2, retry_after: 58, reset_at: 1060 });
        assert_eq!(rl.attempt("k", 2, 60, 1060), Throttle::Allowed { limit: 2, remaining: 1 });
    }
}
