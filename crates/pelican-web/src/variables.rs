//! Egg/server startup variables as form fields
//! (app/Filament/Components/Forms/Fields/StartupVariable.php).

use pelican_db::entities::egg_variables;
use serde::Serialize;
use serde_json::Value;

/// `StartupVariableType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Number,
    Toggle,
    Select,
}

#[derive(Clone, Debug, Serialize)]
pub struct VariableField {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub env: String,
    pub default: String,
    pub rules: Vec<String>,
    pub value: String,
    pub editable: bool,
    pub viewable: bool,
    pub kind: Kind,
    pub options: Vec<String>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub required: bool,
}

/// Rules of a variable: stored as a JSON list (or a legacy `a|b` string).
pub fn rules_of(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|r| r.as_str().map(String::from)).collect(),
        Value::String(s) => s.split('|').map(String::from).filter(|r| !r.is_empty()).collect(),
        _ => Vec::new(),
    }
}

fn bound(rules: &[String], prefix: &str) -> Option<i64> {
    rules.iter().find_map(|r| r.strip_prefix(prefix)).and_then(|v| v.trim().parse().ok())
}

impl VariableField {
    pub fn new(var: &egg_variables::Model, value: Option<&str>) -> VariableField {
        let rules = rules_of(&var.rules.0);
        let in_rule = rules.iter().find_map(|r| r.strip_prefix("in:"));
        let kind = if in_rule.is_some() {
            Kind::Select
        } else if rules.iter().any(|r| r == "boolean") {
            Kind::Toggle
        } else if rules.iter().any(|r| r == "numeric" || r == "integer") {
            Kind::Number
        } else {
            Kind::Text
        };
        VariableField {
            id: var.id.0,
            name: var.name.clone(),
            description: var.description.clone(),
            env: var.env_variable.clone(),
            default: var.default_value.clone(),
            value: value.map(String::from).unwrap_or_else(|| var.default_value.clone()),
            editable: var.user_editable.0,
            viewable: var.user_viewable.0,
            kind,
            options: in_rule.map(|o| o.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default(),
            min: bound(&rules, "min:"),
            max: bound(&rules, "max:"),
            required: rules.iter().any(|r| r == "required"),
            rules,
        }
    }
}

/// Validate submitted values against each variable's rules; returns `(field name, message)`.
pub fn validate(fields: &[VariableField], value_of: impl Fn(&VariableField) -> String) -> Vec<(String, String)> {
    let mut errors = Vec::new();
    for f in fields {
        let v = value_of(f);
        let data = serde_json::json!({ "value": if v.is_empty() { Value::Null } else { Value::String(v) } });
        let mut val = pelican_api::validation::Validator::for_value(&data);
        val.attribute_name("value", &f.name);
        let rules: Vec<&str> = f.rules.iter().map(String::as_str).collect();
        val.rules("value", &rules);
        val.run();
        for (_, msg) in val.error_messages() {
            errors.push((format!("var_{}", f.id), msg));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use pelican_db::{Int, Json, SmallBool};
    use serde_json::json;

    fn var(rules: Value) -> egg_variables::Model {
        egg_variables::Model {
            id: Int(1),
            egg_id: Int(1),
            name: "Version".into(),
            description: String::new(),
            env_variable: "VERSION".into(),
            default_value: "latest".into(),
            user_viewable: SmallBool(true),
            user_editable: SmallBool(true),
            rules: Json(rules),
            created_at: None,
            updated_at: None,
            sort: None,
        }
    }

    #[test]
    fn kinds() {
        let f = VariableField::new(&var(json!(["required", "in:a,b"])), None);
        assert_eq!(f.kind, Kind::Select);
        assert_eq!(f.options, vec!["a", "b"]);
        assert!(f.required);
        assert_eq!(VariableField::new(&var(json!("required|integer|min:1")), Some("5")).kind, Kind::Number);
        assert_eq!(VariableField::new(&var(json!(["boolean"])), None).kind, Kind::Toggle);
    }

    #[test]
    fn validates() {
        let f = VariableField::new(&var(json!(["required", "integer"])), None);
        let errs = validate(&[f], |_| "abc".into());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, "var_1");
    }
}
