//! `ServerDetailsController`, `ServerInstallController`, `ServerContainersController`.

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pelican_core::server_config::{self, ConfigError};
use pelican_core::state::AppState;
use pelican_db::entities::{activity_log_subjects, activity_logs, backups, eggs, servers};
use pelican_db::{Int, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};
use serde_json::{Map, Value, json};

use super::{node, node_forbidden};
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::paginate::LengthAware;
use crate::pipeline::Ctx;
use crate::validation::Validator;

fn config_err(e: ConfigError) -> ApiError {
    match e {
        ConfigError::Db(d) => d.into(),
        ConfigError::Php(code) => ApiError::internal(code),
    }
}

/// R5 `GET /servers/{server}`.
pub async fn show(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let node = node(&ctx)?;
    let server = ctx.bound.server()?.clone();
    let valid = match pelican_core::server_state::active_transfer(&st.db, server.id.0).await? {
        Some(t) => node.id == t.old_node || node.id == t.new_node,
        None => node.id == server.node_id,
    };
    if !valid {
        return Err(node_forbidden());
    }
    let (settings, pc) =
        server_config::render(&st.db, server, st.config.use_binary_prefix).await.map_err(config_err)?;
    let mut body = Map::new();
    body.insert("settings".into(), settings);
    body.insert("process_configuration".into(), pc);
    Ok(PhpJson::ok(Value::Object(body)).into_response())
}

/// R2 `GET /servers?page=&per_page=`.
pub async fn list(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    uri: OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    let node = node(&ctx)?;
    let per_page = match input.get("per_page") {
        None => 50,
        Some(v) => pelican_core::php::intval(&pelican_core::php::string(v)),
    };
    let base = servers::Entity::find().filter(servers::Column::NodeId.eq(node.id));
    let total = base.clone().count(&st.db).await? as i64;
    let p = LengthAware::new(
        total,
        per_page,
        input.get("page"),
        format!("{}{}", ctx.root, uri.path().trim_end_matches('/')),
    );
    let mut q = base.order_by_asc(servers::Column::Id);
    if p.per_page > 0 && total > 0 {
        q = q.limit(p.per_page as u64).offset(((p.current_page - 1) * p.per_page) as u64);
    }
    let rows = if total > 0 { q.all(&st.db).await? } else { Vec::new() };
    let mut data = Vec::with_capacity(rows.len());
    for s in rows {
        let uuid = s.uuid.clone();
        let (settings, pc) = server_config::render(&st.db, s, st.config.use_binary_prefix).await.map_err(config_err)?;
        // JsonResource::filter() re-indexes PHP arrays with all-numeric keys (§3.9);
        // `configs` entries contain stdClass values that are left alone.
        let mut pc = pc;
        if let Some(startup) = pc.get_mut("startup") {
            *startup = server_config::reindex_numeric(startup.take());
        }
        let mut item = Map::new();
        item.insert("uuid".into(), json!(uuid));
        item.insert("settings".into(), server_config::reindex_numeric(settings));
        item.insert("process_configuration".into(), pc);
        data.push(Value::Object(item));
    }
    Ok(PhpJson::ok(p.resource_response(data)).into_response())
}

/// R3 `POST /servers/reset`.
pub async fn reset(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let node = node(&ctx)?;
    let restoring = servers::Entity::find()
        .filter(servers::Column::NodeId.eq(node.id))
        .filter(servers::Column::Status.eq("restoring_backup"))
        .all(&st.db)
        .await?;
    let txn = st.db.begin().await?;
    for server in &restoring {
        // Latest `server:backup.restore-started` activity of this server.
        let log_ids: Vec<Int> = activity_log_subjects::Entity::find()
            .select_only()
            .column(activity_log_subjects::Column::ActivityLogId)
            .filter(activity_log_subjects::Column::SubjectType.eq("server"))
            .filter(activity_log_subjects::Column::SubjectId.eq(server.id))
            .into_tuple()
            .all(&txn)
            .await?;
        if log_ids.is_empty() {
            continue;
        }
        let Some(log) = activity_logs::Entity::find()
            .filter(activity_logs::Column::Id.is_in(log_ids))
            .filter(activity_logs::Column::Event.eq("server:backup.restore-started"))
            .order_by_desc(activity_logs::Column::Timestamp)
            .one(&txn)
            .await?
        else {
            continue;
        };
        let backup_id: Option<Int> = activity_log_subjects::Entity::find()
            .select_only()
            .column(activity_log_subjects::Column::SubjectId)
            .filter(activity_log_subjects::Column::ActivityLogId.eq(log.id))
            .filter(activity_log_subjects::Column::SubjectType.eq("backup"))
            .into_tuple()
            .one(&txn)
            .await?;
        let backup = match backup_id {
            Some(id) => backups::Entity::find_by_id(id).one(&txn).await?,
            None => None,
        };
        // `$backup->name` on null → ErrorException.
        let backup = backup.ok_or_else(|| ApiError::internal("ErrorException"))?;
        ctx.activity("server:backup.restore-failed")
            .subject("server", server.id.0)
            .subject("backup", backup.id.0)
            .property("name", backup.name.clone())
            .save_in(&txn)
            .await?;
    }
    let now = Timestamp(pelican_core::time::now());
    servers::Entity::update_many()
        .col_expr(servers::Column::Status, Expr::value(Option::<String>::None))
        .col_expr(servers::Column::UpdatedAt, Expr::value(now))
        .filter(servers::Column::NodeId.eq(node.id))
        .filter(servers::Column::Status.is_in(["installing", "restoring_backup"]))
        .exec(&txn)
        .await?;
    txn.commit().await?;
    Ok(no_content())
}

/// PHP `empty()` on a nullable string.
fn empty(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(|v| v.is_empty() || v == "0")
}

/// R6 `GET /servers/{server}/install`.
pub async fn install_show(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let node = node(&ctx)?;
    let server = ctx.bound.server()?;
    if server.node_id != node.id {
        return Err(node_forbidden());
    }
    let egg = eggs::Entity::find_by_id(server.egg_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let parent = match egg.copy_script_from {
        Some(id) => eggs::Entity::find_by_id(id).one(&st.db).await?,
        None => None,
    };
    // Egg::getCopyScript*Attribute: own value unless empty and a parent is set.
    let pick = |own: Option<String>, from_parent: fn(&eggs::Model) -> Option<String>| -> Option<String> {
        if !empty(&own) || egg.copy_script_from.is_none() { own } else { parent.as_ref().and_then(from_parent) }
    };
    let container = pick(Some(egg.script_container.clone()), |p| Some(p.script_container.clone()));
    let entry = pick(Some(egg.script_entry.clone()), |p| Some(p.script_entry.clone()));
    let script = pick(egg.script_install.clone(), |p| p.script_install.clone());
    let mut body = Map::new();
    body.insert("container_image".into(), json!(container));
    body.insert("entrypoint".into(), json!(entry));
    body.insert("script".into(), json!(script));
    Ok(PhpJson::ok(Value::Object(body)).into_response())
}

/// R7 `POST /servers/{server}/install`.
pub async fn install_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    let mut v = Validator::new(&input);
    v.rules("successful", &["present", "boolean"]);
    v.rules("reinstall", &["sometimes", "boolean"]);
    v.validate()?;
    let node = node(&ctx)?;
    let server = ctx.bound.server()?.clone();
    if server.node_id != node.id {
        return Err(node_forbidden());
    }
    let truthy = |k: &str| crate::input::php_boolean(input.get(k));
    let successful = truthy("successful");
    let mut status: Option<String> = None;
    if !successful {
        status = Some(if truthy("reinstall") { "reinstall_failed" } else { "install_failed" }.to_string());
    }
    if server.status.as_deref() == Some("suspended") {
        status = Some("suspended".into());
    }
    let initial_install = server.installed_at.is_none();
    let now = Timestamp(pelican_core::time::now());
    let status_dirty = server.status != status;
    let installed_dirty = server.installed_at != Some(now);
    if status_dirty || installed_dirty {
        let mut q = servers::Entity::update_many().filter(servers::Column::Id.eq(server.id));
        if status_dirty {
            q = q.col_expr(servers::Column::Status, Expr::value(status.clone()));
        }
        if installed_dirty {
            q = q.col_expr(servers::Column::InstalledAt, Expr::value(Some(now)));
        }
        q.col_expr(servers::Column::UpdatedAt, Expr::value(now)).exec(&st.db).await?;
    }
    crate::services::server_saved(&st, &ctx, &st.db, &server).await?;
    pelican_core::events::server_installed(&st.db, &server, successful, initial_install, &ctx.root).await?;
    crate::services::server_installed_mail(&st, &server, initial_install).await?;
    Ok(no_content())
}

/// R10 `POST /servers/{server}/container/status`: cache only, body `[]`.
pub async fn container_status(Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let node = node(&ctx)?;
    let server = ctx.bound.server()?;
    if server.node_id != node.id {
        return Err(node_forbidden());
    }
    let mut r = (StatusCode::OK, "[]").into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(r)
}
