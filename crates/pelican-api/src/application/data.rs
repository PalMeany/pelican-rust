//! Application API resources (`app/Data/Api/Application/*`, spec §2) with
//! their includes. Include resolution follows `$availableIncludes` order; each
//! permission-checked include yields `null_resource` when the key may not read
//! the resource (`IncludeContext::allowsAdmin`).

use std::future::Future;
use std::pin::Pin;

use pelican_core::state::AppState;
use pelican_db::entities::{
    allocations, database_hosts, databases, egg_variables, eggs, mounts, nodes, permissions, roles, servers, subusers,
    users,
};
use pelican_db::{Timestamp, raw};
use sea_orm::{ConnectionTrait, EntityTrait, FromQueryResult};
use serde_json::{Map, Value, json};

use crate::envelope::{Includes, item, list, null_resource};
use crate::error::{ApiError, ApiResult};
use crate::pipeline::Ctx;

pub type Fut<'a> = Pin<Box<dyn Future<Output = ApiResult<Value>> + Send + 'a>>;

/// `ApiResource::formatTimestamp(string)`: a null date is a TypeError.
pub fn ts(t: Option<&Timestamp>) -> ApiResult<Value> {
    t.map(|t| json!(pelican_core::time::atom(&t.0))).ok_or_else(|| ApiError::internal("TypeError"))
}

/// `$carbon->toAtomString()` on a non-null date (`->` on null is an Error).
fn atom_strict(t: Option<&Timestamp>) -> ApiResult<Value> {
    t.map(|t| json!(pelican_core::time::atom(&t.0))).ok_or_else(|| ApiError::internal("Error"))
}

/// Eloquent `serializeDate()` (`Y-m-d\TH:i:s.u\Z`).
fn micro(t: Option<&Timestamp>) -> Value {
    t.map(|t| json!(pelican_core::time::iso_micro_z(&t.0))).unwrap_or(Value::Null)
}

fn rows<M: FromQueryResult>(rs: Vec<sea_orm::QueryResult>) -> ApiResult<Vec<M>> {
    rs.iter().map(|r| M::from_query_result(r, "").map_err(Into::into)).collect()
}

async fn select<M: FromQueryResult, C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> ApiResult<Vec<M>> {
    rows(db.query_all_raw(raw::stmt(db.get_database_backend(), sql, values)).await?)
}

/// An include slot: the resolver only runs when the include is requested.
async fn resolve(
    out: &mut Vec<(&'static str, Value)>,
    includes: &Includes,
    name: &'static str,
    allowed: bool,
    f: impl Future<Output = ApiResult<Value>>,
) -> ApiResult<()> {
    if includes.is_requested(name) {
        out.push((name, if allowed { f.await? } else { null_resource() }));
    }
    Ok(())
}

fn collect(object: &str, attrs: Value, rel: Vec<(&'static str, Value)>) -> Value {
    item(object, attrs, rel.into_iter().map(|(k, v)| (k, v)).collect())
}

// --- users ---------------------------------------------------------------

/// `UserData` (`user`). Includes: `servers` (server), `roles` (role).
pub fn user_item<'a>(st: &'a AppState, ctx: &'a Ctx, u: &'a users::Model, inc: &'a Includes) -> Fut<'a> {
    Box::pin(async move {
        let root = pelican_core::auth::is_root_admin(&st.db, u.id.0).await?;
        let mfa = u.mfa_app_secret.as_deref().is_some_and(|s| !s.trim().is_empty());
        let mut a = Map::new();
        a.insert("id".into(), json!(u.id.0));
        a.insert("external_id".into(), json!(u.external_id));
        a.insert("is_managed_externally".into(), json!(u.is_managed_externally.0));
        a.insert("uuid".into(), json!(u.uuid));
        a.insert("username".into(), json!(u.username));
        a.insert("email".into(), json!(u.email));
        a.insert("language".into(), json!(u.language));
        a.insert("root_admin".into(), json!(root));
        a.insert("2fa_enabled".into(), json!(mfa));
        a.insert("2fa".into(), json!(mfa));
        a.insert("created_at".into(), ts(u.created_at.as_ref())?);
        a.insert("updated_at".into(), ts(u.updated_at.as_ref())?);
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "servers", ctx.allows_admin("server"), async {
            let list_ = select::<servers::Model, _>(
                &st.db,
                "select * from {servers} where {servers.owner_id} = ? and {servers.owner_id} is not null",
                vec![u.id.into()],
            )
            .await?;
            server_list(st, ctx, &list_, &inc.descend("servers"), "server").await
        })
        .await?;
        resolve(&mut rel, inc, "roles", ctx.allows_admin("role"), async {
            let rs = user_roles(&st.db, u.id.0).await?;
            let sub = inc.descend("roles");
            let mut data = Vec::new();
            for r in &rs {
                data.push(role_item(st, r, &sub).await?);
            }
            Ok(list(data))
        })
        .await?;
        Ok(collect("user", Value::Object(a), rel))
    })
}

/// spatie `$user->roles` (morph type `user`).
pub async fn user_roles<C: ConnectionTrait>(db: &C, user_id: i64) -> ApiResult<Vec<roles::Model>> {
    select(
        db,
        "select {roles}.* from {roles} inner join {model_has_roles} on {roles.id} = {model_has_roles.role_id} \
         where {model_has_roles.model_id} = ? and {model_has_roles.model_type} = ?",
        vec![user_id.into(), "user".into()],
    )
    .await
}

/// `RoleData` (`role`). Include `permissions` (no permission check).
pub async fn role_item(st: &AppState, r: &roles::Model, inc: &Includes) -> ApiResult<Value> {
    let mut a = Map::new();
    a.insert("id".into(), json!(r.id.0));
    a.insert("name".into(), json!(r.name));
    a.insert("created_at".into(), atom_strict(r.created_at.as_ref())?);
    a.insert("updated_at".into(), atom_strict(r.updated_at.as_ref())?);
    let mut rel = Vec::new();
    resolve(&mut rel, inc, "permissions", true, async {
        let ps = select::<permissions::Model, _>(
            &st.db,
            "select {permissions}.* from {permissions} inner join {role_has_permissions} on {permissions.id} = {role_has_permissions.permission_id} \
             where {role_has_permissions.role_id} = ?",
            vec![r.id.into()],
        )
        .await?;
        let mut data = Vec::new();
        for p in ps {
            let mut pa = Map::new();
            pa.insert("name".into(), json!(p.name));
            pa.insert("created_at".into(), atom_strict(p.created_at.as_ref())?);
            pa.insert("updated_at".into(), atom_strict(p.updated_at.as_ref())?);
            data.push(item("permissions", Value::Object(pa), vec![]));
        }
        Ok(list(data))
    })
    .await?;
    Ok(collect("role", Value::Object(a), rel))
}

// --- servers ---------------------------------------------------------------

pub async fn server_list(
    st: &AppState,
    ctx: &Ctx,
    servers_: &[servers::Model],
    inc: &Includes,
    object: &str,
) -> ApiResult<Value> {
    Ok(list(server_items_as(st, ctx, servers_, inc, object).await?))
}

pub fn server_item<'a>(st: &'a AppState, ctx: &'a Ctx, s: &'a servers::Model, inc: &'a Includes) -> Fut<'a> {
    server_item_as(st, ctx, s, inc, "server", None)
}

/// [`server_item`] for a page of servers, their configuration bundles loaded together.
pub async fn server_items(
    st: &AppState,
    ctx: &Ctx,
    servers_: &[servers::Model],
    inc: &Includes,
) -> ApiResult<Vec<Value>> {
    server_items_as(st, ctx, servers_, inc, "server").await
}

async fn server_items_as(
    st: &AppState,
    ctx: &Ctx,
    servers_: &[servers::Model],
    inc: &Includes,
    object: &str,
) -> ApiResult<Vec<Value>> {
    let bundles =
        pelican_core::server_config::ServerBundle::load_many(&st.db, servers_.to_vec()).await.map_err(config_err)?;
    let mut data = Vec::with_capacity(bundles.len());
    for (s, b) in servers_.iter().zip(bundles) {
        data.push(server_item_as(st, ctx, s, inc, object, Some(b)).await?);
    }
    Ok(data)
}

fn config_err(e: pelican_core::server_config::ConfigError) -> ApiError {
    match e {
        pelican_core::server_config::ConfigError::Db(d) => d.into(),
        pelican_core::server_config::ConfigError::Php(c) => ApiError::internal(c),
    }
}

/// `ServerData` (§2.2). Includes: allocations, user, subusers, egg, variables, node, databases.
fn server_item_as<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    s: &'a servers::Model,
    inc: &'a Includes,
    object: &'a str,
    preloaded: Option<pelican_core::server_config::ServerBundle>,
) -> Fut<'a> {
    Box::pin(async move {
        let bundle = match preloaded {
            Some(b) => b,
            None => pelican_core::server_config::ServerBundle::load(&st.db, s.clone()).await.map_err(config_err)?,
        };
        let env = pelican_core::server_config::environment(&bundle);
        let installed = !matches!(s.status.as_deref(), Some("installing" | "install_failed" | "reinstall_failed"));
        let mut a = Map::new();
        a.insert("id".into(), json!(s.id.0));
        a.insert("external_id".into(), json!(s.external_id));
        a.insert("uuid".into(), json!(s.uuid));
        a.insert("identifier".into(), json!(s.uuid_short));
        a.insert("name".into(), json!(s.name));
        a.insert("description".into(), json!(s.description));
        a.insert("status".into(), json!(s.status));
        a.insert("suspended".into(), json!(s.status.as_deref() == Some("suspended")));
        a.insert(
            "limits".into(),
            json!({
                "memory": s.memory.0, "swap": s.swap.0, "disk": s.disk.0, "io": s.io.0, "cpu": s.cpu.0,
                "threads": s.threads, "oom_disabled": !s.oom_killer.0, "oom_killer": s.oom_killer.0,
            }),
        );
        a.insert(
            "feature_limits".into(),
            json!({
                "databases": s.database_limit.map(|v| v.0),
                "allocations": s.allocation_limit.map(|v| v.0),
                "backups": s.backup_limit.0,
            }),
        );
        a.insert("user".into(), json!(s.owner_id.0));
        a.insert("node".into(), json!(s.node_id.0));
        a.insert("allocation".into(), json!(s.allocation_id.map(|v| v.0)));
        a.insert("egg".into(), json!(s.egg_id.0));
        a.insert(
            "container".into(),
            json!({
                "startup_command": s.startup, "image": s.image,
                "installed": if installed { 1 } else { 0 },
                "environment": Value::Object(env),
            }),
        );
        a.insert("updated_at".into(), ts(s.updated_at.as_ref())?);
        a.insert("created_at".into(), ts(s.created_at.as_ref())?);

        let mut rel = Vec::new();
        resolve(&mut rel, inc, "allocations", ctx.allows_admin("allocation"), async {
            let sub = inc.descend("allocations");
            let mut data = Vec::new();
            for al in &bundle.allocations {
                data.push(allocation_item(st, ctx, al, &sub).await?);
            }
            Ok(list(data))
        })
        .await?;
        resolve(&mut rel, inc, "user", ctx.allows_admin("user"), async {
            match users::Entity::find_by_id(s.owner_id).one(&st.db).await? {
                Some(u) => user_item(st, ctx, &u, &inc.descend("user")).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        resolve(&mut rel, inc, "subusers", ctx.allows_admin("user"), async {
            let subs = select::<subusers::Model, _>(
                &st.db,
                "select * from {subusers} where {subusers.server_id} = ? and {subusers.server_id} is not null",
                vec![s.id.into()],
            )
            .await?;
            let sub = inc.descend("subusers");
            let mut data = Vec::new();
            for su in &subs {
                data.push(subuser_item(st, ctx, su, &sub, "subuser").await?);
            }
            Ok(list(data))
        })
        .await?;
        resolve(&mut rel, inc, "egg", ctx.allows_admin("egg"), async {
            egg_item(st, ctx, &bundle.egg, &inc.descend("egg")).await
        })
        .await?;
        resolve(&mut rel, inc, "variables", ctx.allows_admin("server"), async {
            let sub = inc.descend("variables");
            let mut data = Vec::new();
            for (v, sv) in &bundle.variables {
                data.push(server_variable_item(st, ctx, v, sv.as_deref(), &sub)?);
            }
            Ok(list(data))
        })
        .await?;
        resolve(&mut rel, inc, "node", ctx.allows_admin("node"), async {
            match nodes::Entity::find_by_id(s.node_id).one(&st.db).await? {
                Some(n) => node_item(st, ctx, &n, &inc.descend("node"), NodeExtra::default()).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        resolve(&mut rel, inc, "databases", ctx.allows_admin("server_database"), async {
            let dbs = select::<databases::Model, _>(
                &st.db,
                "select * from {databases} where {databases.server_id} = ? and {databases.server_id} is not null",
                vec![s.id.into()],
            )
            .await?;
            let sub = inc.descend("databases");
            let mut data = Vec::new();
            for d in &dbs {
                data.push(database_item(st, ctx, d, &sub, "databases").await?);
            }
            Ok(list(data))
        })
        .await?;
        Ok(collect(object, Value::Object(a), rel))
    })
}

/// `SubuserData` (`server_subuser`, rendered as `subuser` from servers).
pub fn subuser_item<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    su: &'a subusers::Model,
    inc: &'a Includes,
    object: &'a str,
) -> Fut<'a> {
    Box::pin(async move {
        let mut a = Map::new();
        a.insert("id".into(), json!(su.id.0));
        a.insert("user_id".into(), json!(su.user_id.0));
        a.insert("server_id".into(), json!(su.server_id.0));
        a.insert(
            "permissions".into(),
            pelican_core::server_config::php_assoc(su.permissions.as_ref().map(|p| p.0.clone()).unwrap_or(json!([]))),
        );
        a.insert("created_at".into(), ts(su.created_at.as_ref())?);
        a.insert("updated_at".into(), ts(su.updated_at.as_ref())?);
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "user", ctx.allows_admin("user"), async {
            match users::Entity::find_by_id(su.user_id).one(&st.db).await? {
                Some(u) => user_item(st, ctx, &u, &inc.descend("user")).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        resolve(&mut rel, inc, "server", ctx.allows_admin("server"), async {
            match servers::Entity::find_by_id(su.server_id).one(&st.db).await? {
                Some(s) => server_item(st, ctx, &s, &inc.descend("server")).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        Ok(collect(object, Value::Object(a), rel))
    })
}

// --- allocations / nodes -----------------------------------------------------

/// `AllocationData` (`allocation`). Includes: node, server.
pub fn allocation_item<'a>(st: &'a AppState, ctx: &'a Ctx, al: &'a allocations::Model, inc: &'a Includes) -> Fut<'a> {
    Box::pin(async move {
        let mut a = Map::new();
        a.insert("id".into(), json!(al.id.0));
        a.insert("ip".into(), json!(al.ip));
        a.insert("alias".into(), json!(al.ip_alias));
        a.insert("port".into(), json!(al.port.0));
        a.insert("notes".into(), json!(al.notes));
        a.insert("assigned".into(), json!(al.server_id.is_some()));
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "node", ctx.allows_admin("node"), async {
            match nodes::Entity::find_by_id(al.node_id).one(&st.db).await? {
                Some(n) => node_item(st, ctx, &n, &inc.descend("node"), NodeExtra::default()).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        resolve(&mut rel, inc, "server", ctx.allows_admin("server"), async {
            match al.server_id {
                Some(sid) => match servers::Entity::find_by_id(sid).one(&st.db).await? {
                    Some(s) => server_item(st, ctx, &s, &inc.descend("server")).await,
                    None => Ok(null_resource()),
                },
                None => Ok(null_resource()),
            }
        })
        .await?;
        Ok(collect("allocation", Value::Object(a), rel))
    })
}

/// Extra attributes of a node row (selected aggregates, pivots).
#[derive(Default, Clone)]
pub struct NodeExtra {
    pub after_columns: Vec<(String, Value)>,
}

/// Node table columns as Eloquent `toArray()` emits them (hidden token columns
/// dropped, casts applied, physical order of the backend).
pub fn node_attributes(driver: pelican_db::Driver, n: &nodes::Model) -> ApiResult<Map<String, Value>> {
    let cols = pelican_db::column_order::columns(driver, "nodes").unwrap_or(&[]);
    let mut a = Map::new();
    for c in cols {
        let v = match *c {
            "id" => json!(n.id.0),
            "uuid" => json!(n.uuid),
            "public" => json!(n.public.0),
            "name" => json!(n.name),
            "description" => json!(n.description),
            "fqdn" => json!(n.fqdn),
            "scheme" => json!(n.scheme),
            "behind_proxy" => json!(n.behind_proxy.0),
            "maintenance_mode" => json!(n.maintenance_mode.0),
            "memory" => json!(n.memory.0),
            "memory_overallocate" => json!(n.memory_overallocate.0),
            "disk" => json!(n.disk.0),
            "disk_overallocate" => json!(n.disk_overallocate.0),
            "cpu" => json!(n.cpu.0),
            "cpu_overallocate" => json!(n.cpu_overallocate.0),
            "upload_size" => json!(n.upload_size.0),
            "daemon_listen" => json!(n.daemon_listen.0),
            "daemon_sftp" => json!(n.daemon_sftp.0),
            "daemon_sftp_alias" => json!(n.daemon_sftp_alias),
            "daemon_base" => json!(n.daemon_base),
            "daemon_connect" => json!(n.daemon_connect.0),
            "tags" => {
                pelican_core::server_config::php_assoc(n.tags.as_ref().map(|t| t.0.clone()).unwrap_or(Value::Null))
            }
            "created_at" => ts(n.created_at.as_ref())?,
            "updated_at" => ts(n.updated_at.as_ref())?,
            "daemon_token" | "daemon_token_id" => continue,
            _ => continue,
        };
        a.insert((*c).to_string(), v);
    }
    Ok(a)
}

/// `allocated_resources`: sums over the node's servers.
pub async fn allocated_resources<C: ConnectionTrait>(db: &C, node_id: i64) -> ApiResult<Value> {
    let ss = select::<servers::Model, _>(
        db,
        "select * from {servers} where {servers.node_id} = ? and {servers.node_id} is not null",
        vec![node_id.into()],
    )
    .await?;
    let sum = |f: fn(&servers::Model) -> i64| ss.iter().map(f).sum::<i64>();
    Ok(json!({"memory": sum(|s| s.memory.0), "disk": sum(|s| s.disk.0), "cpu": sum(|s| s.cpu.0)}))
}

/// `NodeData` (`node`, §2.3). Includes: allocations, servers.
pub fn node_item<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    n: &'a nodes::Model,
    inc: &'a Includes,
    extra: NodeExtra,
) -> Fut<'a> {
    Box::pin(async move {
        let mut a = node_attributes(st.driver, n)?;
        for (k, v) in extra.after_columns {
            a.insert(k, v);
        }
        a.insert("allocated_resources".into(), allocated_resources(&st.db, n.id.0).await?);
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "allocations", ctx.allows_admin("allocation"), async {
            let als = select::<allocations::Model, _>(
                &st.db,
                "select * from {allocations} where {allocations.node_id} = ? and {allocations.node_id} is not null",
                vec![n.id.into()],
            )
            .await?;
            let sub = inc.descend("allocations");
            let mut data = Vec::new();
            for al in &als {
                data.push(allocation_item(st, ctx, al, &sub).await?);
            }
            Ok(list(data))
        })
        .await?;
        resolve(&mut rel, inc, "servers", ctx.allows_admin("server"), async {
            let ss = select::<servers::Model, _>(
                &st.db,
                "select * from {servers} where {servers.node_id} = ? and {servers.node_id} is not null",
                vec![n.id.into()],
            )
            .await?;
            server_list(st, ctx, &ss, &inc.descend("servers"), "server").await
        })
        .await?;
        Ok(collect("node", Value::Object(a), rel))
    })
}

/// `Mount::toArray()` (no timestamps; casts id int, read_only/user_mountable bool).
pub fn mount_attributes(driver: pelican_db::Driver, m: &mounts::Model) -> Map<String, Value> {
    let cols = pelican_db::column_order::columns(driver, "mounts").unwrap_or(&[]);
    let mut a = Map::new();
    for c in cols {
        let v = match *c {
            "id" => json!(m.id.0),
            "uuid" => json!(m.uuid),
            "name" => json!(m.name),
            "description" => json!(m.description),
            "source" => json!(m.source),
            "target" => json!(m.target),
            "read_only" => json!(m.read_only.0),
            "user_mountable" => json!(m.user_mountable.0),
            _ => continue,
        };
        a.insert((*c).to_string(), v);
    }
    a
}

// --- eggs -----------------------------------------------------------------

/// `json_decode($v ?: '{}', true)` (invalid → null) re-encoded like PHP arrays.
fn decode_assoc(v: Option<&str>) -> Value {
    let s = v.filter(|s| !s.is_empty() && *s != "0").unwrap_or("{}");
    serde_json::from_str::<Value>(s).map(pelican_core::server_config::php_assoc).unwrap_or(Value::Null)
}

fn first_value(v: Option<&Value>) -> Value {
    match v {
        Some(Value::Object(m)) => m.values().next().cloned().unwrap_or(json!("")),
        Some(Value::Array(a)) => a.first().cloned().unwrap_or(json!("")),
        _ => json!(""),
    }
}

/// `EggData` (`egg`, §2.5). Includes: servers, variables.
pub fn egg_item<'a>(st: &'a AppState, ctx: &'a Ctx, e: &'a eggs::Model, inc: &'a Includes) -> Fut<'a> {
    Box::pin(async move {
        let config_from = match e.config_from {
            Some(id) => eggs::Entity::find_by_id(id).one(&st.db).await?,
            None => None,
        };
        let script_from = match e.copy_script_from {
            Some(id) => eggs::Entity::find_by_id(id).one(&st.db).await?,
            None => None,
        };
        // getInheritConfig*Attribute: own value unless null and config_from is set.
        let inherit = |own: &Option<String>, f: fn(&eggs::Model) -> Option<String>| -> ApiResult<Option<String>> {
            if own.is_some() || e.config_from.is_none() {
                return Ok(own.clone());
            }
            config_from.as_ref().map(f).ok_or_else(|| ApiError::internal("ErrorException"))
        };
        let files_raw = inherit(&e.config_files, |c| c.config_files.clone())?;
        let files_src = files_raw.as_deref().filter(|s| !s.is_empty() && *s != "0").unwrap_or("{}");
        let files = serde_json::from_str::<Value>(files_src)
            .map(pelican_core::server_config::php_assoc)
            .map_err(|_| ApiError::internal("JsonException"))?;
        let startup = decode_assoc(inherit(&e.config_startup, |c| c.config_startup.clone())?.as_deref());
        let logs = decode_assoc(inherit(&e.config_logs, |c| c.config_logs.clone())?.as_deref());
        let stop = inherit(&e.config_stop, |c| c.config_stop.clone())?;
        // getInheritFileDenylistAttribute: always the parent's list when config_from is set.
        let denylist = if e.config_from.is_none() {
            e.file_denylist.as_ref().map(|j| j.0.clone())
        } else {
            config_from.as_ref().and_then(|c| c.file_denylist.as_ref().map(|j| j.0.clone()))
        };
        // getCopyScript*Attribute: own value unless empty and copy_script_from is set.
        let copy = |own: Option<String>, f: fn(&eggs::Model) -> Option<String>| -> ApiResult<Option<String>> {
            if own.as_deref().is_some_and(|s| !s.is_empty() && s != "0") || e.copy_script_from.is_none() {
                return Ok(own);
            }
            script_from.as_ref().map(f).ok_or_else(|| ApiError::internal("ErrorException"))
        };
        let install = copy(e.script_install.clone(), |c| c.script_install.clone())?;
        let entry = copy(Some(e.script_entry.clone()), |c| Some(c.script_entry.clone()))?;
        let container = copy(Some(e.script_container.clone()), |c| Some(c.script_container.clone()))?;
        let images = e.docker_images.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone()));
        let startups = e.startup_commands.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone()));

        let mut a = Map::new();
        a.insert("id".into(), json!(e.id.0));
        a.insert("uuid".into(), json!(e.uuid));
        a.insert("name".into(), json!(e.name));
        a.insert("author".into(), json!(e.author));
        a.insert("description".into(), json!(e.description));
        a.insert("icon".into(), egg_icon(st, e));
        a.insert(
            "features".into(),
            e.features.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(Value::Null),
        );
        a.insert(
            "tags".into(),
            e.tags.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(Value::Null),
        );
        a.insert("docker_image".into(), first_value(images.as_ref()));
        a.insert("docker_images".into(), images.clone().unwrap_or(Value::Null));
        a.insert(
            "config".into(),
            json!({
                "files": files, "startup": startup, "stop": stop, "logs": logs,
                "file_denylist": denylist.map(pelican_core::server_config::php_assoc),
                "extends": e.config_from.map(|v| v.0),
            }),
        );
        a.insert("startup".into(), first_value(startups.as_ref()));
        a.insert("startup_commands".into(), startups.unwrap_or(Value::Null));
        a.insert(
            "script".into(),
            json!({
                "privileged": e.script_is_privileged.0, "install": install, "entry": entry,
                "container": container, "extends": e.copy_script_from.map(|v| v.0),
            }),
        );
        a.insert("created_at".into(), ts(e.created_at.as_ref())?);
        a.insert("updated_at".into(), ts(e.updated_at.as_ref())?);

        let mut rel = Vec::new();
        resolve(&mut rel, inc, "servers", ctx.allows_admin("server"), async {
            let ss = select::<servers::Model, _>(
                &st.db,
                "select * from {servers} where {servers.egg_id} = ? and {servers.egg_id} is not null",
                vec![e.id.into()],
            )
            .await?;
            server_list(st, ctx, &ss, &inc.descend("servers"), "server").await
        })
        .await?;
        resolve(&mut rel, inc, "variables", ctx.allows_admin("egg"), async {
            let vs = select::<egg_variables::Model, _>(
                &st.db,
                "select * from {egg_variables} where {egg_variables.egg_id} = ? and {egg_variables.egg_id} is not null",
                vec![e.id.into()],
            )
            .await?;
            Ok(list(
                vs.iter()
                    .map(|v| item("egg_variable", Value::Object(egg_variable_attributes(st.driver, v, None)), vec![]))
                    .collect(),
            ))
        })
        .await?;
        Ok(collect("egg", Value::Object(a), rel))
    })
}

/// `HasIcon::getIconAttribute`: public-disk URL of an existing icon file.
fn egg_icon(st: &AppState, e: &eggs::Model) -> Value {
    let Some(uuid) = &e.uuid else { return Value::Null };
    for ext in ["png", "jpg", "webp"] {
        let rel = format!("icons/egg/{uuid}.{ext}");
        if st.config.base_path.join("storage/app/public").join(&rel).is_file() {
            return json!(format!("{}/storage/{rel}", st.config.app_url.trim_end_matches('/')));
        }
    }
    Value::Null
}

/// `EggVariable::toArray()` (+ `server_value` when selected), physical column order.
pub fn egg_variable_attributes(
    driver: pelican_db::Driver,
    v: &egg_variables::Model,
    server_value: Option<Option<&str>>,
) -> Map<String, Value> {
    let cols = pelican_db::column_order::columns(driver, "egg_variables").unwrap_or(&[]);
    let mut a = Map::new();
    for c in cols {
        let val = match *c {
            "id" => json!(v.id.0),
            "egg_id" => json!(v.egg_id.0),
            "sort" => json!(v.sort.map(|s| s.0)),
            "name" => json!(v.name),
            "description" => json!(v.description),
            "env_variable" => json!(v.env_variable),
            "default_value" => json!(v.default_value),
            "user_viewable" => json!(v.user_viewable.0),
            "user_editable" => json!(v.user_editable.0),
            "rules" => pelican_core::server_config::php_assoc(v.rules.0.clone()),
            "created_at" => micro(v.created_at.as_ref()),
            "updated_at" => micro(v.updated_at.as_ref()),
            _ => continue,
        };
        a.insert((*c).to_string(), val);
    }
    if let Some(sv) = server_value {
        a.insert("server_value".into(), json!(sv));
    }
    a
}

/// `ServerVariableData` (`server_variable`). Include `parent` → the same row as `variable`.
fn server_variable_item(
    st: &AppState,
    ctx: &Ctx,
    v: &egg_variables::Model,
    server_value: Option<&str>,
    inc: &Includes,
) -> ApiResult<Value> {
    let attrs = egg_variable_attributes(st.driver, v, Some(server_value));
    let mut rel = Vec::new();
    if inc.is_requested("parent") {
        rel.push((
            "parent",
            if ctx.allows_admin("egg") {
                item("variable", Value::Object(attrs.clone()), vec![])
            } else {
                null_resource()
            },
        ));
    }
    Ok(collect("server_variable", Value::Object(attrs), rel))
}

// --- databases -----------------------------------------------------------

/// `ServerDatabaseData` (`server_database`). Includes: password (unchecked), host.
pub fn database_item<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    d: &'a databases::Model,
    inc: &'a Includes,
    object: &'a str,
) -> Fut<'a> {
    Box::pin(async move {
        let mut a = Map::new();
        a.insert("id".into(), json!(d.id.0));
        a.insert("server".into(), json!(d.server_id.0));
        a.insert("host".into(), json!(d.database_host_id.0));
        a.insert("database".into(), json!(d.database));
        a.insert("username".into(), json!(d.username));
        a.insert("remote".into(), json!(d.remote));
        a.insert("max_connections".into(), json!(d.max_connections.map(|m| m.0)));
        a.insert("created_at".into(), atom_strict(d.created_at.as_ref())?);
        a.insert("updated_at".into(), atom_strict(d.updated_at.as_ref())?);
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "password", true, async {
            let pw = st.enc.decrypt_string_lenient(&d.password).map_err(|_| ApiError::internal("DecryptException"))?;
            Ok(json!({"object": "database_password", "attributes": {"password": pw}}))
        })
        .await?;
        resolve(&mut rel, inc, "host", ctx.allows_admin("database_host"), async {
            match database_hosts::Entity::find_by_id(d.database_host_id).one(&st.db).await? {
                Some(h) => database_host_item(st, ctx, &h, &inc.descend("host")).await,
                None => Ok(null_resource()),
            }
        })
        .await?;
        Ok(collect(object, Value::Object(a), rel))
    })
}

/// `DatabaseHostData` (`database_host`). Includes: databases, nodes (with pivot).
pub fn database_host_item<'a>(
    st: &'a AppState,
    ctx: &'a Ctx,
    h: &'a database_hosts::Model,
    inc: &'a Includes,
) -> Fut<'a> {
    Box::pin(async move {
        let mut a = Map::new();
        a.insert("id".into(), json!(h.id.0));
        a.insert("name".into(), json!(h.name));
        a.insert("host".into(), json!(h.host));
        a.insert("port".into(), json!(h.port.0));
        a.insert("username".into(), json!(h.username));
        a.insert("created_at".into(), atom_strict(h.created_at.as_ref())?);
        a.insert("updated_at".into(), atom_strict(h.updated_at.as_ref())?);
        let mut rel = Vec::new();
        resolve(&mut rel, inc, "databases", ctx.allows_admin("server_database"), async {
            let dbs = select::<databases::Model, _>(
                &st.db,
                "select * from {databases} where {databases.database_host_id} = ? and {databases.database_host_id} is not null",
                vec![h.id.into()],
            )
            .await?;
            let sub = inc.descend("databases");
            let mut data = Vec::new();
            for d in &dbs {
                data.push(database_item(st, ctx, d, &sub, "server_database").await?);
            }
            Ok(list(data))
        })
        .await?;
        resolve(&mut rel, inc, "nodes", ctx.allows_admin("node"), async {
            let ns = select::<nodes::Model, _>(
                &st.db,
                "select {nodes}.* from {nodes} inner join {database_host_node} on {nodes.id} = {database_host_node.node_id} \
                 where {database_host_node.database_host_id} = ?",
                vec![h.id.into()],
            )
            .await?;
            let sub = inc.descend("nodes");
            let mut data = Vec::new();
            for n in &ns {
                let extra = NodeExtra {
                    after_columns: vec![(
                        "pivot".into(),
                        json!({"database_host_id": h.id.0, "node_id": n.id.0}),
                    )],
                };
                data.push(node_item(st, ctx, n, &sub, extra).await?);
            }
            Ok(list(data))
        })
        .await?;
        Ok(collect("database_host", Value::Object(a), rel))
    })
}
