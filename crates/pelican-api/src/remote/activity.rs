//! R4 `POST /api/remote/activity` (`ActivityProcessingController`).

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use indexmap::IndexMap;
use pelican_core::activity::{Activity, morph};
use pelican_core::server_config::php_assoc;
use pelican_core::state::AppState;
use pelican_db::entities::{servers, users};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Map, Value};

use super::node;
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::pipeline::Ctx;
use crate::validation::Validator;

fn rfc3339_ok(ts: &str) -> bool {
    // preg_replace('/(\.\d+)Z$/', 'Z', ...) then createFromFormat(RFC3339).
    static FRACTION: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(\.\d+)Z$").expect("valid regex"));
    static RFC: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}([Zz]|[+-]\d{2}:\d{2})$").expect("valid regex")
    });
    RFC.is_match(&FRACTION.replace(ts, "Z"))
}

/// Controller returns `void`: 200, empty body, text/html.
fn void_response() -> Response {
    let mut r = (StatusCode::OK, "").into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    r
}

pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let mut v = Validator::new(&input);
    v.rules("data", &["required", "array"]);
    v.rules("data.*", &["array"]);
    v.rules("data.*.user", &["sometimes", "nullable", "uuid"]);
    v.rules("data.*.server", &["required", "uuid"]);
    v.rules("data.*.event", &["required", "string"]);
    v.rules("data.*.metadata", &["present", "nullable", "array"]);
    v.rules("data.*.ip", &["sometimes", "nullable", "ip"]);
    v.rules("data.*.timestamp", &["required", "string"]);
    v.validate()?;
    let node = node(&ctx)?;
    let data: Vec<&Value> = match input.get("data") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Object(m)) => m.values().collect(),
        _ => Vec::new(),
    };
    let str_of = |d: &Value, k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
    let server_uuids: Vec<String> = data.iter().filter_map(|d| str_of(d, "server")).collect();
    let user_uuids: Vec<String> = data.iter().filter_map(|d| str_of(d, "user")).filter(|u| !u.is_empty()).collect();
    let srv = servers::Entity::find()
        .filter(servers::Column::NodeId.eq(node.id))
        .filter(servers::Column::Uuid.is_in(server_uuids))
        .all(&st.db)
        .await?;
    let usr = if user_uuids.is_empty() {
        Vec::new()
    } else {
        users::Entity::find().filter(users::Column::Uuid.is_in(user_uuids)).all(&st.db).await?
    };

    let mut logs: IndexMap<String, Vec<Activity>> = IndexMap::new();
    for d in data {
        let server_uuid = str_of(d, "server").unwrap_or_default();
        let event = str_of(d, "event").unwrap_or_default();
        if !srv.iter().any(|s| s.uuid == server_uuid) || !event.starts_with("server:") {
            continue;
        }
        let ts = str_of(d, "timestamp").unwrap_or_default();
        let mut metadata = php_assoc(d.get("metadata").cloned().unwrap_or(Value::Null));
        if !rfc3339_ok(&ts) {
            // array_merge($metadata ?? [], ['original_timestamp' => ...])
            let mut m: Map<String, Value> = match metadata {
                Value::Object(m) => m,
                Value::Array(a) => a.into_iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
                _ => Map::new(),
            };
            m.insert("original_timestamp".into(), Value::String(ts));
            metadata = Value::Object(m);
        }
        let ip = match d.get("ip") {
            Some(Value::String(s)) if !s.is_empty() && s != "0" => s.clone(),
            _ => "127.0.0.1".into(),
        };
        // `$datum['user']` on a missing key is a PHP warning → 500.
        let user_key = d.get("user").ok_or_else(|| ApiError::internal("ErrorException"))?;
        let mut a = Activity::new(event, ip).properties(metadata);
        if let Some(u) = user_key.as_str().and_then(|uu| usr.iter().find(|x| x.uuid == uu)) {
            a = a.actor(morph::USER, u.id.0);
        }
        logs.entry(server_uuid).or_default().push(a);
    }
    for (uuid, list) in logs {
        let Some(server) = srv.iter().find(|s| s.uuid == uuid) else { continue };
        for a in list {
            a.subject(morph::SERVER, server.id.0).save(&st.db).await?;
        }
    }
    Ok(void_response())
}
