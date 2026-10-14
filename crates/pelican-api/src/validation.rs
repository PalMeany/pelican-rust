//! Laravel validator semantics (`Illuminate\Validation\Validator` +
//! `Concerns\ValidatesAttributes`) for the rule subset used by the panel.
//!
//! Rules are written in Laravel's string syntax so FormRequest rules can be
//! ported verbatim: `v.rules("name", &["required", "string", "max:191"])`.
//! Messages come from the original's merged `trans('validation')` dump
//! (`data/validation.en.json`).

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::Value;

use crate::error::ApiError;
use crate::input::{Input, get_path};

static MESSAGES: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../data/validation.en.json")).unwrap_or(Value::Null));

const IMPLICIT: &[&str] = &[
    "accepted",
    "accepted_if",
    "declined",
    "declined_if",
    "filled",
    "missing",
    "missing_if",
    "missing_unless",
    "missing_with",
    "missing_with_all",
    "present",
    "present_if",
    "present_unless",
    "present_with",
    "present_with_all",
    "required",
    "required_if",
    "required_if_accepted",
    "required_if_declined",
    "required_unless",
    "required_with",
    "required_with_all",
    "required_without",
    "required_without_all",
];
const SIZE_RULES: &[&str] = &["size", "between", "min", "max", "gt", "lt", "gte", "lte"];
const NUMERIC_RULES: &[&str] = &["numeric", "integer", "decimal"];
const DEPENDENT: &[&str] = &[
    "required_with",
    "required_with_all",
    "required_without",
    "required_without_all",
    "required_if",
    "required_unless",
    "present_with",
    "present_with_all",
    "same",
    "different",
    "confirmed",
];

impl CustomRule {
    /// A rule whose outcome was computed beforehand (e.g. an async DB lookup).
    pub fn precomputed(name: &str, failure: Option<String>) -> CustomRule {
        CustomRule { name: name.to_string(), check: Box::new(move |_, _| failure.clone()) }
    }
}

/// A custom rule: returns `Some(message)` on failure. `name` is what Pelican
/// reports as `meta.rule` (snake case of the rule class).
pub struct CustomRule {
    pub name: String,
    pub check: Box<dyn Fn(&str, &Value) -> Option<String> + Send + Sync>,
}

enum RuleSpec {
    Named { name: String, params: Vec<String> },
    Custom(CustomRule),
}

struct AttrRules {
    attribute: String,
    /// The wildcard pattern this attribute was expanded from.
    primary: Option<String>,
    rules: Vec<RuleSpec>,
}

pub struct Validator<'a> {
    data: &'a Value,
    attrs: Vec<AttrRules>,
    messages: HashMap<String, String>,
    attribute_names: HashMap<String, String>,
    /// (attribute, rule snake, message) in failure order.
    errors: Vec<(String, String, String)>,
    failed: HashMap<String, Vec<String>>,
    ran: bool,
}

fn parse_rule(rule: &str) -> RuleSpec {
    let (name, params) = match rule.split_once(':') {
        Some((n, p)) => {
            let n = n.trim().to_ascii_lowercase();
            // regex patterns may contain commas.
            let params = if n == "regex" || n == "not_regex" {
                vec![p.to_string()]
            } else {
                p.split(',').map(|s| s.to_string()).collect()
            };
            (n, params)
        }
        None => (rule.trim().to_ascii_lowercase(), Vec::new()),
    };
    RuleSpec::Named { name, params }
}

impl<'a> Validator<'a> {
    pub fn new(input: &'a Input) -> Self {
        Self::for_value(&input.data)
    }

    pub fn for_value(data: &'a Value) -> Self {
        Validator {
            data,
            attrs: Vec::new(),
            messages: HashMap::new(),
            attribute_names: HashMap::new(),
            errors: Vec::new(),
            failed: HashMap::new(),
            ran: false,
        }
    }

    /// Add rules for an attribute (supports `*` wildcards, expanded against the data).
    pub fn rules(&mut self, attribute: &str, rules: &[&str]) -> &mut Self {
        let specs = || rules.iter().filter(|r| !r.is_empty()).map(|r| parse_rule(r)).collect::<Vec<_>>();
        if attribute.contains('*') {
            for concrete in expand_wildcard(self.data, attribute) {
                self.attrs.push(AttrRules {
                    attribute: concrete,
                    primary: Some(attribute.to_string()),
                    rules: specs(),
                });
            }
        } else {
            self.attrs.push(AttrRules { attribute: attribute.to_string(), primary: None, rules: specs() });
        }
        self
    }

    /// Append a custom rule object to an attribute (after its string rules).
    pub fn custom(&mut self, attribute: &str, rule: CustomRule) -> &mut Self {
        match self.attrs.iter_mut().find(|a| a.attribute == attribute) {
            Some(a) => a.rules.push(RuleSpec::Custom(rule)),
            None => self.attrs.push(AttrRules {
                attribute: attribute.into(),
                primary: None,
                rules: vec![RuleSpec::Custom(rule)],
            }),
        }
        self
    }

    /// FormRequest `messages()`: keys `attribute.rule` or `rule`.
    pub fn message(&mut self, key: &str, message: &str) -> &mut Self {
        self.messages.insert(key.into(), message.into());
        self
    }

    /// FormRequest `attributes()`.
    pub fn attribute_name(&mut self, attribute: &str, name: &str) -> &mut Self {
        self.attribute_names.insert(attribute.into(), name.into());
        self
    }

    /// Add an error from an `after()` hook (Pelican reports `rule: ""`).
    pub fn add_error(&mut self, attribute: &str, message: &str) {
        self.errors.push((attribute.to_string(), String::new(), message.to_string()));
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn errors_for(&self, attribute: &str) -> bool {
        self.errors.iter().any(|e| e.0 == attribute)
    }

    /// Run every rule (once). Returns the 422 error if anything failed.
    pub fn validate(&mut self) -> Result<(), ApiError> {
        if !self.ran {
            self.run();
        }
        if self.errors.is_empty() { Ok(()) } else { Err(ApiError::validation(self.errors.clone())) }
    }

    /// Run rules without converting to an error (for `after` hooks).
    pub fn run(&mut self) {
        self.ran = true;
        let attrs = std::mem::take(&mut self.attrs);
        for a in &attrs {
            let names: Vec<&str> = a
                .rules
                .iter()
                .filter_map(|r| match r {
                    RuleSpec::Named { name, .. } => Some(name.as_str()),
                    RuleSpec::Custom(_) => None,
                })
                .collect();
            let has = |n: &str| names.contains(&n);
            for rule in &a.rules {
                self.validate_attribute(a, rule, &names);
                if has("bail") && self.errors_for(&a.attribute) {
                    break;
                }
                if names.iter().any(|n| IMPLICIT.contains(n))
                    && self.failed.get(&a.attribute).is_some_and(|f| f.iter().any(|r| IMPLICIT.contains(&r.as_str())))
                {
                    break;
                }
            }
        }
        self.attrs = attrs;
    }

    fn value(&self, attribute: &str) -> Option<&'a Value> {
        get_path(self.data, attribute)
    }

    fn validate_attribute(&mut self, a: &AttrRules, rule: &RuleSpec, names: &[&str]) {
        let attribute = a.attribute.as_str();
        let value = self.value(attribute);
        let (name, params) = match rule {
            RuleSpec::Named { name, params } => (name.as_str(), params.clone()),
            RuleSpec::Custom(c) => {
                if !self.is_validatable("custom", attribute, value, names) {
                    return;
                }
                // hasNotFailedPreviousRuleIfPresenceRule for unique/exists rule objects.
                if (c.name == "unique" || c.name == "exists") && self.errors_for(attribute) {
                    return;
                }
                if let Some(msg) = (c.check)(attribute, value.unwrap_or(&Value::Null)) {
                    let msg = msg.replace(":attribute", &self.display_attribute(a));
                    self.errors.push((attribute.to_string(), c.name.clone(), msg));
                    self.failed.entry(attribute.to_string()).or_default().push(c.name.clone());
                }
                return;
            }
        };
        if matches!(name, "nullable" | "sometimes" | "bail") {
            return;
        }
        let params: Vec<String> = if DEPENDENT.contains(&name) {
            params.iter().map(|p| replace_asterisks(p, attribute, a.primary.as_deref())).collect()
        } else {
            params
        };
        if !self.is_validatable(name, attribute, value, names) {
            return;
        }
        if name == "unique" || name == "exists" {
            // hasNotFailedPreviousRuleIfPresenceRule
            if self.errors_for(attribute) {
                return;
            }
        }
        let has_numeric = names.iter().any(|n| NUMERIC_RULES.contains(n));
        let ok = self.check(name, attribute, value, &params, has_numeric, names);
        if !ok {
            let msg = self.make_message(a, name, &params, names);
            self.errors.push((attribute.to_string(), name.to_string(), msg));
            self.failed.entry(attribute.to_string()).or_default().push(name.to_string());
        }
    }

    fn is_validatable(&self, rule: &str, attribute: &str, value: Option<&Value>, names: &[&str]) -> bool {
        let implicit = IMPLICIT.contains(&rule);
        // presentOrRuleIsImplicit
        let present_or_implicit = match value {
            Some(Value::String(s)) if s.trim().is_empty() => implicit,
            Some(_) => true,
            None => implicit,
        };
        // passesOptionalCheck
        let optional_ok = !names.contains(&"sometimes") || value.is_some();
        // isNotNullIfMarkedAsNullable
        let nullable_ok = implicit || !names.contains(&"nullable") || !matches!(value, Some(Value::Null));
        let _ = attribute;
        present_or_implicit && optional_ok && nullable_ok
    }

    fn other(&self, field: &str) -> Option<&'a Value> {
        self.value(field)
    }

    fn check(
        &self,
        rule: &str,
        attribute: &str,
        value: Option<&Value>,
        p: &[String],
        has_numeric: bool,
        names: &[&str],
    ) -> bool {
        let v = value.unwrap_or(&Value::Null);
        let size = || size_of(v, has_numeric);
        let num = |i: usize| p.get(i).and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
        match rule {
            "required" => is_required(value),
            "filled" => value.is_none() || is_required(value),
            "present" => value.is_some(),
            "required_with" => !p.iter().any(|f| is_required(self.other(f))) || is_required(value),
            "required_with_all" => !p.iter().all(|f| is_required(self.other(f))) || is_required(value),
            "required_without" => !p.iter().any(|f| !is_required(self.other(f))) || is_required(value),
            "required_without_all" => !p.iter().all(|f| !is_required(self.other(f))) || is_required(value),
            "required_if" => !self.dependent_matches(&p[0], &p[1..]) || is_required(value),
            "required_unless" => self.dependent_matches(&p[0], &p[1..]) || is_required(value),
            "string" => v.is_string(),
            "integer" => {
                if p.first().map(String::as_str) == Some("strict") {
                    v.is_i64() || v.is_u64()
                } else {
                    filter_int(v)
                }
            }
            "numeric" => is_numeric(v),
            "boolean" => match v {
                Value::Bool(_) => true,
                Value::Number(n) => n.as_i64().is_some_and(|i| i == 0 || i == 1),
                Value::String(s) => s == "0" || s == "1",
                _ => false,
            },
            "array" | "list" => match v {
                Value::Object(m) => {
                    if rule == "list" {
                        m.keys().enumerate().all(|(i, k)| k == &i.to_string())
                    } else {
                        p.is_empty() || m.keys().all(|k| p.contains(k))
                    }
                }
                Value::Array(_) => true,
                _ => false,
            },
            "max" => size() <= num(0),
            "min" => size() >= num(0),
            "between" => {
                let s = size();
                s >= num(0) && s <= num(1)
            }
            "size" => size() == num(0),
            "in" => match v {
                Value::Array(items) if names.contains(&"array") => items
                    .iter()
                    .all(|e| !e.is_array() && !e.is_object() && p.iter().any(|x| loose_eq(&php_string(e), x))),
                Value::Array(_) | Value::Object(_) => false,
                _ => p.iter().any(|x| loose_eq(&php_string(v), x)),
            },
            "not_in" => match v {
                Value::Array(_) | Value::Object(_) => false,
                _ => !p.iter().any(|x| loose_eq(&php_string(v), x)),
            },
            "regex" | "not_regex" => {
                if !(v.is_string() || is_numeric(v)) {
                    return false;
                }
                let m = php_regex_match(&p[0], &php_string(v));
                if rule == "regex" { m } else { !m }
            }
            "uuid" => v.as_str().is_some_and(is_uuid),
            "ip" => v.as_str().is_some_and(|s| s.parse::<std::net::IpAddr>().is_ok()),
            "ipv4" => v.as_str().is_some_and(|s| s.parse::<std::net::Ipv4Addr>().is_ok()),
            "ipv6" => v.as_str().is_some_and(|s| s.parse::<std::net::Ipv6Addr>().is_ok()),
            "email" => v.as_str().is_some_and(is_email),
            "alpha_dash" => {
                (v.is_string() || is_numeric(v))
                    && php_string(v).chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    && !php_string(v).is_empty()
            }
            "alpha_num" => {
                (v.is_string() || is_numeric(v))
                    && php_string(v).chars().all(char::is_alphanumeric)
                    && !php_string(v).is_empty()
            }
            "alpha" => v.is_string() && v.as_str().is_some_and(|s| !s.is_empty() && s.chars().all(char::is_alphabetic)),
            "json" => v.as_str().is_some_and(|s| serde_json::from_str::<Value>(s).is_ok()),
            "accepted" => matches!(php_string(v).as_str(), "yes" | "on" | "1" | "true") || v == &Value::Bool(true),
            "starts_with" => p.iter().any(|x| php_string(v).starts_with(x.as_str())),
            "ends_with" => p.iter().any(|x| php_string(v).ends_with(x.as_str())),
            "lowercase" => php_string(v) == php_string(v).to_lowercase(),
            "digits" => {
                let s = php_string(v);
                s.chars().all(|c| c.is_ascii_digit()) && s.len() as f64 == num(0)
            }
            "digits_between" => {
                let s = php_string(v);
                let l = s.len() as f64;
                s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() && l >= num(0) && l <= num(1)
            }
            "confirmed" => {
                let other = self.other(&format!("{attribute}_confirmation"));
                other == value
            }
            "distinct" => {
                let Some(primary) = self.attrs_primary_for(attribute) else { return true };
                let siblings = expand_wildcard(self.data, &primary);
                let mine = php_string(v);
                siblings
                    .iter()
                    .filter(|s| s.as_str() != attribute)
                    .all(|s| self.value(s).map(php_string) != Some(mine.clone()))
            }
            "date" => v.as_str().is_some_and(|s| {
                chrono::DateTime::parse_from_rfc3339(s).is_ok()
                    || chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").is_ok()
                    || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
            }),
            "timezone" => v.as_str().is_some_and(is_timezone),
            "url" => v.as_str().is_some_and(|s| s.starts_with("http://") || s.starts_with("https://")),
            other => {
                tracing::error!("validation rule `{other}` is not implemented");
                false
            }
        }
    }

    /// `parseDependentRuleParameters` + `in_array($other, $values, is_bool||is_null)`.
    fn dependent_matches(&self, field: &str, values: &[String]) -> bool {
        let other = self.other(field).cloned().unwrap_or(Value::Null);
        let other_is_bool_rule = self.attrs.iter().any(|a| {
            a.attribute == field
                && a.rules
                    .iter()
                    .any(|r| matches!(r, RuleSpec::Named { name, .. } if name == "boolean" || name == "accepted"))
        });
        let convert_bool = other_is_bool_rule || other.is_boolean();
        let converted: Vec<Value> = values
            .iter()
            .map(|x| match x.as_str() {
                "true" if convert_bool => Value::Bool(true),
                "false" if convert_bool => Value::Bool(false),
                "null" if other.is_null() => Value::Null,
                _ => Value::String(x.clone()),
            })
            .collect();
        let strict = other.is_boolean() || other.is_null();
        converted.iter().any(|c| if strict { c == &other } else { php_loose_eq(&other, c) })
    }

    fn attrs_primary_for(&self, _attribute: &str) -> Option<String> {
        // `distinct` is only meaningful on wildcard attributes; resolved by caller-provided pattern.
        None
    }

    fn display_attribute(&self, a: &AttrRules) -> String {
        if let Some(n) = self.attribute_names.get(&a.attribute) {
            return n.clone();
        }
        if let Some(p) = &a.primary {
            if let Some(n) = self.attribute_names.get(p) {
                return n.clone();
            }
            // implicitAttributes: wildcard-expanded names are shown raw.
            return a.attribute.clone();
        }
        snake(&a.attribute).replace('_', " ")
    }

    fn make_message(&self, a: &AttrRules, rule: &str, params: &[String], names: &[&str]) -> String {
        let attr = &a.attribute;
        let template = self
            .messages
            .get(&format!("{attr}.{rule}"))
            .or_else(|| a.primary.as_ref().and_then(|p| self.messages.get(&format!("{p}.{rule}"))))
            .or_else(|| self.messages.get(rule))
            .cloned()
            .or_else(|| {
                let m = MESSAGES.get(rule)?;
                match m {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(by_type) if SIZE_RULES.contains(&rule) => {
                        let t = if names.iter().any(|n| NUMERIC_RULES.contains(n)) {
                            "numeric"
                        } else if names.iter().any(|n| *n == "array" || *n == "list") {
                            "array"
                        } else {
                            "string"
                        };
                        by_type.get(t).and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                }
            })
            .unwrap_or_else(|| format!("validation.{rule}"));
        let display = self.display_attribute(a);
        let mut msg = template;
        let first = params.first().cloned().unwrap_or_default();
        match rule {
            "max" | "min" | "size" | "digits" => {
                let key = if rule == "digits" { ":digits" } else { &format!(":{rule}") };
                msg = msg.replace(key, &first);
            }
            "between" | "digits_between" => {
                msg = msg.replace(":min", &first).replace(":max", params.get(1).map(String::as_str).unwrap_or(""));
            }
            "in" | "not_in" | "starts_with" | "ends_with" => {
                msg = msg.replace(":values", &params.join(", "));
            }
            "required_with" | "required_with_all" | "required_without" | "required_without_all" => {
                let vals: Vec<String> = params.iter().map(|p| snake(p).replace('_', " ")).collect();
                msg = msg.replace(":values", &vals.join(" / "));
            }
            "required_if" | "required_unless" => {
                let other = params.first().map(|p| snake(p).replace('_', " ")).unwrap_or_default();
                msg = msg.replace(":other", &other);
                if rule == "required_if" {
                    msg = msg.replace(":value", params.get(1).map(String::as_str).unwrap_or(""));
                } else {
                    msg = msg.replace(":values", &params[1..].join(", "));
                }
            }
            _ => {}
        }
        replace_attribute(&msg, &display)
    }
}

fn replace_attribute(msg: &str, display: &str) -> String {
    let mut upper = display.chars();
    let ucfirst = match upper.next() {
        Some(c) => c.to_uppercase().collect::<String>() + upper.as_str(),
        None => String::new(),
    };
    msg.replace(":ATTRIBUTE", &display.to_uppercase()).replace(":Attribute", &ucfirst).replace(":attribute", display)
}

/// `Str::snake` for attribute names (lower-case input stays as is).
fn snake(s: &str) -> String {
    if !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn replace_asterisks(param: &str, attribute: &str, primary: Option<&str>) -> String {
    let Some(primary) = primary else { return param.to_string() };
    // Explicit keys = the concrete segments matching `*` in the primary pattern.
    let keys: Vec<&str> =
        primary.split('.').zip(attribute.split('.')).filter(|(p, _)| *p == "*").map(|(_, a)| a).collect();
    let mut out = param.to_string();
    for k in keys {
        if let Some(pos) = out.find('*') {
            out.replace_range(pos..pos + 1, k);
        }
    }
    out
}

/// Expand `a.*.b` against the data into concrete attribute names.
pub fn expand_wildcard(data: &Value, pattern: &str) -> Vec<String> {
    let segs: Vec<&str> = pattern.split('.').collect();
    let mut out = Vec::new();
    fn walk(v: &Value, segs: &[&str], prefix: String, out: &mut Vec<String>) {
        if segs.is_empty() {
            out.push(prefix);
            return;
        }
        let join = |p: &str, s: &str| if p.is_empty() { s.to_string() } else { format!("{p}.{s}") };
        if segs[0] == "*" {
            match v {
                Value::Array(a) => {
                    for (i, child) in a.iter().enumerate() {
                        walk(child, &segs[1..], join(&prefix, &i.to_string()), out);
                    }
                }
                Value::Object(m) => {
                    for (k, child) in m {
                        walk(child, &segs[1..], join(&prefix, k), out);
                    }
                }
                _ => {}
            }
        } else {
            let child = match v {
                Value::Object(m) => m.get(segs[0]).unwrap_or(&Value::Null),
                _ => &Value::Null,
            };
            if segs.len() == 1 || !child.is_null() {
                walk(child, &segs[1..], join(&prefix, segs[0]), out);
            }
        }
    }
    walk(data, &segs, String::new(), &mut out);
    out
}

/// `validateRequired`.
fn is_required(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
        Some(_) => true,
    }
}

/// PHP `(string)` cast of a JSON value.
pub fn php_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) => String::new(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => laravel_compat::php_serialize::PhpFloat(f).to_string(),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        _ => "Array".into(),
    }
}

/// PHP 8 `is_numeric`.
pub fn is_numeric(v: &Value) -> bool {
    match v {
        Value::Number(_) => true,
        Value::String(s) => is_numeric_str(s),
        _ => false,
    }
}

pub fn is_numeric_str(s: &str) -> bool {
    let t = s
        .trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c'])
        .trim_end_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    if t.is_empty() {
        return false;
    }
    let b = t.as_bytes();
    let mut i = 0;
    if b[0] == b'+' || b[0] == b'-' {
        i = 1;
    }
    let (mut digits, mut dot) = (0, false);
    while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'.' && !dot)) {
        if b[i] == b'.' {
            dot = true;
        } else {
            digits += 1;
        }
        i += 1;
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

/// Loose `==` between a (string) value and a rule parameter.
fn loose_eq(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if is_numeric_str(a) && is_numeric_str(b) {
        return a.trim().parse::<f64>().ok() == b.trim().parse::<f64>().ok();
    }
    false
}

/// PHP 8 loose `==` between two scalar JSON values.
pub fn php_loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Bool(x), other) | (other, Value::Bool(x)) => *x == !php_falsy(other),
        (Value::Null, other) | (other, Value::Null) => {
            php_falsy(other) && !matches!(other, Value::String(s) if s == "0")
        }
        (Value::Number(_), Value::String(s)) | (Value::String(s), Value::Number(_)) => {
            let n = if let Value::Number(n) = a {
                n
            } else if let Value::Number(n) = b {
                n
            } else {
                return false;
            };
            is_numeric_str(s) && s.trim().parse::<f64>().ok() == n.as_f64()
        }
        (Value::String(x), Value::String(y)) => loose_eq(x, y),
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => false,
    }
}

fn php_falsy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => true,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty() || s == "0",
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// `filter_var($v, FILTER_VALIDATE_INT)`.
fn filter_int(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0 && f.abs() < 9.2e18)
        }
        Value::Bool(b) => *b,
        Value::String(s) => {
            let t = s.trim();
            let body = t.strip_prefix(['+', '-']).unwrap_or(t);
            !body.is_empty()
                && body.bytes().all(|c| c.is_ascii_digit())
                && (body == "0" || !body.starts_with('0'))
                && t.parse::<i64>().is_ok()
        }
        _ => false,
    }
}

/// `getSize`: number if numeric rule + numeric value, count for arrays, else mb_strlen.
fn size_of(v: &Value, has_numeric: bool) -> f64 {
    if has_numeric && is_numeric(v) {
        return php_string(v).trim().parse::<f64>().unwrap_or(0.0);
    }
    match v {
        Value::Array(a) => a.len() as f64,
        Value::Object(m) => m.len() as f64,
        other => php_string(other).chars().count() as f64,
    }
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter()
            .enumerate()
            .all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else { return false };
    !local.is_empty()
        && !domain.is_empty()
        && !s.contains(char::is_whitespace)
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

fn is_timezone(s: &str) -> bool {
    s == "UTC" || (s.contains('/') && s.chars().all(|c| c.is_ascii_alphanumeric() || "/_-+".contains(c)))
}

/// `preg_match` with a PHP-delimited pattern (`/…/flags`).
pub fn php_regex_match(pattern: &str, subject: &str) -> bool {
    let Some(delim) = pattern.chars().next() else { return false };
    let close = match delim {
        '(' => ')',
        '{' => '}',
        '[' => ']',
        '<' => '>',
        d => d,
    };
    let Some(end) = pattern.rfind(close).filter(|e| *e > 0) else { return false };
    let body = &pattern[delim.len_utf8()..end];
    let flags = &pattern[end + close.len_utf8()..];
    let mut prefix = String::new();
    for f in flags.chars() {
        match f {
            'i' | 'm' | 's' | 'x' => prefix.push(f),
            _ => {}
        }
    }
    let src = if prefix.is_empty() { body.to_string() } else { format!("(?{prefix}){body}") };
    fancy_regex::Regex::new(&src).ok().and_then(|re| re.is_match(subject).ok()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(data: Value, f: impl FnOnce(&mut Validator)) -> Vec<(String, String, String)> {
        let input = Input::from_value(data);
        let mut v = Validator::new(&input);
        f(&mut v);
        v.run();
        v.errors.clone()
    }

    #[test]
    fn required_stops() {
        let e = run(json!({}), |v| {
            v.rules("name", &["required", "string", "max:5"]);
        });
        assert_eq!(e, vec![("name".into(), "required".into(), "The name field is required.".into())]);
    }

    #[test]
    fn size_messages_by_type() {
        let e = run(json!({"name": "toolongvalue", "count": 50, "allowed_ips": ["1.2.3.4", "nope"]}), |v| {
            v.rules("name", &["string", "max:5"]);
            v.rules("count", &["integer", "max:10"]);
            v.rules("allowed_ips.*", &["ip"]);
        });
        assert_eq!(e[0].2, "The name may not be greater than 5 characters.");
        assert_eq!(e[1].2, "The count may not be greater than 10.");
        assert_eq!(e[2], ("allowed_ips.1".into(), "ip".into(), "The allowed_ips.1 must be a valid IP address.".into()));
    }

    #[test]
    fn nullable_and_sometimes() {
        let e = run(json!({"a": null}), |v| {
            v.rules("a", &["nullable", "string"]);
            v.rules("b", &["sometimes", "string"]);
            v.rules("c", &["string"]);
        });
        assert!(e.is_empty(), "{e:?}");
        let e = run(json!({"a": null}), |v| {
            v.rules("a", &["string"]);
        });
        assert_eq!(e[0].1, "string");
    }

    #[test]
    fn regex_and_in() {
        assert!(php_regex_match("/^[\\w\\-]+$/", "abc-1"));
        assert!(!php_regex_match("/^[a-z]+$/", "ABC"));
        assert!(php_regex_match("/^[a-z]+$/i", "ABC"));
        let e = run(json!({"signal": "boom"}), |v| {
            v.rules("signal", &["required", "string", "in:start,stop,restart,kill"]);
        });
        assert_eq!(e[0].2, "The selected signal is invalid.");
    }
}
