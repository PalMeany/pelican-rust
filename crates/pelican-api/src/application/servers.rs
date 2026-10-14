//! `/api/application/servers` — `ServerController`, `ExternalServerController`,
//! `ServerDetailsController`, `StartupController`, `ServerManagementController`,
//! `DatabaseController` (spec §3.3).

use axum::Extension;
use axum::extract::{OriginalUri, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{database_hosts, databases, nodes, server_transfers, servers, users};
use pelican_db::{Bool, Int, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::Value;

use super::data::{database_item, server_item};
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, list, with_meta};
use crate::error::{ApiError, ApiResult, Style};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::{CustomRule, Validator};

pub(super) fn bound(ctx: &Ctx) -> ApiResult<servers::Model> {
    ctx.bound.server.clone().ok_or_else(ApiError::model_not_found)
}

pub(super) async fn node_conn<C: ConnectionTrait>(
    st: &AppState,
    db: &C,
    node_id: Int,
) -> ApiResult<pelican_wings::Node> {
    let node = nodes::Entity::find_by_id(node_id).one(db).await?.ok_or_else(|| ApiError::internal("ErrorException"))?;
    st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))
}

fn wings_err(e: pelican_wings::WingsError) -> ApiError {
    ApiError::internal(e.code())
}

/// `GET /servers`.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server", READ)?;
    let mut v = Validator::new(&input);
    v.rules("search", &["string", "max:100"]);
    v.validate()?;
    let b = st.db.get_database_backend();
    let names = ["uuid", "uuid_short", "name", "description", "image", "external_id"];
    let fs = filters(&input, &names)?;
    let ss = sorts(&input, &["id", "uuid"])?;
    let page = Page::new(&input, 50)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in names {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("servers.{name}"), v) {
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
    let row = st
        .db
        .query_one_raw(raw::stmt(
            b,
            &format!("select count(*) as {{aggregate}} from {{servers}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = servers::Model::find_by_statement(raw::stmt(
            b,
            &format!("select * from {{servers}}{where_sql}{order}{}", page.sql()),
            vals,
        ))
        .all(&st.db)
        .await?;
        for s in &rows {
            data.push(server_item(&st, &ctx, s, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// `GET /servers/{server}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("server", READ)?;
    let s = bound(&ctx)?;
    Ok(PhpJson::ok(server_item(&st, &ctx, &s, &Includes::parse(&input)).await?).into_response())
}

/// `GET /servers/external/{external_id}`.
pub async fn view_external(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    Path(external_id): Path<String>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server", READ)?;
    let s = servers::Entity::find()
        .filter(servers::Column::ExternalId.eq(external_id))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(server_item(&st, &ctx, &s, &Includes::parse(&input)).await?).into_response())
}

/// `unique:<table>,<column>[,ignore id]` / `exists:<table>,<column>` lookups.
pub(super) async fn count_where(
    st: &AppState,
    table: &str,
    column: &str,
    value: &Value,
    ignore: Option<i64>,
) -> ApiResult<i64> {
    let b = st.db.get_database_backend();
    let mut sql = format!("select count(*) as {{aggregate}} from {{{table}}} where {{{column}}} = ?");
    let v = pelican_core::php::string(value);
    // PDO sends strings untyped; integer columns need an integer bind on PostgreSQL.
    let bind: sea_orm::Value = if column == "id" || (column.ends_with("_id") && column != "external_id") {
        match v.trim().parse::<i64>() {
            Ok(i) => i.into(),
            Err(_) => return Ok(0),
        }
    } else {
        v.into()
    };
    let mut vals = vec![bind];
    if let Some(id) = ignore {
        sql.push_str(" and {id} <> ?");
        vals.push(id.into());
    }
    let row = st.db.query_one_raw(raw::stmt(b, &sql, vals)).await?;
    Ok(row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0))
}

pub(super) async fn unique_rule(
    st: &AppState,
    input: &Input,
    attr: &str,
    table: &str,
    column: &str,
    ignore: Option<i64>,
) -> ApiResult<CustomRule> {
    let taken = match input.get(attr) {
        None | Some(Value::Null) => false,
        Some(v) => count_where(st, table, column, v, ignore).await? > 0,
    };
    Ok(CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())))
}

pub(super) async fn exists_rule(
    st: &AppState,
    value: Option<&Value>,
    table: &str,
    column: &str,
) -> ApiResult<CustomRule> {
    let missing = match value {
        None | Some(Value::Null) => false,
        Some(v) => count_where(st, table, column, v, None).await? == 0,
    };
    Ok(CustomRule::precomputed("exists", missing.then(|| "The selected :attribute is invalid.".to_string())))
}

/// `PATCH /servers/{server}/details` (`DetailsModificationService`).
pub async fn details(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let mut s = bound(&ctx)?;
    let mut v = Validator::new(&input);
    v.rules("external_id", &["sometimes", "nullable", "string", "between:1,255"]);
    v.custom("external_id", unique_rule(&st, &input, "external_id", "servers", "external_id", Some(s.id.0)).await?);
    v.rules("name", &["required", "string", "min:1", "max:255"]);
    v.rules("user", &["required", "integer"]);
    v.custom("user", exists_rule(&st, input.get("user"), "users", "id").await?);
    v.rules("description", &["nullable", "string"]);
    v.attribute_name("user", "User ID");
    v.attribute_name("name", "Server Name");
    v.validate()?;
    let external_id = input.get("external_id").filter(|v| !v.is_null()).map(pelican_core::php::string);
    let owner = input.get("user").map(|v| pelican_core::php::intval(&pelican_core::php::string(v))).unwrap_or(0);
    let name = input.get("name").map(pelican_core::php::string).unwrap_or_default();
    let description =
        input.get("description").filter(|v| !v.is_null()).map(pelican_core::php::string).unwrap_or_default();

    let txn = st.db.begin().await?;
    let old_owner =
        users::Entity::find_by_id(s.owner_id).one(&txn).await?.ok_or_else(|| ApiError::internal("ErrorException"))?;
    let mut am = servers::ActiveModel { id: sea_orm::ActiveValue::Unchanged(s.id), ..Default::default() };
    let mut dirty = false;
    if s.external_id != external_id {
        am.external_id = Set(external_id.clone());
        dirty = true;
    }
    if s.owner_id.0 != owner {
        am.owner_id = Set(Int(owner));
        dirty = true;
    }
    if s.name != name {
        am.name = Set(name.clone());
        dirty = true;
    }
    if s.description.as_deref() != Some(description.as_str()) {
        am.description = Set(Some(description.clone()));
        dirty = true;
    }
    if dirty {
        let now = Timestamp(pelican_core::time::now());
        am.updated_at = Set(Some(now));
        pelican_db::write::update(&txn, &am).await?;
        s.updated_at = Some(now);
    }
    s.external_id = external_id;
    s.owner_id = Int(owner);
    s.name = name;
    s.description = Some(description);
    crate::services::server_saved(&st, &ctx, &txn, &s).await?;
    if s.owner_id != old_owner.id {
        crate::client::subusers::revoke(&st, &txn, &s, &old_owner.uuid).await?;
    }
    txn.commit().await?;
    Ok(PhpJson::ok(server_item(&st, &ctx, &s, &Includes::parse(&input)).await?).into_response())
}

async fn toggle_suspension(st: &AppState, ctx: &Ctx, suspend: bool) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(ctx)?;
    set_suspension(st, ctx, s, suspend).await?;
    Ok(no_content())
}

/// `SuspensionService::handle()` (also used by the web UI).
pub async fn set_suspension(st: &AppState, ctx: &Ctx, mut s: servers::Model, suspend: bool) -> ApiResult<()> {
    if suspend == (s.status.as_deref() == Some("suspended")) {
        return Ok(());
    }
    if pelican_core::server_state::active_transfer(&st.db, s.id.0).await?.is_some() {
        return Err(ApiError::new(
            409,
            "ConflictHttpException",
            "Cannot toggle suspension status on a server that is currently being transferred.",
            Style::Pretty,
        ));
    }
    let status = suspend.then(|| "suspended".to_string());
    pelican_db::write::update(
        &st.db,
        &servers::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(s.id),
            status: Set(status.clone()),
            updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
            ..Default::default()
        },
    )
    .await?;
    s.status = status;
    crate::services::server_saved(st, ctx, &st.db, &s).await?;
    let conn = node_conn(st, &st.db, s.node_id).await?;
    st.wings.sync_server(&conn, &s.uuid).await.map_err(wings_err)?;
    Ok(())
}

/// `POST /servers/{server}/suspend`.
pub async fn suspend(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    toggle_suspension(&st, &ctx, true).await
}

/// `POST /servers/{server}/unsuspend`.
pub async fn unsuspend(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    toggle_suspension(&st, &ctx, false).await
}

/// `POST /servers/{server}/reinstall` (`ReinstallServerService`).
pub async fn reinstall(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(&ctx)?;
    reinstall_server(&st, &ctx, s).await?;
    Ok(no_content())
}

/// `ReinstallServerService::handle()` (also used by the web UI).
pub async fn reinstall_server(st: &AppState, ctx: &Ctx, mut s: servers::Model) -> ApiResult<()> {
    let st = st.clone();
    let conn = node_conn(&st, &st.db, s.node_id).await?;
    let txn = st.db.begin().await?;
    if s.status.as_deref() != Some("installing") {
        pelican_db::write::update(
            &txn,
            &servers::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(s.id),
                status: Set(Some("installing".into())),
                updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
                ..Default::default()
            },
        )
        .await?;
        s.status = Some("installing".into());
    }
    crate::services::server_saved(&st, ctx, &txn, &s).await?;
    st.wings.reinstall_server(&conn, &s.uuid).await.map_err(wings_err)?;
    txn.commit().await?;
    Ok(())
}

/// `POST /servers/{server}/transfer/cancel`.
pub async fn transfer_cancel(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(&ctx)?;
    let Some(t) = pelican_core::server_state::active_transfer(&st.db, s.id.0).await? else {
        // `new Response('', 406)`: Symfony's default HTML content type.
        return Ok((StatusCode::NOT_ACCEPTABLE, [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")], "")
            .into_response());
    };
    pelican_db::write::update(
        &st.db,
        &server_transfers::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(t.id),
            successful: Set(Some(Bool(true))),
            updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
            ..Default::default()
        },
    )
    .await?;
    // DaemonServerRepository::cancelTransfer(): source node, then the target node.
    let old = node_conn(&st, &st.db, s.node_id).await?;
    let path = format!("/api/servers/{}/transfer", s.uuid);
    st.wings
        .send(&old, pelican_wings::client::Call::new(pelican_wings::client::Method::DELETE, &path))
        .await
        .map_err(wings_err)?;
    let new = node_conn(&st, &st.db, t.new_node).await?;
    let path = format!("/api/transfers/{}", s.uuid);
    st.wings
        .send(&new, pelican_wings::client::Call::new(pelican_wings::client::Method::DELETE, &path))
        .await
        .map_err(wings_err)?;
    Ok(no_content())
}

/// `DELETE /servers/{server}[/{force}]`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let force = ctx.bound.param("force") == Some("force");
    let s = bound(&ctx)?;
    delete_server(&st, &ctx, &s, force).await?;
    Ok(no_content())
}

/// `ServerDeletionService::withForce()->handle()`.
pub(super) async fn force_delete(st: &AppState, ctx: &Ctx, s: &servers::Model) -> ApiResult<()> {
    delete_server(st, ctx, s, true).await
}

/// `ServerDeletionService::handle()` (also used by the web UI).
pub async fn delete_server(st: &AppState, ctx: &Ctx, s: &servers::Model, force: bool) -> ApiResult<()> {
    let conn = node_conn(st, &st.db, s.node_id).await?;
    match st.wings.delete_server(&conn, &s.uuid).await {
        Ok(()) => {}
        Err(pelican_wings::WingsError::Connection(_)) if !force => {
            return Err(ApiError::internal("ConnectionException"));
        }
        Err(e) => tracing::warn!(error = %e, "deleting server on wings failed"),
    }
    let txn = st.db.begin().await?;
    let dbs = databases::Model::find_by_statement(raw::stmt(
        st.db.get_database_backend(),
        "select * from {databases} where {databases.server_id} = ? and {databases.server_id} is not null",
        vec![s.id.into()],
    ))
    .all(&txn)
    .await?;
    for d in &dbs {
        if let Err(e) = crate::client::databases::delete_database_in(st, ctx, &txn, d).await {
            if !force {
                return Err(e);
            }
            databases::Entity::delete_by_id(d.id).exec(&txn).await?;
        }
    }
    let b = st.db.get_database_backend();
    let now = Timestamp(pelican_core::time::now());
    txn.execute_raw(raw::stmt(
        b,
        "update {allocations} set {server_id} = ?, {notes} = ?, {is_locked} = ?, {show_port} = ?, {updated_at} = ? \
         where {allocations.server_id} = ? and {allocations.server_id} is not null",
        vec![
            sea_orm::Value::BigInt(None),
            sea_orm::Value::String(None),
            false.into(),
            true.into(),
            now.into(),
            s.id.into(),
        ],
    ))
    .await?;
    servers::Entity::delete_by_id(s.id).exec(&txn).await?;
    txn.commit().await?;
    Ok(())
}

// --- server databases ------------------------------------------------------------

fn bound_db(ctx: &Ctx) -> ApiResult<databases::Model> {
    ctx.bound.database.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /servers/{server}/databases` (unpaginated).
pub async fn databases_index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server_database", READ)?;
    let s = bound(&ctx)?;
    let dbs = databases::Model::find_by_statement(raw::stmt(
        st.db.get_database_backend(),
        "select * from {databases} where {databases.server_id} = ? and {databases.server_id} is not null",
        vec![s.id.into()],
    ))
    .all(&st.db)
    .await?;
    let inc = Includes::parse(&input);
    let mut data = Vec::new();
    for d in &dbs {
        data.push(database_item(&st, &ctx, d, &inc, "server_database").await?);
    }
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `GET /servers/{server}/databases/{database}`.
pub async fn databases_view(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server_database", READ)?;
    let d = bound_db(&ctx)?;
    Ok(PhpJson::ok(database_item(&st, &ctx, &d, &Includes::parse(&input), "server_database").await?).into_response())
}

/// `POST /servers/{server}/databases`.
pub async fn databases_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server_database", WRITE)?;
    let s = bound(&ctx)?;
    let raw_name = match input.get("database") {
        Some(Value::Array(_)) | Some(Value::Object(_)) => return Err(ApiError::internal("TypeError")),
        Some(v) if !v.is_null() => pelican_core::php::string(v),
        _ => String::new(),
    };
    let prefixed = pelican_core::databases::unique_name(&raw_name, s.id.0);
    let mut v = Validator::new(&input);
    v.rules("database", &["required", "alpha_dash", "min:1", "max:48"]);
    // Rule::unique('databases') on `database = input` AND `database = prefixed(input)`.
    let taken = input.get("database").is_some_and(|v| !v.is_null())
        && prefixed == raw_name
        && databases::Entity::find()
            .filter(databases::Column::ServerId.eq(s.id))
            .filter(databases::Column::Database.eq(prefixed.clone()))
            .one(&st.db)
            .await?
            .is_some();
    v.custom(
        "database",
        CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
    );
    v.rules("remote", &["required", "string", r"regex:/^[0-9%.]{1,15}$/"]);
    v.rules("host", &["required", "integer"]);
    v.custom("host", exists_rule(&st, input.get("host"), "database_hosts", "id").await?);
    v.attribute_name("host", "Database Host Server ID");
    v.attribute_name("remote", "Remote Connection String");
    v.attribute_name("database", "Database Name");
    v.validate()?;
    let host_id = input.get("host").map(|v| pelican_core::php::intval(&pelican_core::php::string(v))).unwrap_or(0);
    let host =
        database_hosts::Entity::find_by_id(Int(host_id)).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    let remote = input.get("remote").map(pelican_core::php::string).unwrap_or_default();
    let mut created =
        crate::client::databases::create_database(&st, &ctx, &s, &host, &prefixed, &remote, false).await?;
    created.max_connections = None;
    let body = with_meta(
        database_item(&st, &ctx, &created, &Includes::parse(&input), "server_database").await?,
        resource_meta(&st, &format!("servers/{}/databases/{}", s.id.0, created.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// `POST /servers/{server}/databases/{database}/reset-password` (no activity).
pub async fn databases_reset_password(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
) -> ApiResult<Response> {
    ctx.acl("server_database", WRITE)?;
    let d = bound_db(&ctx)?;
    let txn = st.db.begin().await?;
    crate::client::databases::rotate_password_in(&st, &txn, &d).await?;
    txn.commit().await?;
    Ok(no_content())
}

/// `DELETE /servers/{server}/databases/{database}`.
pub async fn databases_delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("server_database", WRITE)?;
    let d = bound_db(&ctx)?;
    let txn = st.db.begin().await?;
    crate::client::databases::delete_database_in(&st, &ctx, &txn, &d).await?;
    txn.commit().await?;
    Ok(no_content())
}
