//! `/api/application/mounts` — `MountController`, `MountData` (spec §3.6, §2.9).
//! Relation endpoints reuse the egg/node/server ACL resources (`GetEggsRequest`
//! etc.), not the mount permission.

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{eggs, mounts, nodes, servers};
use pelican_db::{Int, SmallBool, raw};
use sea_orm::{ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult};
use serde_json::{Map, Value, json};

use super::data::{Fut, NodeExtra, egg_item, node_item, server_item};
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, list, null_resource, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input, php_boolean};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::{CustomRule, Validator};

fn bound_mount(ctx: &Ctx) -> ApiResult<mounts::Model> {
    ctx.bound.mount.clone().ok_or_else(ApiError::model_not_found)
}

/// `MountData::toArray()` = Eloquent `toArray()` of `Mount` ($timestamps=false):
/// column order id..user_mountable.
pub fn mount_attributes(m: &mounts::Model) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("id".into(), json!(m.id.0));
    a.insert("uuid".into(), json!(m.uuid));
    a.insert("name".into(), json!(m.name));
    a.insert("description".into(), json!(m.description));
    a.insert("source".into(), json!(m.source));
    a.insert("target".into(), json!(m.target));
    a.insert("read_only".into(), json!(m.read_only.0));
    a.insert("user_mountable".into(), json!(m.user_mountable.0));
    a
}

/// `MountData` (`mount`). Includes in `$availableIncludes` order: eggs, nodes,
/// servers; nodes carry the morph pivot (§2.3).
pub fn mount_item<'a>(st: &'a AppState, ctx: &'a Ctx, m: &'a mounts::Model, inc: &'a Includes) -> Fut<'a> {
    Box::pin(async move {
        let mut rel: Vec<(&'static str, Value)> = Vec::new();
        if inc.is_requested("eggs") {
            let v = if ctx.allows_admin("egg") { relation_eggs(st, ctx, m.id.0, inc).await? } else { null_resource() };
            rel.push(("eggs", v));
        }
        if inc.is_requested("nodes") {
            let v =
                if ctx.allows_admin("node") { relation_nodes(st, ctx, m.id.0, inc).await? } else { null_resource() };
            rel.push(("nodes", v));
        }
        if inc.is_requested("servers") {
            let v = if ctx.allows_admin("server") {
                relation_servers(st, ctx, m.id.0, inc).await?
            } else {
                null_resource()
            };
            rel.push(("servers", v));
        }
        Ok(crate::envelope::item("mount", Value::Object(mount_attributes(m)), rel))
    })
}

async fn joined_models<M: FromQueryResult>(st: &AppState, table: &str, ty: &str, mount_id: i64) -> ApiResult<Vec<M>> {
    st.db
        .query_all_raw(raw::stmt(
            st.db.get_database_backend(),
            &format!(
                "select {{%t%}}.* from {{%t%}} inner join {{mountables}} on {{%t%.id}} = {{mountables.mountable_id}} \
                 where {{mountables.mount_id}} = ? and {{mountables.mountable_type}} = ?"
            )
            .replace("%t%", table),
            vec![Int(mount_id).into(), ty.into()],
        ))
        .await?
        .iter()
        .map(|r| M::from_query_result(r, "").map_err(Into::into))
        .collect()
}

async fn relation_eggs(st: &AppState, ctx: &Ctx, mount_id: i64, inc: &Includes) -> ApiResult<Value> {
    let es = joined_models::<eggs::Model>(st, "eggs", "egg", mount_id).await?;
    let sub = inc.descend("eggs");
    let mut data = Vec::new();
    for e in &es {
        data.push(egg_item(st, ctx, e, &sub).await?);
    }
    Ok(list(data))
}

async fn relation_nodes(st: &AppState, ctx: &Ctx, mount_id: i64, inc: &Includes) -> ApiResult<Value> {
    let ns = joined_models::<nodes::Model>(st, "nodes", "node", mount_id).await?;
    let sub = inc.descend("nodes");
    let mut data = Vec::new();
    for n in &ns {
        // MorphToMany pivot (vendor .../Relations/MorphToMany.php): after table columns.
        // MorphToMany pivot attribute order (vendor .../Relations/MorphToMany.php:181-191):
        // morphType, foreignPivotKey, relatedPivotKey.
        let extra = NodeExtra {
            after_columns: vec![(
                "pivot".into(),
                json!({"mountable_type": "node", "mount_id": mount_id, "mountable_id": n.id.0}),
            )],
        };
        data.push(node_item(st, ctx, n, &sub, extra).await?);
    }
    Ok(list(data))
}

async fn relation_servers(st: &AppState, ctx: &Ctx, mount_id: i64, inc: &Includes) -> ApiResult<Value> {
    let ss = joined_models::<servers::Model>(st, "servers", "server", mount_id).await?;
    let sub = inc.descend("servers");
    let mut data = Vec::new();
    for s in &ss {
        data.push(server_item(st, ctx, s, &sub).await?);
    }
    Ok(list(data))
}

/// `GET /mounts`: filters `uuid`,`name`, sorts `id`,`uuid`, default per_page 50.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("mount", READ)?;
    let b = st.db.get_database_backend();
    let allowed = ["uuid", "name"];
    let fs = filters(&input, &allowed)?;
    let ss = sorts(&input, &["id", "uuid"])?;
    let page = Page::new(&input, 50)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in allowed {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("mounts.{name}"), v) {
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
            &format!("select count(*) as {{aggregate}} from {{mounts}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = total_row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = st
            .db
            .query_all_raw(raw::stmt(b, &format!("select * from {{mounts}}{where_sql}{order}{}", page.sql()), vals))
            .await?;
        for r in rows {
            let m = mounts::Model::from_query_result(&r, "")?;
            data.push(mount_item(&st, &ctx, &m, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// `GET /mounts/{mount}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("mount", READ)?;
    let m = bound_mount(&ctx)?;
    Ok(PhpJson::ok(mount_item(&st, &ctx, &m, &Includes::parse(&input)).await?).into_response())
}

/// `unique:mounts,name[,<ignore id>]`.
async fn unique_name(st: &AppState, value: Option<&Value>, ignore: Option<i64>) -> ApiResult<CustomRule> {
    let taken = match value {
        None | Some(Value::Null) => false,
        Some(v) => {
            let b = st.db.get_database_backend();
            let mut sql = String::from("select count(*) as {aggregate} from {mounts} where {name} = ?");
            let mut vals: Vec<sea_orm::Value> = vec![pelican_core::php::string(v).into()];
            if let Some(id) = ignore {
                sql.push_str(" and {id} <> ?");
                vals.push(Int(id).into());
            }
            let row = st.db.query_one_raw(raw::stmt(b, &sql, vals)).await?;
            row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).is_some_and(|c| c.0 > 0)
        }
    };
    Ok(CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())))
}

/// `Mount::getRules()` (app/Models/Mount.php).
async fn validate_mount(st: &AppState, input: &Input, ignore: Option<i64>) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("name", &["required", "string", "min:2", "max:64"]);
    v.custom("name", unique_name(st, input.get("name"), ignore).await?);
    v.rules("description", &["nullable", "string", "max:255"]);
    v.rules("source", &["required", "string", "not_in:/etc/pelican,/var/lib/pelican/volumes,/srv/daemon-data"]);
    v.rules("target", &["required", "string", "not_in:/home/container"]);
    v.rules("read_only", &["sometimes", "boolean"]);
    v.rules("user_mountable", &["sometimes", "boolean"]);
    v.validate()
}

const FIELDS: [&str; 6] = ["name", "description", "source", "target", "read_only", "user_mountable"];

/// `$request->validated()`: present keys that have rules.
fn validated(input: &Input) -> Vec<(&'static str, Value)> {
    FIELDS.iter().filter_map(|k| input.get(k).map(|v| (*k, v.clone()))).collect()
}

/// `POST /mounts`: `fill(validated)` + uuid, model defaults read_only/user_mountable=false.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("mount", WRITE)?;
    validate_mount(&st, &input, None).await?;
    let data = validated(&input);
    let get = |k: &str| data.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let am = mounts::ActiveModel {
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        name: Set(get("name").map(|v| pelican_core::php::string(&v)).unwrap_or_default()),
        description: Set(get("description")
            .as_ref()
            .and_then(|v| if v.is_null() { None } else { Some(pelican_core::php::string(v)) })),
        source: Set(get("source").map(|v| pelican_core::php::string(&v)).unwrap_or_default()),
        target: Set(get("target").map(|v| pelican_core::php::string(&v)).unwrap_or_default()),
        read_only: Set(SmallBool(get("read_only").is_some_and(|v| php_boolean(Some(&v))))),
        user_mountable: Set(SmallBool(get("user_mountable").is_some_and(|v| php_boolean(Some(&v))))),
        ..Default::default()
    };
    let m = pelican_db::write::insert(&st.db, am).await?;
    // MountController::store → Mount::create() (eloquent.created)
    crate::model_events::created(&st, &m).await;
    let body = with_meta(
        mount_item(&st, &ctx, &m, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("mounts/{}", m.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// `PATCH /mounts/{mount}`: same rules, unique ignores this id; responds with
/// the bound model (not refreshed).
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("mount", WRITE)?;
    let m = bound_mount(&ctx)?;
    validate_mount(&st, &input, Some(m.id.0)).await?;
    let data = validated(&input);
    let get = |k: &str| data.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let mut am = mounts::ActiveModel { id: sea_orm::ActiveValue::Unchanged(m.id), ..Default::default() };
    let mut dirty = false;
    if let Some(v) = get("name") {
        let s = pelican_core::php::string(&v);
        if s != m.name {
            am.name = Set(s);
            dirty = true;
        }
    }
    if let Some(v) = get("description") {
        let s = if v.is_null() { None } else { Some(pelican_core::php::string(&v)) };
        if s != m.description {
            am.description = Set(s);
            dirty = true;
        }
    }
    for k in ["source", "target"] {
        if let Some(v) = get(k) {
            let s = pelican_core::php::string(&v);
            let cur = if k == "source" { &m.source } else { &m.target };
            if &s != cur {
                if k == "source" {
                    am.source = Set(s);
                } else {
                    am.target = Set(s);
                }
                dirty = true;
            }
        }
    }
    for k in ["read_only", "user_mountable"] {
        if let Some(v) = get(k) {
            let b = SmallBool(php_boolean(Some(&v)));
            let cur = if k == "read_only" { m.read_only } else { m.user_mountable };
            if b != cur {
                if k == "read_only" {
                    am.read_only = Set(b);
                } else {
                    am.user_mountable = Set(b);
                }
                dirty = true;
            }
        }
    }
    if dirty {
        pelican_db::write::update(&st.db, &am).await?;
        // $mount->save() when isDirty (eloquent.updated); pivot mountables
        // attach/detach below fires nothing.
        crate::model_events::fire_id::<mounts::Entity>(&st, crate::model_events::Action::Updated, m.id.0).await;
        let fresh = mounts::Entity::find_by_id(m.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
        return Ok(PhpJson::ok(mount_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?).into_response());
    }
    Ok(PhpJson::ok(mount_item(&st, &ctx, &m, &Includes::parse(&input)).await?).into_response())
}

/// `DELETE /mounts/{mount}`: guard servers attached (trans('exceptions.mount.servers_attached')).
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("mount", WRITE)?;
    let m = bound_mount(&ctx)?;
    let rows = st
        .db
        .query_one_raw(raw::stmt(
            st.db.get_database_backend(),
            "select count(*) as {aggregate} from {mountables} where {mount_id} = ? and {mountable_type} = ?",
            vec![Int(m.id.0).into(), "server".into()],
        ))
        .await?;
    let attached = rows.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).is_some_and(|c| c.0 > 0);
    if attached {
        return Err(ApiError::display(
            "HasActiveServersException",
            400,
            "A mount must have no servers attached to it in order to be deleted.",
        ));
    }
    mounts::Entity::delete_by_id(m.id).exec(&st.db).await?; // mountables cascade via FK
    // $mount->delete() (eloquent.deleted, attributes as before the delete)
    crate::model_events::deleted(&st, &m).await;
    Ok(no_content())
}

/// `exists:<table>,id` over the array elements of `<field>` (`UpdateMount*Request`).
/// The exists rule object reports on the PARENT attribute, evaluated before the
/// per-element `integer` rules.
async fn validate_ids(st: &AppState, input: &Input, field: &str, table: &str) -> ApiResult<()> {
    let mut missing = false;
    if let Some(Value::Array(items)) = input.get(field) {
        let b = st.db.get_database_backend();
        for e in items {
            // Numeric strings are queried verbatim (`id IN ('two')`); a
            // non-numeric string makes PostgreSQL's bigint cast fail → 500.
            let parsed = match e {
                Value::Number(n) => n.as_i64(),
                Value::String(s) if s == "0" || !s.starts_with('0') && s.chars().all(|c| c.is_ascii_digit()) => {
                    s.parse::<i64>().ok()
                }
                _ => None,
            };
            match parsed {
                Some(i) => {
                    if !exists_in_table(st, table, i).await? {
                        missing = true;
                        break;
                    }
                }
                None => {
                    if b == sea_orm::DbBackend::Postgres {
                        return Err(ApiError::internal("QueryException"));
                    }
                    missing = true;
                    break;
                }
            }
        }
    }
    let mut v = Validator::new(input);
    v.rules(field, &["required", "array"]);
    v.custom(
        field,
        CustomRule::precomputed("exists", missing.then(|| "The selected :attribute is invalid.".to_string())),
    );
    v.rules(&format!("{field}.*"), &["integer"]);
    v.validate()
}

async fn exists_in_table(st: &AppState, table: &str, id: i64) -> ApiResult<bool> {
    Ok(match table {
        "eggs" => eggs::Entity::find_by_id(Int(id)).one(&st.db).await?.is_some(),
        "nodes" => nodes::Entity::find_by_id(Int(id)).one(&st.db).await?.is_some(),
        "servers" => servers::Entity::find_by_id(Int(id)).one(&st.db).await?.is_some(),
        _ => false,
    })
}

/// `POST /mounts/{mount}/eggs|nodes|servers` — `attach(ids)` into mountables.
/// An already-attached id violates the composite PK → 500 (original parity).
async fn add_relation(
    st: &AppState,
    ctx: &Ctx,
    input: Input,
    field: &'static str,
    table: &'static str,
    ty: &'static str,
) -> ApiResult<Response> {
    ctx.acl("mount", WRITE)?;
    validate_ids(st, &input, field, table).await?;
    let m = bound_mount(ctx)?;
    if let Some(Value::Array(ids)) = input.get(field) {
        for id in ids {
            if let Some(i) = id.as_i64() {
                // attach already-attached → composite PK violation (UniqueConstraintViolationException)
                let dup = st
                    .db
                    .query_one_raw(raw::stmt(
                        st.db.get_database_backend(),
                        "select count(*) as {aggregate} from {mountables} where {mount_id} = ? and {mountable_type} = ? and {mountable_id} = ?",
                        vec![Int(m.id.0).into(), ty.into(), Int(i).into()],
                    ))
                    .await?
                    .and_then(|r| r.try_get::<Int>("", "aggregate").ok())
                    .is_some_and(|c| c.0 > 0);
                if dup {
                    return Err(ApiError::internal("UniqueConstraintViolationException"));
                }
                st.db
                    .execute_raw(raw::stmt(
                        st.db.get_database_backend(),
                        "insert into {mountables} ({mount_id}, {mountable_type}, {mountable_id}) values (?, ?, ?)",
                        vec![Int(m.id.0).into(), ty.into(), Int(i).into()],
                    ))
                    .await?;
            }
        }
    }
    Ok(PhpJson::ok(mount_item(st, ctx, &m, &Includes::parse(&input)).await?).into_response())
}

/// `DELETE /mounts/{mount}/eggs/{egg_id}` etc — `detach(id)` (no-op when absent).
/// A non-numeric id raises TypeError → 500 (typed int controller param).
async fn detach_relation(st: &AppState, ctx: &Ctx, param: &str, ty: &str) -> ApiResult<Response> {
    ctx.acl("mount", WRITE)?;
    let m = bound_mount(ctx)?;
    let raw_id = ctx.bound.param(param).unwrap_or("");
    let id = raw_id.parse::<i64>().map_err(|_| ApiError::internal("TypeError"))?;
    st.db
        .execute_raw(raw::stmt(
            st.db.get_database_backend(),
            "delete from {mountables} where {mount_id} = ? and {mountable_type} = ? and {mountable_id} = ?",
            vec![Int(m.id.0).into(), ty.into(), Int(id).into()],
        ))
        .await?;
    Ok(no_content())
}

/// `GET /mounts/{mount}/eggs` (perm `egg:R`).
pub async fn get_eggs(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("egg", READ)?;
    let m = bound_mount(&ctx)?;
    let body = relation_eggs(&st, &ctx, m.id.0, &Includes::parse(&input)).await?;
    Ok(PhpJson::ok(body).into_response())
}

/// `GET /mounts/{mount}/nodes` (perm `node:R`, items carry pivot).
pub async fn get_nodes(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("node", READ)?;
    let m = bound_mount(&ctx)?;
    let body = relation_nodes(&st, &ctx, m.id.0, &Includes::parse(&input)).await?;
    Ok(PhpJson::ok(body).into_response())
}

/// `GET /mounts/{mount}/servers` (perm `server:R`).
pub async fn get_servers(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server", READ)?;
    let m = bound_mount(&ctx)?;
    let body = relation_servers(&st, &ctx, m.id.0, &Includes::parse(&input)).await?;
    Ok(PhpJson::ok(body).into_response())
}

pub async fn add_eggs(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    add_relation(&st, &ctx, input, "eggs", "eggs", "egg").await
}

pub async fn add_nodes(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    add_relation(&st, &ctx, input, "nodes", "nodes", "node").await
}

pub async fn add_servers(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    add_relation(&st, &ctx, input, "servers", "servers", "server").await
}

pub async fn delete_egg(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    detach_relation(&st, &ctx, "egg_id", "egg").await
}

pub async fn delete_node(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    detach_relation(&st, &ctx, "node_id", "node").await
}

pub async fn delete_server(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    detach_relation(&st, &ctx, "server_id", "server").await
}
