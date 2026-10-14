//! spatie/laravel-query-builder semantics: `filter[...]`, `sort` (client-api.md §7.5).

use pelican_db::raw;
use sea_orm::{DbBackend, Value as DbValue};
use serde_json::Value;

use crate::error::{ApiError, Style};
use crate::input::Input;

#[derive(Clone, Debug, PartialEq)]
pub enum FilterValue {
    Str(String),
    Bool(bool),
    List(Vec<FilterValue>),
    Null,
}

impl FilterValue {
    /// PHP `(string)` cast.
    pub fn as_php_string(&self) -> String {
        match self {
            FilterValue::Str(s) => s.clone(),
            FilterValue::Bool(true) => "1".into(),
            FilterValue::Bool(false) | FilterValue::Null => String::new(),
            FilterValue::List(_) => "Array".into(),
        }
    }
}

fn convert(v: &Value) -> FilterValue {
    match v {
        // getFilterValue: empty values pass through unchanged.
        Value::Null => FilterValue::Null,
        Value::String(s) if s == "true" => FilterValue::Bool(true),
        Value::String(s) if s == "false" => FilterValue::Bool(false),
        // splitFilterValue with the default "," delimiter.
        Value::String(s) if s.contains(',') => {
            FilterValue::List(s.split(',').map(|p| FilterValue::Str(p.to_string())).collect())
        }
        Value::String(s) => FilterValue::Str(s.clone()),
        Value::Bool(b) => FilterValue::Bool(*b),
        Value::Number(n) => FilterValue::Str(n.to_string()),
        Value::Array(a) => FilterValue::List(a.iter().map(convert).collect()),
        Value::Object(m) => FilterValue::List(m.values().map(convert).collect()),
    }
}

/// Parse `filter[...]`; unknown names → `InvalidFilterQuery` 400.
pub fn filters(input: &Input, allowed: &[&str]) -> Result<Vec<(String, FilterValue)>, ApiError> {
    let raw = match input.get("filter") {
        Some(Value::Object(m)) => m.clone(),
        _ => return Ok(Vec::new()),
    };
    let unknown: Vec<&String> = raw.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
    if !unknown.is_empty() {
        let u = unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Err(ApiError::new(
            400,
            "InvalidFilterQuery",
            format!("Requested filter(s) `{u}` are not allowed. Allowed filter(s) are `{}`.", allowed.join(", ")),
            Style::Pretty,
        ));
    }
    Ok(raw.iter().map(|(k, v)| (k.clone(), convert(v))).collect())
}

/// Parse `sort=a,-b`; unknown names → `InvalidSortQuery` 400. Returns (name, descending).
pub fn sorts(input: &Input, allowed: &[&str]) -> Result<Vec<(String, bool)>, ApiError> {
    let raw = match input.get("sort") {
        Some(Value::String(s)) => s.clone(),
        _ => return Ok(Vec::new()),
    };
    let parsed: Vec<(String, bool)> = raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix('-') {
            Some(n) => (n.to_string(), true),
            None => (s.to_string(), false),
        })
        .collect();
    let unknown: Vec<&str> =
        parsed.iter().filter(|(n, _)| !allowed.contains(&n.as_str())).map(|(n, _)| n.as_str()).collect();
    if !unknown.is_empty() {
        return Err(ApiError::new(
            400,
            "InvalidSortQuery",
            format!(
                "Requested sort(s) `{}` is not allowed. Allowed sort(s) are `{}`.",
                unknown.join(", "),
                allowed.join(", ")
            ),
            Style::Pretty,
        ));
    }
    Ok(parsed)
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('_', "\\_").replace('%', "\\%")
}

/// `FiltersPartial`: `LOWER(col) LIKE ?` (OR-group for lists). Returns a SQL
/// fragment with `?` placeholders (convert with `raw::stmt`) and its values.
pub fn partial(backend: DbBackend, column: &str, value: &FilterValue) -> Option<(String, Vec<DbValue>)> {
    let esc = if backend == DbBackend::Sqlite { " ESCAPE '\\'" } else { "" };
    let one = |v: &FilterValue| {
        let lowered = v.as_php_string().to_lowercase();
        (
            format!("LOWER({}) LIKE ?{esc}", raw::q(backend, column)),
            DbValue::from(format!("%{}%", escape_like(&lowered))),
        )
    };
    match value {
        FilterValue::Null => None,
        FilterValue::List(items) => {
            let items: Vec<&FilterValue> = items.iter().filter(|i| !i.as_php_string().is_empty()).collect();
            if items.is_empty() {
                return None;
            }
            let parts: Vec<(String, DbValue)> = items.into_iter().map(one).collect();
            let sql = format!("({})", parts.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>().join(" or "));
            Some((sql, parts.into_iter().map(|(_, v)| v).collect()))
        }
        other => {
            let (s, v) = one(other);
            Some((s, vec![v]))
        }
    }
}
