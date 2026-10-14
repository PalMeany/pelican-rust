//! Urlencoded form input and per-field validation errors for page handlers.

use axum::body::to_bytes;
use axum::extract::{FromRequest, Request};
use indexmap::IndexMap;
use pelican_api::validation::Validator;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::WebError;

/// Submitted fields in order; `name[]` collects lists.
#[derive(Clone, Debug, Default)]
pub struct Form {
    pub fields: IndexMap<String, Vec<String>>,
}

impl Form {
    pub fn parse(body: &[u8]) -> Form {
        let mut fields: IndexMap<String, Vec<String>> = IndexMap::new();
        for (k, v) in form_urlencoded::parse(body) {
            if k == "_token" {
                continue;
            }
            fields.entry(k.into_owned()).or_default().push(v.into_owned());
        }
        Form { fields }
    }

    pub fn get(&self, key: &str) -> &str {
        self.fields.get(key).and_then(|v| v.last()).map(String::as_str).unwrap_or("")
    }

    pub fn opt(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| v.last()).map(String::as_str)
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.fields.get(&format!("{key}[]")).or_else(|| self.fields.get(key)).cloned().unwrap_or_default()
    }

    /// Local return URL from the `_back` field (tables add it to row actions).
    pub fn back(&self, default: &str) -> String {
        self.opt("_back")
            .filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains('\\'))
            .unwrap_or(default)
            .to_string()
    }

    /// Checkbox/toggle: present and not `0`/`false`/empty.
    pub fn bool(&self, key: &str) -> bool {
        self.opt(key).is_some_and(|v| !matches!(v, "" | "0" | "false" | "off"))
    }

    /// Data for the validator: blank strings become null (Laravel's
    /// `ConvertEmptyStringsToNull`), `a[]` becomes a list under `a`.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        for (k, vals) in &self.fields {
            if let Some(base) = k.strip_suffix("[]") {
                m.insert(base.to_string(), Value::Array(vals.iter().map(|v| Value::String(v.clone())).collect()));
            } else if let Some(v) = vals.last() {
                let t = v.trim();
                m.insert(k.clone(), if t.is_empty() { Value::Null } else { Value::String(t.to_string()) });
            }
        }
        Value::Object(m)
    }
}

impl<S: Send + Sync> FromRequest<S> for Form {
    type Rejection = WebError;

    async fn from_request(req: Request, _state: &S) -> Result<Self, Self::Rejection> {
        let bytes = to_bytes(req.into_body(), 16 * 1024 * 1024).await.map_err(|_| WebError::PayloadTooLarge)?;
        Ok(Form::parse(&bytes))
    }
}

/// First message per field, rendered next to the inputs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Errors(pub IndexMap<String, String>);

impl Errors {
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0.entry(field.to_string()).or_insert_with(|| message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn from_validator(v: &Validator) -> Errors {
        let mut e = Errors::default();
        for (field, msg) in v.error_messages() {
            e.add(&field, msg);
        }
        e
    }
}
