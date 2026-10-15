//! R11–R13 backup callbacks (`BackupRemoteUploadController`, `BackupStatusController`).

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{backup_hosts, backups, nodes, servers};
use pelican_db::{Bool, Int, SmallBool, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};

use super::{node, node_forbidden};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, php_boolean};
use crate::json::no_content;
use crate::pipeline::Ctx;
use crate::services::LogActivity;
use crate::validation::Validator;

async fn find_backup(st: &AppState, uuid: &str) -> ApiResult<(backups::Model, servers::Model)> {
    let b = backups::Entity::find()
        .filter(backups::Column::Uuid.eq(uuid))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    // `$model->server` on a dangling FK would be null → ErrorException.
    let s = servers::Entity::find_by_id(b.server_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    Ok((b, s))
}

/// R11 `GET /backups/{backup}?size=`.
pub async fn upload_parts(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    let size = input.get("size").map(|v| pelican_core::php::intval(&pelican_core::php::string(v))).unwrap_or(0);
    if size == 0 {
        return Err(ApiError::bad_request("A non-empty \"size\" query parameter must be provided."));
    }
    let uuid = ctx.bound.param("backup").unwrap_or_default().to_string();
    let (backup, server) = find_reported_backup(&st, &uuid).await?;
    let node = node(&ctx)?;
    if server.node_id != node.id {
        return Err(node_forbidden());
    }
    if backup.completed_at.is_some() {
        return Err(ApiError::new(
            409,
            "ConflictHttpException",
            "This backup is already in a completed state.",
            crate::error::Style::Pretty,
        ));
    }
    let host = backup_hosts::Entity::find_by_id(backup.backup_host_id).one(&st.db).await?;
    let Some(host) = host else { return Err(ApiError::internal("ErrorException")) };
    if host.schema != "s3" {
        return Err(ApiError::bad_request("The configured backup schema is not an S3 compatible."));
    }
    // S3BackupSchema::getUploadParts().
    let bucket = crate::s3::Bucket::from_host(&host);
    let key = crate::s3::backup_key(&server.uuid, &backup.uuid);
    let upload_id = bucket.create_multipart_upload(&key).await?;
    let lifespan = st.config.env.int_or("BACKUP_PRESIGNED_URL_LIFESPAN", 60);
    let mut max_part = st.config.env.int_or("BACKUP_MAX_PART_SIZE", crate::s3::DEFAULT_MAX_PART_SIZE);
    if !(crate::s3::MIN_MAX_PART_SIZE..=crate::s3::MAX_MAX_PART_SIZE).contains(&max_part) {
        max_part = crate::s3::DEFAULT_MAX_PART_SIZE;
    }
    let now = chrono::Utc::now();
    // `for ($i = 0; $i < ($size / $maxPartSize); $i++)` with float division.
    let count = (size as f64 / max_part as f64).ceil() as i64;
    let parts: Vec<String> = (1..=count.max(0))
        .map(|n| {
            bucket.presign(
                "PUT",
                &key,
                &[("partNumber", n.to_string()), ("uploadId", upload_id.clone())],
                lifespan * 60,
                now,
            )
        })
        .collect();
    backups::Entity::update_many()
        .col_expr(backups::Column::UploadId, Expr::value(Some(upload_id)))
        .col_expr(backups::Column::UpdatedAt, Expr::value(Timestamp(pelican_core::time::now())))
        .filter(backups::Column::Id.eq(backup.id))
        .exec(&st.db)
        .await?;
    // S3BackupSchema::backupFrom Wings path: $backup->update(['upload_id' => ...]).
    crate::model_events::fire_id::<backups::Entity>(&st, crate::model_events::Action::Updated, backup.id.0).await;
    let concurrent = st.config.env.int_or("BACKUP_MAX_CONCURRENT_UPLOADS", crate::s3::MAX_CONCURRENT_UPLOADS);
    Ok(crate::json::PhpJson::ok(serde_json::json!({
        "parts": parts,
        "part_size": max_part,
        "max_concurrent_uploads": concurrent,
    }))
    .into_response())
}

/// The backup Wings uploads or reports on, waiting briefly for it to become visible: Wings answers
/// `create_backup` before the panel commits the row, and a small server's backup can be
/// done (and reported) before that commit. A 404 makes Wings delete the archive. PHP's
/// slower request start-up hides the same race in the original.
async fn find_reported_backup(st: &AppState, uuid: &str) -> ApiResult<(backups::Model, servers::Model)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match find_backup(st, uuid).await {
            Err(e) if e.status == axum::http::StatusCode::NOT_FOUND && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            other => return other,
        }
    }
}

/// R12 `POST /backups/{backup}`.
pub async fn status(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let mut v = Validator::new(&input);
    v.rules("successful", &["required", "boolean"]);
    v.rules("checksum", &["nullable", "string", "required_if:successful,true"]);
    v.rules("checksum_type", &["nullable", "string", "required_if:successful,true"]);
    v.rules("size", &["nullable", "numeric", "required_if:successful,true"]);
    v.rules("parts", &["nullable", "array"]);
    v.rules("parts.*.etag", &["required", "string"]);
    v.rules("parts.*.part_number", &["required", "numeric"]);
    v.validate()?;
    let node = node(&ctx)?;
    let uuid = ctx.bound.param("backup").unwrap_or_default().to_string();
    let (backup, server) = find_reported_backup(&st, &uuid).await?;
    if server.node_id != node.id {
        return Err(node_forbidden());
    }
    if backup.is_successful.0 {
        return Err(ApiError::bad_request("Cannot update the status of a backup that is already marked as completed."));
    }
    let successful = php_boolean(input.get("successful"));
    let event = if successful { "server:backup.complete" } else { "server:backup.fail" };
    let host = backup_hosts::Entity::find_by_id(backup.backup_host_id).one(&st.db).await?;
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    let checksum = if successful {
        Some(format!(
            "{}:{}",
            input.get("checksum_type").map(pelican_core::php::string).unwrap_or_default(),
            input.get("checksum").map(pelican_core::php::string).unwrap_or_default()
        ))
    } else {
        None
    };
    let bytes: i64 = if successful {
        input.get("size").map(|v| pelican_core::php::string(v).parse::<f64>().unwrap_or(0.0) as i64).unwrap_or(0)
    } else {
        0
    };
    let is_locked = if successful { backup.is_locked.0 } else { false };
    // Eloquent fill()->save(): only dirty columns + updated_at.
    let mut q = backups::Entity::update_many().filter(backups::Column::Id.eq(backup.id));
    if backup.is_successful.0 != successful {
        q = q.col_expr(backups::Column::IsSuccessful, Expr::value(Bool(successful)));
    }
    if backup.is_locked.0 != is_locked {
        q = q.col_expr(backups::Column::IsLocked, Expr::value(SmallBool(is_locked)));
    }
    if backup.checksum != checksum {
        q = q.col_expr(backups::Column::Checksum, Expr::value(checksum.clone()));
    }
    if backup.bytes.0 != bytes {
        q = q.col_expr(backups::Column::Bytes, Expr::value(Int(bytes)));
    }
    q.col_expr(backups::Column::CompletedAt, Expr::value(Some(now)))
        .col_expr(backups::Column::UpdatedAt, Expr::value(now))
        .exec(&txn)
        .await?;
    // `$model->backupHost->schema` on a missing host → ErrorException (rolled back).
    let Some(host) = host else { return Err(ApiError::internal("ErrorException")) };
    if host.schema == "s3" {
        // S3BackupSchema::completeMultipartUpload().
        match backup.upload_id.as_deref().filter(|u| !u.is_empty()) {
            None if successful => return Err(ApiError::internal("Exception")),
            None => {}
            Some(upload_id) => {
                let bucket = crate::s3::Bucket::from_host(&host);
                let key = crate::s3::backup_key(&server.uuid, &backup.uuid);
                if !successful {
                    bucket.abort_multipart_upload(&key, upload_id).await?;
                } else {
                    let parts = match input.get("parts").filter(|p| !p.is_null()) {
                        Some(list) => list
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|p| {
                                        let n = p.get("part_number").map(pelican_core::php::string).unwrap_or_default();
                                        let etag = p.get("etag").map(pelican_core::php::string).unwrap_or_default();
                                        (pelican_core::php::intval(&n), etag)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        None => bucket.list_parts(&key, upload_id).await?,
                    };
                    bucket.complete_multipart_upload(&key, upload_id, &parts).await?;
                }
            }
        }
    }
    ctx.activity(event)
        .subject(morph::BACKUP, backup.id.0)
        .subject(morph::SERVER, server.id.0)
        .property("name", backup.name.clone())
        .log_in(&st, &txn)
        .await?;
    txn.commit().await?;
    // BackupStatusController: $model->fill([...])->save() inside the txn (always
    // dirty — completed_at is set to a fresh now()).
    crate::model_events::fire_id::<backups::Entity>(&st, crate::model_events::Action::Updated, backup.id.0).await;
    let fresh = backups::Entity::find_by_id(backup.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    pelican_core::events::backup_completed(&st.db, &fresh, &server, &ctx.root).await?;
    Ok(no_content())
}

/// R13 `POST /backups/{backup}/restore`.
pub async fn restore(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let uuid = ctx.bound.param("backup").unwrap_or_default().to_string();
    let (backup, server) = find_backup(&st, &uuid).await?;
    let node = node(&ctx)?;
    let server_node = nodes::Entity::find_by_id(server.node_id).one(&st.db).await?;
    if server_node.map(|n| n.id) != Some(node.id) {
        return Err(node_forbidden());
    }
    if server.status.is_some() {
        let now = Timestamp(pelican_core::time::now());
        servers::Entity::update_many()
            .col_expr(servers::Column::Status, Expr::value(Option::<String>::None))
            .col_expr(servers::Column::UpdatedAt, Expr::value(now))
            .filter(servers::Column::Id.eq(server.id))
            .exec(&st.db)
            .await?;
        // BackupStatusController::restore: $model->server->update(['status' => null]).
        crate::model_events::fire_id::<servers::Entity>(&st, crate::model_events::Action::Updated, server.id.0).await;
    }
    // The failure event name contains a `.` instead of `:` in the original.
    let event = if php_boolean(input.get("successful")) {
        "server:backup.restore-complete"
    } else {
        "server.backup.restore-failed"
    };
    ctx.activity(event)
        .subject(morph::BACKUP, backup.id.0)
        .subject(morph::SERVER, server.id.0)
        .property("name", backup.name.clone())
        .log(&st)
        .await?;
    Ok(no_content())
}
