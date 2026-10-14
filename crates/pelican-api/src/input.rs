//! Request input as Laravel sees it (`$request->input()`): body (JSON or form)
//! united with the query string (`body + query`), after the global
//! `TrimStrings` and `ConvertEmptyStringsToNull` middleware.

use axum::body::Bytes;
use axum::http::HeaderMap;
use serde_json::{Map, Value};

/// Keys never trimmed (`TrimStrings::$except` framework defaults).
const NO_TRIM: &[&str] = &["current_password", "password", "password_confirmation"];

#[derive(Clone, Debug, Default)]
pub struct Input {
    pub data: Value,
    /// The cleaned query bag alone (`$request->query`).
    pub query: Value,
    /// Raw request body (`$request->getContent()`).
    pub raw: Bytes,
}

impl Input {
    pub fn parse(query: Option<&str>, headers: &HeaderMap, body: &Bytes, trim: bool) -> Input {
        let mut query_v = parse_query(query.unwrap_or(""));
        let ct = headers.get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        let mut body_v = if ct.contains("/json") || ct.contains("+json") {
            match serde_json::from_slice::<Value>(body) {
                Ok(Value::Object(m)) => Value::Object(m),
                // json_decode to a scalar/list: Laravel's json() bag holds it, input() sees a list's indices.
                Ok(Value::Array(a)) => {
                    Value::Object(a.into_iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect())
                }
                _ => Value::Object(Map::new()),
            }
        } else if ct.starts_with("application/x-www-form-urlencoded") {
            parse_query(std::str::from_utf8(body).unwrap_or(""))
        } else {
            Value::Object(Map::new())
        };
        clean(&mut query_v, None, trim);
        clean(&mut body_v, None, trim);
        // `getInputSource()->all() + query->all()`: body wins.
        if let (Value::Object(b), Value::Object(q)) = (&mut body_v, &query_v) {
            for (k, v) in q {
                b.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        Input { data: body_v, query: query_v, raw: body.clone() }
    }

    pub fn from_value(data: Value) -> Input {
        Input { data, ..Default::default() }
    }

    /// Symfony `$request->get($key)`: query bag first, then the body.
    pub fn get_query_first(&self, key: &str) -> Option<&Value> {
        self.query.as_object().and_then(|m| m.get(key)).or_else(|| self.data.as_object().and_then(|m| m.get(key)))
    }

    /// `Arr::get($data, 'a.b.0')`.
    pub fn get(&self, path: &str) -> Option<&Value> {
        get_path(&self.data, path)
    }

    pub fn has(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    pub fn str(&self, path: &str) -> Option<&str> {
        self.get(path).and_then(Value::as_str)
    }
}

pub fn get_path<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(v) = data.as_object().and_then(|m| m.get(path)) {
        return Some(v);
    }
    let mut cur = data;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// TrimStrings (except password keys) + ConvertEmptyStringsToNull, recursively.
fn clean(v: &mut Value, key: Option<&str>, trim: bool) {
    match v {
        Value::String(s) => {
            if trim && !key.is_some_and(|k| NO_TRIM.contains(&k)) {
                let t = php_trim(s);
                if t.len() != s.len() {
                    *s = t.to_string();
                }
            }
            if s.is_empty() {
                *v = Value::Null;
            }
        }
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                clean(child, Some(k), trim);
            }
        }
        Value::Array(a) => {
            for child in a.iter_mut() {
                clean(child, None, trim);
            }
        }
        _ => {}
    }
}

/// `TrimStrings` trims with `preg_replace('~^[\s\x{FEFF}\x{200B}\x{200E}]+|[\s...]+$~u')`.
fn php_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{FEFF}' | '\u{200B}' | '\u{200E}'))
}

/// PHP `parse_str` (bracket syntax: `a[b]=1&a[]=2`).
pub fn parse_query(q: &str) -> Value {
    let mut root = Value::Object(Map::new());
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let k = url_decode(k);
        let v = Value::String(url_decode(v));
        let (base, rest) = match k.find('[') {
            Some(i) if i > 0 => (k[..i].to_string(), Some(&k[i..])),
            _ => (k.clone(), None),
        };
        // PHP replaces `.` and ` ` in the top-level name with `_`.
        let base = base.replace(['.', ' '], "_");
        let mut keys = vec![base];
        if let Some(mut r) = rest {
            while let Some(stripped) = r.strip_prefix('[') {
                let Some(end) = stripped.find(']') else { break };
                keys.push(stripped[..end].to_string());
                r = &stripped[end + 1..];
            }
        }
        insert_path(&mut root, &keys, v);
    }
    root
}

fn insert_path(node: &mut Value, keys: &[String], v: Value) {
    let Value::Object(m) = node else { return };
    let key = if keys[0].is_empty() {
        // `a[]=` appends with the next integer key.
        let next = m.keys().filter_map(|k| k.parse::<i64>().ok()).max().map(|n| n + 1).unwrap_or(0);
        next.to_string()
    } else {
        keys[0].clone()
    };
    if keys.len() == 1 {
        m.insert(key, v);
        return;
    }
    let child = m.entry(key).or_insert_with(|| Value::Object(Map::new()));
    if !child.is_object() {
        *child = Value::Object(Map::new());
    }
    insert_path(child, &keys[1..], v);
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn query_brackets() {
        assert_eq!(
            parse_query("filter%5Bname%5D=a+b&page=2&x[]=1&x[]=2"),
            json!({"filter":{"name":"a b"},"page":"2","x":{"0":"1","1":"2"}})
        );
    }

    #[test]
    fn trims_and_nulls() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        let i = Input::parse(
            Some("q=%20x%20&e="),
            &h,
            &Bytes::from_static(br#"{"name":"  a ","password":" p ","e":""}"#),
            true,
        );
        assert_eq!(i.data, json!({"name":"a","password":" p ","e":null,"q":"x"}));
    }
}

/// Axum extractor yielding Laravel's `$request->input()` view of the request.
pub struct In(pub Input);

fn skip_trim(path: &str) -> bool {
    // RouteServiceProvider::FILE_PATH_REGEX (only the 36-char UUID form).
    let lower = path.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("/api/client/servers/") else { return false };
    let (id, tail) = rest.split_at(rest.len().min(36));
    id.len() == 36
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && (tail == "/files" || tail == "/files/" || tail.starts_with("/files/"))
}

impl<S: Send + Sync> axum::extract::FromRequest<S> for In {
    type Rejection = axum::response::Response;

    async fn from_request(req: axum::extract::Request, _state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let bytes = axum::body::to_bytes(body, 64 * 1024 * 1024).await.map_err(|_| {
            axum::response::IntoResponse::into_response(crate::error::ApiError::bad_request("Invalid request body."))
        })?;
        // `nest()` strips the `/api/client` prefix from `parts.uri`.
        let full = parts.extensions.get::<axum::extract::OriginalUri>().map(|o| o.0.path().to_string());
        let trim = !skip_trim(full.as_deref().unwrap_or(parts.uri.path()));
        Ok(In(Input::parse(parts.uri.query(), &parts.headers, &bytes, trim)))
    }
}

/// `$request->boolean($key)` = `filter_var($v, FILTER_VALIDATE_BOOLEAN)`.
pub fn php_boolean(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        _ => false,
    }
}
