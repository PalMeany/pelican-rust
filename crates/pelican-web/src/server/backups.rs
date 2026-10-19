//! Server panel › Backups (app/Filament/Server/Resources/Backups):
//! list, create (`InitiateBackupService`), rename / lock / download /
//! restore / delete — reusing the Client API logic of
//! `pelican_api::client::backups`.

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::Utc;
use minijinja::{Value as Mj, context};
use pelican_api::model_events;
use pelican_api::pipeline::Ctx;
use pelican_api::services::LogActivity;
use pelican_api::validation::Validator;
use pelican_api::{binding::Bound, client::backups as api};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{backup_hosts, backups, nodes, server_user_settings, servers};
use pelican_db::raw;
use pelican_db::{Int, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Value, json};

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};
use crate::views::{bytes, diff_for_humans};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/backups", get(index).post(create))
        .route("/server/{server}/backups/notifications", post(save_notifications))
        .route("/server/{server}/backups/{id}/rename", get(open_rename).post(rename))
        .route("/server/{server}/backups/{id}/restore", get(open_restore).post(restore))
        .route("/server/{server}/backups/{id}/lock", post(lock))
        .route("/server/{server}/backups/{id}/download", get(download))
        .route("/server/{server}/backups/{id}/delete", post(delete))
}

#[derive(FromQueryResult)]
struct Aggregate {
    aggregate: Int,
}

/// A client-API `Ctx` for domain functions and activity logging: ip and actor
/// like this web session, default subject = the server (`ServerSubject`).
fn api_ctx(p: &ServerPanel) -> Result<Ctx, WebError> {
    let ip: IpAddr = p.ip.parse().map_err(|_| WebError::internal("invalid request ip"))?;
    Ok(Ctx {
        ip,
        user: Some(Arc::new(p.user.clone())),
        bound: Arc::new(Bound { server: Some(p.server.clone()), ..Default::default() }),
        node: None,
        root: p.st.config.app_url.clone(),
        subject: Some((morph::SERVER, p.server.id.0)),
    })
}

/// A backup row scoped to the panel's server.
async fn find(p: &ServerPanel, id: i64) -> Result<backups::Model, WebError> {
    backups::Entity::find_by_id(Int(id))
        .filter(backups::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// "successful only" visibility of rename/lock/download/restore.
fn is_successful(b: &backups::Model) -> bool {
    b.is_successful.0 && b.completed_at.is_some()
}

/// True while a backup has not completed yet.
fn pending(b: &backups::Model) -> bool {
    b.completed_at.is_none()
}

/// PHP binds booleans as 1 for PostgreSQL boolean columns (api/client/backups.rs).
fn bool_val(backend: sea_orm::DbBackend) -> sea_orm::Value {
    if backend == sea_orm::DbBackend::Postgres { true.into() } else { 1.into() }
}

/// Count of non-failed backups — the number the backup limit applies to
/// (`InitiateBackupService`).
async fn non_failed_count<C: ConnectionTrait>(db: &C, server_id: i64) -> Result<i64, sea_orm::DbErr> {
    let b = db.get_database_backend();
    let sql = "select count(*) as {aggregate} from {backups} where {backups.server_id} = ? \
               and ({completed_at} is null or {is_successful} = ?)";
    Ok(Aggregate::find_by_statement(raw::stmt(b, sql, vec![server_id.into(), bool_val(b)]))
        .one(db)
        .await?
        .map(|a| a.aggregate.0.max(0))
        .unwrap_or(0))
}

/// Backup host behind a record; a missing host is an internal error
/// (`ErrorException` in the original).
async fn host_of<C: ConnectionTrait>(db: &C, id: Int) -> Result<backup_hosts::Model, WebError> {
    backup_hosts::Entity::find_by_id(id).one(db).await?.ok_or_else(|| WebError::internal("ErrorException"))
}

/// Searchable/sortable list SQL over the server's backups (+ total count).
async fn rows(p: &ServerPanel, lp: &ListParams) -> Result<(Vec<backups::Model>, u64), WebError> {
    let b = p.st.db.get_database_backend();
    let order = match lp.sort.as_deref() {
        Some("created_at") => format!("{{backups.created_at}} {}", if lp.desc { "desc" } else { "asc" }),
        // No default sort: Filament's key sort.
        _ => "{backups.id} asc".to_string(),
    };
    let mut where_sql = "where {backups.server_id} = ?".to_string();
    let mut vals = vec![p.server.id.into()];
    if !lp.search.is_empty() {
        where_sql.push_str(" and LOWER({name}) LIKE ?");
        vals.push(format!("%{}%", lp.search.to_lowercase()).into());
    }
    let mut sql = format!("select {{backups}}.* from {{backups}} {where_sql} order by {order}");
    if let Some((limit, offset)) = lp.limit_offset() {
        sql.push_str(&format!(" limit {limit} offset {offset}"));
    }
    let found = backups::Model::find_by_statement(raw::stmt(b, &sql, vals.clone())).all(&p.st.db).await?;
    let count_sql = format!("select count(*) as {{aggregate}} from {{backups}} {where_sql}");
    let total = Aggregate::find_by_statement(raw::stmt(b, &count_sql, vals))
        .one(&p.st.db)
        .await?
        .map(|a| a.aggregate.0.max(0) as u64)
        .unwrap_or(0);
    Ok((found, total))
}

/// Current user's notification toggles for this server
/// (`ServerUserSettingKey`, default false).
async fn notification_settings(p: &ServerPanel) -> Result<(bool, bool), WebError> {
    let s = server_user_settings::Entity::find()
        .filter(server_user_settings::Column::UserId.eq(p.user.id()))
        .filter(server_user_settings::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?;
    let get = |key: &str| {
        s.as_ref()
            .and_then(|s| s.settings.as_ref())
            .and_then(|j| j.0.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Ok((get("manual_backup_notifications"), get("scheduled_backup_notifications")))
}

/// `User::updateServerSetting()` inside the caller's transaction: merge one
/// key, keep only the known keys (`array_intersect_key` against the two
/// defaults), save.
/// Returns ("created", Some(id)) / ("updated", None) / ("clean", None)
/// (Eloquent firstOrNew + save(): a clean model writes nothing and fires nothing).
#[allow(clippy::type_complexity)]
async fn save_setting<C: ConnectionTrait>(
    db: &C,
    user_id: i64,
    server_id: i64,
    key: &str,
    value: bool,
) -> Result<(&'static str, Option<i64>), sea_orm::DbErr> {
    const KNOWN: [&str; 2] = ["manual_backup_notifications", "scheduled_backup_notifications"];
    let existing = server_user_settings::Entity::find()
        .filter(server_user_settings::Column::UserId.eq(user_id))
        .filter(server_user_settings::Column::ServerId.eq(server_id))
        .one(db)
        .await?;
    let mut m =
        existing.as_ref().and_then(|e| e.settings.as_ref()).and_then(|j| j.0.as_object().cloned()).unwrap_or_default();
    let prev_same = m.get(key) == Some(&Value::Bool(value));
    m.insert(key.into(), Value::Bool(value));
    m.retain(|k, _| KNOWN.contains(&k.as_str()));
    if prev_same {
        return Ok(("clean", None));
    }
    let now = Timestamp(pelican_core::time::now());
    match existing {
        Some(e) => {
            let am = server_user_settings::ActiveModel {
                id: Set(e.id),
                settings: Set(Some(pelican_db::Json(Value::Object(m)))),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            pelican_db::write::update(db, &am).await?;
            Ok(("updated", Some(e.id.0)))
        }
        None => {
            let am = server_user_settings::ActiveModel {
                user_id: Set(Int(user_id)),
                server_id: Set(Int(server_id)),
                settings: Set(Some(pelican_db::Json(Value::Object(m)))),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            let new_id = pelican_db::write::insert_id(db, &am).await?.unwrap_or(0);
            Ok(("created", Some(new_id)))
        }
    }
}

/// First `errors[0].detail` of an error object list, for notification bodies.
fn first_detail(details: Option<&[pelican_api::error::ErrorObject]>) -> Option<String> {
    details.and_then(|d| d.first()).map(|o| o.detail.clone()).filter(|d| !d.is_empty())
}

/// PHP truthiness of a toggle posting `0`/`1`.
fn php_bool(v: Option<&str>) -> bool {
    v.is_some_and(|v| !matches!(v, "" | "0" | "false" | "off"))
}

/// Values + open-dialog state carried through re-rendered list pages.
struct ViewState {
    create_name: String,
    create_errors: Errors,
    create_open: bool,
    rename_value: String,
    rename_errors: Errors,
    rename_open: Option<i64>,
    restore_errors: Errors,
    restore_open: Option<i64>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            create_name: String::new(),
            create_errors: Errors::default(),
            create_open: false,
            rename_value: String::new(),
            rename_errors: Errors::default(),
            rename_open: None,
            restore_errors: Errors::default(),
            restore_open: None,
        }
    }
}

async fn render_list(p: ServerPanel, lp: ListParams, view: ViewState) -> WebResult<Response> {
    let base = p.base_url();
    let opts = [10, 25, 50, 0];
    let (models, total) = rows(&p, &lp).await?;
    let non_failed = non_failed_count(&p.st.db, p.server.id.0).await?;

    // Toolbar CreateAction: disabled red with its tooltip at the limit; a
    // limit of 0 always disables it (`count >= backup_limit`).
    let at_limit = p.server.backup_limit.0 == 0 || non_failed >= p.server.backup_limit.0;

    let can_delete = p.can("backup.delete");
    let locale = p.locale();
    let t = |k: &str| p.t(&format!("server/backup.{k}"));

    let mut table = Table::new("backups", &format!("{base}/backups"));
    table.columns = vec![
        Column::new("name", t("actions.create.name")),
        Column::new("size", t("size")),
        Column::new("created_at", t("created_at")).sortable(),
        Column::new("status", t("status")),
        Column::new("is_locked", t("is_locked")).wide_only(),
    ];
    let download_label = t("actions.download");
    for b in &models {
        // BackupStatus: label, colour and icon of the badge.
        let (label, color, icon) = if pending(b) {
            (t("backup_status.in_progress"), "primary", "circle-dashed")
        } else if b.is_successful.0 {
            (t("backup_status.successful"), "success", "circle-check")
        } else {
            (t("backup_status.failed"), "danger", "circle-x")
        };
        let status =
            Cell::Badge { text: label, color: color.into(), icon: Some(icon.into()), tooltip: None, copy: None };
        let created =
            diff_for_humans(&locale, b.created_at.map(|c| c.0.and_utc()).unwrap_or_else(Utc::now), Utc::now());
        let mut row = Row::new(b.id.0.to_string())
            .cell("name", Cell::text(b.name.clone()))
            .cell("size", Cell::text(bytes(b.bytes.0, p.st.config.use_binary_prefix)))
            .cell("created_at", Cell::text(created))
            .cell("status", status)
            .cell(
                "is_locked",
                // A boolean IconColumn: success when true, danger when false.
                Cell::icon(
                    if b.is_locked.0 { "lock" } else { "lock-open" },
                    if b.is_locked.0 { "success" } else { "danger" },
                ),
            );

        // One action group per row (BackupsResource::defaultTable actions).
        let successful = is_successful(b);
        if can_delete && successful {
            row.group.push(Action::link(
                t("actions.rename.title"),
                "edit",
                format!("{base}/backups/{}/rename", b.id.0),
            ));
            row.group.push(Action::post(
                if b.is_locked.0 { t("actions.lock.unlock") } else { t("actions.lock.lock") },
                if b.is_locked.0 { "lock-open" } else { "lock" },
                format!("{base}/backups/{}/lock", b.id.0),
            ));
        }
        if p.can("backup.download") && successful {
            row.group.push(Action::link(
                download_label.clone(),
                "download",
                format!("{base}/backups/{}/download", b.id.0),
            ));
        }
        if p.can("backup.restore") && successful {
            row.group.push(Action::link(
                t("actions.restore.title"),
                "reload",
                format!("{base}/backups/{}/restore", b.id.0),
            ));
        }
        if can_delete && !pending(b) {
            // A locked successful backup is refused by DeleteBackupService
            // (the original disables the button there); the POST keeps the
            // proper failure notification.
            let mut a = Action::post(
                t("filament-actions::delete.single.label"),
                "trash",
                format!("{base}/backups/{}/delete", b.id.0),
            )
            .color("danger")
            .confirm(p.t_with("server/backup.actions.delete.description", &[("backup", &b.name)]));
            if b.is_locked.0 && b.is_successful.0 {
                a = a.color("gray");
            }
            row.group.push(a);
        }
        table.rows.push(row);
    }
    table.empty_icon = "file-zip".into();
    table.empty_heading = t("empty");
    table.searchable = true;
    table.finish(&lp, total, &opts, &locale);

    let (manual, scheduled) = notification_settings(&p).await?;

    // Per-row action dialogs: rename + restore, the one requested through
    // GET or re-rendered after validation errors carrying `open`.
    let mut rename_dialogs = Vec::new();
    let mut restore_dialogs = Vec::new();
    for b in &models {
        if can_delete && is_successful(b) {
            let open = view.rename_open == Some(b.id.0);
            rename_dialogs.push(json!({
                "id": b.id.0,
                "name": b.name,
                "value": if open { view.rename_value.clone() } else { b.name.clone() },
                "error": open.then(|| view.rename_errors.0.get("name").cloned()),
                "open": open,
            }));
        }
        if p.can("backup.restore") && is_successful(b) {
            restore_dialogs.push(json!({
                "id": b.id.0,
                "name": b.name,
                "error":
                    (view.restore_open == Some(b.id.0)).then(|| view.restore_errors.0.get("truncate").cloned()),
                "open": view.restore_open == Some(b.id.0),
            }));
        }
    }

    p.page(
        "backups",
        "server/backups.html",
        context! {
            title => p.t("server/backup.title"),
            base => base,
            table => Mj::from_serialize(&table),
            at_limit => at_limit,
            can_create => p.can("backup.create"),
            v_name => view.create_name,
            v_errors => Mj::from_serialize(&view.create_errors),
            create_open => view.create_open || !view.create_errors.is_empty(),
            locked_field => can_delete,
            manual => manual,
            scheduled => scheduled,
            rename_dialogs => rename_dialogs,
            restore_dialogs => restore_dialogs,
        },
    )
}

/// The list state of the current request (before the panel moves into a
/// handler).
fn parse_lp(q: Option<&str>) -> ListParams {
    ListParams::parse(q, &[10, 25, 50, 0], 10)
}

/// `GET /server/{server}/backups`.
async fn index(p: ServerPanel) -> WebResult<Response> {
    p.authorize("backup.read")?;
    let lp = parse_lp(p.query.as_deref());
    render_list(p, lp, ViewState::default()).await
}

/// Name validation of the Client API store request (model rule max 255).
fn validate_create(p: &ServerPanel, name: &str) -> Errors {
    let input = json!({ "name": if name.is_empty() { Value::Null } else { Value::from(name) } });
    let mut v = Validator::for_value(&input);
    v.attribute_name("name", &p.t("server/backup.actions.create.name").to_lowercase());
    v.rules("name", &["nullable", "string", "max:255"]);
    v.run();
    Errors::from_validator(&v)
}

/// `POST …/backups` — toolbar CreateAction (Client API `store`): validation,
// `lock_server`, `initiate_backup` and the `server:backup.start` activity.
async fn create(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("backup.create")?;
    let base = p.base_url();
    let list_url = format!("{base}/backups");

    // Laravel does not trim; empty values become null, the service trims.
    let name = form.opt("name").unwrap_or("").to_string();
    let errors = validate_create(&p, &name);
    if !errors.is_empty() {
        let lp = parse_lp(p.query.as_deref());
        return render_list(
            p,
            lp,
            ViewState { create_name: name, create_errors: errors, create_open: true, ..Default::default() },
        )
        .await;
    }

    let ignored: Vec<String> = form.get("ignored").split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect();
    let posted_lock = php_bool(form.opt("locked"));
    let is_locked = p.can("backup.delete") && posted_lock;

    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let server = p.server.clone();
    let opts = api::InitiateBackup {
        name: (!name.is_empty()).then_some(name),
        ignored,
        is_locked,
        is_scheduled: false,
        override_limit: false,
    };
    let result: Result<backups::Model, pelican_api::error::ApiError> = async {
        let txn = pelican_db::write::begin_immediate(&st.db).await?;
        api::lock_server(&txn, server.id.0).await?;
        let backup = api::initiate_backup(&st, &txn, &server, opts).await?;
        ctx.activity("server:backup.start")
            .subject(morph::BACKUP, backup.id.0)
            .property("name", backup.name.clone())
            .property("locked", posted_lock)
            .log_in(&st, &txn)
            .await?;
        txn.commit().await?;
        // InitiateBackupService: `Backup::create([...])`.
        model_events::created(&st, &backup).await;
        Ok(backup)
    }
    .await;

    match result {
        Ok(backup) => p.session.flash(
            "success",
            p.t("server/backup.actions.create.notification_success"),
            Some(p.t_with("server/backup.actions.create.created", &[("name", &backup.name)])),
        ),
        Err(e) => {
            tracing::warn!(error=?e, "backup creation failed");
            // Throttle (429): same text plus Retry-After like the original UI.
            let retry = e
                .headers
                .iter()
                .find(|(k, _)| k.as_str() == "retry-after")
                .and_then(|(_, v)| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            let body = retry.map(|n| {
                format!(
                    "{} Try again in {n} seconds.",
                    e.errors.first().map(|o| o.detail.trim_end_matches('.')).unwrap_or_default()
                )
            });
            p.flash_danger(
                p.t("server/backup.actions.create.notification_fail"),
                body.or_else(|| first_detail(Some(&e.errors))),
            );
        }
    }
    Ok(p.redirect(&list_url))
}

/// `POST …/notifications` — header Notification Settings action: two toggles
/// written via `User::updateServerSetting()` semantics. No dedicated subuser
/// permission beyond reaching the page (`backup.read`).
async fn save_notifications(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("backup.read")?;
    let list_url = format!("{}/backups", p.base_url());
    let manual = php_bool(form.opt("toggle_manual"));
    let scheduled = php_bool(form.opt("toggle_scheduled"));
    let uid = p.user.id();
    let sid = p.server.id.0;
    let result = async {
        let txn = p.st.db.begin().await?;
        // `updateServerSetting()` per key: firstOrNew + `$row->save()` → created for a new
        // row, updated when the key changed, nothing for a clean save.
        let mut events: Vec<(model_events::Action, i64)> = Vec::new();
        for (key, val) in [("manual_backup_notifications", manual), ("scheduled_backup_notifications", scheduled)] {
            match save_setting(&txn, uid, sid, key, val).await? {
                ("created", Some(id)) => events.push((model_events::Action::Created, id)),
                ("updated", Some(id)) => events.push((model_events::Action::Updated, id)),
                _ => {}
            }
        }
        txn.commit().await?;
        for (action, id) in events {
            model_events::fire_id::<server_user_settings::Entity>(&p.st, action, id).await;
        }
        Ok::<(), sea_orm::DbErr>(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(p.t("server/backup.notifications.saved")),
        Err(e) => {
            tracing::warn!(error=?e, "notification settings save failed");
            p.flash_danger(p.t("filament-panels::database.exceptions.connection"), None);
        }
    }
    Ok(p.redirect(&form.back(&list_url)))
}

/// Activity builder with the signed-in web user as actor, subject = server +
/// record morph — same shape the Client API handlers produce.
fn ctx_activity(p: &ServerPanel, event: &str) -> pelican_core::activity::Activity {
    p.activity_for_server(event)
}

/// Log an activity entry, API errors surfaced through the web error type.
async fn log_act(a: pelican_core::activity::Activity, st: &AppState) -> Result<(), WebError> {
    a.log(st)
        .await
        .map(|_| ())
        .map_err(|e| WebError::internal(e.errors.first().map(|o| o.detail.as_str()).unwrap_or("activity log failed")))
}

/// GET half of the Rename dialog: renders the list page with that dialog
/// opened (EditAction-style entry point, cf. `server/users.rs`).
async fn open_rename(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("backup.delete")?;
    let b = find(&p, id).await?;
    if !is_successful(&b) {
        return Err(WebError::NotFound);
    }
    let value = b.name.clone();
    let lp = parse_lp(p.query.as_deref());
    render_list(p, lp, ViewState { rename_open: Some(id), rename_value: value, ..Default::default() }).await
}

/// POST half: Client API `rename` rules and activity as a form action.
async fn rename(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("backup.delete")?;
    let b = find(&p, id).await?;
    let back = form.back(&format!("{}/backups", p.base_url()));

    let name = form.opt("name").unwrap_or("").to_string();
    let input = json!({ "name": if name.is_empty() { Value::Null } else { Value::from(name.clone()) } });
    let mut v = Validator::for_value(&input);
    v.attribute_name("name", &p.t("server/backup.actions.rename.new_name").to_lowercase());
    v.rules("name", &["required", "string", "max:255"]);
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        let lp = parse_lp(p.query.as_deref());
        return render_list(
            p,
            lp,
            ViewState { rename_open: Some(id), rename_value: name, rename_errors: errors, ..Default::default() },
        )
        .await;
    }

    let old = b.name.clone();
    if old != name {
        let now = Timestamp(pelican_core::time::now());
        // Client API `rename` → `$backup->update(['name' => …])` (updated).
        backups::Entity::update_many()
            .col_expr(backups::Column::Name, Expr::value(name.clone()))
            .col_expr(backups::Column::UpdatedAt, Expr::value(now))
            .filter(backups::Column::Id.eq(b.id))
            .exec(&p.st.db)
            .await?;
        model_events::fire_id::<backups::Entity>(&p.st, model_events::Action::Updated, id).await;
        log_act(
            ctx_activity(&p, "server:backup.rename").property("old_name", old).property("new_name", name.clone()),
            &p.st,
        )
        .await?;
    }
    p.flash_success(p.t("server/backup.actions.rename.notification_success"));
    Ok(p.redirect(&back))
}

/// GET half of the Restore dialog.
async fn open_restore(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("backup.restore")?;
    let b = find(&p, id).await?;
    if !is_successful(&b) {
        return Err(WebError::NotFound);
    }
    let lp = parse_lp(p.query.as_deref());
    render_list(p, lp, ViewState { restore_open: Some(id), ..Default::default() }).await
}

/// Adapter download URL minted at click time (`api::download` mapping:
/// wings → signed link, s3 unsupported, unknown schema → bad request).
async fn adapter_download_url(p: &ServerPanel, ctx: &Ctx, b: &backups::Model) -> Result<String, String> {
    // Missing host rows are internal errors (`ErrorException`).
    let host = match host_of(&p.st.db, b.backup_host_id).await {
        Ok(h) => h,
        Err(WebError::Internal(m)) => return Err(m),
        Err(_) => return Err("ErrorException".into()),
    };
    match host.schema.as_str() {
        "wings" => api::download_link(&p.st, ctx, &p.server, b).await.map_err(|e| {
            e.errors
                .first()
                .map(|o| o.detail.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| "download failed".into())
        }),
        "s3" => Err("S3Exception".into()),
        _ => Err("The backup requested references an unknown disk driver type and cannot be downloaded.".into()),
    }
}

/// POST half: Client API `restore` — status flips to `restoring_backup` and
/// the Wings call run in one transaction.
async fn restore(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("backup.restore")?;
    let b = find(&p, id).await?;
    let back = form.back(&format!("{}/backups", p.base_url()));
    let t = |k: &str| p.t(&format!("server/backup.actions.restore.{k}"));

    if p.server.status.is_some() {
        p.flash_danger(t("notification_fail"), Some(t("notification_fail_body_1")));
        return Ok(p.redirect(&back));
    }
    if !b.is_successful.0 && b.completed_at.is_none() {
        p.flash_danger(t("notification_fail"), Some(t("notification_fail_body_2")));
        return Ok(p.redirect(&back));
    }

    let ctx = api_ctx(&p)?;
    let url = adapter_download_url(&p, &ctx, &b).await;

    let truncate_posted = form.opt("truncate");
    let truncate_raw = truncate_posted.map(|v| json!(v)).unwrap_or(Value::Null);
    let input = json!({ "truncate": truncate_raw });
    let mut v = Validator::for_value(&input);
    v.attribute_name("truncate", &t("delete_all").to_lowercase());
    v.rules("truncate", &["required", "boolean"]);
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        let lp = parse_lp(p.query.as_deref());
        return render_list(p, lp, ViewState { restore_open: Some(id), restore_errors: errors, ..Default::default() })
            .await;
    }

    match url {
        Ok(url) => {
            let truncate = php_bool(truncate_posted);
            let st = p.st.clone();
            let server = p.server.clone();
            let schema = host_of(&st.db, b.backup_host_id).await?.schema;
            let backup_uuid = b.uuid.clone();
            let name = b.name.clone();
            let subject_id = b.id.0;
            let result: Result<(), pelican_api::error::ApiError> = async {
                let txn = st.db.begin().await?;
                servers_touch_restoring(&txn, server.id.0).await?;
                let node = nodes::Entity::find_by_id(server.node_id)
                    .one(&st.db)
                    .await?
                    .ok_or_else(|| pelican_api::error::ApiError::internal("ErrorException"))?;
                let conn =
                    st.node_conn(&node).map_err(|_| pelican_api::error::ApiError::internal("DecryptException"))?;
                st.wings
                    .restore_backup(&conn, &server.uuid, &backup_uuid, &schema, truncate, &url)
                    .await
                    .map_err(|e| pelican_api::error::ApiError::internal(e.code()))?;
                ctx.activity("server:backup.restore")
                    .subject(morph::BACKUP, subject_id)
                    .property("name", name.clone())
                    .property("truncate", truncate_raw.clone())
                    .log_in(&st, &txn)
                    .await?;
                txn.commit().await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    // Client API `restore` → `$server->update(['status' => …RestoringBackup])`.
                    if p.server.status.as_deref() != Some("restoring_backup") {
                        model_events::fire_id::<servers::Entity>(&p.st, model_events::Action::Updated, p.server.id.0)
                            .await;
                    }
                    p.flash_success(t("notification_started"));
                }
                Err(e) => {
                    tracing::warn!(error=?e, "backup restore failed");
                    p.flash_danger(t("notification_fail"), first_detail(Some(&e.errors)));
                }
            }
        }
        Err(detail) => {
            tracing::warn!("backup restore url minting failed: {detail}");
            p.flash_danger(t("notification_fail"), Some(detail));
        }
    }
    Ok(p.redirect(&back))
}

/// `servers.status = restoring_backup` inside the caller's transaction
/// (api/client/backups.rs `restore`).
async fn servers_touch_restoring<C: ConnectionTrait>(db: &C, id: i64) -> Result<(), sea_orm::DbErr> {
    use pelican_db::entities::servers;
    let now = Timestamp(pelican_core::time::now());
    servers::Entity::update_many()
        .col_expr(servers::Column::Status, Expr::value(Some("restoring_backup".to_string())))
        .col_expr(servers::Column::UpdatedAt, Expr::value(now))
        .filter(servers::Column::Id.eq(Int(id)))
        .exec(db)
        .await?;
    Ok(())
}

/// `GET …/download` — minted `DownloadAction` URL; the activity matches what
/// the Client API logs for downloads (`server:backup.download`).
async fn download(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("backup.download")?;
    let b = find(&p, id).await?;
    if !is_successful(&b) {
        return Err(WebError::NotFound);
    }
    let ctx = api_ctx(&p)?;
    match adapter_download_url(&p, &ctx, &b).await {
        Ok(url) => {
            log_act(
                ctx.activity("server:backup.download").subject(morph::BACKUP, b.id.0).property("name", b.name.clone()),
                &p.st,
            )
            .await?;
            Ok(Redirect::to(&url).into_response())
        }
        Err(detail) => {
            tracing::warn!("backup download url minting failed: {detail}");
            p.flash_danger(detail, None);
            Ok(p.redirect(&format!("{}/backups", p.base_url())))
        }
    }
}

/// `POST …/lock` — `BackupController::toggleLock` semantics (`api::lock`):
/// toggles `is_locked`, event per direction.
async fn lock(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("backup.delete")?;
    let b = find(&p, id).await?;
    let event = if b.is_locked.0 { "server:backup.unlock" } else { "server:backup.lock" };
    let now = Timestamp(pelican_core::time::now());
    // Client API `toggleLock` → `$backup->update(['is_locked' => …])` (updated).
    backups::Entity::update_many()
        .col_expr(backups::Column::IsLocked, Expr::value(!b.is_locked.0))
        .col_expr(backups::Column::UpdatedAt, Expr::value(now))
        .filter(backups::Column::Id.eq(b.id))
        .exec(&p.st.db)
        .await?;
    model_events::fire_id::<backups::Entity>(&p.st, model_events::Action::Updated, id).await;
    log_act(ctx_activity(&p, event).property("name", b.name.clone()), &p.st).await?;
    let _ = form;
    Ok(p.redirect(&form.back(&format!("{}/backups", p.base_url()))))
}

/// `POST …/delete` — `DeleteAction` via `DeleteBackupService`
/// (`api::delete_backup`), then the `server:backup.delete` activity.
async fn delete(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("backup.delete")?;
    let b = find(&p, id).await?;
    let list_url = format!("{}/backups", p.base_url());
    let back = form.back(&list_url);
    let t = |k: &str| p.t(&format!("server/backup.actions.delete.{k}"));

    let ctx = api_ctx(&p)?;
    match api::delete_backup(&p.st, &p.server, &b).await {
        Ok(()) => {
            log_act(
                ctx.activity("server:backup.delete")
                    .subject(morph::BACKUP, b.id.0)
                    .property("name", b.name.clone())
                    .property("failed", !b.is_successful.0),
                &p.st,
            )
            .await?;
            p.flash_success(t("notification_success"));
        }
        Err(e) => {
            tracing::warn!(error=?e, "backup deletion failed");
            // Connection failures have their own explanatory body string.
            let connection = e.errors.iter().any(|o| o.code.contains("ConnectionException"));
            let body = if connection { Some(t("notification_fail_body")) } else { first_detail(Some(&e.errors)) };
            p.flash_danger(t("notification_fail"), body.or_else(|| Some(pelican_api::error::GENERIC_500.into())));
        }
    }
    Ok(p.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_toggle_truthiness() {
        assert!(php_bool(Some("1")));
        assert!(!php_bool(Some("0")));
        assert!(!php_bool(None));
    }

    /// The list template renders with the empty state (no server needed).
    #[test]
    fn list_template_renders() {
        let mut t = Table::new("backups", "/server/x/backups");
        t.finish(&ListParams::default(), 0, &[10, 25, 50, 0], "en");
        let ctx = context! {
            title => "Backups",
            base => "/server/x",
            table => Mj::from_serialize(&t),
            at_limit => false,
            can_create => true,
            v_name => "",
            v_errors => Mj::from_serialize(&Errors::default()),
            create_open => false,
            locked_field => true,
            manual => false,
            scheduled => false,
            rename_dialogs => Vec::<Value>::new(),
            restore_dialogs => Vec::<Value>::new(),
        };
        crate::views::render("server/backups.html", ctx).expect("template renders");
    }
}
