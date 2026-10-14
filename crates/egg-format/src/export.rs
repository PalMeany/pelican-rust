//! `EggExporterService` helpers: JSON/YAML serialisation of an export document.

use serde_json::{Map, Value};

/// `json_encode($struct, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)`.
pub fn to_json(doc: &Value) -> Option<String> {
    use laravel_compat::json::{JSON_PRETTY_PRINT, JSON_UNESCAPED_SLASHES, to_string_with_flags};
    to_string_with_flags(doc, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES).ok()
}

/// `Yaml::dump(yamlExport($struct), 10, 2, MULTI_LINE_LITERAL_BLOCK | OBJECT_AS_MAP)`.
pub fn to_yaml(doc: &Value) -> String {
    crate::yaml::dump(&yaml_export(doc))
}

/// `EggExporterService::yamlExport()`: strings that are valid JSON are decoded,
/// CRLF → LF elsewhere, long/multi-line `description` values are word-wrapped.
pub fn yaml_export(v: &Value) -> Value {
    match v {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(decoded) => yaml_export(&php_assoc(decoded)),
            Err(_) => Value::String(s.replace("\r\n", "\n")),
        },
        Value::Array(a) => Value::Array(a.iter().map(yaml_export).collect()),
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let wrapped = match val {
                    Value::String(s) if k.to_lowercase() == "description" && (s.contains('\n') || s.len() > 80) => {
                        Some(Value::String(wordwrap(s, 100)))
                    }
                    _ => None,
                };
                out.insert(k.clone(), wrapped.unwrap_or_else(|| yaml_export(val)));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `json_decode(..., true)` result as a PHP array: objects whose keys are
/// exactly `0..n-1` become lists, empty objects become empty arrays.
pub fn php_assoc(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            if m.is_empty() {
                return Value::Array(vec![]);
            }
            if m.keys().enumerate().all(|(i, k)| *k == i.to_string()) {
                Value::Array(m.into_iter().map(|(_, v)| php_assoc(v)).collect())
            } else {
                Value::Object(m.into_iter().map(|(k, v)| (k, php_assoc(v))).collect())
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(php_assoc).collect()),
        other => other,
    }
}

/// PHP `wordwrap($s, $width, "\n", false)` (single-character break fast path).
pub fn wordwrap(s: &str, width: usize) -> String {
    let mut out = s.as_bytes().to_vec();
    let text = s.as_bytes();
    let (mut laststart, mut lastspace) = (0usize, 0usize);
    for current in 0..text.len() {
        if text[current] == b'\n' {
            laststart = current + 1;
            lastspace = current + 1;
        } else if text[current] == b' ' {
            if current - laststart >= width {
                out[current] = b'\n';
                laststart = current + 1;
            }
            lastspace = current;
        } else if current - laststart >= width && laststart != lastspace {
            out[lastspace] = b'\n';
            laststart = lastspace + 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// `Egg::getKebabName()`: `str($name)->kebab()->lower()->trim()->split('/[^\w\-]/')->join('')`.
pub fn kebab_name(name: &str) -> String {
    // Str::snake($v, '-'): unless ctype_lower, ucwords + strip whitespace + '-' before uppercase.
    let snake = if !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase()) {
        name.to_string()
    } else {
        let mut words = String::new();
        let mut at_start = true;
        for c in name.chars() {
            if at_start {
                words.extend(c.to_uppercase());
            } else {
                words.push(c);
            }
            at_start = matches!(c, ' ' | '\t' | '\r' | '\n' | '\x0c' | '\x0b');
        }
        let compact: String = words.chars().filter(|c| !c.is_whitespace()).collect();
        let chars: Vec<char> = compact.chars().collect();
        let mut dashed = String::new();
        for (i, c) in chars.iter().enumerate() {
            dashed.push(*c);
            if chars.get(i + 1).is_some_and(|n| n.is_ascii_uppercase()) {
                dashed.push('-');
            }
        }
        dashed.to_lowercase()
    };
    snake.to_lowercase().trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab() {
        assert_eq!(kebab_name("Vanilla Minecraft"), "vanilla-minecraft");
        assert_eq!(kebab_name("Counter-Strike: Global Offensive"), "counter--strike-global-offensive");
        assert_eq!(kebab_name("paper"), "paper");
        assert_eq!(kebab_name("Paper 2"), "paper2");
    }

    #[test]
    fn wrap() {
        let s = "word ".repeat(30);
        let w = wordwrap(s.trim_end(), 20);
        assert!(w.lines().all(|l| l.len() <= 20));
    }
}
