//! Minimal Laravel translator for the strings the backend renders itself
//! (database notifications), dumped from the original's `lang/*`.

use std::sync::LazyLock;

use serde_json::Value;

static LANG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../data/i18n.json")).unwrap_or(Value::Null));

fn lookup<'a>(root: &'a Value, locale: &str, key: &str) -> Option<&'a str> {
    let mut cur = root.get(locale)?;
    for seg in key.split('.') {
        cur = cur.get(seg)?;
    }
    cur.as_str()
}

/// `trans($key, $replace, $locale)` with fallback to `en`; `:name`, `:Name`, `:NAME` placeholders.
pub fn trans(locale: &str, key: &str, replace: &[(&str, &str)]) -> String {
    let line = lookup(&LANG, locale, key).or_else(|| lookup(&LANG, "en", key)).unwrap_or(key);
    let mut out = line.to_string();
    // Longer keys first, like Laravel's sortReplacements.
    let mut reps: Vec<&(&str, &str)> = replace.iter().collect();
    reps.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    for (k, v) in reps {
        let mut cap = k.chars();
        let ucfirst = cap.next().map(|c| c.to_uppercase().collect::<String>() + cap.as_str()).unwrap_or_default();
        let mut vcap = v.chars();
        let v_ucfirst = vcap.next().map(|c| c.to_uppercase().collect::<String>() + vcap.as_str()).unwrap_or_default();
        out = out
            .replace(&format!(":{}", k.to_uppercase()), &v.to_uppercase())
            .replace(&format!(":{ucfirst}"), &v_ucfirst)
            .replace(&format!(":{k}"), v);
    }
    out
}
