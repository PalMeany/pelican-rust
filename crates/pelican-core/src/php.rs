//! Small PHP value semantics shared by services.

use serde_json::Value;

/// PHP `(string)` cast of a decoded JSON value.
pub fn string(v: &Value) -> String {
    match v {
        Value::Null | Value::Bool(false) => String::new(),
        Value::Bool(true) => "1".into(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => laravel_compat::php_serialize::PhpFloat(f).to_string(),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        _ => "Array".into(),
    }
}

/// PHP `empty()`.
pub fn empty(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => true,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty() || s == "0",
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// PHP 8 `is_numeric` for strings.
pub fn is_numeric_str(s: &str) -> bool {
    let t = s.trim_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    if t.is_empty() {
        return false;
    }
    let (mantissa, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let digits = mantissa.chars().filter(|c| c.is_ascii_digit()).count();
    let dots = mantissa.chars().filter(|c| *c == '.').count();
    let mantissa_ok = digits > 0 && dots <= 1 && mantissa.chars().all(|c| c.is_ascii_digit() || c == '.');
    let exp_ok = match exp {
        None => true,
        Some(e) => {
            let e = e.strip_prefix(['+', '-']).unwrap_or(e);
            !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
        }
    };
    mantissa_ok && exp_ok
}

/// PHP `(int)` cast of a string (leading integer part).
pub fn intval(s: &str) -> i64 {
    crate::env::php_intval(s)
}
