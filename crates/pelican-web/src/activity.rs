//! Activity log entries for the UI (app/Models/ActivityLog.php: getLabel,
//! wrapProperties, htmlable).

use pelican_db::entities::{activity_logs, users};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::i18n;

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub username: String,
    pub avatar: String,
    pub event: String,
    pub label: String,
    /// Only with `seeIps activityLog`.
    pub ip: Option<String>,
    pub timestamp: String,
    pub has_metadata: bool,
    pub properties: String,
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => {
            if *b {
                "1".into()
            } else {
                String::new()
            }
        }
        other => other.to_string(),
    }
}

/// `wrapProperties()`.
pub fn wrap_properties(props: &Value) -> Vec<(String, String)> {
    let Value::Object(m) = props else { return Vec::new() };
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in m {
        match v {
            Value::Array(a) => {
                match a.first() {
                    Some(Value::Array(_)) | Some(Value::Object(_)) => {}
                    Some(first) => out.push((k.clone(), strip_tags(&scalar(first)))),
                    None => {}
                }
                out.push((format!("{k}_count"), a.len().to_string()));
            }
            Value::Object(o) => {
                if let Some(first) = o.values().next() {
                    if !first.is_array() && !first.is_object() {
                        out.push((k.clone(), strip_tags(&scalar(first))));
                    }
                }
                out.push((format!("{k}_count"), o.len().to_string()));
            }
            _ => {
                let mut s = scalar(v);
                if k == "directory" {
                    s = format!("/{}/", s.trim_matches('/')).replace("//", "/");
                }
                out.push((k.clone(), strip_tags(&s)));
            }
        }
    }
    let counts: Vec<usize> =
        out.iter().enumerate().filter(|(_, (k, _))| k.ends_with("_count")).map(|(i, _)| i).collect();
    if counts.len() == 1 {
        let (_, v) = out.remove(counts[0]);
        out.push(("count".into(), v));
    }
    out
}

fn event_key(event: &str) -> String {
    format!("activity.{}", event.replace(':', "."))
}

/// `getLabel()`.
pub fn label(locale: &str, event: &str, props: &Value) -> String {
    let wrapped = wrap_properties(props);
    let count = wrapped.iter().find(|(k, _)| k == "count").and_then(|(_, v)| v.parse::<f64>().ok()).unwrap_or(1.0);
    i18n::trans_choice(locale, &event_key(event), count, &wrapped)
}

/// `hasAdditionalMetadata()`.
pub fn has_metadata(locale: &str, event: &str, props: &Value) -> bool {
    let Value::Object(m) = props else { return false };
    if m.is_empty() {
        return false;
    }
    let wrapped = wrap_properties(props);
    let count = wrapped.iter().find(|(k, _)| k == "count").and_then(|(_, v)| v.parse::<f64>().ok()).unwrap_or(1.0);
    let line = i18n::choose(&i18n::trans(locale, &event_key(event), &[]), count, locale);
    let re = regex_lite_keys(&line);
    m.keys().any(|k| !re.contains(k) && !["ip", "useragent", "using_sftp"].contains(&k.as_str()))
}

/// Placeholder names in a translation line (`/:(?<key>[\w.-]+\w)/`).
fn regex_lite_keys(line: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || "_.-".contains(chars[j])) {
                j += 1;
            }
            let mut key: String = chars[i + 1..j].iter().collect();
            while key.ends_with(['.', '-']) {
                key.pop();
            }
            if !key.is_empty() {
                keys.push(key);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    keys
}

/// Initials avatar for an actor (no external avatar provider, ADR-0013).
pub fn initials(name: &str) -> String {
    name.split([' ', '_', '-', '.']).filter_map(|p| p.chars().next()).take(2).flat_map(char::to_uppercase).collect()
}

/// Build view entries for rows (actor usernames resolved in one query).
pub async fn entries(
    db: &sea_orm::DatabaseConnection,
    rows: Vec<activity_logs::Model>,
    locale: &str,
    see_ips: bool,
) -> Result<Vec<Entry>, sea_orm::DbErr> {
    let actor_ids: Vec<i64> = rows
        .iter()
        .filter(|r| r.actor_type.as_deref() == Some("user"))
        .filter_map(|r| r.actor_id.map(|a| a.0))
        .collect();
    let actors: Map<String, Value> = if actor_ids.is_empty() {
        Map::new()
    } else {
        users::Entity::find()
            .filter(users::Column::Id.is_in(actor_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|u| (u.id.0.to_string(), Value::String(u.username)))
            .collect()
    };
    Ok(rows
        .into_iter()
        .map(|r| {
            let username = r
                .actor_id
                .and_then(|a| actors.get(&a.0.to_string()))
                .and_then(|v| v.as_str())
                .unwrap_or("system")
                .to_string();
            let props = r.properties.0.clone();
            Entry {
                id: r.id.0,
                avatar: initials(&username),
                username,
                label: label(locale, &r.event, &props),
                has_metadata: has_metadata(locale, &r.event, &props),
                properties: serde_json::to_string_pretty(&props).unwrap_or_default(),
                event: r.event,
                ip: see_ips.then_some(r.ip),
                timestamp: r.timestamp.0.format("%Y-%m-%d %H:%M:%S").to_string(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wraps_lists_into_count() {
        let w = wrap_properties(&json!({"files": ["a.txt", "b.txt"], "directory": "x//y"}));
        assert_eq!(
            w,
            vec![("files".into(), "a.txt".into()), ("directory".into(), "/x/y/".into()), ("count".into(), "2".into())]
        );
    }

    #[test]
    fn placeholder_keys() {
        assert_eq!(regex_lite_keys("Deleted :files.: and :count."), vec!["files", "count"]);
    }
}
