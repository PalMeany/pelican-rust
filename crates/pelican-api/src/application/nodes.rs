//! `/api/application/nodes` — `NodeController`, `NodeConfigurationController`,
//! `NodeDeploymentController`, `AllocationController` (spec §3.2).
//!
//! Create/update responses render the in-memory Eloquent model, so attribute
//! order and value types follow PHP (`$attributes` defaults, fill order, raw
//! input types for uncast columns) rather than a fresh database row.

use std::net::IpAddr;

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, mounts, nodes, servers};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use super::data::{NodeExtra, allocated_resources, allocation_item, node_attributes, node_item, ts};
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, item, list, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{FilterValue, filters, partial, sorts};
use crate::validation::Validator;

const DEFAULT_DAEMON_BASE: &str = "/var/lib/pelican/volumes";

fn bound(ctx: &Ctx) -> ApiResult<nodes::Model> {
    ctx.bound.node.clone().ok_or_else(ApiError::model_not_found)
}

/// PHP `(int)` of a request value.
fn int_cast(v: &Value) -> i64 {
    match v {
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => {
            let t = s.trim();
            t.parse::<i64>()
                .unwrap_or_else(|_| t.parse::<f64>().map(|f| f as i64).unwrap_or_else(|_| pelican_core::php::intval(t)))
        }
        _ => 0,
    }
}

fn bool_cast(v: &Value) -> bool {
    !pelican_core::php::empty(v)
}

/// Node casts applied by `toArray()` (`Node::casts()`); hidden token columns dropped.
fn cast(key: &str, v: Value) -> Option<Value> {
    Some(match key {
        "daemon_token" | "daemon_token_id" => return None,
        "memory" | "disk" | "cpu" | "daemon_listen" | "daemon_connect" | "daemon_sftp" => json!(int_cast(&v)),
        "behind_proxy" | "public" | "maintenance_mode" => json!(bool_cast(&v)),
        "tags" => match v {
            Value::String(s) => {
                serde_json::from_str::<Value>(&s).map(pelican_core::server_config::php_assoc).unwrap_or(Value::Null)
            }
            other => pelican_core::server_config::php_assoc(other),
        },
        _ => v,
    })
}

/// `GET /nodes`.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("node", READ)?;
    let b = st.db.get_database_backend();
    let names = ["uuid", "name", "fqdn", "daemon_token_id"];
    let fs = filters(&input, &names)?;
    let ss = sorts(&input, &["id", "uuid", "memory", "disk", "cpu"])?;
    let page = Page::new(&input, 50)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in names {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("nodes.{name}"), v) {
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
    let total =
        count(&st.db, &format!("select count(*) as {{aggregate}} from {{nodes}}{where_sql}"), vals.clone()).await?;
    let mut data = Vec::new();
    if total > 0 {
        let rows = nodes::Model::find_by_statement(raw::stmt(
            b,
            &format!("select * from {{nodes}}{where_sql}{order}{}", page.sql()),
            vals,
        ))
        .all(&st.db)
        .await?;
        for n in &rows {
            data.push(node_item(&st, &ctx, n, &includes, NodeExtra::default()).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

async fn count<C: ConnectionTrait>(db: &C, sql: &str, vals: Vec<sea_orm::Value>) -> ApiResult<i64> {
    let row = db.query_one_raw(raw::stmt(db.get_database_backend(), sql, vals)).await?;
    Ok(row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0))
}

/// `GET /nodes/{node}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("node", READ)?;
    let n = bound(&ctx)?;
    Ok(PhpJson::ok(node_item(&st, &ctx, &n, &Includes::parse(&input), NodeExtra::default()).await?).into_response())
}

/// `Node::getConfiguration()` from attribute values (`upload_limit` / `data` keep their raw type).
pub fn configuration(st: &AppState, attrs: &Map<String, Value>, token: &str, mounts_: &[mounts::Model]) -> Value {
    let get = |k: &str| attrs.get(k).cloned().unwrap_or(Value::Null);
    let fqdn = pelican_core::php::string(&get("fqdn")).to_lowercase();
    let ssl = !bool_cast(&get("behind_proxy")) && get("scheme") == json!("https");
    json!({
        "debug": false,
        "uuid": get("uuid"),
        "token_id": get("daemon_token_id"),
        "token": token,
        "api": {
            "host": "0.0.0.0",
            "port": int_cast(&get("daemon_listen")),
            "ssl": {
                "enabled": ssl,
                "cert": format!("/etc/letsencrypt/live/{fqdn}/fullchain.pem"),
                "key": format!("/etc/letsencrypt/live/{fqdn}/privkey.pem"),
            },
            "upload_limit": get("upload_size"),
        },
        "system": {"data": get("daemon_base"), "sftp": {"bind_port": int_cast(&get("daemon_sftp"))}},
        "allowed_mounts": mounts_.iter().map(|m| json!(m.source)).collect::<Vec<_>>(),
        "remote": st.config.app_url,
    })
}

/// All columns of a node row as raw attribute values (incl. hidden ones), physical order.
pub fn raw_attributes(st: &AppState, n: &nodes::Model) -> ApiResult<Map<String, Value>> {
    let cols = pelican_db::column_order::columns(st.driver, "nodes").unwrap_or(&[]);
    let mut visible = node_attributes(st.driver, n)?;
    let mut out = Map::new();
    for c in cols {
        let v = match *c {
            "daemon_token" => json!(n.daemon_token),
            "daemon_token_id" => json!(n.daemon_token_id),
            "created_at" => n.created_at.map(|t| json!(t.0.format("%F %T").to_string())).unwrap_or(Value::Null),
            "updated_at" => n.updated_at.map(|t| json!(t.0.format("%F %T").to_string())).unwrap_or(Value::Null),
            other => visible.remove(other).unwrap_or(Value::Null),
        };
        out.insert((*c).to_string(), v);
    }
    Ok(out)
}

/// Mounts of a node (`morphToMany`), natural order.
pub async fn node_mounts<C: ConnectionTrait>(db: &C, node_id: i64) -> ApiResult<Vec<mounts::Model>> {
    Ok(mounts::Model::find_by_statement(raw::stmt(
        db.get_database_backend(),
        "select {mounts}.* from {mounts} inner join {mountables} on {mounts.id} = {mountables.mount_id} \
         where {mountables.mountable_id} = ? and {mountables.mountable_type} = ?",
        vec![node_id.into(), "node".into()],
    ))
    .all(db)
    .await?)
}

/// `GET /nodes/{node}/configuration` (not enveloped, token unredacted).
pub async fn config(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("node", READ)?;
    let n = bound(&ctx)?;
    let token = st.enc.decrypt_string_lenient(&n.daemon_token).map_err(|_| ApiError::internal("DecryptException"))?;
    let ms = node_mounts(&st.db, n.id.0).await?;
    Ok(PhpJson::ok(configuration(&st, &raw_attributes(&st, &n)?, &token, &ms)).into_response())
}

/// `Node::getRules()` (`StoreNodeRequest` / `UpdateNodeRequest`).
const RULES: &[(&str, &[&str])] = &[
    ("name", &["required", "string", "min:1", "max:100"]),
    ("description", &["string", "nullable"]),
    ("public", &["boolean"]),
    ("fqdn", &["required", "string", "not_in:0.0.0.0,127.0.0.1,localhost"]),
    ("scheme", &["required", "string", "in:http,https"]),
    ("behind_proxy", &["boolean"]),
    ("memory", &["required", "numeric", "min:0"]),
    ("memory_overallocate", &["required", "numeric", "min:-1"]),
    ("disk", &["required", "numeric", "min:0"]),
    ("disk_overallocate", &["required", "numeric", "min:-1"]),
    ("cpu", &["required", "numeric", "min:0"]),
    ("cpu_overallocate", &["required", "numeric", "min:-1"]),
    ("daemon_base", &["sometimes", "required", r"regex:/^([\/][\d\w.\-\/]+)$/"]),
    ("daemon_sftp", &["required", "numeric", "between:1,65535"]),
    ("daemon_sftp_alias", &["nullable", "string"]),
    ("daemon_listen", &["required", "numeric", "between:1,65535"]),
    ("daemon_connect", &["required", "numeric", "between:1,65535"]),
    ("maintenance_mode", &["boolean"]),
    ("upload_size", &["integer", "min:1"]),
    ("tags", &["array"]),
];

/// Validate and return `validated()` (rule order, `daemon_base` defaulted).
fn validate(input: &Input) -> ApiResult<Vec<(String, Value)>> {
    let mut v = Validator::new(input);
    for (k, r) in RULES {
        v.rules(k, r);
    }
    v.attribute_name("daemon_base", "Daemon Base Path");
    v.attribute_name("upload_size", "File Upload Size Limit");
    v.attribute_name("public", "Node Visibility");
    v.validate()?;
    let mut out: Vec<(String, Value)> =
        RULES.iter().filter_map(|(k, _)| input.get(k).map(|v| ((*k).to_string(), v.clone()))).collect();
    match out.iter_mut().find(|(k, _)| k == "daemon_base") {
        Some((_, v)) if v.is_null() => *v = json!(DEFAULT_DAEMON_BASE),
        Some(_) => {}
        None => out.push(("daemon_base".into(), json!(DEFAULT_DAEMON_BASE))),
    }
    Ok(out)
}

/// Raw attribute value → typed ActiveModel column.
fn set_column(am: &mut nodes::ActiveModel, key: &str, v: &Value) {
    let s = || pelican_core::php::string(v);
    let opt = || if v.is_null() { None } else { Some(pelican_core::php::string(v)) };
    match key {
        "name" => am.name = Set(s()),
        "description" => am.description = Set(opt()),
        "public" => am.public = Set(SmallBool(bool_cast(v))),
        "fqdn" => am.fqdn = Set(s()),
        "scheme" => am.scheme = Set(s()),
        "behind_proxy" => am.behind_proxy = Set(Bool(bool_cast(v))),
        "memory" => am.memory = Set(Int(int_cast(v))),
        "memory_overallocate" => am.memory_overallocate = Set(Int(int_cast(v))),
        "disk" => am.disk = Set(Int(int_cast(v))),
        "disk_overallocate" => am.disk_overallocate = Set(Int(int_cast(v))),
        "cpu" => am.cpu = Set(Int(int_cast(v))),
        "cpu_overallocate" => am.cpu_overallocate = Set(Int(int_cast(v))),
        "daemon_base" => am.daemon_base = Set(s()),
        "daemon_sftp" => am.daemon_sftp = Set(Int(int_cast(v))),
        "daemon_sftp_alias" => am.daemon_sftp_alias = Set(opt()),
        "daemon_listen" => am.daemon_listen = Set(Int(int_cast(v))),
        "daemon_connect" => am.daemon_connect = Set(Int(int_cast(v))),
        "maintenance_mode" => am.maintenance_mode = Set(Bool(bool_cast(v))),
        "upload_size" => am.upload_size = Set(Int(int_cast(v))),
        "tags" => am.tags = Set(Some(Json(pelican_core::server_config::php_assoc(v.clone())))),
        _ => {}
    }
}

/// Node `$attributes` defaults in declaration order (raw types; `tags` is the raw
/// JSON string like Eloquent holds it until a cast reads it).
fn default_attributes() -> Map<String, Value> {
    Map::from_iter([
        ("public".to_string(), json!(true)),
        ("behind_proxy".to_string(), json!(false)),
        ("memory".to_string(), json!(0)),
        ("memory_overallocate".to_string(), json!(0)),
        ("disk".to_string(), json!(0)),
        ("disk_overallocate".to_string(), json!(0)),
        ("cpu".to_string(), json!(0)),
        ("cpu_overallocate".to_string(), json!(0)),
        ("daemon_base".to_string(), json!(DEFAULT_DAEMON_BASE)),
        ("daemon_sftp".to_string(), json!(2022)),
        ("daemon_listen".to_string(), json!(8080)),
        ("daemon_connect".to_string(), json!(8080)),
        ("maintenance_mode".to_string(), json!(false)),
        ("tags".to_string(), json!("[]")),
    ])
}

/// Attributes after applying `Node::casts()` (`HasValidation::validate()` runs its rules
/// over `addCastAttributesToArray`).
fn casted(attrs: &Map<String, Value>) -> Map<String, Value> {
    attrs.iter().filter_map(|(k, v)| cast(k, v.clone()).map(|v| (k.clone(), v))).collect()
}

/// Result of [`create`]: the inserted row plus the prepared attributes it was rendered from.
#[derive(Debug)]
pub struct CreatedNode {
    pub node: nodes::Model,
    /// In-memory Eloquent attributes (model defaults + fill + created row), raw values.
    pub attrs: Map<String, Value>,
}

/// `Node::create($data)` outside the HTTP pipeline (`p:node:make`), shared with [`store`]:
/// model defaults + the fillable pairs (validated order), the model-level validation pass,
/// the creating hook (`uuid`, random `daemon_token`/`daemon_token_id`), insert and
/// `eloquent.created`. Returns the row and its prepared raw attributes for rendering.
///
/// # Errors
/// Validation errors have the same shape as [`validate`] (ApiError::validation).
pub async fn create(st: &AppState, data: Vec<(String, Value)>) -> ApiResult<CreatedNode> {
    // Model `$attributes` defaults, then the fill in given order.
    let mut attrs = default_attributes();
    for (k, v) in &data {
        attrs.insert(k.clone(), v.clone());
    }
    // ValidationObserver::saving → HasValidation::validate() — before the creating hook.
    let attrs_value = Value::Object(casted(&attrs));
    let mut v = Validator::for_value(&attrs_value);
    for (k, r) in RULES {
        v.rules(k, r);
    }
    v.validate()?;
    let uuid = uuid::Uuid::new_v4().to_string();
    let token = pelican_wings::jwt::str_random(64);
    let token_id = pelican_wings::jwt::str_random(16);
    attrs.insert("uuid".into(), json!(uuid.clone()));
    attrs.insert("daemon_token".into(), json!(""));
    attrs.insert("daemon_token_id".into(), json!(token_id.clone()));

    let now = Timestamp(pelican_core::time::now());
    let mut am = nodes::ActiveModel {
        public: Set(SmallBool(true)),
        behind_proxy: Set(Bool(false)),
        memory: Set(Int(0)),
        memory_overallocate: Set(Int(0)),
        disk: Set(Int(0)),
        disk_overallocate: Set(Int(0)),
        cpu: Set(Int(0)),
        cpu_overallocate: Set(Int(0)),
        daemon_base: Set(DEFAULT_DAEMON_BASE.into()),
        daemon_sftp: Set(Int(2022)),
        daemon_listen: Set(Int(8080)),
        daemon_connect: Set(Int(8080)),
        maintenance_mode: Set(Bool(false)),
        tags: Set(Some(Json(json!([])))),
        uuid: Set(Some(uuid)),
        daemon_token: Set(st.enc.encrypt_string(&token)),
        daemon_token_id: Set(Some(token_id)),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    for (k, v) in &data {
        set_column(&mut am, k, v);
    }
    let created = pelican_db::write::insert(&st.db, am).await?;
    crate::model_events::created(st, &created).await;
    attrs.insert("updated_at".into(), ts(Some(&now))?);
    attrs.insert("created_at".into(), ts(Some(&now))?);
    attrs.insert("id".into(), json!(created.id.0));
    Ok(CreatedNode { node: created, attrs })
}

/// `json_encode($v, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)`
/// (`Node::getJsonConfiguration(true)` and `p:node:list --format=json`).
pub fn pretty_json(v: &Value) -> ApiResult<String> {
    laravel_compat::json::to_string_with_flags(
        v,
        laravel_compat::json::JSON_PRETTY_PRINT | laravel_compat::json::JSON_UNESCAPED_SLASHES,
    )
    .map_err(|_| ApiError::internal("JsonException"))
}

/// `Yaml::dump($v, 4, 2, DUMP_EMPTY_ARRAY_AS_SEQUENCE)` (`Node::getYamlConfiguration()`,
/// also how the admin UI renders `/etc/pelican/config.yml`).
pub fn yaml_configuration(v: &Value) -> String {
    egg_format::yaml::dump_empty_as_sequence(v, 4)
}

/// The `GET /nodes/{node}/configuration` payload without the HTTP pipeline (ACL, binding):
/// lets `p:node:configuration` print exactly what the API serves.
///
/// # Errors
/// `DecryptException` when the token cannot be decrypted, database errors otherwise.
pub async fn node_configuration(st: &AppState, n: &nodes::Model) -> ApiResult<Value> {
    let token = st.enc.decrypt_string_lenient(&n.daemon_token).map_err(|_| ApiError::internal("DecryptException"))?;
    let ms = node_mounts(&st.db, n.id.0).await?;
    Ok(configuration(st, &raw_attributes(st, n)?, &token, &ms))
}

/// `POST /nodes`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("node", WRITE)?;
    let data = validate(&input)?;
    let CreatedNode { node: created, attrs } = create(&st, data).await?;
    let mut out = Map::new();
    for (k, v) in attrs {
        if let Some(v) = cast(&k, v) {
            out.insert(k, v);
        }
    }
    out.insert("allocated_resources".into(), json!({"memory": 0, "disk": 0, "cpu": 0}));
    let body = with_meta(
        node_envelope(&st, &ctx, &created, out, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("nodes/{}", created.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// Wrap prepared attributes with the node includes (allocations, servers).
async fn node_envelope(
    st: &AppState,
    ctx: &Ctx,
    n: &nodes::Model,
    attrs: Map<String, Value>,
    inc: &Includes,
) -> ApiResult<Value> {
    // Resolve includes through the shared resource, then swap the attributes in.
    let full = node_item(st, ctx, n, inc, NodeExtra::default()).await?;
    let rel = full.get("attributes").and_then(|a| a.get("relationships")).cloned();
    let mut a = attrs;
    if let Some(r) = rel {
        a.insert("relationships".into(), r);
    }
    Ok(item("node", Value::Object(a), vec![]))
}

/// `PATCH /nodes/{node}` (`NodeUpdateService`).
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("node", WRITE)?;
    let mut node = bound(&ctx)?;
    let data = validate(&input)?;
    let reset = input.get("reset_secret") == Some(&Value::Bool(true));
    let includes = Includes::parse(&input);

    // `$node->replicate()`: every column except id/created_at/updated_at, then forceFill.
    let mut attrs = raw_attributes(&st, &node)?;
    // shift_remove: Map::remove would swap the last column into the hole.
    attrs.shift_remove("id");
    attrs.shift_remove("created_at");
    attrs.shift_remove("updated_at");
    for (k, v) in &data {
        attrs.insert(k.clone(), v.clone());
    }
    let mut token =
        st.enc.decrypt_string_lenient(&node.daemon_token).map_err(|_| ApiError::internal("DecryptException"))?;
    let old_conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    let mut am = nodes::ActiveModel { id: sea_orm::ActiveValue::Unchanged(node.id), ..Default::default() };
    for (k, v) in &data {
        set_column(&mut am, k, v);
    }
    if reset {
        token = pelican_wings::jwt::str_random(64);
        let id = pelican_wings::jwt::str_random(16);
        am.daemon_token = Set(st.enc.encrypt_string(&token));
        am.daemon_token_id = Set(Some(id.clone()));
        attrs.insert("daemon_token_id".into(), json!(id));
    }
    let now = Timestamp(pelican_core::time::now());
    am.updated_at = Set(Some(now));
    attrs.insert("id".into(), json!(node.id.0));
    attrs.insert("created_at".into(), ts(node.created_at.as_ref())?);
    attrs.insert("updated_at".into(), ts(Some(&now))?);

    let txn = st.db.begin().await?;
    pelican_db::write::update(&txn, &am).await?;
    let ms = node_mounts(&txn, node.id.0).await?;
    let body = configuration(&st, &attrs, &token, &ms);
    // `setNode($node)` after `$node->fqdn = $updated->fqdn`: old scheme/port/token, new fqdn.
    let new_fqdn = attrs.get("fqdn").map(pelican_core::php::string).unwrap_or_else(|| node.fqdn.clone());
    let mut conn = old_conn;
    conn.fqdn = new_fqdn.clone();
    let call = pelican_wings::client::Call::new(pelican_wings::client::Method::POST, "/api/update").json(body);
    let outcome = st.wings.send(&conn, call).await;
    node.fqdn = new_fqdn;
    match outcome {
        Ok(_) => {
            txn.commit().await?;
            // NodeUpdateService: $updated->forceFill(...)->save() (eloquent.updated
            // after the commit; the rolled-back path fires nothing here).
            crate::model_events::fire_id::<nodes::Entity>(&st, crate::model_events::Action::Updated, node.id.0).await;
        }
        Err(pelican_wings::WingsError::Connection(_)) => {
            // Stored anyway; ConfigurationNotPersistedException is reported, the original node returned.
            txn.commit().await?;
            crate::model_events::fire_id::<nodes::Entity>(&st, crate::model_events::Action::Updated, node.id.0).await;
            return Ok(PhpJson::ok(node_item(&st, &ctx, &node, &includes, NodeExtra::default()).await?).into_response());
        }
        Err(_) => {
            txn.rollback().await?;
            return Ok(PhpJson::ok(node_item(&st, &ctx, &node, &includes, NodeExtra::default()).await?).into_response());
        }
    }
    let mut out = Map::new();
    for (k, v) in attrs {
        if let Some(v) = cast(&k, v) {
            out.insert(k, v);
        }
    }
    // getConfiguration() loaded `mounts`, which leaks into toArray().
    out.insert(
        "mounts".into(),
        Value::Array(
            ms.iter()
                .map(|m| {
                    let mut a = super::data::mount_attributes(st.driver, m);
                    a.insert(
                        "pivot".into(),
                        json!({"mountable_type": "node", "mountable_id": node.id.0, "mount_id": m.id.0}),
                    );
                    Value::Object(a)
                })
                .collect(),
        ),
    );
    out.insert("allocated_resources".into(), allocated_resources(&st.db, node.id.0).await?);
    let fresh = nodes::Entity::find_by_id(node.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(node_envelope(&st, &ctx, &fresh, out, &includes).await?).into_response())
}

/// `DELETE /nodes/{node}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("node", WRITE)?;
    let n = bound(&ctx)?;
    let used = servers::Entity::find().filter(servers::Column::NodeId.eq(n.id)).one(&st.db).await?;
    if used.is_some() {
        return Err(ApiError::display(
            "HasActiveServersException",
            400,
            "A node must have no servers linked to it in order to be deleted.",
        ));
    }
    nodes::Entity::delete_by_id(n.id).exec(&st.db).await?;
    // NodeDeletionService: $node->delete() (eloquent.deleted)
    crate::model_events::deleted(&st, &n).await;
    Ok(no_content())
}

/// `GET /nodes/deployable` (`FindViableNodesService`, not paginated).
pub async fn deployable(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("node", READ)?;
    let mut v = Validator::new(&input);
    v.rules("page", &["integer"]);
    v.rules("memory", &["required", "integer", "min:0"]);
    v.rules("disk", &["required", "integer", "min:0"]);
    v.rules("cpu", &["sometimes", "integer", "min:0"]);
    v.rules("tags", &["sometimes", "array"]);
    v.validate()?;
    let num = |k: &str| input.get(k).map(int_cast).unwrap_or(0);
    let (memory, disk, cpu) = (num("memory"), num("disk"), num("cpu"));
    let tags: Vec<String> = match input.get("tags") {
        Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
        Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
        _ => Vec::new(),
    };
    let b = st.db.get_database_backend();
    // `where('public', true)`: PDO sends the bool as '1' (`public` is smallint on PostgreSQL).
    let truthy: sea_orm::Value = 1.into();
    // MySQL's SUM() is DECIMAL, which PDO hands back as a string.
    let sum = |col: &str| {
        let q = format!("(select sum({{servers.{col}}}) from {{servers}} where {{nodes.id}} = {{servers.node_id}})");
        if b == sea_orm::DbBackend::MySql { format!("cast({q} as char)") } else { q }
    };
    let sql = format!(
        "select {{nodes}}.*, {} as {{servers_sum_memory}}, {} as {{servers_sum_disk}}, {} as {{servers_sum_cpu}} from {{nodes}} where {{public}} = ?",
        sum("memory"),
        sum("disk"),
        sum("cpu")
    );
    let rows = st.db.query_all_raw(raw::stmt(b, &sql, vec![truthy])).await?;
    let includes = Includes::parse(&input);
    let mut data = Vec::new();
    for r in rows {
        let n = nodes::Model::from_query_result(&r, "")?;
        let node_tags: Vec<String> = match n.tags.as_ref().map(|t| &t.0) {
            Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
            Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
            _ => Vec::new(),
        };
        if !tags.is_empty() && !node_tags.iter().any(|t| tags.contains(t)) {
            continue;
        }
        let mut sums = Vec::new();
        let mut used = [0i64; 3];
        for (i, col) in ["servers_sum_memory", "servers_sum_disk", "servers_sum_cpu"].iter().enumerate() {
            let v = sum_value(&r, col, b);
            used[i] = int_cast(&v);
            sums.push(((*col).to_string(), v));
        }
        let viable = |limit: i64, over: i64, sum: i64, req: i64| {
            !(limit > 0 && over >= 0) || (sum + req) as f64 <= limit as f64 * (1.0 + over as f64 / 100.0)
        };
        if !viable(n.memory.0, n.memory_overallocate.0, used[0], memory)
            || !viable(n.disk.0, n.disk_overallocate.0, used[1], disk)
            || !viable(n.cpu.0, n.cpu_overallocate.0, used[2], cpu)
        {
            continue;
        }
        data.push(node_item(&st, &ctx, &n, &includes, NodeExtra { after_columns: sums }).await?);
    }
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `withSum()` column as PDO returns it: MySQL SUM is DECIMAL → string.
fn sum_value(r: &sea_orm::QueryResult, col: &str, b: sea_orm::DbBackend) -> Value {
    if b == sea_orm::DbBackend::MySql {
        return match r.try_get::<Option<String>>("", col) {
            Ok(Some(s)) => json!(s),
            _ => Value::Null,
        };
    }
    match r.try_get::<Option<i64>>("", col) {
        Ok(Some(i)) => json!(i),
        _ => Value::Null,
    }
}

// --- allocations ---------------------------------------------------------------

/// `GET /nodes/{node}/allocations`.
pub async fn allocations_index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("allocation", READ)?;
    let n = bound(&ctx)?;
    let b = st.db.get_database_backend();
    let fs = filters(&input, &["ip", "port", "ip_alias", "server_id"])?;
    let page = Page::new(&input, 50)?;
    let includes = Includes::parse(&input);
    let mut wh = vec!["{allocations.node_id} = ?".to_string(), "{allocations.node_id} is not null".to_string()];
    let mut vals: Vec<sea_orm::Value> = vec![n.id.into()];
    for (name, value) in [
        ("ip", fs.iter().find(|(k, _)| k == "ip")),
        ("port", fs.iter().find(|(k, _)| k == "port")),
        ("ip_alias", fs.iter().find(|(k, _)| k == "ip_alias")),
        ("server_id", fs.iter().find(|(k, _)| k == "server_id")),
    ] {
        let Some((_, v)) = value else { continue };
        match name {
            "ip" | "port" => match v {
                FilterValue::Null => {}
                FilterValue::List(items) => {
                    let marks = vec!["?"; items.len()].join(", ");
                    wh.push(format!("{{allocations.{name}}} in ({marks})"));
                    vals.extend(items.iter().map(|i| typed(name, i.as_php_string())));
                }
                other => {
                    wh.push(format!("{{allocations.{name}}} = ?"));
                    vals.push(typed(name, other.as_php_string()));
                }
            },
            "ip_alias" => {
                if let Some((sql, mut values)) = partial(b, "allocations.ip_alias", v) {
                    wh.push(sql);
                    vals.append(&mut values);
                }
            }
            _ => match v {
                FilterValue::Null => {}
                FilterValue::List(_) => return Err(ApiError::internal("ErrorException")),
                FilterValue::Str(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
                    wh.push("{server_id} = ?".into());
                    vals.push(typed("server_id", s.clone()));
                }
                _ => wh.push("{server_id} is null".into()),
            },
        }
    }
    let where_sql = format!(" where {}", wh.join(" and "));
    let total =
        count(&st.db, &format!("select count(*) as {{aggregate}} from {{allocations}}{where_sql}"), vals.clone())
            .await?;
    let mut data = Vec::new();
    if total > 0 {
        let rows = allocations::Model::find_by_statement(raw::stmt(
            b,
            &format!("select * from {{allocations}}{where_sql}{}", page.sql()),
            vals,
        ))
        .all(&st.db)
        .await?;
        for a in &rows {
            data.push(allocation_item(&st, &ctx, a, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// IPv4/IPv6 network for `IPTools\Network::parse` (single address or CIDR).
fn parse_network(ip: &str) -> Result<Vec<String>, String> {
    let (addr, mask) = match ip.split_once('/') {
        Some((a, m)) => (a, Some(m)),
        None => (ip, None),
    };
    let parsed: IpAddr = addr.trim().parse().map_err(|_| "Invalid IP address format".to_string())?;
    match (parsed, mask) {
        (IpAddr::V4(v4), Some(m)) => {
            let bits: u32 = m.parse().map_err(|_| "Invalid prefix length".to_string())?;
            let base = u32::from(v4) & (if bits == 0 { 0 } else { u32::MAX << (32 - bits) });
            let size = 1u64 << (32 - bits);
            Ok((0..size).map(|i| std::net::Ipv4Addr::from(base + i as u32).to_string()).collect())
        }
        (IpAddr::V6(v6), Some(m)) => {
            let bits: u32 = m.parse().map_err(|_| "Invalid prefix length".to_string())?;
            if bits < 120 {
                return Err("network too large".into());
            }
            let base = u128::from(v6) & (u128::MAX << (128 - bits));
            Ok((0..(1u128 << (128 - bits))).map(|i| std::net::Ipv6Addr::from(base + i).to_string()).collect())
        }
        (a, None) => Ok(vec![a.to_string()]),
    }
}

/// PDO sends strings untyped, so PostgreSQL coerces `'25565'` to the integer
/// column; sqlx would send text. Bind integral strings as integers there.
fn typed(column: &str, s: String) -> sea_orm::Value {
    match (column, s.trim().parse::<i64>()) {
        ("port" | "server_id", Ok(i)) => i.into(),
        _ => s.into(),
    }
}

fn display(code: &str, msg: impl Into<String>) -> ApiError {
    ApiError::display(code, 400, msg)
}

/// `POST /nodes/{node}/allocations` (`AssignmentService`).
pub async fn allocations_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("allocation", WRITE)?;
    let n = bound(&ctx)?;
    let mut v = Validator::new(&input);
    v.rules("ip", &["required", "string"]);
    v.rules("alias", &["sometimes", "nullable", "string", "max:255"]);
    v.rules("ports", &["required", "array"]);
    v.rules("ports.*", &["string"]);
    v.validate()?;
    let ip = input.get("ip").map(pelican_core::php::string).unwrap_or_default();
    let alias = input.get("alias").filter(|v| !v.is_null()).map(pelican_core::php::string);
    let ports: Vec<Value> = match input.get("ports") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        _ => Vec::new(),
    };
    assign_allocations(&st, n.id, &ip, &ports, alias, None, false).await?;
    Ok(no_content())
}

/// `AssignmentService::handle()`: create allocations for every IP of `ip`
/// (address or CIDR) × `ports` (single ports or `a-b` ranges). Returns the ids.
pub async fn assign_allocations(
    st: &AppState,
    node_id: Int,
    ip: &str,
    ports: &[Value],
    alias: Option<String>,
    server_id: Option<Int>,
    is_locked: bool,
) -> ApiResult<Vec<i64>> {
    let ip = ip.to_string();
    if let Some((_, mask)) = ip.split_once('/') {
        let bad = !(!mask.is_empty() && mask.chars().all(|c| c.is_ascii_digit()))
            || mask.parse::<i64>().map(|m| !(25..=32).contains(&m)).unwrap_or(true);
        if bad {
            return Err(display("CidrOutOfRangeException", "CIDR notation only allows masks between /25 and /32."));
        }
    }
    let ips = parse_network(&ip).map_err(|e| {
        display("DisplayException", format!("Could not parse provided allocation IP address ({ip}): {e}"))
    })?;
    let range_re = regex::Regex::new(r"^(\d{4,5})-(\d{4,5})$").map_err(|_| ApiError::internal("Error"))?;
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    let mut ids = Vec::new();
    for addr in &ips {
        for p in ports {
            let ps = pelican_core::php::string(p);
            let digit = !matches!(p, Value::Bool(_)) && !ps.is_empty() && ps.chars().all(|c| c.is_ascii_digit());
            let caps = range_re.captures(&ps);
            if !digit && caps.is_none() {
                return Err(display(
                    "InvalidPortMappingException",
                    format!("The mapping provided for {ps} was invalid and could not be processed."),
                ));
            }
            let block: Vec<i64> = if let Some(c) = caps {
                let a: i64 = c[1].parse().unwrap_or(0);
                let b: i64 = c[2].parse().unwrap_or(0);
                let block: Vec<i64> = if a <= b { (a..=b).collect() } else { (b..=a).rev().collect() };
                if block.len() > 1000 {
                    return Err(display(
                        "TooManyPortsInRangeException",
                        "Adding more than 1000 ports in a single range at once is not supported.",
                    ));
                }
                if a < 1024 || b > 65535 {
                    return Err(port_range());
                }
                block
            } else {
                let port = pelican_core::php::intval(&ps);
                if !(1024..=65535).contains(&port) {
                    return Err(port_range());
                }
                vec![port]
            };
            for port in block {
                let id = pelican_db::write::insert_id(
                    &txn,
                    &allocations::ActiveModel {
                        node_id: Set(node_id),
                        ip: Set(addr.clone()),
                        port: Set(Int(port)),
                        ip_alias: Set(alias.clone()),
                        server_id: Set(server_id),
                        is_locked: Set(Bool(is_locked)),
                        show_port: Set(Bool(true)),
                        created_at: Set(Some(now)),
                        updated_at: Set(Some(now)),
                        ..Default::default()
                    },
                )
                .await?;
                ids.extend(id);
            }
        }
    }
    txn.commit().await?;
    // AssignmentService: Allocation::query()->create($newAllocation) per row
    // (eloquent.created for every allocation, in insert order)
    for id in &ids {
        crate::model_events::fire_id::<allocations::Entity>(st, crate::model_events::Action::Created, *id).await;
    }
    Ok(ids)
}

fn port_range() -> ApiError {
    display(
        "PortOutOfRangeException",
        "Ports in an allocation must be greater than or equal to 1024 and less than or equal to 65535.",
    )
}

/// `DELETE /nodes/{node}/allocations/{allocation}`.
pub async fn allocations_delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("allocation", WRITE)?;
    let a = ctx.bound.allocation.clone().ok_or_else(ApiError::model_not_found)?;
    if a.server_id.is_some() {
        return Err(display(
            "ServerUsingAllocationException",
            "A server is currently assigned to this allocation. An allocation can only be deleted if no server is currently assigned.",
        ));
    }
    allocations::Entity::delete_by_id(a.id).exec(&st.db).await?;
    // $allocation->delete() (eloquent.deleted)
    crate::model_events::deleted(&st, &a).await;
    Ok(no_content())
}
