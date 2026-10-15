//! Schedule execution: `ProcessScheduleService` and `RunTaskJob` (ADR-0012).
//!
//! Queue semantics follow `QUEUE_CONNECTION`: `sync` runs a dispatched job
//! inline (delays ignored, exceptions call the job's `failed()` and propagate
//! to the dispatcher, like Laravel's `SyncQueue`); anything else runs it on a
//! background task after its delay, logging failures after `failed()`.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use pelican_core::state::AppState;
use pelican_db::entities::{schedules, servers, tasks};
use pelican_db::{Bool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult, TransactionTrait};
use serde_json::{Value, json};

use crate::client::backups::{InitiateBackup, initiate_backup};
use crate::error::{ApiError, ApiResult};
use crate::model_events::{self, Action};

/// Instance-update events of `$task->update(['is_queued' => v])` (RunTaskJob):
/// `updated(Task)`, then `Task::$touches` saves the parent schedule
/// (`updated(Schedule)` with only its fresh timestamp). Callers outside a
/// transaction invoke this when [`set_queued`] actually wrote.
async fn task_updated_events(st: &AppState, task_id: i64, schedule_id: i64) {
    model_events::fire_id::<tasks::Entity>(st, Action::Updated, task_id).await;
    model_events::fire_id::<schedules::Entity>(st, Action::Updated, schedule_id).await;
}

/// `Utilities::getScheduleNextRunDate()`: evaluated in `timezone`, returned in UTC.
pub fn next_run_date(
    minute: &str,
    hour: &str,
    day_of_month: &str,
    month: &str,
    day_of_week: &str,
    timezone: &str,
) -> Result<chrono::NaiveDateTime, ()> {
    let tz: chrono_tz::Tz = timezone.parse().map_err(|_| ())?;
    let expr = cron_php::CronExpression::new(&format!("{minute} {hour} {day_of_month} {month} {day_of_week}"))
        .map_err(|_| ())?;
    let now = chrono::Utc::now().with_timezone(&tz);
    let next = expr.next_run_date(now).map_err(|_| ())?;
    Ok(next.with_timezone(&chrono::Utc).naive_utc())
}

fn schedule_next_run(s: &schedules::Model) -> ApiResult<chrono::NaiveDateTime> {
    next_run_date(&s.cron_minute, &s.cron_hour, &s.cron_day_of_month, &s.cron_month, &s.cron_day_of_week, &s.timezone)
        .map_err(|_| ApiError::internal("InvalidArgumentException"))
}

fn now_ts() -> Timestamp {
    Timestamp(pelican_core::time::now())
}

/// `$task->update(['is_queued' => $v])`: writes only when dirty, then touches
/// the schedule (`Task::$touches`). Returns whether the instance write happened.
async fn set_queued<C: ConnectionTrait>(db: &C, task: &mut tasks::Model, value: bool) -> ApiResult<bool> {
    if task.is_queued.0 == value {
        return Ok(false);
    }
    let ts = now_ts();
    pelican_db::write::update(
        db,
        &tasks::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(task.id),
            is_queued: Set(Bool(value)),
            updated_at: Set(Some(ts)),
            ..Default::default()
        },
    )
    .await?;
    task.is_queued = Bool(value);
    task.updated_at = Some(ts);
    touch_schedule(db, task.schedule_id.0).await?;
    Ok(true)
}

/// `BelongsTo::touch()`: `update schedules set updated_at = now where id = ?`.
pub async fn touch_schedule<C: ConnectionTrait>(db: &C, schedule_id: i64) -> ApiResult<()> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(
        b,
        "update {schedules} set {updated_at} = ? where {schedules.id} = ?",
        vec![now_ts().into(), schedule_id.into()],
    ))
    .await?;
    Ok(())
}

/// `markScheduleComplete()`: query-builder update (Eloquent adds updated_at).
async fn mark_schedule_complete<C: ConnectionTrait>(db: &C, schedule_id: i64) -> ApiResult<()> {
    let b = db.get_database_backend();
    let now = now_ts();
    db.execute_raw(raw::stmt(
        b,
        "update {schedules} set {is_processing} = ?, {last_run_at} = ?, {updated_at} = ? where {schedules.id} = ?",
        vec![false.into(), now.into(), now.into(), schedule_id.into()],
    ))
    .await?;
    Ok(())
}

/// `RunTaskJob::failed()`: markTaskNotQueued (instance update + touch), then the
/// mass `schedule()->update()` (query builder — no events).
async fn failed(st: &AppState, task: &mut tasks::Model) -> ApiResult<()> {
    if set_queued(&st.db, task, false).await? {
        task_updated_events(st, task.id.0, task.schedule_id.0).await;
    }
    mark_schedule_complete(&st.db, task.schedule_id.0).await
}

async fn first_task<C: ConnectionTrait>(db: &C, schedule_id: i64) -> ApiResult<Option<tasks::Model>> {
    Ok(tasks::Model::find_by_statement(raw::stmt(
        db.get_database_backend(),
        "select * from {tasks} where {tasks.schedule_id} = ? and {tasks.schedule_id} is not null order by {sequence_id} asc limit 1",
        vec![schedule_id.into()],
    ))
    .one(db)
    .await?)
}

/// `ProcessScheduleService::handle($schedule, $now)`.
pub async fn process_schedule(st: &AppState, schedule: &mut schedules::Model, now: bool) -> ApiResult<()> {
    let Some(mut task) = first_task(&st.db, schedule.id.0).await? else {
        return Err(ApiError::display(
            "DisplayException",
            400,
            "Cannot process schedule for task execution: no tasks are registered.",
        ));
    };
    let next = Timestamp(schedule_next_run(schedule)?);
    let txn = st.db.begin().await?;
    // forceFill([...])->saveOrFail(): only dirty attributes (plus updated_at) are written.
    let mut am = schedules::ActiveModel { id: sea_orm::ActiveValue::Unchanged(schedule.id), ..Default::default() };
    let mut dirty = false;
    if !schedule.is_processing.0 {
        am.is_processing = Set(Bool(true));
        dirty = true;
    }
    if schedule.next_run_at.map(|t| t.0.format("%F %T").to_string()) != Some(next.0.format("%F %T").to_string()) {
        am.next_run_at = Set(Some(next));
        dirty = true;
    }
    if dirty {
        let ts = now_ts();
        am.updated_at = Set(Some(ts));
        pelican_db::write::update(&txn, &am).await?;
        schedule.updated_at = Some(ts);
    }
    schedule.is_processing = Bool(true);
    schedule.next_run_at = Some(next);
    let task_queued_now = set_queued(&txn, &mut task, true).await?;
    txn.commit().await?;

    // ProcessScheduleService: $schedule->forceFill([...])->saveOrFail() then
    // $task->update(['is_queued' => true]) (+touch) — instance saves inside the
    // transaction; webhook delivery waits for the commit.
    if dirty {
        model_events::fire_id::<schedules::Entity>(st, Action::Updated, schedule.id.0).await;
    }
    if task_queued_now {
        task_updated_events(st, task.id.0, task.schedule_id.0).await;
    }

    if schedule.only_when_online.0 {
        let state = server_state(st, schedule.server_id.0).await?;
        // ContainerStatus::tryFrom(...) ?? Offline; isOffline() = offline | missing.
        let known = [
            "created",
            "starting",
            "running",
            "restarting",
            "exited",
            "paused",
            "dead",
            "removing",
            "stopping",
            "offline",
            "missing",
        ];
        let state = state.filter(|s| known.contains(&s.as_str())).unwrap_or_else(|| "offline".into());
        if state == "offline" || state == "missing" {
            tracing::warn!(schedule = schedule.id.0, %state, "skipping schedule: server offline");
            return failed(st, &mut task).await;
        }
    }

    if !now {
        if is_sync(st) {
            return run_queued(st, task.id.0, false).await;
        }
        dispatch(st.clone(), task.id.0, false, task.time_offset.0);
        return Ok(());
    }
    // dispatchNow(): failed() is not called by the dispatcher, so do it here.
    match run_task(st, &mut task, true).await {
        Ok(()) => Ok(()),
        Err(e) => {
            failed(st, &mut task).await?;
            Err(e)
        }
    }
}

/// `DaemonServerRepository::getDetails(timeout: 5)['state']` (`missing` on any error).
async fn server_state(st: &AppState, server_id: i64) -> ApiResult<Option<String>> {
    let server = servers::Entity::find_by_id(server_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    let node = pelican_db::entities::nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    let path = format!("/api/servers/{}", server.uuid);
    let mut call = pelican_wings::client::Call::new(pelican_wings::client::Method::GET, &path);
    call.timeout = Some(Duration::from_secs(5));
    call.connect_timeout = Some(Duration::from_secs(5));
    let details = match st.wings.send(&conn, call).await {
        Ok(r) => r.json().unwrap_or(Value::Null),
        Err(_) => json!({"state": "missing"}),
    };
    Ok(details.get("state").map(pelican_core::php::string))
}

/// Queue a `RunTaskJob` (see module docs for the sync/background split).
fn dispatch(st: AppState, task_id: i64, manual: bool, delay: i64) {
    tokio::spawn(async move {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay as u64)).await;
        }
        if let Err(e) = run_queued(&st, task_id, manual).await {
            tracing::error!(task = task_id, error = ?e.errors.first().map(|o| &o.code), "scheduled task failed");
        }
    });
}

/// A queued job: the task is re-read from the database (SerializesModels);
/// failures call `failed()` before being reported.
fn run_queued<'a>(
    st: &'a AppState,
    task_id: i64,
    manual: bool,
) -> Pin<Box<dyn Future<Output = ApiResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut task = tasks::Entity::find_by_id(task_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
        match run_task(st, &mut task, manual).await {
            Ok(()) => Ok(()),
            Err(e) => {
                failed(st, &mut task).await?;
                Err(e)
            }
        }
    })
}

fn is_sync(st: &AppState) -> bool {
    st.config.env.string_or("QUEUE_CONNECTION", "database") == "sync"
}

/// `RunTaskJob::handle()`.
pub async fn run_task(st: &AppState, task: &mut tasks::Model, manual: bool) -> ApiResult<()> {
    let schedule =
        schedules::Entity::find_by_id(task.schedule_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    if !schedule.is_active.0 && !manual {
        if set_queued(&st.db, task, false).await? {
            task_updated_events(st, task.id.0, task.schedule_id.0).await;
        }
        return mark_schedule_complete(&st.db, schedule.id.0).await;
    }
    let server =
        servers::Entity::find_by_id(schedule.server_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    if server.status.is_some() {
        return failed(st, task).await;
    }
    if let Err(e) = run_action(st, &server, task).await {
        let connection = e.errors.first().is_some_and(|o| o.code == "ConnectionException");
        if !(task.continue_on_failure.0 && connection) {
            return Err(e);
        }
    }
    // markTaskNotQueued() at the end of RunTaskJob::handle().
    if set_queued(&st.db, task, false).await? {
        task_updated_events(st, task.id.0, task.schedule_id.0).await;
    }

    // queueNextTask()
    let next = tasks::Model::find_by_statement(raw::stmt(
        st.db.get_database_backend(),
        "select * from {tasks} where {schedule_id} = ? and {sequence_id} > ? order by {sequence_id} asc limit 1",
        vec![task.schedule_id.into(), task.sequence_id.into()],
    ))
    .one(&st.db)
    .await?;
    let Some(mut next) = next else {
        return mark_schedule_complete(&st.db, schedule.id.0).await;
    };
    if set_queued(&st.db, &mut next, true).await? {
        task_updated_events(st, next.id.0, next.schedule_id.0).await;
    }
    if is_sync(st) {
        // SyncQueue: runs now, calls failed() on error and rethrows.
        run_queued(st, next.id.0, manual).await
    } else {
        dispatch(st.clone(), next.id.0, manual, next.time_offset.0);
        Ok(())
    }
}

fn wings_err(e: pelican_wings::WingsError) -> ApiError {
    ApiError::internal(e.code())
}

/// `TaskService` schemas (`app/Extensions/Tasks/Schemas`).
async fn run_action(st: &AppState, server: &servers::Model, task: &tasks::Model) -> ApiResult<()> {
    let node = pelican_db::entities::nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = || st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"));
    match task.action.as_str() {
        "command" => st.wings.send_commands(&conn()?, &server.uuid, &[task.payload.clone()]).await.map_err(wings_err),
        "power" => st.wings.power(&conn()?, &server.uuid, &task.payload).await.map_err(wings_err),
        "backup" => {
            let ignored: Vec<String> = task.payload.split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect();
            let txn = st.db.begin().await?;
            let opts =
                InitiateBackup { name: None, ignored, is_locked: false, is_scheduled: true, override_limit: true };
            let created = initiate_backup(st, &txn, server, opts).await?;
            txn.commit().await?;
            // TaskSchema\BackupSchema -> InitiateBackupService: Backup::create([...]).
            model_events::created(st, &created).await;
            Ok(())
        }
        "delete_files" => delete_files(st, &conn()?, server, &task.payload).await,
        // Task types registered by PHP plugins run in the sidecar (ADR-0020).
        other if crate::sidecar::task_type(other).is_some() => crate::sidecar::run_task(st, task.id.0).await,
        // Task types registered by WASM plugins (ADR-0018).
        other => {
            let owner = pelican_plugins::runtime::task_types().into_iter().find(|(_, t)| t.id == other).map(|(p, _)| p);
            let (Some(plugin), Some(rt)) = (owner, pelican_plugins::runtime::global()) else {
                return Err(ApiError::internal("InvalidArgumentException"));
            };
            match rt.run_task(&plugin, other, &task.payload, &crate::plugin_routes::server_json(server)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => {
                    tracing::warn!(plugin, task = other, error = %e, "plugin task failed");
                    Err(ApiError::internal("PluginTaskException"))
                }
                Err(_) => Err(ApiError::internal("PluginTaskException")),
            }
        }
    }
}

/// `DeleteFilesService::handle()`: each line is `dirname/glob`.
async fn delete_files(
    st: &AppState,
    conn: &pelican_wings::Node,
    server: &servers::Model,
    payload: &str,
) -> ApiResult<()> {
    use pelican_wings::client::{Call, Method};
    let mut to_delete: Vec<String> = Vec::new();
    for line in payload.split('\n') {
        let path = php_dirname(line);
        let pattern = php_basename(line);
        let p = format!("/api/servers/{}/files/list-directory", server.uuid);
        let q = [("directory", path.clone())];
        let resp = st.wings.send(conn, Call::new(Method::GET, &p).query(&q)).await.map_err(wings_err)?;
        let items: Vec<Value> = match resp.json() {
            Some(Value::Array(a)) => a,
            Some(Value::Object(m)) => m.into_iter().map(|(_, v)| v).collect(),
            _ => Vec::new(),
        };
        for item in items {
            let name = item.get("name").map(pelican_core::php::string).unwrap_or_default();
            if str_is(&pattern, &name) {
                to_delete.push(format!("{path}/{name}"));
            }
        }
    }
    if !to_delete.is_empty() {
        let p = format!("/api/servers/{}/files/delete", server.uuid);
        st.wings
            .send(conn, Call::new(Method::POST, &p).json(json!({"root": "/", "files": to_delete})))
            .await
            .map_err(wings_err)?;
    }
    Ok(())
}

/// `Str::is($pattern, $value)`: `*` matches anything, the rest is literal.
fn str_is(pattern: &str, value: &str) -> bool {
    if pattern == value {
        return true;
    }
    let re = format!("^{}$", regex::escape(pattern).replace(r"\*", ".*"));
    regex::RegexBuilder::new(&re).dot_matches_new_line(true).build().is_ok_and(|r| r.is_match(value))
}

/// PHP `basename()` (no suffix).
fn php_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("").to_string()
}

/// PHP `dirname()` (one level).
fn php_dirname(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(i) => {
            let parent = trimmed[..i].trim_end_matches('/');
            if parent.is_empty() { "/".into() } else { parent.to_string() }
        }
    }
}

/// `p:schedule:process` (every minute): due, active, idle schedules of servers
/// without a status; failures are logged per schedule. Returns how many ran.
pub async fn process_runnable(st: &AppState) -> ApiResult<usize> {
    let b = st.db.get_database_backend();
    let truthy: sea_orm::Value = if b == sea_orm::DbBackend::Postgres { true.into() } else { 1.into() };
    let falsy: sea_orm::Value = if b == sea_orm::DbBackend::Postgres { false.into() } else { 0.into() };
    let due = schedules::Model::find_by_statement(raw::stmt(
        b,
        "select * from {schedules} where exists (select * from {servers} where {schedules.server_id} = {servers.id} and {status} is null) \
         and {is_active} = ? and {is_processing} = ? and {next_run_at} <= ?",
        vec![truthy, falsy, Timestamp(pelican_core::time::now()).into()],
    ))
    .all(&st.db)
    .await?;
    let mut ran = 0;
    for mut s in due {
        if first_task(&st.db, s.id.0).await?.is_none() {
            continue;
        }
        match process_schedule(st, &mut s, false).await {
            Ok(()) => ran += 1,
            Err(e) => tracing::error!(schedule = s.id.0, error = ?e.errors.first().map(|o| &o.code), "schedule failed"),
        }
    }
    Ok(ran)
}

/// The panel scheduler (`schedule:run` every minute): server schedules plus the
/// maintenance tasks of `app/Console/Kernel.php`.
pub fn spawn_scheduler(live: pelican_core::state::Live) {
    tokio::spawn(async move {
        loop {
            let now = chrono::Utc::now();
            let wait = 60 - (now.timestamp() % 60) as u64;
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let st = live.get();
            let now = chrono::Utc::now();
            crate::health::record_heartbeats(now);
            run_schedule(&st, now).await;
        }
    });
}

/// `RevocationListener` (user deleted / password changed): one `RevokeSftpAccessJob`
/// per node hosting a server the user owns or is a subuser of. A connection
/// failure only releases the job; other Wings errors fail it (and propagate in sync mode).
pub async fn revoke_sftp_access(st: &AppState, user: &pelican_db::entities::users::Model) -> ApiResult<()> {
    let b = st.db.get_database_backend();
    let nodes = pelican_db::entities::nodes::Model::find_by_statement(raw::stmt(
        b,
        "select * from {nodes} where {nodes.id} in (select distinct {servers.node_id} from {servers} \
         left join {subusers} on {subusers.server_id} = {servers.id} where ({servers.owner_id} = ? or {subusers.user_id} = ?)) \
         order by {nodes.id} asc",
        vec![user.id.into(), user.id.into()],
    ))
    .all(&st.db)
    .await?;
    for node in nodes {
        let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
        let uuid = user.uuid.clone();
        if is_sync(st) {
            match st.wings.deauthorize_user(&conn, &uuid, &[]).await {
                Ok(()) | Err(pelican_wings::WingsError::Connection(_)) => {}
                Err(e) => return Err(wings_err(e)),
            }
        } else {
            let st = st.clone();
            tokio::spawn(async move {
                if let Err(e) = st.wings.deauthorize_user(&conn, &uuid, &[]).await {
                    tracing::warn!(error = %e, "revoking SFTP access failed");
                }
            });
        }
    }
    Ok(())
}

// --- maintenance (app/Console/Kernel.php schedule) ------------------------------

/// `p:maintenance:prune-backups`: uncompleted backups older than `BACKUP_PRUNE_AGE`
/// minutes (default 360) are marked failed. Returns how many were updated.
pub async fn prune_backups(st: &AppState, minutes: i64) -> ApiResult<u64> {
    let b = st.db.get_database_backend();
    let now = pelican_core::time::now();
    let since = Timestamp(now - chrono::Duration::minutes(minutes));
    let falsy: sea_orm::Value = if b == sea_orm::DbBackend::Postgres { false.into() } else { 0.into() };
    let res = st
        .db
        .execute_raw(raw::stmt(
            b,
            "update {backups} set {is_successful} = ?, {completed_at} = ?, {updated_at} = ? where {completed_at} is null and {created_at} <= ?",
            vec![falsy, Timestamp(now).into(), Timestamp(now).into(), since.into()],
        ))
        .await?;
    Ok(res.rows_affected())
}

/// `model:prune` for `ActivityLog` (`timestamp`) / `Webhook` (`created_at`):
/// mass deletion without model events (subjects cascade).
pub async fn prune_table(st: &AppState, table: &str, column: &str, days: i64) -> ApiResult<u64> {
    let b = st.db.get_database_backend();
    let before = Timestamp(pelican_core::time::now() - chrono::Duration::days(days));
    let sql = format!("delete from {{{table}}} where {{{column}}} <= ?");
    Ok(st.db.execute_raw(raw::stmt(b, &sql, vec![before.into()])).await?.rows_affected())
}

/// `p:maintenance:prune-images`: `DELETE /api/system/docker/image/prune` on every
/// node (bare daemon client, 5 s connect / 30 s timeout; errors only logged).
pub async fn prune_images(st: &AppState) -> ApiResult<()> {
    let all = pelican_db::entities::nodes::Entity::find().all(&st.db).await?;
    for node in all {
        let Ok(conn) = st.node_conn(&node) else { continue };
        let mut call =
            pelican_wings::client::Call::new(pelican_wings::client::Method::DELETE, "/api/system/docker/image/prune");
        call.throw = false;
        call.connect_timeout = Some(Duration::from_secs(5));
        call.timeout = Some(Duration::from_secs(30));
        if let Err(e) = st.wings.send(&conn, call).await {
            tracing::warn!(node = node.id.0, error = %e, "pruning docker images failed");
        }
    }
    Ok(())
}

/// `p:maintenance:clean-service-backups`: `services/.bak` files of the `local` disk (Laravel 11+:
/// `storage/app/private`) older than 5 minutes.
pub fn clean_service_backups(st: &AppState) {
    let dir = st.config.base_path.join("storage/app/private/services/.bak");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if old.is_some_and(|d| d > Duration::from_secs(5 * 60)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Everything `app/Console/Kernel.php` schedules, for the minute `now` (UTC).
async fn run_schedule(st: &AppState, now: chrono::DateTime<chrono::Utc>) {
    use chrono::Timelike;
    let log = |what: &str, e: ApiError| tracing::error!(task = what, error = ?e.errors.first().map(|o| &o.code), "scheduled maintenance failed");
    if let Err(e) = process_runnable(st).await {
        log("p:schedule:process", e);
    }
    let env = &st.config.env;
    let (hour, minute) = (now.hour(), now.minute());
    if minute % 5 == 0 {
        // RunHealthChecksCommand (everyFiveMinutes).
        crate::health::run_checks(st).await;
    }
    if minute % 30 == 0 {
        let age = env.int_or("BACKUP_PRUNE_AGE", 360);
        if age > 0 {
            if let Err(e) = prune_backups(st, age).await {
                log("p:maintenance:prune-backups", e);
            }
        }
    }
    if hour == 0 && minute == 0 {
        // CheckEggUpdatesCommand / UpdateEggIndexCommand (daily).
        crate::egg_index::check_updates(st).await;
        if let Err(e) = crate::egg_index::update_index(st).await {
            tracing::warn!(error = %e, "p:egg:update-index failed");
        }
        clean_service_backups(st);
        if let Err(e) = prune_images(st).await {
            log("p:maintenance:prune-images", e);
        }
        let activity_days = env.int_or("APP_ACTIVITY_PRUNE_DAYS", 90);
        if activity_days > 0 {
            if let Err(e) = prune_table(st, "activity_logs", "timestamp", activity_days).await {
                log("model:prune ActivityLog", e);
            }
        }
        let webhook_days = env.int_or("APP_WEBHOOK_PRUNE_DAYS", 30);
        if webhook_days > 0 {
            if let Err(e) = prune_table(st, "webhooks", "created_at", webhook_days).await {
                log("model:prune Webhook", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_path_helpers() {
        for (p, d, b) in [
            ("a.txt", ".", "a.txt"),
            ("/a.txt", "/", "a.txt"),
            ("/logs/*.log", "/logs", "*.log"),
            ("a/b/", "a", "b"),
            ("//x", "/", "x"),
            ("/", "/", ""),
        ] {
            assert_eq!(php_dirname(p), d, "dirname {p}");
            assert_eq!(php_basename(p), b, "basename {p}");
        }
        assert!(str_is("*.log", "latest.log"));
        assert!(!str_is("*.log", "latest.txt"));
        assert!(str_is("a.b", "a.b") && !str_is("a.b", "axb"));
    }
}
