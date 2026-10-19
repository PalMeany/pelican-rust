//! `/servers/{server}/backups` — `BackupController`, Initiate/DeleteBackupService.

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{backup_hosts, backups, nodes, servers};
use pelican_db::raw;
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use super::servers::count;
use crate::envelope::{Pagination, item, list, with_meta};
use crate::error::{ApiError, ApiResult, Style};
use crate::input::{In, Input, parse_query, php_boolean};
use crate::json::{PhpJson, no_content};
use crate::model_events::{self, Action};
use crate::pipeline::Ctx;
use crate::resources::atom;
use crate::services::LogActivity;
use crate::validation::Validator;

/// `BackupData` (resource `backup`).
pub fn backup_item(b: &backups::Model) -> Value {
    let ignored = pelican_core::server_config::php_assoc(b.ignored_files.0.clone());
    let mut m = Map::new();
    m.insert("uuid".into(), json!(b.uuid));
    m.insert("is_successful".into(), json!(b.is_successful.0));
    m.insert("is_locked".into(), json!(b.is_locked.0));
    m.insert("is_scheduled".into(), json!(b.is_scheduled.0));
    m.insert("name".into(), json!(b.name));
    m.insert("ignored_files".into(), ignored);
    m.insert("checksum".into(), json!(b.checksum));
    m.insert("bytes".into(), json!(b.bytes.0));
    m.insert("created_at".into(), json!(atom(b.created_at.as_ref())));
    m.insert("completed_at".into(), json!(atom(b.completed_at.as_ref())));
    item("backup", Value::Object(m), vec![])
}

async fn require(st: &AppState, ctx: &Ctx, perm: &str) -> ApiResult<()> {
    if ctx.can(st, perm).await? { Ok(()) } else { Err(ApiError::unauthorized_action()) }
}

const NON_FAILED: &str = "({completed_at} is null or {is_successful} = ?)";

fn bool_val(backend: sea_orm::DbBackend) -> sea_orm::Value {
    // PDO binds PHP true as 1 (PostgreSQL coerces it for boolean columns).
    if backend == sea_orm::DbBackend::Postgres { true.into() } else { 1.into() }
}

/// `GET /backups`.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    uri: OriginalUri,
) -> ApiResult<Response> {
    require(&st, &ctx, "backup.read").await?;
    let server = ctx.bound.server()?;
    let db = &st.db;
    let b = db.get_database_backend();
    let q = Input::from_value(parse_query(uri.query().unwrap_or("")));
    let per = match q.get("per_page") {
        None | Some(Value::Null) => 20,
        Some(_) => crate::envelope::per_page(&q, 20, 50),
    };
    let page = crate::paginate::resolve_page(q.get("page"));
    let base = "select * from {backups} where {backups.server_id} = ? and {backups.server_id} is not null";
    let total = count(db, b, base, vec![server.id.0.into()]).await?;
    let rows = if total > 0 {
        backups::Model::find_by_statement(raw::stmt(
            b,
            &format!("{base} limit ? offset ?"),
            vec![server.id.0.into(), per.into(), ((page - 1) * per).into()],
        ))
        .all(db)
        .await?
    } else {
        Vec::new()
    };
    let backup_count = count(
        db,
        b,
        &format!("select * from {{backups}} where {{backups.server_id}} = ? and {{backups.server_id}} is not null and {NON_FAILED}"),
        vec![server.id.0.into(), bool_val(b)],
    )
    .await?;
    let data: Vec<Value> = rows.iter().map(backup_item).collect();
    let p = Pagination {
        total,
        count: data.len() as i64,
        per_page: per,
        current_page: page,
        url: format!("{}{}", ctx.root, uri.path().trim_end_matches('/')),
        appends: Vec::new(),
    };
    let mut meta = Map::new();
    meta.insert("backup_count".into(), json!(backup_count));
    meta.insert("pagination".into(), p.meta());
    Ok(PhpJson::ok(with_meta(list(data), meta)).into_response())
}

/// Backup host: the node's first linked host, else the first host with no nodes.
async fn backup_host<C: ConnectionTrait>(db: &C, node_id: i64) -> ApiResult<backup_hosts::Model> {
    let b = db.get_database_backend();
    let linked = backup_hosts::Model::find_by_statement(raw::stmt(
        b,
        "select {backup_hosts}.* from {backup_hosts} inner join {backup_host_node} on {backup_hosts.id} = {backup_host_node.backup_host_id} \
         where {backup_host_node.node_id} = ? limit 1",
        vec![node_id.into()],
    ))
    .one(db)
    .await?;
    if let Some(h) = linked {
        return Ok(h);
    }
    backup_hosts::Model::find_by_statement(raw::stmt(
        b,
        "select * from {backup_hosts} where not exists (select * from {backup_host_node} where {backup_hosts.id} = {backup_host_node.backup_host_id}) limit 1",
        vec![],
    ))
    .one(db)
    .await?
    .ok_or_else(ApiError::model_not_found)
}

async fn node_conn(st: &AppState, server: &servers::Model) -> ApiResult<(nodes::Model, pelican_wings::Node)> {
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    Ok((node, conn))
}

fn wings_err(e: pelican_wings::WingsError) -> ApiError {
    ApiError::internal(e.code())
}

/// `POST /backups`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "backup.create").await?;
    let mut v = Validator::new(&input);
    v.rules("name", &["nullable", "string", "max:255"]);
    v.rules("is_locked", &["nullable", "boolean"]);
    v.rules("ignored", &["nullable", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let ignored: Vec<String> = input
        .get("ignored")
        .map(pelican_core::php::string)
        .unwrap_or_default()
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let is_locked = if ctx.can(&st, "backup.delete").await? { php_boolean(input.get("is_locked")) } else { false };
    let txn = pelican_db::write::begin_immediate(&st.db).await?;
    lock_server(&txn, server.id.0).await?;
    let opts = InitiateBackup {
        name: input.get("name").map(pelican_core::php::string),
        ignored,
        is_locked,
        is_scheduled: false,
        override_limit: false,
    };
    let backup = initiate_backup(&st, &txn, &server, opts).await?;
    let name = backup.name.clone();
    ctx.activity("server:backup.start")
        .subject(morph::BACKUP, backup.id.0)
        .property("name", name)
        .property("locked", php_boolean(input.get("is_locked")))
        .log_in(&st, &txn)
        .await?;
    txn.commit().await?;
    // InitiateBackupService::handle(): Backup::create([...]).
    model_events::created(&st, &backup).await;
    Ok(PhpJson::ok(backup_item(&backup)).into_response())
}

/// Options of `InitiateBackupService`.
pub struct InitiateBackup {
    pub name: Option<String>,
    /// `setIgnoredFiles()`: already split, empty lines dropped.
    pub ignored: Vec<String>,
    pub is_locked: bool,
    pub is_scheduled: bool,
    /// `handle(..., $override)`: scheduled backups rotate the oldest unlocked backup.
    pub override_limit: bool,
}

/// `SELECT id FROM servers WHERE id = ? FOR UPDATE` (no lock clause on SQLite).
pub async fn lock_server<C: ConnectionTrait>(db: &C, id: i64) -> ApiResult<()> {
    let b = db.get_database_backend();
    let lock = if b == sea_orm::DbBackend::Sqlite { "" } else { " for update" };
    let sql = format!("select {{id}} from {{servers}} where {{servers.id}} = ? limit 1{lock}");
    db.query_one_raw(raw::stmt(b, &sql, vec![id.into()])).await?;
    Ok(())
}

/// `InitiateBackupService::handle()` on `db` (the caller's transaction).
pub async fn initiate_backup<C: ConnectionTrait>(
    st: &AppState,
    db: &C,
    server: &servers::Model,
    o: InitiateBackup,
) -> ApiResult<backups::Model> {
    let b = db.get_database_backend();
    let limit = st.config.env.int_or("BACKUP_THROTTLE_LIMIT", 2);
    let period = st.config.env.int_or("BACKUP_THROTTLE_PERIOD", 600);
    let now = pelican_core::time::now();
    if period > 0 {
        let since = Timestamp(now - chrono::Duration::seconds(period));
        let previous = backups::Model::find_by_statement(raw::stmt(
            b,
            &format!("select * from {{backups}} where {{backups.server_id}} = ? and {{backups.server_id}} is not null and {{created_at}} >= ? and {NON_FAILED}"),
            vec![server.id.0.into(), since.into(), bool_val(b)],
        ))
        .all(db)
        .await?;
        if previous.len() as i64 >= limit {
            let last = previous.last().and_then(|p| p.created_at).map(|t| t.0).unwrap_or(now);
            let retry = (last + chrono::Duration::seconds(period) - now).num_seconds();
            return Err(ApiError::new(
                429,
                "TooManyRequestsHttpException",
                format!("Only {limit} backups may be generated within a {period} second span of time."),
                Style::Pretty,
            )
            .with_header("retry-after", retry));
        }
    }
    let successful = "select * from {backups} where {backups.server_id} = ? and {backups.server_id} is not null";
    let non_failed =
        count(db, b, &format!("{successful} and {NON_FAILED}"), vec![server.id.0.into(), bool_val(b)]).await?;
    let too_many = || {
        ApiError::display(
            "TooManyBackupsException",
            400,
            format!(
                "Cannot create a new backup, this server has reached its limit of {} backups.",
                server.backup_limit.0
            ),
        )
    };
    if server.backup_limit.0 == 0 || non_failed >= server.backup_limit.0 {
        if !o.override_limit || server.backup_limit.0 <= 0 {
            return Err(too_many());
        }
        let oldest = backups::Model::find_by_statement(raw::stmt(
            b,
            &format!("{successful} and {NON_FAILED} and {{is_locked}} = ? order by {{created_at}} asc limit 1"),
            vec![server.id.0.into(), bool_val(b), 0.into()],
        ))
        .one(db)
        .await?
        .ok_or_else(too_many)?;
        delete_backup_in(st, db, server, &oldest).await?;
    }
    let host = backup_host(db, server.node_id.0).await?;
    if host.schema != "wings" && host.schema != "s3" {
        return Err(ApiError::internal("Exception"));
    }
    let name = o.name.unwrap_or_default();
    let name = name.trim();
    let name =
        if name.is_empty() { format!("Backup at {}", now.format("%Y-%m-%d %H:%M:%S")) } else { name.to_string() };
    let ts = Timestamp(now);
    let am = backups::ActiveModel {
        server_id: Set(server.id),
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        name: Set(name),
        ignored_files: Set(Json(json!(o.ignored))),
        backup_host_id: Set(host.id),
        is_locked: Set(SmallBool(o.is_locked)),
        is_scheduled: Set(Bool(o.is_scheduled)),
        is_successful: Set(Bool(false)),
        checksum: Set(None),
        bytes: Set(Int(0)),
        upload_id: Set(None),
        created_at: Set(Some(ts)),
        updated_at: Set(Some(ts)),
        ..Default::default()
    };
    let backup = pelican_db::write::insert(db, am).await?;
    // Both schemas ask Wings to create it (`DaemonBackupRepository::create`, adapter = schema).
    let (_, conn) = node_conn(st, server).await?;
    st.wings
        .create_backup(&conn, &server.uuid, &host.schema, &backup.uuid, &o.ignored.join("\n"))
        .await
        .map_err(wings_err)?;
    Ok(backup)
}

fn bound(ctx: &Ctx) -> ApiResult<&backups::Model> {
    ctx.bound.backup.as_ref().ok_or_else(|| ApiError::internal("InvalidArgumentException"))
}

/// `GET /backups/{backup}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    require(&st, &ctx, "backup.read").await?;
    Ok(PhpJson::ok(backup_item(bound(&ctx)?)).into_response())
}

/// Wings download URL (`WingsBackupSchema::getDownloadLink`).
pub async fn download_link(
    st: &AppState,
    ctx: &Ctx,
    server: &servers::Model,
    backup: &backups::Model,
) -> ApiResult<String> {
    let user = ctx.user()?;
    let (_, conn) = node_conn(st, server).await?;
    let now = chrono::Utc::now().timestamp();
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: conn.connection_address(),
        identified_by: format!("{}{}", user.id(), server.uuid),
        issued_at: now,
        expires_at: now + 900,
        subject: None,
        claims: vec![("backup_uuid".into(), json!(backup.uuid)), ("server_uuid".into(), json!(server.uuid))],
        user_uuid: Some(user.user.uuid.clone()),
        scope: "backup-download".into(),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&conn.token)
    .map_err(|_| ApiError::internal("InvalidKeyProvided"))?;
    Ok(format!("{}/download/backup?token={token}", conn.connection_address()))
}

async fn host_of<C: ConnectionTrait>(db: &C, backup: &backups::Model) -> ApiResult<backup_hosts::Model> {
    backup_hosts::Entity::find_by_id(backup.backup_host_id)
        .one(db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))
}

/// `S3BackupSchema::getDownloadLink`: a presigned GET valid for five minutes.
fn s3_download_link(host: &backup_hosts::Model, server: &servers::Model, backup: &backups::Model) -> String {
    crate::s3::Bucket::from_host(&host).presign(
        "GET",
        &crate::s3::backup_key(&server.uuid, &backup.uuid),
        &[],
        300,
        chrono::Utc::now(),
    )
}

/// `GET /backups/{backup}/download`.
pub async fn download(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    require(&st, &ctx, "backup.download").await?;
    let server = ctx.bound.server()?.clone();
    let backup = bound(&ctx)?.clone();
    let host = host_of(&st.db, &backup).await?;
    let url = match host.schema.as_str() {
        "wings" => download_link(&st, &ctx, &server, &backup).await?,
        "s3" => s3_download_link(&host, &server, &backup),
        _ => {
            return Err(ApiError::bad_request(
                "The backup requested references an unknown disk driver type and cannot be downloaded.",
            ));
        }
    };
    ctx.activity("server:backup.download")
        .subject(morph::BACKUP, backup.id.0)
        .property("name", backup.name.clone())
        .log(&st)
        .await?;
    Ok(PhpJson::ok(json!({"object": "signed_url", "attributes": {"url": url}})).into_response())
}

/// `PUT /backups/{backup}/rename` (permission `backup.delete`).
pub async fn rename(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "backup.delete").await?;
    let mut v = Validator::new(&input);
    v.rules("name", &["required", "string", "max:255"]);
    v.validate()?;
    let mut backup = bound(&ctx)?.clone();
    let new_name = input.get("name").map(pelican_core::php::string).unwrap_or_default();
    let old = backup.name.clone();
    if old != new_name {
        let now = Timestamp(pelican_core::time::now());
        backups::Entity::update_many()
            .col_expr(backups::Column::Name, Expr::value(new_name.clone()))
            .col_expr(backups::Column::UpdatedAt, Expr::value(now))
            .filter(backups::Column::Id.eq(backup.id))
            .exec(&st.db)
            .await?;
        // BackupController: $backup->update(['name' => $newName]).
        model_events::updated(&st, &backup).await;
        backup.name = new_name.clone();
        backup.updated_at = Some(now);
        ctx.activity("server:backup.rename")
            .subject(morph::BACKUP, backup.id.0)
            .property("old_name", old)
            .property("new_name", new_name)
            .log(&st)
            .await?;
    }
    Ok(PhpJson::ok(backup_item(&backup)).into_response())
}

/// `POST /backups/{backup}/lock`.
pub async fn lock(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    require(&st, &ctx, "backup.delete").await?;
    let mut backup = bound(&ctx)?.clone();
    let event = if backup.is_locked.0 { "server:backup.unlock" } else { "server:backup.lock" };
    let now = Timestamp(pelican_core::time::now());
    backups::Entity::update_many()
        .col_expr(backups::Column::IsLocked, Expr::value(SmallBool(!backup.is_locked.0)))
        .col_expr(backups::Column::UpdatedAt, Expr::value(now))
        .filter(backups::Column::Id.eq(backup.id))
        .exec(&st.db)
        .await?;
    backup.is_locked = SmallBool(!backup.is_locked.0);
    backup.updated_at = Some(now);
    // BackupController: $backup->update(['is_locked' => !$backup->is_locked]).
    model_events::updated(&st, &backup).await;
    ctx.activity(event).subject(morph::BACKUP, backup.id.0).property("name", backup.name.clone()).log(&st).await?;
    Ok(PhpJson::ok(backup_item(&backup)).into_response())
}

/// `POST /backups/{backup}/restore`.
pub async fn restore(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "backup.restore").await?;
    let mut v = Validator::new(&input);
    v.rules("truncate", &["required", "boolean"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let backup = bound(&ctx)?.clone();
    if server.status.is_some() {
        return Err(ApiError::bad_request(
            "This server is not currently in a state that allows for a backup to be restored.",
        ));
    }
    if !backup.is_successful.0 && backup.completed_at.is_none() {
        return Err(ApiError::bad_request("This backup cannot be restored at this time: not completed or failed."));
    }
    let host = host_of(&st.db, &backup).await?;
    let url = match host.schema.as_str() {
        "wings" => download_link(&st, &ctx, &server, &backup).await?,
        "s3" => s3_download_link(&host, &server, &backup),
        _ => return Err(ApiError::internal("Exception")),
    };
    let truncate_raw = input.get("truncate").cloned().unwrap_or(Value::Null);
    let truncate = php_boolean(Some(&truncate_raw));
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    servers::Entity::update_many()
        .col_expr(servers::Column::Status, Expr::value(Some("restoring_backup".to_string())))
        .col_expr(servers::Column::UpdatedAt, Expr::value(now))
        .filter(servers::Column::Id.eq(server.id))
        .exec(&txn)
        .await?;
    let (_, conn) = node_conn(&st, &server).await?;
    st.wings
        .restore_backup(&conn, &server.uuid, &backup.uuid, &host.schema, truncate, &url)
        .await
        .map_err(wings_err)?;
    ctx.activity("server:backup.restore")
        .subject(morph::BACKUP, backup.id.0)
        .property("name", backup.name.clone())
        .property("truncate", truncate_raw)
        .log_in(&st, &txn)
        .await?;
    txn.commit().await?;
    // BackupController: $server->update(['status' => ServerState::RestoringBackup]).
    {
        // Relations loaded by AuthenticateServerAccess travel with the model.
        let loaded = model_events::client_server_loaded(&st, &ctx, &server).await;
        model_events::fire_id_with::<servers::Entity, _>(&st, &st.db, Action::Updated, server.id.0, loaded).await;
    }
    Ok(no_content())
}

/// `DELETE /backups/{backup}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    require(&st, &ctx, "backup.delete").await?;
    let server = ctx.bound.server()?.clone();
    let backup = bound(&ctx)?.clone();
    delete_backup(&st, &server, &backup).await?;
    ctx.activity("server:backup.delete")
        .subject(morph::BACKUP, backup.id.0)
        .property("name", backup.name.clone())
        .property("failed", !backup.is_successful.0)
        .log(&st)
        .await?;
    Ok(no_content())
}

/// `DeleteBackupService::handle`.
pub async fn delete_backup(st: &AppState, server: &servers::Model, backup: &backups::Model) -> ApiResult<()> {
    let txn = pelican_db::write::begin_immediate(&st.db).await?;
    delete_backup_in(st, &txn, server, backup).await?;
    txn.commit().await?;
    // DeleteBackupService::handle(): $backup->delete() (panel-web callers share this wrapper).
    model_events::deleted(st, backup).await;
    Ok(())
}

/// `DeleteBackupService::handle` inside the caller's transaction.
pub async fn delete_backup_in<C: ConnectionTrait>(
    st: &AppState,
    db: &C,
    server: &servers::Model,
    backup: &backups::Model,
) -> ApiResult<()> {
    if backup.is_locked.0 && backup.is_successful.0 && backup.completed_at.is_some() {
        return Err(ApiError::display(
            "BackupLockedException",
            400,
            "Cannot delete a backup that is marked as locked.",
        ));
    }
    let host = host_of(db, backup).await?;
    if host.schema != "wings" && host.schema != "s3" {
        return Err(ApiError::internal("Exception"));
    }
    let result = if host.schema == "wings" {
        let (_, conn) = node_conn(st, server).await?;
        match st.wings.delete_backup(&conn, &server.uuid, &backup.uuid).await {
            Err(pelican_wings::WingsError::Request { status: 404, .. }) => Ok(()),
            other => other.map_err(wings_err),
        }
    } else {
        let bucket = crate::s3::Bucket::from_host(&host);
        bucket.delete_object(&crate::s3::backup_key(&server.uuid, &backup.uuid)).await
    };
    if let Err(e) = result {
        if backup.is_successful.0 {
            return Err(e);
        }
    }
    backups::Entity::delete_by_id(backup.id).exec(db).await?;
    Ok(())
}
