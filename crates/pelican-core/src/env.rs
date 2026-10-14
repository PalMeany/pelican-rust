//! `.env` + process environment with Laravel `env()` semantics.
//!
//! Like Laravel's immutable Dotenv repository, variables already present in
//! the process environment win over `.env`. Values `true`/`(true)`,
//! `false`/`(false)`, `empty`/`(empty)`, `null`/`(null)` are converted as by
//! `Illuminate\Support\Env::get`.

use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Env {
    vars: HashMap<String, String>,
}

/// A converted env value.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvValue {
    Bool(bool),
    Null,
    Str(String),
}

impl Env {
    /// Load `<base>/.env` (if present) under the current process environment.
    pub fn load(base: &Path) -> anyhow::Result<Env> {
        let mut vars: HashMap<String, String> = HashMap::new();
        let file = base.join(".env");
        if file.exists() {
            for item in dotenvy::from_path_iter(&file)? {
                let (k, v) = item?;
                vars.insert(k, v);
            }
        }
        for (k, v) in std::env::vars() {
            vars.insert(k, v);
        }
        Ok(Env { vars })
    }

    pub fn from_map(vars: HashMap<String, String>) -> Env {
        Env { vars }
    }

    pub fn raw(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn value(&self, key: &str) -> Option<EnvValue> {
        let v = self.vars.get(key)?;
        Some(match v.to_ascii_lowercase().as_str() {
            "true" | "(true)" => EnvValue::Bool(true),
            "false" | "(false)" => EnvValue::Bool(false),
            "empty" | "(empty)" => EnvValue::Str(String::new()),
            "null" | "(null)" => EnvValue::Null,
            _ => EnvValue::Str(v.clone()),
        })
    }

    /// String value; `null` and missing → None.
    pub fn string(&self, key: &str) -> Option<String> {
        match self.value(key)? {
            EnvValue::Str(s) => Some(s),
            EnvValue::Bool(b) => Some(if b { "1".into() } else { String::new() }),
            EnvValue::Null => None,
        }
    }

    pub fn string_or(&self, key: &str, default: &str) -> String {
        self.string(key).unwrap_or_else(|| default.to_string())
    }

    /// PHP truthiness of the converted value (`"0"` and `""` are false).
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        match self.value(key) {
            None => default,
            Some(EnvValue::Bool(b)) => b,
            Some(EnvValue::Null) => false,
            Some(EnvValue::Str(s)) => !(s.is_empty() || s == "0"),
        }
    }

    /// PHP `(int)` cast of the value (leading digits), default when missing.
    pub fn int_or(&self, key: &str, default: i64) -> i64 {
        match self.value(key) {
            None => default,
            Some(EnvValue::Bool(b)) => i64::from(b),
            Some(EnvValue::Null) => 0,
            Some(EnvValue::Str(s)) => php_intval(&s),
        }
    }

    pub fn lookup(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |k| self.string(k)
    }
}

/// PHP `intval()` for strings: optional sign and leading digits, else 0.
pub fn php_intval(s: &str) -> i64 {
    let t = s.trim_start();
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    let v: i64 = digits[..end].parse().unwrap_or(0);
    if neg { -v } else { v }
}
