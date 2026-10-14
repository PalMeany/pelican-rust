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

/// PHP `version_compare($a, $b)` (php_version_compare: canonical form, special forms
/// `dev < alpha = a < beta = b < RC = rc < # < pl = p`).
pub fn version_compare(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ca, cb) = (canonicalize_version(a), canonicalize_version(b));
    if ca.is_empty() || cb.is_empty() {
        return match (ca.is_empty(), cb.is_empty()) {
            (true, true) => Ordering::Equal,
            (false, true) => Ordering::Greater,
            _ => Ordering::Less,
        };
    }
    let pa: Vec<&str> = ca.split('.').collect();
    let pb: Vec<&str> = cb.split('.').collect();
    let numeric = |s: &str| s.as_bytes().first().is_some_and(u8::is_ascii_digit);
    for (x, y) in pa.iter().zip(pb.iter()) {
        let ord = match (numeric(x), numeric(y)) {
            (true, true) => super::env::php_intval(x).cmp(&super::env::php_intval(y)),
            (false, false) => special_form(x).cmp(&special_form(y)),
            (true, false) => special_form("#").cmp(&special_form(y)),
            (false, true) => special_form(x).cmp(&special_form("#")),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    let n = pa.len().min(pb.len());
    if pa.len() > n {
        if numeric(pa[n]) { Ordering::Greater } else { special_form(pa[n]).cmp(&special_form("#")) }
    } else if pb.len() > n {
        if numeric(pb[n]) { Ordering::Less } else { special_form("#").cmp(&special_form(pb[n])) }
    } else {
        Ordering::Equal
    }
}

fn canonicalize_version(v: &str) -> String {
    let chars: Vec<char> = v.chars().collect();
    let Some(&first) = chars.first() else { return String::new() };
    let dig = |c: char| c.is_ascii_digit();
    let ndig = |c: char| !c.is_ascii_digit() && c != '.';
    let mut out = String::from(first);
    let mut lp = first;
    for &c in &chars[1..] {
        let last_dot = out.ends_with('.');
        if matches!(c, '-' | '_' | '+') {
            if !last_dot {
                out.push('.');
            }
        } else if (ndig(lp) && dig(c)) || (dig(lp) && ndig(c)) {
            if !last_dot {
                out.push('.');
            }
            out.push(c);
        } else if !c.is_ascii_alphanumeric() {
            if !last_dot {
                out.push('.');
            }
        } else {
            out.push(c);
        }
        lp = c;
    }
    out
}

fn special_form(s: &str) -> i32 {
    const FORMS: [(&str, i32); 10] = [
        ("dev", 0),
        ("alpha", 1),
        ("a", 1),
        ("beta", 2),
        ("b", 2),
        ("RC", 3),
        ("rc", 3),
        ("#", 4),
        ("pl", 5),
        ("p", 5),
    ];
    FORMS.iter().find(|(f, _)| s.starts_with(f)).map_or(-6, |(_, o)| *o)
}

#[cfg(test)]
mod version_tests {
    use super::version_compare;
    use std::cmp::Ordering::*;

    #[test]
    fn matches_php() {
        assert_eq!(version_compare("1.11.13", "1.12.0"), Less);
        assert_eq!(version_compare("1.12.0", "1.12.0"), Equal);
        assert_eq!(version_compare("1.12.1", "1.12"), Greater);
        assert_eq!(version_compare("1.0.0-beta1", "1.0.0"), Less);
        assert_eq!(version_compare("1.0.0rc1", "1.0.0beta2"), Greater);
        assert_eq!(version_compare("1.0.0-dev", "1.0.0-alpha"), Less);
        assert_eq!(version_compare("1.0.0pl1", "1.0.0"), Greater);
        assert_eq!(version_compare("10.0", "9.9"), Greater);
    }
}
