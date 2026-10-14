//! Fractal-compatible envelope (`app/Data/Api/Envelope.php`, `PanelResponse.php`,
//! `IncludeContext.php`): `{object, attributes}` items, `{object: "list", data}`
//! lists, `attributes.relationships` for includes, `meta.pagination`.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::input::Input;

pub fn null_resource() -> Value {
    json!({"object": "null_resource", "attributes": null})
}

/// `{object, attributes}` with optional relationships appended last.
pub fn item(object: &str, attributes: impl Serialize, relationships: Vec<(&str, Value)>) -> Value {
    let mut attrs = match serde_json::to_value(attributes) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    if !relationships.is_empty() {
        let mut rel = Map::new();
        for (k, v) in relationships {
            rel.insert(k.to_string(), v);
        }
        attrs.insert("relationships".into(), Value::Object(rel));
    }
    let mut m = Map::new();
    m.insert("object".into(), json!(object));
    m.insert("attributes".into(), Value::Object(attrs));
    Value::Object(m)
}

pub fn list(data: Vec<Value>) -> Value {
    let mut m = Map::new();
    m.insert("object".into(), json!("list"));
    m.insert("data".into(), Value::Array(data));
    Value::Object(m)
}

/// Append a `meta` object to an item/list envelope.
pub fn with_meta(mut envelope: Value, meta: Map<String, Value>) -> Value {
    if let Value::Object(m) = &mut envelope {
        m.insert("meta".into(), Value::Object(meta));
    }
    envelope
}

/// Requested includes (`PanelResponse::requestedIncludes`): `?include=a,b.c`
/// (query or body, string or array), each cut to 2 segments, parents implied.
#[derive(Clone, Debug, Default)]
pub struct Includes {
    requested: Vec<String>,
    path: Vec<String>,
}

impl Includes {
    pub fn parse(input: &Input) -> Includes {
        let raw: Vec<String> = match input.get("include") {
            Some(Value::String(s)) => s.split(',').map(str::to_string).collect(),
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Some(Value::Object(m)) => m.values().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        let mut requested: Vec<String> = Vec::new();
        for r in raw {
            let r = r.trim();
            if r.is_empty() {
                continue;
            }
            let name = r.split(':').next().unwrap_or("");
            let segs: Vec<&str> = name.split('.').take(2).collect();
            for i in 1..=segs.len() {
                let p = segs[..i].join(".");
                if !requested.contains(&p) {
                    requested.push(p);
                }
            }
        }
        Includes { requested, path: Vec::new() }
    }

    /// Force includes (`parseIncludes([...])`).
    pub fn with(mut self, names: &[&str]) -> Includes {
        for n in names {
            if !self.requested.iter().any(|r| r == n) {
                self.requested.push((*n).to_string());
            }
        }
        self
    }

    pub fn is_requested(&self, include: &str) -> bool {
        let mut p = self.path.clone();
        p.push(include.to_string());
        self.requested.contains(&p.join("."))
    }

    pub fn descend(&self, include: &str) -> Includes {
        let mut path = self.path.clone();
        path.push(include.to_string());
        Includes { requested: self.requested.clone(), path }
    }

    /// Names to resolve: defaults, then requested available ones in declaration order.
    pub fn names<'a>(&self, defaults: &[&'a str], available: &[&'a str]) -> Vec<&'a str> {
        let mut names: Vec<&str> = defaults.to_vec();
        for a in available {
            if !names.contains(a) && self.is_requested(a) {
                names.push(a);
            }
        }
        names
    }
}

/// `meta.pagination` (PanelResponse::paginationMeta).
pub struct Pagination {
    pub total: i64,
    pub count: i64,
    pub per_page: i64,
    pub current_page: i64,
    /// `{scheme}://{host}{path}` of the request.
    pub url: String,
    /// Query parameters to keep (`appends($request->query())`), in order, without `page`.
    pub appends: Vec<(String, String)>,
}

/// PHP `http_build_query(..., PHP_QUERY_RFC3986)` value encoding.
pub fn rfc3986(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Pagination {
    pub fn total_pages(&self) -> i64 {
        ((self.total as f64) / (self.per_page.max(1) as f64)).ceil().max(1.0) as i64
    }

    fn url_for(&self, page: i64) -> String {
        let mut parts: Vec<String> =
            self.appends.iter().map(|(k, v)| format!("{}={}", rfc3986(k), rfc3986(v))).collect();
        parts.push(format!("page={page}"));
        format!("{}?{}", self.url, parts.join("&"))
    }

    pub fn meta(&self) -> Value {
        let total_pages = self.total_pages();
        let mut links = Map::new();
        if self.current_page > 1 {
            links.insert("previous".into(), json!(self.url_for(self.current_page - 1)));
        }
        if self.current_page < total_pages {
            links.insert("next".into(), json!(self.url_for(self.current_page + 1)));
        }
        let mut p = Map::new();
        p.insert("total".into(), json!(self.total));
        p.insert("count".into(), json!(self.count));
        p.insert("per_page".into(), json!(self.per_page));
        p.insert("current_page".into(), json!(self.current_page));
        p.insert("total_pages".into(), json!(total_pages));
        p.insert("links".into(), Value::Object(links));
        Value::Object(p)
    }
}

/// Flatten the raw query string into `(key, value)` pairs as PHP's
/// `$request->query()` + `http_build_query` would re-emit them, minus `page`.
pub fn query_appends(query: Option<&str>) -> Vec<(String, String)> {
    let parsed = crate::input::parse_query(query.unwrap_or(""));
    let mut out = Vec::new();
    fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m {
                    let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}[{k}]") };
                    flatten(&key, child, out);
                }
            }
            Value::String(s) => out.push((prefix.to_string(), s.clone())),
            Value::Null => {}
            other => out.push((prefix.to_string(), other.to_string())),
        }
    }
    if let Value::Object(m) = parsed {
        for (k, v) in m {
            if k == "page" {
                continue;
            }
            flatten(&k, &v, &mut out);
        }
    }
    out
}

/// `per_page = min(query('per_page', default), max)` with PHP 8 loose comparison,
/// then Eloquent's `perPage ?: 15`.
pub fn per_page(input: &Input, default: i64, max: i64) -> i64 {
    let raw = match input.get("per_page") {
        None => default.to_string(),
        Some(Value::Null) => String::new(),
        Some(v) => pelican_core::php::string(v),
    };
    let chosen = if pelican_core::php::is_numeric_str(&raw) {
        let n = raw.trim().parse::<f64>().unwrap_or(0.0);
        if n < max as f64 { n as i64 } else { max }
    } else if raw.is_empty() {
        // "" < "100" as strings → "" chosen → falsy → 15.
        0
    } else {
        // Non-numeric strings compare greater than numbers in PHP 8.
        max
    };
    if chosen == 0 { 15 } else { chosen }
}
