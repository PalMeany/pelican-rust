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
        let external: HashMap<String, String> = std::env::vars().collect();
        let file = base.join(".env");
        if !file.exists() {
            return Ok(Env { vars: external });
        }
        let content = std::fs::read_to_string(&file)?;
        let vars = crate::dotenv::load(&content, &external).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
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

/// A value for [`write_variables`]: strings are quoted as needed, raw values
/// (numbers) are written as they are, `Null` leaves `KEY=`.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvWrite {
    Str(String),
    Raw(String),
    Null,
}

/// `Illuminate\Support\Env::writeVariables($values, $path, $overwrite)`.
pub fn write_variables(path: &Path, values: &[(String, EnvWrite)], overwrite: bool) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let mut lines: Vec<String> = content.split('\n').map(String::from).collect();
    for (key, value) in values {
        add_variable(key, value, &mut lines, overwrite);
    }
    std::fs::write(path, lines.join("\n"))
}

/// `Env::addVariableToEnvContents`.
fn add_variable(key: &str, value: &EnvWrite, lines: &mut Vec<String>, overwrite: bool) {
    let prefix = format!("{}_", key.split('_').next().unwrap_or(key));
    let raw = match value {
        EnvWrite::Str(s) | EnvWrite::Raw(s) => s.as_str(),
        EnvWrite::Null => "",
    };
    let should_quote = raw.is_empty() || !raw.chars().all(|c| c.is_ascii_alphanumeric());
    let quoted = match value {
        EnvWrite::Str(s) => format!("{key}={}", prepare_quoted_value(s)),
        _ => format!("{key}={raw}"),
    };
    let plain = format!("{key}={raw}");
    let mut line_to_add = if should_quote { quoted.clone() } else { plain.clone() };
    if matches!(value, EnvWrite::Str(s) if s.is_empty()) {
        line_to_add = format!("{key}=");
    }
    let mut last_prefix: Option<usize> = None;
    let empty = format!("{key}=");
    for (i, line) in lines.iter_mut().enumerate() {
        if line.starts_with(&prefix) {
            last_prefix = Some(i);
        }
        if *line == quoted || *line == plain {
            return;
        }
        if *line == empty {
            *line = line_to_add;
            return;
        }
        if line.starts_with(&empty) {
            if overwrite {
                *line = line_to_add;
            }
            return;
        }
    }
    match last_prefix {
        None => {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
            lines.push(line_to_add);
        }
        Some(i) => lines.insert(i + 1, line_to_add),
    }
}

/// `Env::prepareQuotedValue`: double quotes unless the value contains one.
fn prepare_quoted_value(input: &str) -> String {
    if input.contains('"') {
        format!("'{}'", add_slashes_except(input, '"'))
    } else {
        format!("\"{}\"", add_slashes_except(input, '\''))
    }
}

/// `addslashes()` without escaping `except`.
fn add_slashes_except(input: &str, except: char) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '\'' | '"' | '\\' | '\0') && c != except {
            out.push('\\');
        }
        if c == '\0' {
            out.push('0');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod write_tests {
    use super::*;

    fn run(content: &str, values: &[(&str, EnvWrite)]) -> String {
        let mut lines: Vec<String> = content.split('\n').map(String::from).collect();
        for (k, v) in values {
            add_variable(k, v, &mut lines, true);
        }
        lines.join("\n")
    }

    #[test]
    fn replaces_inserts_after_prefix_and_appends() {
        let env = "APP_NAME=Pelican\nAPP_URL=http://x\n\nDB_HOST=a\n";
        let out = run(
            env,
            &[
                ("APP_NAME", EnvWrite::Str("My Panel".into())),
                ("APP_LOCALE", EnvWrite::Str("en".into())),
                ("MAIL_PORT", EnvWrite::Raw("587".into())),
                ("DB_HOST", EnvWrite::Str("".into())),
            ],
        );
        assert_eq!(out, "APP_NAME=\"My Panel\"\nAPP_URL=http://x\nAPP_LOCALE=en\n\nDB_HOST=\n\nMAIL_PORT=587");
    }

    /// Output of `Env::writeVariables` in the original panel container for the same input.
    #[test]
    fn matches_laravel_output() {
        let env = "APP_NAME=Pelican\nAPP_URL=http://x\n\nDB_HOST=a\n";
        let s = |v: &str| EnvWrite::Str(v.into());
        let out = run(
            env,
            &[
                ("APP_NAME", s("My Panel")),
                ("APP_LOCALE", s("en")),
                ("MAIL_PORT", EnvWrite::Raw("587".into())),
                ("DB_HOST", s("")),
                ("Q1", s("a\"b")),
                ("Q2", s("it's")),
                ("Q3", s("a\\b")),
                ("Q4", EnvWrite::Null),
                ("Q5", s("1.5")),
                ("Q6", EnvWrite::Raw("1.5".into())),
                ("Q1X", s("x")),
            ],
        );
        assert_eq!(
            out,
            "APP_NAME=\"My Panel\"\nAPP_URL=http://x\nAPP_LOCALE=en\n\nDB_HOST=\n\nMAIL_PORT=587\n\nQ1='a\"b'\n\nQ2=\"it's\"\n\nQ3=\"a\\\\b\"\n\nQ4=\n\nQ5=\"1.5\"\n\nQ6=1.5\n\nQ1X=x"
        );
    }

    #[test]
    fn quoting_follows_laravel() {
        assert_eq!(prepare_quoted_value(r#"a"b"#), r#"'a"b'"#);
        assert_eq!(prepare_quoted_value("it's"), r#""it's""#);
        assert_eq!(prepare_quoted_value(r"a\b"), r#""a\\b""#);
        assert_eq!(run("X=1", &[("X", EnvWrite::Null)]), "X=");
        assert_eq!(run("X=1", &[("X", EnvWrite::Str("true".into()))]), "X=true");
    }
}
