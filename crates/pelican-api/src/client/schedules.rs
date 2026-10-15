//! `/servers/{server}/schedules` — `ScheduleController`, `ScheduleTaskController`
//! (client-api.md §8.6).

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{schedules, tasks};
use pelican_db::{Bool, Int, SmallBool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, TransactionTrait};
use serde_json::{Map, Value, json};

use crate::envelope::{item, list};
use crate::error::{ApiError, ApiResult, Style};
use crate::input::{In, Input, php_boolean};
use crate::jobs::{next_run_date, touch_schedule};
use crate::json::{PhpJson, no_content};
use crate::model_events::{self, Action};
use crate::pipeline::Ctx;
use crate::resources::{atom, format_timestamp};
use crate::services::LogActivity;
use crate::validation::Validator;

/// `TaskData` (`schedule_task`).
fn task_item(t: &tasks::Model) -> Value {
    let mut a = Map::new();
    a.insert("id".into(), json!(t.id.0));
    a.insert("sequence_id".into(), json!(t.sequence_id.0));
    a.insert("action".into(), json!(t.action));
    a.insert("payload".into(), json!(t.payload));
    a.insert("time_offset".into(), json!(t.time_offset.0));
    a.insert("is_queued".into(), json!(t.is_queued.0));
    a.insert("continue_on_failure".into(), json!(t.continue_on_failure.0));
    a.insert("created_at".into(), json!(format_timestamp(t.created_at.as_ref())));
    a.insert("updated_at".into(), json!(format_timestamp(t.updated_at.as_ref())));
    item("schedule_task", Value::Object(a), vec![])
}

/// `ScheduleData` (`server_schedule`) with the default `tasks` include.
fn schedule_item(s: &schedules::Model, tasks: &[tasks::Model]) -> Value {
    let mut a = Map::new();
    a.insert("id".into(), json!(s.id.0));
    a.insert("name".into(), json!(s.name));
    a.insert(
        "cron".into(),
        json!({
            "day_of_week": s.cron_day_of_week,
            "day_of_month": s.cron_day_of_month,
            "month": s.cron_month,
            "hour": s.cron_hour,
            "minute": s.cron_minute,
        }),
    );
    a.insert("is_active".into(), json!(s.is_active.0));
    a.insert("is_processing".into(), json!(s.is_processing.0));
    a.insert("only_when_online".into(), json!(s.only_when_online.0));
    a.insert("last_run_at".into(), json!(atom(s.last_run_at.as_ref())));
    a.insert("next_run_at".into(), json!(atom(s.next_run_at.as_ref())));
    a.insert("created_at".into(), json!(format_timestamp(s.created_at.as_ref())));
    a.insert("updated_at".into(), json!(format_timestamp(s.updated_at.as_ref())));
    item("server_schedule", Value::Object(a), vec![("tasks", list(tasks.iter().map(task_item).collect()))])
}

/// Eager load `tasks` (`where schedule_id in (...)`, natural order — ADR-0011).
async fn tasks_of<C: ConnectionTrait>(db: &C, schedule_ids: &[i64]) -> ApiResult<Vec<tasks::Model>> {
    if schedule_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = schedule_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
    let sql = format!("select * from {{tasks}} where {{tasks.schedule_id}} in ({ids})");
    Ok(tasks::Model::find_by_statement(raw::stmt(db.get_database_backend(), &sql, vec![])).all(db).await?)
}

fn schedule(ctx: &Ctx) -> ApiResult<schedules::Model> {
    ctx.bound.schedule.clone().ok_or_else(ApiError::model_not_found)
}

fn task(ctx: &Ctx) -> ApiResult<tasks::Model> {
    ctx.bound.task.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /schedules`.
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.read").await?;
    let server = ctx.bound.server()?;
    let b = st.db.get_database_backend();
    let rows = schedules::Model::find_by_statement(raw::stmt(
        b,
        "select * from {schedules} where {schedules.server_id} = ? and {schedules.server_id} is not null",
        vec![server.id.into()],
    ))
    .all(&st.db)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|s| s.id.0).collect();
    let all_tasks = tasks_of(&st.db, &ids).await?;
    let data = rows
        .iter()
        .map(|s| {
            let ts: Vec<tasks::Model> = all_tasks.iter().filter(|t| t.schedule_id == s.id).cloned().collect();
            schedule_item(s, &ts)
        })
        .collect();
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `StoreScheduleRequest::rules()`.
fn validate_schedule(input: &Input) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("name", &["required", "string", "max:255"]);
    v.rules("is_active", &["filled", "boolean"]);
    v.rules("only_when_online", &["boolean"]);
    for f in ["minute", "hour", "day_of_month", "month", "day_of_week"] {
        v.rules(f, &["required", "string"]);
    }
    v.validate()
}

fn input_str(input: &Input, key: &str) -> String {
    input.get(key).map(pelican_core::php::string).unwrap_or_default()
}

/// PHP `(bool) $request->input($key)`.
fn input_bool(input: &Input, key: &str) -> bool {
    input.get(key).is_some_and(|v| !pelican_core::php::empty(v))
}

/// `ScheduleController::getNextRunAt()`.
fn next_run_at(input: &Input, timezone: &str) -> ApiResult<Timestamp> {
    next_run_date(
        &input_str(input, "minute"),
        &input_str(input, "hour"),
        &input_str(input, "day_of_month"),
        &input_str(input, "month"),
        &input_str(input, "day_of_week"),
        timezone,
    )
    .map(Timestamp)
    .map_err(|_| {
        ApiError::display("DisplayException", 400, "The cron data provided does not evaluate to a valid expression.")
    })
}

/// `POST /schedules`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.create").await?;
    validate_schedule(&input)?;
    let server = ctx.bound.server()?;
    let next = next_run_at(&input, "UTC")?;
    let now = Timestamp(pelican_core::time::now());
    let am = schedules::ActiveModel {
        name: Set(input_str(&input, "name")),
        cron_day_of_week: Set(input_str(&input, "day_of_week")),
        cron_month: Set(input_str(&input, "month")),
        cron_day_of_month: Set(input_str(&input, "day_of_month")),
        cron_hour: Set(input_str(&input, "hour")),
        cron_minute: Set(input_str(&input, "minute")),
        timezone: Set("UTC".into()),
        is_active: Set(Bool(input_bool(&input, "is_active"))),
        is_processing: Set(Bool(false)),
        only_when_online: Set(SmallBool(input_bool(&input, "only_when_online"))),
        server_id: Set(server.id),
        next_run_at: Set(Some(next)),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let created = pelican_db::write::insert(&st.db, am).await?;
    // ScheduleController::store: Schedule::query()->create([...]).
    model_events::created(&st, &created).await;
    ctx.activity("server:schedule.create")
        .subject(morph::SCHEDULE, created.id.0)
        .property("name", created.name.clone())
        .log(&st)
        .await?;
    Ok(PhpJson::ok(schedule_item(&created, &[])).into_response())
}

/// `GET /schedules/{schedule}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.read").await?;
    let s = schedule(&ctx)?;
    let ts = tasks_of(&st.db, &[s.id.0]).await?;
    Ok(PhpJson::ok(schedule_item(&s, &ts)).into_response())
}

fn same_minute(a: Option<Timestamp>, b: Timestamp) -> bool {
    a.is_some_and(|a| a.0.format("%F %T").to_string() == b.0.format("%F %T").to_string())
}

/// `POST /schedules/{schedule}`.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.update").await?;
    validate_schedule(&input)?;
    let s = schedule(&ctx)?;
    let active = input_bool(&input, "is_active");
    let next = next_run_at(&input, &s.timezone)?;
    // `$schedule->update($data)`: only dirty attributes (plus updated_at).
    let mut am = schedules::ActiveModel { id: sea_orm::ActiveValue::Unchanged(s.id), ..Default::default() };
    let mut dirty = false;
    macro_rules! set_if {
        ($field:ident, $new:expr) => {{
            let new = $new;
            if s.$field != new {
                am.$field = Set(new);
                dirty = true;
            }
        }};
    }
    set_if!(name, input_str(&input, "name"));
    set_if!(cron_day_of_week, input_str(&input, "day_of_week"));
    set_if!(cron_month, input_str(&input, "month"));
    set_if!(cron_day_of_month, input_str(&input, "day_of_month"));
    set_if!(cron_hour, input_str(&input, "hour"));
    set_if!(cron_minute, input_str(&input, "minute"));
    set_if!(is_active, Bool(active));
    set_if!(only_when_online, SmallBool(input_bool(&input, "only_when_online")));
    if !same_minute(s.next_run_at, next) {
        am.next_run_at = Set(Some(next));
        dirty = true;
    }
    // Re-enabling/disabling resets a stuck processing flag.
    if s.is_active.0 != active && s.is_processing.0 {
        am.is_processing = Set(Bool(false));
        dirty = true;
    }
    if dirty {
        am.updated_at = Set(Some(Timestamp(pelican_core::time::now())));
        pelican_db::write::update(&st.db, &am).await?;
        // ScheduleController::update: $schedule->update($data).
        model_events::fire_id::<schedules::Entity>(&st, Action::Updated, s.id.0).await;
    }
    let mut props = Map::new();
    props.insert("name".into(), json!(input_str(&input, "name")));
    props.insert("active".into(), json!(active));
    ctx.activity("server:schedule.update")
        .subject(morph::SCHEDULE, s.id.0)
        .properties(Value::Object(props))
        .log(&st)
        .await?;
    let fresh = schedules::Entity::find_by_id(s.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    let ts = tasks_of(&st.db, &[s.id.0]).await?;
    Ok(PhpJson::ok(schedule_item(&fresh, &ts)).into_response())
}

/// `POST /schedules/{schedule}/execute`.
pub async fn execute(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.update").await?;
    let mut s = schedule(&ctx)?;
    crate::jobs::process_schedule(&st, &mut s, true).await?;
    ctx.activity("server:schedule.execute")
        .subject(morph::SCHEDULE, s.id.0)
        .property("name", s.name.clone())
        .log(&st)
        .await?;
    Ok(PhpJson::with_status(StatusCode::ACCEPTED, json!([])).into_response())
}

/// `DELETE /schedules/{schedule}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.delete").await?;
    let s = schedule(&ctx)?;
    // ScheduleController::delete: $schedule->delete(); the payload carries the tasks
    // (`$with`), which the cascade removes, so it is built first.
    let event = model_events::pending(&st, Action::Deleted, &s).await;
    schedules::Entity::delete_by_id(s.id).exec(&st.db).await?;
    if let Some(ev) = event {
        ev.send(&st).await;
    }
    ctx.activity("server:schedule.delete")
        .subject(morph::SCHEDULE, s.id.0)
        .property("name", s.name.clone())
        .log(&st)
        .await?;
    Ok(no_content())
}

tokio::task_local! {
    /// Set by the panel's task form: the Filament form offers every registered task type
    /// (plugins included), while `StoreTaskRequest` lists only the core four.
    pub static PANEL_FORM: bool;
}

/// `StoreTaskRequest::rules()`.
fn validate_task(input: &Input) -> ApiResult<()> {
    let mut v = Validator::new(input);
    let mut actions = String::from("in:command,power,backup,delete_files");
    if PANEL_FORM.try_with(|f| *f).unwrap_or(false) {
        for (_, t) in pelican_plugins::runtime::task_types() {
            actions.push(',');
            actions.push_str(&t.id);
        }
    }
    v.rules("action", &["required", actions.as_str()]);
    v.rules("payload", &["required_unless:action,backup", "string", "nullable"]);
    v.rules("time_offset", &["required", "numeric", "min:0", "max:900"]);
    v.rules("sequence_id", &["sometimes", "required", "numeric", "min:1"]);
    v.rules("continue_on_failure", &["sometimes", "required", "boolean"]);
    v.validate()
}

fn backup_forbidden(st_backup_limit: i64, input: &Input) -> ApiResult<()> {
    if st_backup_limit == 0 && input.get("action").and_then(Value::as_str) == Some("backup") {
        return Err(ApiError::new(
            403,
            "HttpForbiddenException",
            "A backup task cannot be created when the server's backup limit is set to 0.",
            Style::Pretty,
        ));
    }
    Ok(())
}

/// PHP `intval()` of a request value.
fn php_intval(v: &Value) -> i64 {
    match v {
        Value::Null => 0,
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => pelican_core::php::intval(s),
        Value::Array(a) => (!a.is_empty()) as i64,
        Value::Object(m) => (!m.is_empty()) as i64,
    }
}

/// `$request->integer($key, $default)`: `intval(input($key, $default))`.
fn input_int(input: &Input, key: &str, default: i64) -> i64 {
    match input.get(key) {
        None => default,
        Some(v) => php_intval(v),
    }
}

/// `time_offset` is stored as sent (the integer cast applies on read): returns
/// (value written, value the model reports).
fn time_offset_value(backend: DbBackend, v: Option<&Value>) -> ApiResult<(i64, i64)> {
    let raw = v.cloned().unwrap_or(Value::Null);
    let cast = php_intval(&raw);
    let s = pelican_core::php::string(&raw);
    if let Ok(i) = s.trim().parse::<i64>() {
        return Ok((i, cast));
    }
    let f: f64 = s.trim().parse().unwrap_or(0.0);
    match backend {
        // PDO sends the text; PostgreSQL rejects "5.5" for an integer column.
        DbBackend::Postgres => Err(ApiError::internal("QueryException")),
        // MySQL rounds a fractional string assigned to an INT column.
        DbBackend::MySql => Ok((f.round() as i64, cast)),
        // SQLite would keep a REAL; reads cast it back to int.
        _ => Ok((cast, cast)),
    }
}

/// `update tasks set sequence_id = sequence_id ± 1, updated_at = ? where ...`
async fn shift<C: ConnectionTrait>(db: &C, schedule_id: i64, op: &str, cond: &str, vals: Vec<i64>) -> ApiResult<()> {
    let b = db.get_database_backend();
    let sql = format!(
        "update {{tasks}} set {{sequence_id}} = {{sequence_id}} {op} 1, {{updated_at}} = ? where {{tasks.schedule_id}} = ? and {{tasks.schedule_id}} is not null and {cond}"
    );
    let mut values: Vec<sea_orm::Value> = vec![Timestamp(pelican_core::time::now()).into(), schedule_id.into()];
    values.extend(vals.into_iter().map(sea_orm::Value::from));
    db.execute_raw(raw::stmt(b, &sql, values)).await?;
    Ok(())
}

/// `POST /schedules/{schedule}/tasks`.
pub async fn task_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.update").await?;
    validate_task(&input)?;
    let server = ctx.bound.server()?;
    let s = schedule(&ctx)?;
    let b = st.db.get_database_backend();
    let existing = tasks_of(&st.db, &[s.id.0]).await?;
    let limit = st.config.env.int_or("PANEL_PER_SCHEDULE_TASK_LIMIT", 10);
    if existing.len() as i64 >= limit {
        return Err(ApiError::display(
            "ServiceLimitExceededException",
            400,
            format!(
                "Schedules may not have more than {limit} tasks associated with them. Creating this task would put this schedule over the limit."
            ),
        ));
    }
    backup_forbidden(server.backup_limit.0, &input)?;
    let last = existing.iter().map(|t| t.sequence_id.0).max();
    let (offset, offset_cast) = time_offset_value(b, input.get("time_offset"))?;

    let txn = st.db.begin().await?;
    let mut sequence = last.unwrap_or(0) + 1;
    let requested = input_int(&input, "sequence_id", sequence).max(1);
    if requested < sequence {
        shift(&txn, s.id.0, "+", "{sequence_id} >= ?", vec![requested]).await?;
        sequence = requested;
    }
    let now = Timestamp(pelican_core::time::now());
    let action = input_str(&input, "action");
    let payload = match input.get("payload") {
        None | Some(Value::Null) => String::new(),
        Some(p) => pelican_core::php::string(p),
    };
    let continue_on_failure = php_boolean(input.get("continue_on_failure"));
    let id = pelican_db::write::insert_id(
        &txn,
        &tasks::ActiveModel {
            schedule_id: Set(s.id),
            sequence_id: Set(Int(sequence)),
            action: Set(action.clone()),
            payload: Set(payload.clone()),
            time_offset: Set(Int(offset)),
            is_queued: Set(Bool(false)),
            continue_on_failure: Set(SmallBool(continue_on_failure)),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?
    .ok_or_else(|| ApiError::internal("QueryException"))?;
    touch_schedule(&txn, s.id.0).await?;
    txn.commit().await?;

    // ScheduleTaskController::store: Task::query()->create([...]) inside the transaction,
    // then Task::$touches fires a save of the parent schedule (updated_at only).
    let t = tasks::Model {
        id: Int(id),
        schedule_id: s.id,
        sequence_id: Int(sequence),
        action,
        payload,
        time_offset: Int(offset_cast),
        is_queued: Bool(false),
        created_at: Some(now),
        updated_at: Some(now),
        continue_on_failure: SmallBool(continue_on_failure),
    };
    model_events::created(&st, &t).await;
    model_events::fire_id::<schedules::Entity>(&st, Action::Updated, s.id.0).await;

    let mut props = Map::new();
    props.insert("name".into(), json!(s.name));
    props.insert("action".into(), json!(t.action));
    props.insert("payload".into(), json!(t.payload));
    ctx.activity("server:task.create")
        .subject(morph::SCHEDULE, s.id.0)
        .subject(morph::TASK, id)
        .properties(Value::Object(props))
        .log(&st)
        .await?;
    Ok(PhpJson::ok(task_item(&t)).into_response())
}

/// `POST /schedules/{schedule}/tasks/{task}`.
pub async fn task_update(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "schedule.update").await?;
    validate_task(&input)?;
    let server = ctx.bound.server()?;
    let s = schedule(&ctx)?;
    let t = task(&ctx)?;
    backup_forbidden(server.backup_limit.0, &input)?;
    let b = st.db.get_database_backend();
    let (offset, offset_cast) = time_offset_value(b, input.get("time_offset"))?;

    let txn = st.db.begin().await?;
    let old = t.sequence_id.0;
    let sequence = input_int(&input, "sequence_id", old).max(1);
    if sequence < old {
        shift(&txn, s.id.0, "+", "{sequence_id} >= ? and {sequence_id} < ?", vec![sequence, old]).await?;
    } else if sequence > old {
        shift(&txn, s.id.0, "-", "{sequence_id} > ? and {sequence_id} <= ?", vec![old, sequence]).await?;
    }
    let action = input_str(&input, "action");
    let payload = match input.get("payload") {
        None | Some(Value::Null) => String::new(),
        Some(p) => pelican_core::php::string(p),
    };
    let continue_on_failure = php_boolean(input.get("continue_on_failure"));
    let dirty = t.sequence_id.0 != sequence
        || t.action != action
        || t.payload != payload
        || t.time_offset.0 != offset_cast
        || t.continue_on_failure.0 != continue_on_failure;
    if dirty {
        let mut am = tasks::ActiveModel { id: sea_orm::ActiveValue::Unchanged(t.id), ..Default::default() };
        if t.sequence_id.0 != sequence {
            am.sequence_id = Set(Int(sequence));
        }
        if t.action != action {
            am.action = Set(action.clone());
        }
        if t.payload != payload {
            am.payload = Set(payload.clone());
        }
        if t.continue_on_failure.0 != continue_on_failure {
            am.continue_on_failure = Set(SmallBool(continue_on_failure));
        }
        if t.time_offset.0 != offset_cast {
            am.time_offset = Set(Int(offset));
        }
        am.updated_at = Set(Some(Timestamp(pelican_core::time::now())));
        pelican_db::write::update(&txn, &am).await?;
        touch_schedule(&txn, s.id.0).await?;
    }
    txn.commit().await?;

    // ScheduleTaskController::update: $task->update([...]) (only when dirty),
    // then Task::$touches fires a save of the parent schedule.
    if dirty {
        model_events::fire_id::<tasks::Entity>(&st, Action::Updated, t.id.0).await;
        model_events::fire_id::<schedules::Entity>(&st, Action::Updated, s.id.0).await;
    }

    let mut props = Map::new();
    props.insert("name".into(), json!(s.name));
    props.insert("action".into(), json!(action));
    props.insert("payload".into(), json!(payload));
    ctx.activity("server:task.update")
        .subject(morph::SCHEDULE, s.id.0)
        .subject(morph::TASK, t.id.0)
        .properties(Value::Object(props))
        .log(&st)
        .await?;
    let fresh = tasks::Entity::find_by_id(t.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(task_item(&fresh)).into_response())
}

/// `DELETE /schedules/{schedule}/tasks/{task}`.
pub async fn task_delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let s = schedule(&ctx)?;
    let t = task(&ctx)?;
    if !ctx.can(&st, "schedule.delete").await? {
        return Err(ApiError::new(
            403,
            "HttpForbiddenException",
            "You do not have permission to perform this action.",
            Style::Pretty,
        ));
    }
    shift(&st.db, s.id.0, "-", "{sequence_id} > ?", vec![t.sequence_id.0]).await?;
    // Model::delete() touches owners before deleting (Task::$touches).
    touch_schedule(&st.db, s.id.0).await?;
    model_events::fire_id::<schedules::Entity>(&st, Action::Updated, s.id.0).await;
    tasks::Entity::delete_by_id(t.id).exec(&st.db).await?;
    // ScheduleTaskController::delete: $task->delete().
    model_events::deleted(&st, &t).await;
    ctx.activity("server:task.delete")
        .subject(morph::SCHEDULE, s.id.0)
        .subject(morph::TASK, t.id.0)
        .property("name", s.name.clone())
        .log(&st)
        .await?;
    Ok(no_content())
}
