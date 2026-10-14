//! `/api/application/webhooks` — `WebhookController`, `StoreWebhookRequest`,
//! `UpdateWebhookRequest`, `TestWebhookRequest` (spec §3.9, §2.13, §2.14).
//!
//! `WebhookConfiguration` soft-deletes; deliveries live in `webhooks`
//! (`ProcessWebhook` runs inline under `QUEUE_CONNECTION=sync`, so the /test
//! endpoint performs the real HTTP POST before responding).

use std::sync::LazyLock;
use std::time::Duration;

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{servers, webhook_configurations, webhooks};
use pelican_db::{Int, Json, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult};
use serde_json::{Map, Value, json};

use super::data::Fut;
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, item, list, null_resource, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::{CustomRule, Validator, php_regex_match, php_string};

/// `filamentCheckboxList(scope)` dumps (exported from panel-orig; order matters).
static EVENTS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../data/webhook-events.json")).unwrap_or(Value::Null));

/// Only one schema is registered in core (`WebhookServiceProvider`).
const REGULAR: &str = "regular";
// Object-rule failures report their class basename through `Str::snake`
// (`Illuminate\Validation\Rules\{In,Enum}` and the header-name closure).
// Object rules report their string form here: `Rule::in` fails as "in"
// (verified against panel-orig); enum handled below; closure = FQCN snake.
const RULE_IN: &str = "in";
const RULE_ENUM: &str = r"illuminate\_validation\_rules\_enum";
const RULE_CLOSURE: &str = r"illuminate\_validation\_closure_validation_rule";

static VAR_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\{\{(.*?)\}\}").expect("valid regex"));

fn bound_webhook(ctx: &Ctx) -> ApiResult<webhook_configurations::Model> {
    ctx.bound.webhook.clone().ok_or_else(ApiError::model_not_found)
}

/// PHP `blank()` / `filled()` of a JSON value.
fn php_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// `$request->scalarInput($key)` (StoreWebhookRequest): a plain filled scalar,
/// else absent (left for the rules to reject with 422).
fn scalar_input(input: &Input, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|v| match v {
            Value::Bool(_) | Value::Number(_) | Value::String(_) => Some(php_string(v)),
            _ => None,
        })
        .filter(|s| !s.trim().is_empty())
}

fn has<'a>(input: &'a Input, key: &str) -> bool {
    input.get(key).is_some()
}

fn filled(input: &Input, key: &str) -> bool {
    input.get(key).is_some_and(|v| !php_blank(v))
}

/// `WebhookScope::tryFrom($scope) ?? Global`.
fn scope_of(scalar: Option<&str>) -> &'static str {
    match scalar {
        Some("server") => "server",
        _ => "global",
    }
}

// --- event catalogue -------------------------------------------------------

fn catalogue(scope: &str) -> &Value {
    EVENTS.get(scope).unwrap_or(&Value::Null)
}

fn allowed_events(scope: &str) -> Vec<&str> {
    catalogue(scope).as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default()
}

// --- validation ------------------------------------------------------------

/// `Rule::enum(WebhookScope::class)` as an object rule: strict membership in the
/// backing values (null fails like Laravel's enum rule).
fn scope_rule() -> CustomRule {
    CustomRule {
        name: RULE_ENUM.to_string(),
        check: Box::new(|_: &str, v: &Value| {
            let ok = v.as_str().is_some_and(|s| s == "global" || s == "server");
            (!ok).then(|| "The selected :attribute is invalid.".to_string())
        }),
    }
}

/// `Rule::in(array_keys(WebhookTypes::getOptions()))` = only core types.
fn type_rule() -> CustomRule {
    CustomRule {
        name: RULE_IN.to_string(),
        check: Box::new(|_: &str, v: &Value| {
            let ok = php_string(v) == REGULAR;
            (!ok).then(|| "The selected :attribute is invalid.".to_string())
        }),
    }
}

/// RFC 7230 token + space (the boundary check before save-normalization).
fn valid_header_name(name: &str) -> bool {
    php_regex_match(r"/\A[A-Za-z0-9 !#$%&'*+.^_`|~-]+\z/", name)
}

/// Common base rules of Store/Update; `update` flips required → sometimes for
/// the fields a PATCH may omit (UpdateWebhookRequest::rules()).
fn validate_base(v: &mut Validator, input: &Input, update: bool) -> Result<(), ()> {
    if update {
        v.rules("name", &["sometimes", "string", "max:191"]);
    } else {
        v.rules("name", &["required", "string", "max:191"]);
    }
    v.rules("description", &["nullable", "string", "max:191"]);
    if update {
        v.rules("endpoint", &["sometimes", "string", "url", "max:191"]);
    } else {
        v.rules("endpoint", &["required", "string", "url", "max:191"]);
    }
    v.rules("type", &["sometimes", "string"]);
    v.custom("type", type_rule());
    v.custom("scope", scope_rule());
    v.rules("server_id", &["nullable", "integer"]);
    // The header-name closure reports its message verbatim (`$fail($msg)`),
    // without :attribute substitution.
    v.custom(
        "headers",
        CustomRule::precomputed(
            RULE_CLOSURE,
            header_name_violation(input).then(|| "The headers field contains an invalid header name.".to_string()),
        ),
    );
    v.rules("headers.*", &["string", r"regex:/\A[^\r\n]*\z/"]);
    Ok(())
}

fn header_name_violation(input: &Input) -> bool {
    match input.get("headers") {
        Some(Value::Object(m)) => m.keys().any(|k| !valid_header_name(k)),
        _ => false,
    }
}

/// `exists:servers,id`: the value reached this rule only when the integer rule
/// passed; numeric strings compare like Eloquent's query binding.
async fn exists_server(st: &AppState, value: Option<&Value>) -> ApiResult<CustomRule> {
    let id = match value {
        None | Some(Value::Null) => return Ok(CustomRule::precomputed("exists", None)),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        // The integer rule already accepted it, so any numeric shape casts.
        Some(other) => Some(pelican_core::php::intval(&php_string(other))),
    };
    let exists = match id {
        Some(i) => servers::Entity::find_by_id(Int(i)).one(&st.db).await?.is_some(),
        None => false,
    };
    Ok(CustomRule::precomputed("exists", (!exists).then(|| "The selected :attribute is invalid.".to_string())))
}

/// The remaining per-attribute rules shared by store/update.
fn attach_common_rules(v: &mut Validator<'_>, update: bool) {
    if update {
        v.rules("events", &["sometimes", "array", "min:1"]);
    } else {
        v.rules("events", &["required", "array", "min:1"]);
    }
    v.rules("events.*", &["required", "string"]);
    v.rules("payload", &["nullable", "array"]);
}

/// `WebhookConfiguration::$fillable` field set (validated keys, in rule order).
const FIELDS: [&str; 9] =
    ["name", "description", "endpoint", "type", "scope", "server_id", "events", "payload", "headers"];

/// `$request->validated()`: present keys that have rules, including explicit
/// nulls (`data_get` finds them even though `sometimes` skipped the rules).
fn validated(input: &Input) -> Vec<(&'static str, Value)> {
    FIELDS.iter().filter_map(|k| input.get(k).map(|v| (*k, v.clone()))).collect()
}

fn field<'a>(data: &'a [(&'static str, Value)], k: &str) -> Option<&'a Value> {
    data.iter().find(|(n, _)| *n == k).map(|(_, v)| v)
}

/// Errors added by `StoreWebhookRequest::withValidator()->after()` — the
/// resolved-scope checks plus every event against `filamentCheckboxList`.
struct AfterCheck<'a> {
    input: &'a Input,
    scope: String,
    has_server: bool,
    /// Error tuples appended after the base rules ran.
    errors: Vec<(String, String, String)>,
}

impl<'a> AfterCheck<'a> {
    fn new(input: &'a Input, scope: String, has_server: bool) -> Self {
        AfterCheck { input, scope, has_server, errors: Vec::new() }
    }

    fn run(mut self) -> Self {
        if self.scope == "server" && !self.has_server {
            self.errors.push(("server_id".into(), String::new(), "The server id field is required.".into()));
        }
        if self.scope == "global" && filled(self.input, "server_id") {
            self.errors.push(("server_id".into(), String::new(), "The server id field is prohibited.".into()));
        }
        let allowed = allowed_events(&self.scope);
        let events: Vec<(String, Value)> = match self.input.get("events") {
            Some(Value::Array(a)) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => Vec::new(),
        };
        for (index, value) in events {
            let ok = matches!(value, Value::String(s) if allowed.contains(&s.as_str()));
            if !ok {
                self.errors.push((
                    format!("events.{index}"),
                    String::new(),
                    format!("The selected events.{index} is invalid."),
                ));
            }
        }
        self
    }
}

/// The delivered payload template samples (`getWebhookSampleData` /
/// `getServerWebhookSampleData`, verbatim from WebhookConfiguration.php).

static GLOBAL_SAMPLE: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str::<Value>(concat!(
        r#"{"id":4,"uuid":"4864a058-9a3b-44a9-a6cf-c1355e89406e","uuid_short":"4864a058","node_id":1,"#,
        r#""name":"Example Server","owner_id":1,"memory":6144,"swap":0,"disk":20480,"io":500,"cpu":300,"egg_id":1,"#,
        r#""startup":"java -Xms128M -XX:MaxRAMPercentage=95.0 -Dterminal.jline=false -Dterminal.ansi=true -jar {{SERVER_JARFILE}}","#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-11T22:45:14.000000Z","allocation_id":4,"#,
        r#""image":"ghcr.io\/pelican-eggs\/yolks:java_21","description":"This is an example server description.","#,
        r#""skip_scripts":false,"external_id":null,"database_limit":5,"allocation_limit":5,"threads":null,"backup_limit":5,"#,
        r#""status":null,"installed_at":"2025-09-06T03:02:31.000000Z","oom_killer":false,"docker_labels":[],"#,
        r#""allocation":{"id":4,"node_id":1,"ip":"0.0.0.0","port":25565,"server_id":4,"#,
        r#""created_at":"2025-07-01T20:12:41.000000Z","updated_at":"2025-09-09T17:47:22.000000Z","ip_alias":null,"notes":null},"#,
        r#""variables":["#,
        // NOTE: descriptions contain LITERAL backslash-r/backslash-n sequences
        // (PHP single-quoted strings in WebhookConfiguration.php), hence \\r.
        r#"{"id":1,"egg_id":1,"name":"Build Number","description":"The build number for the paper release.\\r\\n\\r\\nLeave at latest to always get the latest version. Invalid versions will default to latest.","#,
        r#""env_variable":"BUILD_NUMBER","default_value":"latest","user_viewable":true,"user_editable":true,"#,
        r#""rules":["required","string","max:20"],"created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":4,"server_value":"latest"},"#,
        r#"{"id":2,"egg_id":1,"name":"Download Path","description":"A URL to use to download a server.jar rather than the ones in the install script. This is not user\\nviewable.","#,
        r#""env_variable":"DL_PATH","default_value":"","user_viewable":false,"user_editable":false,"rules":["nullable","string"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":3,"server_value":""},"#,
        r#"{"id":3,"egg_id":1,"name":"Minecraft Version","description":"The version of minecraft to download. \\r\\n\\r\\nLeave at latest to always get the latest version. Invalid versions will default to latest.","#,
        r#""env_variable":"MINECRAFT_VERSION","default_value":"latest","user_viewable":true,"user_editable":true,"rules":["nullable","string","max:20"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":1,"server_value":"1.21.8"},"#,
        r#"{"id":4,"egg_id":1,"name":"Server Jar File","description":"The name of the server jarfile to run the server with.","#,
        r#""env_variable":"SERVER_JARFILE","default_value":"server.jar","user_viewable":true,"user_editable":true,"#,
        r#""rules":["required","regex:/^([\\w\\d._-]+)(\\.jar)$/"],"created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":2,"server_value":"server.jar"}],"#,
        r#""server_variables":{"record-21":{"id":21,"server_id":4,"variable_id":3,"variable_value":"1.21.8","#,
        r#""created_at":"2025-09-06T06:00:58.000000Z","updated_at":"2025-09-09T17:59:40.000000Z","variable":{"id":3,"egg_id":1,"name":"Minecraft Version","#,
        r#""description":"The version of minecraft to download. \\r\\n\\r\\nLeave at latest to always get the latest version. Invalid versions will default to latest.","#,
        r#""env_variable":"MINECRAFT_VERSION","default_value":"latest","user_viewable":true,"user_editable":true,"rules":["nullable","string","max:20"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":1}},"#,
        r#""record-22":{"id":22,"server_id":4,"variable_id":4,"variable_value":"server.jar","#,
        r#""created_at":"2025-09-06T06:00:58.000000Z","updated_at":"2025-09-06T06:01:05.000000Z","variable":{"id":4,"egg_id":1,"name":"Server Jar File","#,
        r#""description":"The name of the server jarfile to run the server with.","#,
        r#""env_variable":"SERVER_JARFILE","default_value":"server.jar","user_viewable":true,"user_editable":true,"rules":["required","regex:/^([\\w\\d._-]+)(\\.jar)$/"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":2}},"#,
        r#""record-20":{"id":20,"server_id":4,"variable_id":2,"variable_value":"","#,
        r#""created_at":"2025-09-06T06:00:58.000000Z","updated_at":"2025-09-06T06:00:58.000000Z","variable":{"id":2,"egg_id":1,"name":"Download Path","#,
        r#""description":"A URL to use to download a server.jar rather than the ones in the install script. This is not user\\nviewable.","#,
        r#""env_variable":"DL_PATH","default_value":"","user_viewable":false,"user_editable":false,"rules":["nullable","string"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":3}},"#,
        r#""record-19":{"id":19,"server_id":4,"variable_id":1,"variable_value":"latest","#,
        r#""created_at":"2025-09-06T06:00:58.000000Z","updated_at":"2025-09-06T06:00:58.000000Z","variable":{"id":1,"egg_id":1,"name":"Build Number","#,
        r#""description":"The build number for the paper release.\\r\\n\\r\\nLeave at latest to always get the latest version. Invalid versions will default to latest.","#,
        r#""env_variable":"BUILD_NUMBER","default_value":"latest","user_viewable":true,"user_editable":true,"rules":["required","string","max:20"],"#,
        r#""created_at":"2025-09-05T01:15:43.000000Z","updated_at":"2025-09-05T01:15:43.000000Z","sort":4}}},"#,
        r#""event":"updated: Server"}"#
    ))
    .unwrap_or(Value::Null)
});

static SERVER_SAMPLE: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str::<Value>(concat!(
        r#"{"user":{"uuid":"12345678-1234-5678-9012-123456789012","username":"admin","email":"admin@example.com","#,
        r#""image":"https://www.gravatar.com/avatar/default","admin":true,"language":"en","#,
        r#""created_at":"2025-06-01T12:31:50.000000Z","updated_at":"2025-06-01T12:31:50.000000Z"},"#,
        r#""server":{"uuid":"87654321-4321-8765-2109-876543210987","name":"Example Server","node":"node1.example.com","description":"Sample Minecraft server"}}"#
    ))
    .unwrap_or(Value::Null)
});

/// `transformClassName(event)` (WebhookConfiguration.php:366-373).
fn transform_class_name(event: &str) -> String {
    let rest = match event.split_once("eloquent.") {
        Some((_, rest)) => rest,
        None => event,
    };
    rest.replace(r"App\Models\", "").replace(r"App\Events\", "event: ")
}

/// `replaceVars`: `{{path}}` → `data_get(data, path, trimmed-token)`.
fn replace_vars(subject: &Value, data: &Value) -> String {
    let raw = php_string(subject);
    VAR_RE
        .replace_all(&raw, |c: &regex::Captures<'_>| {
            let trimmed = c[1].trim();
            data_get(data, trimmed).as_ref().map(|v| php_string(v)).unwrap_or_else(|| trimmed.to_string())
        })
        .into_owned()
}

/// Laravel `data_get`.
fn data_get(target: &Value, path: &str) -> Option<Value> {
    let mut cur = target;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}

/// PHP `Arr::wrap($value)`.
fn arr_wrap(v: Value) -> Value {
    match v {
        arr @ (Value::Array(_) | Value::Object(_)) => arr,
        other => Value::Array(vec![other]),
    }
}

/// `ProcessWebhook::normalizeData($this->data[0])` with the `event` key
/// appended afterwards (an existing key keeps its position, like PHP arrays).
fn normalize_event_data(first: Option<&Value>, event: &str) -> Value {
    let mut inner: Value = match first {
        // is_string → Arr::wrap(json_decode(...) ?? []).
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).map(arr_wrap).unwrap_or_else(|_| Value::Array(vec![]))
        }
        // is_object → Arr::wrap(toArray) ≡ the object itself.
        Some(obj @ Value::Object(_)) => obj.clone(),
        Some(other) => arr_wrap(other.clone()),
        None => Value::Array(vec![]),
    };
    let transformed = transform_class_name(event);
    match &mut inner {
        Value::Object(m) => {
            m.insert("event".into(), json!(transformed));
        }
        Value::Array(a) => {
            let mut indexed = Map::new();
            for (i, v) in std::mem::take(a).into_iter().enumerate() {
                indexed.insert(i.to_string(), v);
            }
            indexed.insert("event".into(), json!(transformed));
            inner = Value::Object(indexed);
        }
        _ => {}
    }
    inner
}

/// RegularSchema::prepareHeaders (values go through `replaceVars`).
fn prepare_headers(config: &webhook_configurations::Model, data: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let headers = config.headers.as_ref().map(|j| j.0.clone()).unwrap_or(Value::Null);
    if let Value::Object(m) = pelican_core::server_config::php_assoc(headers) {
        for (k, v) in m {
            out.push((k, replace_vars(&v, data)));
        }
    }
    out
}

/// BaseSchema::deliver — Guzzle POST with JSON flags 0; transport errors are
/// caught inside the job (like PHP's try/catch), so they only mark failure.
async fn deliver(st: &AppState, endpoint: &str, payload: &Value, headers: &[(String, String)]) -> Option<u16> {
    let body = laravel_compat::json::to_string(payload).ok()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .danger_accept_invalid_certs(!st.config.is_production())
        .build()
        .ok()?;
    let mut req = client.post(endpoint).header("content-type", "application/json").body(body);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req.send().await.ok()?.status().as_u16().into()
}

/// ProcessWebhook::handle: deliver and record one `webhooks` row.
async fn process_webhook(
    st: &AppState,
    wh: &webhook_configurations::Model,
    event_name: &str,
    event_data: &Value,
) -> ApiResult<()> {
    process_webhook_in(st, &st.db, wh, event_name, event_data).await
}

/// `ProcessWebhook` on the caller's connection (dispatched inside a transaction).
pub(crate) async fn process_webhook_in<C: sea_orm::ConnectionTrait>(
    st: &AppState,
    db: &C,
    wh: &webhook_configurations::Model,
    event_name: &str,
    event_data: &Value,
) -> ApiResult<()> {
    let data = normalize_event_data(Some(event_data), event_name);
    // RegularSchema.preparePayload = identity (FallbackSchema arrives with M6 plugins).
    let payload = data;
    let headers = prepare_headers(wh, &payload);
    // A transport error or non-2xx leaves `successful_at` unset; regular
    // webhooks never ask for a retry.
    let successful = deliver(st, &wh.endpoint, &payload, &headers)
        .await
        .filter(|s| (200..300).contains(s))
        .map(|_| Timestamp(pelican_core::time::now()));
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::insert(
        db,
        webhooks::ActiveModel {
            webhook_configuration_id: Set(wh.id),
            event: Set(event_name.to_string()),
            endpoint: Set(wh.endpoint.clone()),
            successful_at: Set(successful),
            payload: Set(Json(payload)),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

// --- resources -------------------------------------------------------------

/// `WebhookDeliveryData` (`webhook_delivery`); timestamps are null-safe ATOM.
fn delivery_item(m: &webhooks::Model) -> Value {
    let mut a = Map::new();
    a.insert("id".into(), json!(m.id.0));
    a.insert("webhook_configuration_id".into(), json!(m.webhook_configuration_id.0));
    a.insert("event".into(), json!(m.event));
    a.insert("endpoint".into(), json!(m.endpoint));
    a.insert("payload".into(), pelican_core::server_config::php_assoc(m.payload.0.clone()));
    a.insert("successful".into(), json!(m.successful_at.is_some()));
    a.insert(
        "successful_at".into(),
        m.successful_at.as_ref().map(|t| json!(pelican_core::time::atom(&t.0))).unwrap_or(Value::Null),
    );
    a.insert(
        "created_at".into(),
        m.created_at.as_ref().map(|t| json!(pelican_core::time::atom(&t.0))).unwrap_or(Value::Null),
    );
    a.insert(
        "updated_at".into(),
        m.updated_at.as_ref().map(|t| json!(pelican_core::time::atom(&t.0))).unwrap_or(Value::Null),
    );
    item("webhook_delivery", Value::Object(a), vec![])
}

/// `WebhookConfigurationData` (`webhook`). Includes ($availableIncludes order):
/// server (perm `server`; null when unset or denied), deliveries (unchecked).
pub fn webhook_item<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    m: &'a webhook_configurations::Model,
    inc: &'a Includes,
) -> Fut<'a> {
    Box::pin(async move {
        let mut a = Map::new();
        a.insert("id".into(), json!(m.id.0));
        a.insert("name".into(), json!(m.name));
        a.insert("description".into(), json!(m.description));
        a.insert("scope".into(), json!(m.scope));
        a.insert("server_id".into(), json!(m.server_id.map(|v| v.0)));
        a.insert("type".into(), json!(m.r#type));
        // A plugin-provided type is no longer registered → false.
        a.insert("type_available".into(), json!(m.r#type.as_deref() == Some(REGULAR)));
        a.insert("endpoint".into(), json!(m.endpoint));
        a.insert("events".into(), pelican_core::server_config::php_assoc(m.events.0.clone()));
        a.insert(
            "payload".into(),
            m.payload.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(Value::Null),
        );
        a.insert(
            "headers".into(),
            m.headers.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(Value::Null),
        );
        a.insert(
            "created_at".into(),
            m.created_at.as_ref().map(|t| json!(pelican_core::time::atom(&t.0))).unwrap_or(Value::Null),
        );
        a.insert(
            "updated_at".into(),
            m.updated_at.as_ref().map(|t| json!(pelican_core::time::atom(&t.0))).unwrap_or(Value::Null),
        );

        let mut rel = Vec::new();
        if inc.is_requested("server") {
            let allowed = ctx.allows_admin("server");
            let v = if allowed && m.server_id.is_some() {
                match servers::Entity::find_by_id(m.server_id.unwrap()).one(&st.db).await? {
                    Some(s) => super::data::server_item(st, ctx, &s, &inc.descend("server")).await?,
                    None => null_resource(),
                }
            } else {
                null_resource()
            };
            rel.push(("server", v));
        }
        if inc.is_requested("deliveries") {
            let rs = st
                .db
                .query_all_raw(raw::stmt(
                    st.db.get_database_backend(),
                    "select * from {webhooks} where {webhook_configuration_id} = ? and {webhook_configuration_id} is not null \
                     order by {id} desc limit 25",
                    vec![m.id.into()],
                ))
                .await?;
            let mut data = Vec::new();
            for r in &rs {
                data.push(delivery_item(&webhooks::Model::from_query_result(r, "")?));
            }
            rel.push(("deliveries", list(data)));
        }
        Ok(item("webhook", Value::Object(a), rel))
    })
}

// --- GET endpoints ---------------------------------------------------------

/// GET requests still pass GetWebhookRequest validation (per_page 1–100,
/// scope enum) on every action of this controller.
fn validate_list(input: &Input) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("per_page", &["sometimes", "integer", "min:1", "max:100"]);
    v.custom("scope", scope_rule());
    v.validate()
}

/// GET /webhooks (all listed filters partial; sorts id,name,type,created_at).
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("webhook", READ)?;
    validate_list(&input)?;
    let b = st.db.get_database_backend();
    let allowed = ["name", "description", "endpoint", "type", "scope", "server_id"];
    let fs = filters(&input, &allowed)?;
    let ss = sorts(&input, &["id", "name", "type", "created_at"])?;
    // The page size comes from the VALIDATED per_page (§3.9), not the raw query.
    // Values arrive as JSON numbers or digit strings ("2" from the query bag).
    let per_page = match input.get("per_page") {
        Some(Value::Number(n)) => n.as_f64().map(|f| f.trunc() as i64).unwrap_or(50),
        Some(other) if !other.is_null() && pelican_core::php::is_numeric_str(php_string(other).trim()) => {
            php_string(other).trim().parse::<i64>().map(|n| if n == 0 { 50 } else { n }).unwrap_or(50)
        }
        _ => 50,
    };
    let page = Page { per_page, page: crate::paginate::resolve_page(input.get("page")) };
    let includes = Includes::parse(&input);
    let mut wh = vec!["{deleted_at} is null".to_string()];
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in allowed {
        if let Some((_, v)) = fs.iter().find(|(n, _)| *n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("webhook_configurations.{name}"), v) {
                wh.push(sql);
                vals.append(&mut values);
            }
        }
    }
    let where_sql = if wh.is_empty() { String::new() } else { format!(" where {}", wh.join(" and ")) };
    let order = if ss.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> =
            ss.iter().map(|(n, d)| format!("{} {}", raw::q(b, n), if *d { "desc" } else { "asc" })).collect();
        format!(" order by {}", parts.join(", "))
    };
    let total_row = st
        .db
        .query_one_raw(raw::stmt(
            b,
            &format!("select count(*) as {{aggregate}} from {{webhook_configurations}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = total_row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = st
            .db
            .query_all_raw(raw::stmt(
                b,
                &format!("select * from {{webhook_configurations}}{where_sql}{order}{}", page.sql()),
                vals,
            ))
            .await?;
        for r in rows {
            let m = webhook_configurations::Model::from_query_result(&r, "")?;
            data.push(webhook_item(&st, &ctx, &m, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// GET /webhooks/{webhook}.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", READ)?;
    validate_list(&input)?;
    let wh = bound_webhook(&ctx)?;
    trashed_guard(&wh)?;
    Ok(PhpJson::ok(webhook_item(&st, &ctx, &wh, &Includes::parse(&input)).await?).into_response())
}

/// SoftDeletes global scope: a deleted row never binds again.
fn trashed_guard(m: &webhook_configurations::Model) -> ApiResult<()> {
    if m.deleted_at.is_some() { Err(ApiError::model_not_found()) } else { Ok(()) }
}

/// GET /webhooks/types — not enveloped; only the core schema is registered.
pub async fn types(State(_st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", READ)?;
    validate_list(&input)?;
    Ok(PhpJson::ok(json!({
        "data": [{"id": "regular", "label": "Regular", "has_preview": false}]
    }))
    .into_response())
}

/// GET /webhooks/events?scope=global|server — not enveloped.
pub async fn events(State(_st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", READ)?;
    validate_list(&input)?;
    let scope = scope_of(scalar_input(&input, "scope").as_deref());
    let items: Vec<Value> = catalogue(scope)
        .as_object()
        .map(|m| m.into_iter().map(|(event, label)| json!({"event": event, "label": label})).collect())
        .unwrap_or_default();
    Ok(PhpJson::ok(json!({"data": items, "meta": {"scope": scope}})).into_response())
}

/// `POST /webhooks/{webhook}/test` — `WebhookConfiguration::run(event, data)`;
/// with QUEUE_CONNECTION=sync the delivery happens before the response.
pub async fn test(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", WRITE)?;
    let mut v = Validator::new(&input);
    v.rules("event", &["sometimes", "string"]);
    v.rules("data", &["sometimes", "array"]);
    v.validate()?;
    let wh = bound_webhook(&ctx)?;
    trashed_guard(&wh)?;

    // run(): server-scoped webhooks default to the file.write sample event;
    // everything else defaults to the created-Server sample data.
    let mut event_name = input.get("event").filter(|v| !v.is_null()).map(php_string);
    let mut event_data = input.get("data").cloned();
    if wh.scope == "server" {
        event_name.get_or_insert_with(|| "server:file.write".to_string());
        event_data.get_or_insert_with(|| SERVER_SAMPLE.clone());
    }
    let event_name = event_name.unwrap_or_else(|| WELCOME_GLOBAL_EVENT.to_string());
    let event_data = event_data.unwrap_or_else(|| GLOBAL_SAMPLE.clone());

    process_webhook(&st, &wh, &event_name, &event_data).await?;

    Ok(PhpJson::with_status(StatusCode::ACCEPTED, json!({"dispatched": true, "endpoint": wh.endpoint})).into_response())
}

// --- POST / PATCH ------------------------------------------------------------

/// StoreWebhookRequest::resolveScope().
fn resolve_scope_store(input: &Input) -> String {
    match scalar_input(input, "scope") {
        Some(s) => scope_of(Some(&s)).to_string(),
        None if filled(input, "server_id") => "server".to_string(),
        None => "global".to_string(),
    }
}

/// StoreWebhookRequest::resolveType() — detection always falls back to the
/// default schema (only `regular` is registered in core).
fn resolve_type_store(input: &Input) -> String {
    scalar_input(input, "type").unwrap_or_else(|| REGULAR.to_string())
}

/// UpdateWebhookRequest::resolveScope(): an explicit `server_id: null` does not
/// imply the global scope; a stored scope is kept instead.
fn resolve_scope_update(input: &Input, rec: &webhook_configurations::Model) -> String {
    match scalar_input(input, "scope") {
        Some(s) => scope_of(Some(&s)).to_string(),
        None if filled(input, "server_id") => "server".to_string(),
        None => rec.scope.clone(),
    }
}

/// UpdateWebhookRequest::resolveType(): only an explicit input re-detects; a
/// stored type is deliberate even when it is null or unknown.
fn resolve_type_update<'a>(input: &'a Input, rec: &'a webhook_configurations::Model) -> Option<String> {
    match scalar_input(input, "type") {
        Some(s) => Some(s),
        None => rec.r#type.clone(),
    }
}

/// UpdateWebhookRequest::hasServer() with the explicit-null passthrough.
fn has_server_update(input: &Input, rec: &webhook_configurations::Model) -> bool {
    if has(input, "server_id") { filled(input, "server_id") } else { rec.server_id.is_some() }
}

/// RegularSchema::mutateFormDataBeforeSave: header names are rewritten
/// space to dash before saving.
fn mutate_headers_save(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                out.insert(k.replace(' ', "-"), val.clone());
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

const WELCOME_GLOBAL_EVENT: &str = r"eloquent.created: App\Models\Server";

/// `POST /webhooks`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", WRITE)?;
    let mut v = Validator::new(&input);
    validate_base(&mut v, &input, false).map_err(|_| ApiError::internal("LogicException"))?;
    attach_common_rules(&mut v, false);
    v.custom("server_id", exists_server(&st, input.get("server_id")).await?);
    // withValidator()->after() runs unconditionally, mirroring Laravel's hook order.
    let resolved_scope = resolve_scope_store(&input);
    let after = AfterCheck::new(&input, resolved_scope.clone(), filled(&input, "server_id")).run();
    v.run();
    for (attr, msg) in after.errors.iter().map(|(a, _, m)| (a, m)) {
        v.add_error(attr, msg);
    }
    v.validate()?;

    let data = validated(&input);
    let get = |k: &str| field(&data, k);
    // withDefaults: `??=` fills absent AND explicit-null inputs.
    let scope = match get("scope") {
        Some(Value::String(s)) => s.clone(),
        _ => resolved_scope,
    };
    let wh_type = match get("type") {
        Some(Value::String(s)) => s.clone(),
        _ => resolve_type_store(&input),
    };
    let name = get("name").map(|v| php_string(v)).unwrap_or_default();
    let endpoint = get("endpoint").map(|v| php_string(v)).unwrap_or_default();
    let description = get("description").and_then(|v| match v {
        Value::Null => None,
        other => Some(php_string(other)),
    });
    let server_id = match get("server_id") {
        Some(Value::Null) | None => None,
        Some(other) => Some(Int(pelican_core::php::intval(&php_string(other)))),
    };
    // The resolved scope is always consistent with server_id on store.
    let payload = get("payload").filter(|v| !v.is_null()).cloned();
    let mut headers = get("headers").filter(|v| !v.is_null()).cloned();
    if headers.is_some() {
        headers = headers.map(|h| mutate_headers_save(&h));
    }
    let events = get("events").cloned().unwrap_or_else(|| json!([]));
    let now = Timestamp(pelican_core::time::now());
    let wh = pelican_db::write::insert(
        &st.db,
        webhook_configurations::ActiveModel {
            endpoint: Set(endpoint),
            name: Set(name),
            description: Set(description),
            scope: Set(scope),
            server_id: Set(server_id),
            r#type: Set(Some(wh_type)),
            events: Set(Json(events)),
            headers: Set(headers.map(Json)),
            payload: Set(payload.map(Json)),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    let body = with_meta(
        webhook_item(&st, &ctx, &wh, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("webhooks/{}", wh.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// Compares JSON attributes the way Eloquent's array-cast dirty check does:
/// byte-equal PHP-serializations (key order included). Returns true on a
/// difference (= dirty).
fn json_dirty(current: Option<&Value>, stored: Option<&Json>) -> bool {
    let encode = |v: &Value| laravel_compat::json::to_string(v).unwrap_or_else(|_| "null".into());
    let cur = current.map(encode).unwrap_or_else(|| "null".into());
    let old =
        stored.map(|j| encode(&pelican_core::server_config::php_assoc(j.0.clone()))).unwrap_or_else(|| "null".into());
    cur != old
}

/// Optional string attribute as PHP would store it (`null` included).
fn opt_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| match v {
        Value::Null => None,
        other => Some(php_string(other)),
    })
}

/// `PATCH /webhooks/{webhook}`.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", WRITE)?;
    let rec = bound_webhook(&ctx)?;
    trashed_guard(&rec)?;

    // prepareForValidation(): pull stored values into validation when the type
    // or the scope changes without sending their payload/events.
    let mut data_v = match input.data.clone() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let effective_type = resolve_type_update(&input, &rec);
    if !has(&input, "payload") && effective_type.as_deref() != rec.r#type.as_deref() {
        data_v.insert(
            "payload".into(),
            rec.payload.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(json!([])),
        );
    }
    let effective_scope = resolve_scope_update(&input, &rec);
    if !has(&input, "events") && effective_scope != rec.scope {
        data_v.insert("events".into(), pelican_core::server_config::php_assoc(rec.events.0.clone()));
    }
    let merged = Input { data: Value::Object(data_v), ..input.clone() };

    let mut v = Validator::new(&merged);
    validate_base(&mut v, &merged, true).map_err(|_| ApiError::internal("LogicException"))?;
    attach_common_rules(&mut v, true);
    v.custom("server_id", exists_server(&st, merged.get("server_id")).await?);
    let resolved_scope = effective_scope;
    let after = AfterCheck::new(&merged, resolved_scope.clone(), has_server_update(&merged, &rec)).run();
    v.run();
    for (attr, msg) in after.errors.iter().map(|(a, _, m)| (a, m)) {
        v.add_error(attr, msg);
    }
    v.validate()?;

    // UpdateWebhookRequest::withDefaults(): an ABSENT server_id means "leave
    // unchanged"; an explicit null (or global-force) is a real assignment.
    let data = validated(&merged);
    let get = |k: &str| field(&data, k);
    let explicit_server: Option<Option<Int>> = match get("server_id") {
        Some(Value::Null) => Some(None),
        Some(other) => Some(Some(Int(pelican_core::php::intval(&php_string(other))))),
        None => None,
    };
    let server_final: Option<Option<Int>> = if resolved_scope == "global" { Some(None) } else { explicit_server };

    let mut am =
        webhook_configurations::ActiveModel { id: sea_orm::ActiveValue::Unchanged(rec.id), ..Default::default() };
    let mut dirty = false;

    if let Some(n) = get("name") {
        let s = php_string(n);
        if s != rec.name {
            am.name = Set(s);
            dirty = true;
        }
    }
    if let Some(d) = get("description") {
        let s = opt_str(Some(d));
        if s != rec.description {
            am.description = Set(s);
            dirty = true;
        }
    }
    if let Some(e) = get("endpoint") {
        let s = php_string(e);
        if s != rec.endpoint {
            am.endpoint = Set(s);
            dirty = true;
        }
    }
    // The type is never re-detected on update; an explicit input replaces it.
    if let Some(t) = get("type") {
        let s = opt_str(Some(t));
        if s != rec.r#type {
            am.r#type = Set(s);
            dirty = true;
        }
    }
    // scope assignment: either an explicitly sent value or the one implied by
    // server_id, resolved above.
    let scope_sent = get("scope").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    });
    if let Some(scope) = scope_sent.or_else(|| {
        if get("server_id").is_some() && get("scope").is_none() { Some(resolved_scope.clone()) } else { None }
    }) {
        if scope != rec.scope {
            am.scope = Set(scope);
            dirty = true;
        }
    }
    if let Some(new_sid) = server_final {
        if new_sid != rec.server_id {
            am.server_id = Set(new_sid);
            dirty = true;
        }
    }
    if let Some(ev) = get("events") {
        if json_dirty(Some(ev), Some(&rec.events)) {
            am.events = Set(Json(ev.clone()));
            dirty = true;
        }
    }
    if let Some(p) = get("payload") {
        if json_dirty(if p.is_null() { None } else { Some(p) }, rec.payload.as_ref()) {
            am.payload = Set(if p.is_null() { None } else { Some(Json(p.clone())) });
            dirty = true;
        }
    }
    if let Some(h) = get("headers") {
        let mutated = if h.is_null() { None } else { Some(mutate_headers_save(h)) };
        if json_dirty(mutated.as_ref(), rec.headers.as_ref()) {
            am.headers = Set(mutated.map(Json));
            dirty = true;
        }
    }
    if dirty {
        am.updated_at = Set(Some(Timestamp(pelican_core::time::now())));
        pelican_db::write::update(&st.db, &am).await?;
    }
    let fresh =
        webhook_configurations::Entity::find_by_id(rec.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(webhook_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?).into_response())
}

/// `DELETE /webhooks/{webhook}` — SoftDeletes: UPDATE sets deleted_at AND
/// touched updated_at (SoftDeletes::runSoftDelete, usesTimestamps=true).
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("webhook", WRITE)?;
    validate_list(&input)?;
    let wh = bound_webhook(&ctx)?;
    trashed_guard(&wh)?;
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::update(
        &st.db,
        &webhook_configurations::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(wh.id),
            deleted_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    Ok(no_content())
}
