//! Laravel translator over the dump of the original's `lang/*` and vendor
//! namespaces (`data/lang.json`, regenerate with `scripts/i18n/dump-lang`).

use std::sync::LazyLock;

use serde_json::Value;

static LANG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../data/lang.json")).unwrap_or(Value::Null));

/// Locales shipped by the original panel, plus those of PHP language plugins (sidecar).
pub fn locales() -> Vec<String> {
    let mut out: Vec<String> = LANG.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    if let Some(extra) = pelican_api::sidecar::translations() {
        for code in extra.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() {
            if !out.contains(&code) {
                out.push(code);
            }
        }
    }
    out
}

/// Resolve `ns::group.item.path` (group may contain `/`) to its raw value in one tree.
fn lookup_in(root: &Value, key: &str) -> Option<Value> {
    let (ns, rest) = match key.split_once("::") {
        Some((ns, rest)) => (Some(ns), rest),
        None => (None, key),
    };
    let (group, item) = match rest.split_once('.') {
        Some((g, i)) => (g, Some(i)),
        None => (rest, None),
    };
    let group_key = match ns {
        Some(ns) => format!("{ns}::{group}"),
        None => group.to_string(),
    };
    let mut cur = root.get(group_key.as_str());
    if cur.is_none() && ns.is_none() {
        // JSON translations (`__('Some text')`).
        return root.get("*").and_then(|j| j.get(key)).cloned();
    }
    if let Some(item) = item {
        for seg in item.split('.') {
            cur = cur?.get(seg);
        }
    }
    cur.cloned()
}

/// Language plugins' lines (`$translator->addPath()`) override the panel's.
fn lookup(locale: &str, key: &str) -> Option<Value> {
    if let Some(extra) = pelican_api::sidecar::translations() {
        if let Some(v) = extra.get(locale).and_then(|root| lookup_in(root, key)) {
            return Some(v);
        }
    }
    lookup_in(LANG.get(locale)?, key)
}

fn line(locale: &str, key: &str) -> Option<String> {
    lookup(locale, key).or_else(|| lookup("en", key)).and_then(|v| v.as_str().map(str::to_string))
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Laravel `makeReplacements`: `:name`, `:Name`, `:NAME`, longest key first.
pub fn replace(line: &str, replace: &[(String, String)]) -> String {
    let mut out = line.to_string();
    let mut reps: Vec<&(String, String)> = replace.iter().collect();
    reps.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    for (k, v) in reps {
        out = out
            .replace(&format!(":{}", k.to_uppercase()), &v.to_uppercase())
            .replace(&format!(":{}", ucfirst(k)), &ucfirst(v))
            .replace(&format!(":{k}"), v);
    }
    out
}

/// `trans($key, $replace)` with `en` fallback; unknown keys come back verbatim.
pub fn trans(locale: &str, key: &str, params: &[(String, String)]) -> String {
    replace(&line(locale, key).unwrap_or_else(|| key.to_string()), params)
}

/// `trans_choice($key, $number, $replace)`; `:count` is added like Laravel does.
pub fn trans_choice(locale: &str, key: &str, number: f64, params: &[(String, String)]) -> String {
    let raw = line(locale, key).unwrap_or_else(|| key.to_string());
    let chosen = choose(&raw, number, locale);
    let mut params = params.to_vec();
    if !params.iter().any(|(k, _)| k == "count") {
        let count = if number.fract() == 0.0 { format!("{}", number as i64) } else { number.to_string() };
        params.push(("count".into(), count));
    }
    replace(&chosen, &params)
}

/// Port of `Illuminate\Translation\MessageSelector::choose`.
pub fn choose(line: &str, number: f64, locale: &str) -> String {
    let segments: Vec<&str> = line.split('|').collect();
    for part in &segments {
        if let Some(v) = extract(part, number) {
            return v.trim().to_string();
        }
    }
    let stripped: Vec<String> = segments.iter().map(|p| strip_condition(p).to_string()).collect();
    let idx = plural_index(locale, number);
    stripped.get(idx).filter(|_| stripped.len() > 1).unwrap_or(&stripped[0]).clone()
}

fn condition(part: &str) -> Option<(&str, &str)> {
    let open = part.chars().next()?;
    if open != '{' && open != '[' {
        return None;
    }
    let body = &part[1..];
    let end = body.find(['}', ']'])?;
    let cond = &body[..end];
    cond.chars().all(|c| c.is_ascii_digit() || "-?|*,.".contains(c)).then(|| (cond, &body[end + 1..]))
}

fn php_num(s: &str) -> f64 {
    s.trim().parse().unwrap_or(0.0)
}

fn extract(part: &str, n: f64) -> Option<&str> {
    let (cond, value) = condition(part)?;
    if let Some((from, to)) = cond.split_once(',') {
        if (to == "*" && n >= php_num(from)) || (from == "*" && n <= php_num(to)) {
            return Some(value);
        }
        if from != "*" && to != "*" && n >= php_num(from) && n <= php_num(to) {
            return Some(value);
        }
    }
    (cond.parse::<f64>().ok() == Some(n)).then_some(value)
}

fn strip_condition(part: &str) -> &str {
    condition(part).map(|(_, v)| v).unwrap_or(part)
}

/// `MessageSelector::getPluralIndex` for the locales the panel ships.
pub fn plural_index(locale: &str, number: f64) -> usize {
    let n = number.abs();
    let i = n as i64;
    let lang = locale.split(['_', '-']).next().unwrap_or(locale);
    match locale {
        "pt_BR" => return usize::from(!(n == 0.0 || n == 1.0)),
        "pt_PT" => return usize::from(n != 1.0),
        _ => {}
    }
    match lang {
        "az" | "bo" | "dz" | "id" | "ja" | "jv" | "ka" | "km" | "kn" | "ko" | "ms" | "th" | "tr" | "vi" | "zh" => 0,
        "af" | "bn" | "bg" | "ca" | "da" | "de" | "el" | "en" | "eo" | "es" | "et" | "eu" | "fa" | "fi" | "fo"
        | "fur" | "fy" | "gl" | "gu" | "ha" | "he" | "hu" | "is" | "it" | "ku" | "lb" | "ml" | "mn" | "mr" | "nah"
        | "nb" | "ne" | "nl" | "nn" | "no" | "oc" | "om" | "or" | "pa" | "pap" | "ps" | "pt" | "so" | "sq" | "sv"
        | "sw" | "ta" | "te" | "tk" | "ur" | "zu" => usize::from(n != 1.0),
        "am" | "bh" | "fil" | "fr" | "guz" | "hi" | "ln" | "mg" | "nso" | "ti" | "wa" | "xbr" => {
            usize::from(!(n == 0.0 || n == 1.0))
        }
        "be" | "bs" | "hr" | "ru" | "sh" | "sr" | "uk" => {
            if i % 10 == 1 && i % 100 != 11 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "cs" | "sk" => {
            if n == 1.0 {
                0
            } else if (2.0..=4.0).contains(&n) {
                1
            } else {
                2
            }
        }
        "ga" => match i {
            1 => 0,
            2 => 1,
            _ => 2,
        },
        "lt" => {
            if i % 10 == 1 && i % 100 != 11 {
                0
            } else if i % 10 >= 2 && (i % 100 < 10 || i % 100 >= 20) {
                1
            } else {
                2
            }
        }
        "sl" => match i % 100 {
            1 => 0,
            2 => 1,
            3 | 4 => 2,
            _ => 3,
        },
        "mk" => usize::from(i % 10 != 1),
        "lv" => {
            if n == 0.0 {
                0
            } else if i % 10 == 1 && i % 100 != 11 {
                1
            } else {
                2
            }
        }
        "pl" => {
            if n == 1.0 {
                0
            } else if (2..=4).contains(&(i % 10)) && (i % 100 < 12 || i % 100 > 14) {
                1
            } else {
                2
            }
        }
        "ro" => {
            if n == 1.0 {
                0
            } else if n == 0.0 || (i % 100 > 0 && i % 100 < 20) {
                1
            } else {
                2
            }
        }
        "ar" => {
            if n == 0.0 {
                0
            } else if n == 1.0 {
                1
            } else if n == 2.0 {
                2
            } else if (3..=10).contains(&(i % 100)) {
                3
            } else if (11..=99).contains(&(i % 100)) {
                4
            } else {
                5
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(trans("en", "filament-panels::auth/pages/login.heading", &[]), "Sign in");
        assert_eq!(trans("en", "auth.failed", &[]), "These credentials do not match our records.");
        assert_eq!(trans("en", "no.such.key", &[]), "no.such.key");
        assert_eq!(
            trans("en", "filament-panels::layout.avatar.alt", &[("name".into(), "bob".into())]),
            "Avatar of bob"
        );
    }

    #[test]
    fn choices() {
        let l = "{1} Notifications, :count unread notification|[2,*] Notifications, :count unread notifications";
        assert_eq!(choose(l, 1.0, "en"), "Notifications, :count unread notification");
        assert_eq!(choose(l, 5.0, "en"), "Notifications, :count unread notifications");
        assert_eq!(choose("apple|apples", 1.0, "en"), "apple");
        assert_eq!(choose("apple|apples", 2.0, "en"), "apples");
        assert_eq!(choose("a|b|c", 22.0, "ru"), "b");
        assert_eq!(choose("a|b|c", 11.0, "ru"), "c");
        assert_eq!(choose("a|b", 0.0, "fr"), "a");
    }
}
