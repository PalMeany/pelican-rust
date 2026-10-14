//! Normalization of run artifacts so two runs can be compared byte-for-byte.
//!
//! Dynamic values (fresh UUIDs, "now" timestamps, encrypted payloads, bcrypt
//! hashes, JWTs, declared random tokens) are collected into a substitution
//! table in a deterministic traversal order and replaced textually — in raw
//! bodies too, so escaping and key order are still compared exactly.

use std::collections::HashSet;

use base64::Engine;
use chrono::{DateTime, NaiveDateTime, Utc};
use indexmap::IndexMap;
use laravel_compat::Encrypter;
use regex::Regex;

pub struct Normalizer<'a> {
    enc: &'a Encrypter,
    /// Values present in fixtures/scenario: never treated as dynamic.
    known: HashSet<String>,
    start: NaiveDateTime,
    end: NaiveDateTime,
    /// value → placeholder, in first-seen order.
    subs: IndexMap<String, String>,
    counters: IndexMap<&'static str, usize>,
    random_patterns: Vec<Regex>,
    re_uuid: Regex,
    re_time: Regex,
    re_enc: Regex,
    re_bcrypt: Regex,
    re_jwt: Regex,
}

impl<'a> Normalizer<'a> {
    pub fn new(
        enc: &'a Encrypter,
        known_text: &str,
        start: NaiveDateTime,
        end: NaiveDateTime,
        random_patterns: &[String],
    ) -> anyhow::Result<Self> {
        let re_uuid = Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")?;
        let mut known: HashSet<String> = re_uuid
            .find_iter(known_text)
            .map(|m| m.as_str().to_string())
            .collect();
        let re_time =
            Regex::new(r"\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")?;
        known.extend(
            re_time
                .find_iter(known_text)
                .map(|m| m.as_str().to_string()),
        );
        Ok(Normalizer {
            enc,
            known,
            start,
            end,
            subs: IndexMap::new(),
            counters: IndexMap::new(),
            random_patterns: random_patterns
                .iter()
                .map(|p| Regex::new(p))
                .collect::<Result<_, _>>()?,
            re_uuid,
            re_time,
            // base64 of `{"iv":` possibly with JSON-escaped slashes.
            re_enc: Regex::new(r"eyJpdiI6(?:[A-Za-z0-9+=]|\\?/)+")?,
            re_bcrypt: Regex::new(r"\$2[aby]\$\d\d\$(?:[.A-Za-z0-9]|\\?/){53}")?,
            re_jwt: Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")?,
        })
    }

    fn placeholder(&mut self, kind: &'static str, extra: &str) -> String {
        let n = self.counters.entry(kind).or_insert(0);
        *n += 1;
        if extra.is_empty() {
            format!("<{kind}#{n}>")
        } else {
            format!("<{kind}#{n}:{extra}>")
        }
    }

    fn register(&mut self, value: &str, kind: &'static str, extra: &str) {
        if value.is_empty() || self.subs.contains_key(value) || self.known.contains(value) {
            return;
        }
        let ph = self.placeholder(kind, extra);
        self.subs.insert(value.to_string(), ph.clone());
        // The same value as it appears inside JSON strings.
        let escaped = value.replace('/', "\\/");
        if escaped != value {
            self.subs.insert(escaped, ph);
        }
    }

    /// Register a value known to be random (declared column/path).
    pub fn register_random(&mut self, value: &str) {
        let len = value.chars().count().to_string();
        self.register(value, "rand", &len);
    }

    /// Scan an artifact and register every dynamic value found in it.
    pub fn scan(&mut self, text: &str) {
        for m in self
            .re_enc
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>()
        {
            let raw = m.replace("\\/", "/");
            match self.enc.decrypt_string_lenient(&raw) {
                Ok(plain) => {
                    // Name the ciphertext after its (normalized) plaintext.
                    self.scan(&plain);
                    let p = self.apply(&plain);
                    self.register(&m, "enc", &p);
                    self.register(&raw, "enc", &p);
                }
                Err(_) => self.register(&m, "enc-invalid", ""),
            }
        }
        for m in self
            .re_jwt
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>()
        {
            let claims = m
                .split('.')
                .nth(1)
                .and_then(|p| {
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(p)
                        .ok()
                })
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            self.scan(&claims);
            let c = self.apply(&claims);
            self.register(&m, "jwt", &c);
        }
        for m in self
            .re_bcrypt
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>()
        {
            self.register(&m.replace("\\/", "/"), "bcrypt", "");
        }
        for m in self
            .re_uuid
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>()
        {
            self.register(&m, "uuid", "");
        }
        for m in self
            .re_time
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>()
        {
            if let Some(label) = self.time_label(&m) {
                if !self.known.contains(&m) && !self.subs.contains_key(&m) {
                    self.subs.insert(m.clone(), label);
                }
            }
        }
        let pats = self.random_patterns.clone();
        for re in &pats {
            for m in re
                .find_iter(text)
                .map(|m| m.as_str().to_string())
                .collect::<Vec<_>>()
            {
                self.register_random(&m);
            }
        }
    }

    /// `<now>` for times inside the run window, `<now±Ns>` (10s buckets) nearby.
    fn time_label(&self, s: &str) -> Option<String> {
        let t = parse_time(s)?;
        let fmt_kind = if s.contains('T') { "iso" } else { "sql" };
        let slack = chrono::Duration::seconds(2);
        if t >= self.start - slack && t <= self.end + slack {
            return Some(format!("<now:{fmt_kind}>"));
        }
        let delta = (t - self.start).num_seconds();
        if delta.abs() <= 3 * 86400 {
            let bucket = (delta as f64 / 10.0).round() as i64 * 10;
            return Some(format!("<now{bucket:+}s:{fmt_kind}>"));
        }
        None
    }

    /// Replace every registered value (longest first).
    pub fn apply(&self, text: &str) -> String {
        let mut keys: Vec<&String> = self.subs.keys().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        let mut out = text.to_string();
        for k in keys {
            if out.contains(k.as_str()) {
                out = out.replace(k.as_str(), &self.subs[k]);
            }
        }
        out
    }
}

fn parse_time(s: &str) -> Option<NaiveDateTime> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc).naive_utc());
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(d);
        }
        if let Ok(d) = DateTime::parse_from_str(s, f) {
            return Some(d.with_timezone(&Utc).naive_utc());
        }
    }
    None
}
