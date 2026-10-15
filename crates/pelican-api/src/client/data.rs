//! Client API resources (`app/Data/Api/Client/*Data.php`), client-api.md §9.

use std::sync::LazyLock;

use pelican_core::server_config::{ServerBundle, php_assoc};
use pelican_core::state::AppState;
use pelican_db::entities::{activity_logs, allocations, egg_variables, eggs, nodes, servers, subusers, users};
use pelican_db::raw;
use sea_orm::{ConnectionTrait, EntityTrait, FromQueryResult};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::envelope::{Includes, item, list, null_resource};
use crate::error::{ApiError, ApiResult};
use crate::pipeline::Ctx;
use crate::resources::{atom, format_timestamp, gravatar};

/// `AllocationData`.
pub fn allocation_data(a: &allocations::Model, server_allocation_id: Option<i64>, hide_notes: bool) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(a.id.0));
    m.insert("ip".into(), json!(a.ip));
    m.insert("ip_alias".into(), json!(a.ip_alias));
    m.insert("port".into(), json!(a.port.0));
    m.insert("notes".into(), if hide_notes { Value::Null } else { json!(a.notes) });
    m.insert("is_default".into(), json!(server_allocation_id == Some(a.id.0)));
    item("allocation", Value::Object(m), vec![])
}

/// `EggVariableData` (rules array joined with `|`).
pub fn egg_variable_data(v: &egg_variables::Model, server_value: Option<&str>) -> Value {
    let rules = match &v.rules.0 {
        Value::Array(a) => a.iter().map(pelican_core::php::string).collect::<Vec<_>>().join("|"),
        Value::Object(m) => m.values().map(pelican_core::php::string).collect::<Vec<_>>().join("|"),
        other => pelican_core::php::string(other),
    };
    let mut m = Map::new();
    m.insert("name".into(), json!(v.name));
    m.insert("description".into(), json!(v.description));
    m.insert("env_variable".into(), json!(v.env_variable));
    m.insert("default_value".into(), json!(v.default_value));
    m.insert("server_value".into(), json!(server_value));
    m.insert("is_editable".into(), json!(v.user_editable.0));
    m.insert("rules".into(), json!(rules));
    item("egg_variable", Value::Object(m), vec![])
}

/// Attributes shared by `UserData` and `SubuserData`.
async fn user_attributes<C: ConnectionTrait>(db: &C, u: &users::Model) -> ApiResult<Map<String, Value>> {
    let root = pelican_core::auth::is_root_admin(db, u.id.0).await?;
    let mut m = Map::new();
    m.insert("uuid".into(), json!(u.uuid));
    m.insert("username".into(), json!(u.username));
    m.insert("email".into(), json!(u.email));
    m.insert("language".into(), json!(u.language));
    m.insert("image".into(), json!(gravatar(&u.email)));
    m.insert("admin".into(), json!(root));
    m.insert("root_admin".into(), json!(root));
    m.insert("2fa_enabled".into(), json!(u.mfa_app_secret.as_deref().is_some_and(|s| !s.trim().is_empty())));
    m.insert("created_at".into(), json!(format_timestamp(u.created_at.as_ref())));
    m.insert("updated_at".into(), json!(format_timestamp(u.updated_at.as_ref())));
    Ok(m)
}

pub async fn user_item<C: ConnectionTrait>(db: &C, u: &users::Model) -> ApiResult<Value> {
    Ok(item("user", Value::Object(user_attributes(db, u).await?), vec![]))
}

/// `SubuserData` (resource `server_subuser`).
pub async fn subuser_item<C: ConnectionTrait>(db: &C, sub: &subusers::Model, u: &users::Model) -> ApiResult<Value> {
    let mut m = user_attributes(db, u).await?;
    let perms = sub.permissions.as_ref().map(|j| php_assoc(j.0.clone())).unwrap_or(Value::Array(vec![]));
    m.insert("permissions".into(), perms);
    Ok(item("server_subuser", Value::Object(m), vec![]))
}

/// `server.subusers` in natural order (HasMany, no ORDER BY).
pub async fn server_subusers<C: ConnectionTrait>(db: &C, server_id: i64) -> ApiResult<Vec<subusers::Model>> {
    let b = db.get_database_backend();
    Ok(subusers::Model::find_by_statement(raw::stmt(
        b,
        "select * from {subusers} where {subusers.server_id} = ? and {subusers.server_id} is not null",
        vec![server_id.into()],
    ))
    .all(db)
    .await?)
}

/// `StartupCommandService::handle($server, hideAllValues)`.
pub fn startup_command(b: &ServerBundle, hide_all: bool) -> String {
    let mut find: Vec<String> = vec!["{{SERVER_MEMORY}}".into(), "{{SERVER_IP}}".into(), "{{SERVER_PORT}}".into()];
    let mut replace: Vec<String> = vec![
        b.server.memory.0.to_string(),
        b.allocation.as_ref().map(|a| a.ip.clone()).unwrap_or_else(|| "127.0.0.1".into()),
        b.allocation.as_ref().map(|a| a.port.0.to_string()).unwrap_or_else(|| "0".into()),
    ];
    for (v, sv) in &b.variables {
        find.push(format!("{{{{{}}}}}", v.env_variable));
        replace.push(if v.user_viewable.0 && !hide_all {
            sv.clone().unwrap_or_else(|| v.default_value.clone())
        } else {
            "[hidden]".into()
        });
    }
    // str_replace with arrays applies each pair in order to the running result.
    let mut out = b.server.startup.clone();
    for (f, r) in find.iter().zip(replace.iter()) {
        out = out.replace(f.as_str(), r);
    }
    out
}

/// `Egg::inherit_features`: own features or the config parent's when own is NULL.
fn inherit_features(egg: &eggs::Model, parent: Option<&eggs::Model>) -> Value {
    match (&egg.features, egg.config_from, parent) {
        (Some(f), _, _) => php_assoc(f.0.clone()),
        (None, Some(_), Some(p)) => p.features.as_ref().map(|f| php_assoc(f.0.clone())).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

#[derive(Serialize)]
struct Limits {
    memory: i64,
    swap: i64,
    disk: i64,
    io: i64,
    cpu: i64,
    threads: Option<String>,
    oom_disabled: bool,
    oom_killer: bool,
}

/// `ServerData` item with includes (default `allocations`, `variables`).
fn config_err(e: pelican_core::server_config::ConfigError) -> ApiError {
    match e {
        pelican_core::server_config::ConfigError::Db(d) => d.into(),
        pelican_core::server_config::ConfigError::Php(c) => ApiError::internal(c),
    }
}

pub async fn server_item(st: &AppState, ctx: &Ctx, server: &servers::Model, includes: &Includes) -> ApiResult<Value> {
    let db = &st.db;
    let b = ServerBundle::load(db, server.clone()).await.map_err(config_err)?;
    let node =
        nodes::Entity::find_by_id(server.node_id).one(db).await?.ok_or_else(|| ApiError::internal("ErrorException"))?;
    let transferring = pelican_core::server_state::active_transfer(db, server.id.0).await?.is_some();
    render_server(st, ctx, &b, &node, transferring, includes).await
}

/// [`server_item`] for a page of servers: bundles, nodes and transfer states loaded per
/// page instead of per server (the output is the same, server by server).
pub async fn server_items(
    st: &AppState,
    ctx: &Ctx,
    servers: &[servers::Model],
    includes: &Includes,
) -> ApiResult<Vec<Value>> {
    use sea_orm::{ColumnTrait, QueryFilter};
    let db = &st.db;
    if servers.is_empty() {
        return Ok(Vec::new());
    }
    let bundles = ServerBundle::load_many(db, servers.to_vec()).await.map_err(config_err)?;
    let node_ids: Vec<pelican_db::Int> = servers.iter().map(|s| s.node_id).collect();
    let nodes: std::collections::HashMap<i64, nodes::Model> = nodes::Entity::find()
        .filter(nodes::Column::Id.is_in(node_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|n| (n.id.0, n))
        .collect();
    let ids: Vec<pelican_db::Int> = servers.iter().map(|s| s.id).collect();
    let transferring: std::collections::HashSet<i64> = pelican_db::entities::server_transfers::Entity::find()
        .filter(pelican_db::entities::server_transfers::Column::ServerId.is_in(ids))
        .filter(pelican_db::entities::server_transfers::Column::Successful.is_null())
        .all(db)
        .await?
        .into_iter()
        .map(|t| t.server_id.0)
        .collect();
    let mut out = Vec::with_capacity(bundles.len());
    for b in &bundles {
        let node = nodes.get(&b.server.node_id.0).ok_or_else(|| ApiError::internal("ErrorException"))?;
        out.push(render_server(st, ctx, b, node, transferring.contains(&b.server.id.0), includes).await?);
    }
    Ok(out)
}

async fn render_server(
    st: &AppState,
    ctx: &Ctx,
    b: &ServerBundle,
    node: &nodes::Model,
    transferring: bool,
    includes: &Includes,
) -> ApiResult<Value> {
    let user = ctx.user()?;
    let db = &st.db;
    let server = &b.server;
    let has = |perm: &'static str| pelican_core::permissions::has_server_permission(db, user, server, perm);
    let gate = |perm: &'static str| pelican_core::permissions::gate_server_permission(db, user, server, perm);
    let mut m = Map::new();
    m.insert("server_owner".into(), json!(user.id() == server.owner_id.0));
    m.insert("identifier".into(), json!(server.uuid_short));
    m.insert("internal_id".into(), json!(server.id.0));
    m.insert("uuid".into(), json!(server.uuid));
    m.insert("name".into(), json!(server.name));
    m.insert("node".into(), json!(node.name));
    m.insert("is_node_under_maintenance".into(), json!(node.maintenance_mode.0));
    m.insert(
        "sftp_details".into(),
        json!({"ip": node.fqdn, "alias": node.daemon_sftp_alias, "port": node.daemon_sftp.0}),
    );
    if st.config.env.bool_or("PANEL_EDITABLE_SERVER_DESCRIPTIONS", true) {
        m.insert("description".into(), json!(server.description));
    }
    m.insert(
        "limits".into(),
        serde_json::to_value(Limits {
            memory: server.memory.0,
            swap: server.swap.0,
            disk: server.disk.0,
            io: server.io.0,
            cpu: server.cpu.0,
            threads: server.threads.clone(),
            oom_disabled: !server.oom_killer.0,
            oom_killer: server.oom_killer.0,
        })
        .unwrap_or(Value::Null),
    );
    m.insert("invocation".into(), json!(startup_command(b, !has("startup.read").await?)));
    m.insert("docker_image".into(), json!(server.image));
    m.insert("egg_features".into(), inherit_features(&b.egg, b.config_from.as_ref()));
    m.insert(
        "feature_limits".into(),
        json!({"databases": server.database_limit.map(|v| v.0), "allocations": server.allocation_limit.map(|v| v.0), "backups": server.backup_limit.0}),
    );
    m.insert("status".into(), json!(server.status));
    m.insert("is_suspended".into(), json!(server.status.as_deref() == Some("suspended")));
    m.insert("is_installing".into(), json!(!pelican_core::server_state::is_installed(server)));
    m.insert("is_transferring".into(), json!(transferring));

    let mut rels: Vec<(&str, Value)> = Vec::new();
    for name in includes.names(&["allocations", "variables"], &["egg", "subusers"]) {
        let v = match name {
            "allocations" => {
                if gate("allocation.read").await? {
                    list(
                        b.allocations
                            .iter()
                            .map(|a| allocation_data(a, server.allocation_id.map(|x| x.0), false))
                            .collect(),
                    )
                } else {
                    // `clone $server->allocation` with no primary allocation → Error.
                    let primary = b.allocation.as_ref().ok_or_else(|| ApiError::internal("Error"))?;
                    list(vec![allocation_data(primary, server.allocation_id.map(|x| x.0), true)])
                }
            }
            "variables" => {
                if gate("startup.read").await? {
                    list(
                        b.variables
                            .iter()
                            .filter(|(v, _)| v.user_viewable.0)
                            .map(|(v, sv)| egg_variable_data(v, sv.as_deref()))
                            .collect(),
                    )
                } else {
                    null_resource()
                }
            }
            "egg" => item("egg", json!({"uuid": b.egg.uuid, "name": b.egg.name}), vec![]),
            "subusers" => {
                if gate("user.read").await? {
                    let mut out = Vec::new();
                    for s in server_subusers(db, server.id.0).await? {
                        if let Some(u) = users::Entity::find_by_id(s.user_id).one(db).await? {
                            out.push(subuser_item(db, &s, &u).await?);
                        }
                    }
                    list(out)
                } else {
                    null_resource()
                }
            }
            _ => continue,
        };
        rels.push((name, v));
    }
    Ok(item("server", Value::Object(m), rels))
}

static ACTIVITY_LANG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../data/activity.en.json")).unwrap_or(Value::Null));

/// PHP `strip_tags` (simplified: drops `<...>` sequences).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
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

/// `ActivityLog::wrapProperties()`.
pub fn wrap_properties(props: &Value) -> Value {
    let pairs: Vec<(String, Value)> = match php_assoc(props.clone()) {
        Value::Object(m) => m.into_iter().collect(),
        Value::Array(a) => a.into_iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => Vec::new(),
    };
    if pairs.is_empty() {
        return json!([]);
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    for (k, v) in pairs {
        match v {
            Value::Array(_) | Value::Object(_) => {
                let items: Vec<Value> = match v {
                    Value::Array(a) => a,
                    Value::Object(m) => m.into_iter().map(|(_, v)| v).collect(),
                    _ => Vec::new(),
                };
                let first = items.first().cloned().unwrap_or(Value::Null);
                let count_key = format!("{k}_count");
                if first.is_array() || first.is_object() {
                    out.push((count_key, json!(items.len())));
                } else {
                    out.push((k, json!(strip_tags(&pelican_core::php::string(&first)))));
                    out.push((count_key, json!(items.len())));
                }
            }
            other => {
                let mut s = pelican_core::php::string(&other);
                if k == "directory" {
                    s = format!("/{}/", s.trim_matches('/')).replace("//", "/");
                }
                out.push((k, json!(strip_tags(&s))));
            }
        }
    }
    let counts: Vec<String> = out.iter().filter(|(k, _)| k.ends_with("_count")).map(|(k, _)| k.clone()).collect();
    if counts.len() == 1 {
        let key = &counts[0];
        let val = out.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or(Value::Null);
        // merge(['count' => ..]) replaces an existing key in place, else appends; then except().
        match out.iter_mut().find(|(k, _)| k == "count") {
            Some(slot) => slot.1 = val,
            None => out.push(("count".into(), val)),
        }
        out.retain(|(k, _)| k != key);
    }
    // PHP arrays with sequential integer keys encode as lists.
    php_assoc(Value::Object(out.into_iter().collect()))
}

/// Laravel `trans_choice` for the English activity strings.
fn activity_line(event: &str, count: i64) -> String {
    let key = event.replace(':', ".");
    let mut cur = &*ACTIVITY_LANG;
    for seg in key.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return format!("activity.{key}"),
        }
    }
    let Some(line) = cur.as_str() else { return format!("activity.{key}") };
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() == 1 {
        return line.to_string();
    }
    for p in &parts {
        // Explicit ranges like `{0}` or `[2,*]`.
        if let Some(rest) = p.strip_prefix('{') {
            if let Some((n, text)) = rest.split_once('}') {
                if n.trim().parse::<i64>().ok() == Some(count) {
                    return text.trim().to_string();
                }
            }
        }
    }
    let idx = if count == 1 { 0 } else { 1 };
    parts.get(idx).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// `ActivityLog::hasAdditionalMetadata()`.
pub fn has_additional_metadata(event: &str, props: &Value) -> bool {
    let keys: Vec<String> = match php_assoc(props.clone()) {
        Value::Object(m) => m.keys().cloned().collect(),
        Value::Array(a) if !a.is_empty() => (0..a.len()).map(|i| i.to_string()).collect(),
        _ => return false,
    };
    let wrapped = wrap_properties(props);
    let count = wrapped.get("count").and_then(|c| c.as_i64()).unwrap_or(1);
    let line = activity_line(event, count);
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r":(?P<key>[\w.-]+\w)(?:[^\w:]?|$)").expect("valid regex"));
    let mut exclude: Vec<String> = RE.captures_iter(&line).map(|c| c["key"].to_string()).collect();
    exclude.extend(["ip".into(), "useragent".into(), "using_sftp".into()]);
    keys.iter().any(|k| !exclude.contains(k))
}

/// `ActivityLogData` with optional `actor` include.
pub async fn activity_item(
    st: &AppState,
    ctx: &Ctx,
    log: &activity_logs::Model,
    includes: &Includes,
) -> ApiResult<Value> {
    let user = ctx.user()?;
    let actor = match (log.actor_type.as_deref(), log.actor_id) {
        (Some("user"), Some(id)) => users::Entity::find_by_id(id).one(&st.db).await?,
        _ => None,
    };
    let can_view_ip = actor.as_ref().is_some_and(|a| a.id.0 == user.id()) || user.gate_before("seeIps activityLog");
    use sha1::Digest;
    let mut m = Map::new();
    m.insert("id".into(), json!(hex::encode(sha1::Sha1::digest(log.id.0.to_string().as_bytes()))));
    m.insert("event".into(), json!(log.event));
    m.insert("is_api".into(), json!(log.api_key_id.is_some()));
    m.insert("ip".into(), if can_view_ip { json!(log.ip) } else { Value::Null });
    m.insert("description".into(), json!(log.description));
    m.insert("properties".into(), wrap_properties(&log.properties.0));
    m.insert("has_additional_metadata".into(), json!(has_additional_metadata(&log.event, &log.properties.0)));
    m.insert("timestamp".into(), json!(atom(Some(&log.timestamp))));
    let mut rels = Vec::new();
    if includes.is_requested("actor") {
        let v = match &actor {
            Some(a) => user_item(&st.db, a).await?,
            None => null_resource(),
        };
        rels.push(("actor", v));
    }
    Ok(item("activity_log", Value::Object(m), rels))
}
