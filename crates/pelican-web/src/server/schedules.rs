//! Server panel › Schedules (app/Filament/Server/Resources/Schedules):
//! list, create/edit/view form with cron presets, run now, export/import
//! (sharing services) and the Tasks relation manager (create / edit / delete
//! / reorder). Scheduled-task domain calls are reused verbatim from
//! `pelican_api::client::schedules` (same authorization, limits, transaction
//! sequencing and activity records).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Extension, FromRequest, FromRequestParts, Path, Request};
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use indexmap::IndexMap;
use minijinja::{Value as Mj, context};
use pelican_api::binding::Bound;
use pelican_api::client::schedules as api;
use pelican_api::error::{ApiError, Kind as ApiKind};
use pelican_api::input::{In, Input};
use pelican_api::jobs::next_run_date;
use pelican_api::model_events;
use pelican_api::pipeline::Ctx;
use pelican_api::services::LogActivity;
use pelican_api::validation::Validator;
use pelican_core::activity::morph;
use pelican_core::env::php_intval;
use pelican_core::state::AppState;
use pelican_db::entities::{schedules, tasks};
use pelican_db::{Bool, Int, SmallBool, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde::Serialize;

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

/// Client-API task actions in registration order (`TaskServiceProvider`):
/// `(id, default payload)`; the panel offers these four schemas.
const ACTIONS: &[(&str, Option<&str>)] =
    &[("command", None), ("power", Some("restart")), ("backup", None), ("delete_files", None)];

/// Labels resolved from `server/schedule.tasks.actions.<id>` plus the
/// payload label shown as a tooltip (`TaskSchema::getPayloadLabel()`).
const ACTION_LABELS: &[(&str, &str)] = &[
    ("command", "server/schedule.tasks.actions.command.command"),
    ("power", "server/schedule.tasks.actions.power.action"),
    ("backup", "server/schedule.tasks.actions.backup.files_to_ignore"),
    ("delete_files", "server/schedule.tasks.actions.delete_files.files_to_delete"),
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/schedules", get(index))
        .route("/server/{server}/schedules/import", post(import))
        .route("/server/{server}/schedules/create", get(create_page).post(create_submit))
        .route("/server/{server}/schedules/{id}", get(view_page))
        .route("/server/{server}/schedules/{id}/edit", get(edit_page).post(edit_submit))
        .route("/server/{server}/schedules/{id}/delete", post(delete_schedule))
        .route("/server/{server}/schedules/{id}/run", post(run_now))
        .route("/server/{server}/schedules/{id}/export", get(export))
        .route("/server/{server}/schedules/{id}/tasks/create", post(task_create))
        .route("/server/{server}/schedules/{id}/tasks/{task}/edit", get(open_task_edit))
        .route("/server/{server}/schedules/{id}/tasks/{task}/update", post(task_update))
        .route("/server/{server}/schedules/{id}/tasks/{task}/delete", post(task_delete))
        .route("/server/{server}/schedules/{id}/tasks/{task}/move", post(task_move))
}

// ----- shared helpers ------------------------------------------------------

/// A client-API `Ctx`: ip and actor like this web session, default subject =
/// the server (`ServerSubject` of the client group), like `server/backups.rs`.
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

async fn find(p: &ServerPanel, id: i64) -> Result<schedules::Model, WebError> {
    schedules::Entity::find_by_id(Int(id))
        .filter(schedules::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// Eager load `tasks` (natural order, ADR-0011) — the same statement shape
/// the Client API `index`/`view` use.
async fn tasks_of<C: ConnectionTrait>(db: &C, schedule_id: i64) -> Result<Vec<tasks::Model>, WebError> {
    let sql = "select * from {tasks} where {tasks.schedule_id} = ?";
    Ok(tasks::Model::find_by_statement(raw::stmt(db.get_database_backend(), sql, vec![schedule_id.into()]))
        .all(db)
        .await?)
}

/// First message of an error object list, for notification bodies.
fn first_detail(e: &ApiError) -> Option<String> {
    e.errors.first().map(|o| o.detail.clone()).filter(|d| !d.is_empty())
}

/// Per-field messages of a validation-shaped `ApiError` (`meta.source_field`).
fn field_errors(e: &ApiError) -> Errors {
    let mut out = Errors::default();
    for o in &e.errors {
        if let Some(meta) = &o.meta {
            out.add(&meta.source_field, &o.detail);
        }
    }
    out
}

/// Run one client-API call and translate the outcome for a form/dialog flow.
enum ApiOutcome<T> {
    Done(T),
    Validation(Errors),
    Failed(ApiError),
}

async fn call<T>(
    fut: impl std::future::Future<Output = pelican_api::error::ApiResult<Response>>,
    take: impl FnOnce(Response) -> T,
) -> ApiOutcome<T> {
    match fut.await {
        Ok(resp) => ApiOutcome::Done(take(resp)),
        Err(e) if e.kind == ApiKind::Validation => ApiOutcome::Validation(field_errors(&e)),
        Err(e) => ApiOutcome::Failed(e),
    }
}

/// Log an activity entry built through the web user's ctx; API-level failures
/// surface as internal web errors.
async fn log_act(
    p: &ServerPanel,
    event: &str,
    subjects: &[(&str, i64)],
    props: Vec<(&str, serde_json::Value)>,
) -> Result<(), WebError> {
    let ctx = api_ctx(p)?;
    let mut a = ctx.activity(event);
    for (kind, id) in subjects {
        a = a.subject(*kind, *id);
    }
    for (k, v) in props {
        a = a.property(k, v);
    }
    a.log(&p.st)
        .await
        .map(|_| ())
        .map_err(|e| WebError::internal(e.errors.first().map(|o| o.detail.as_str()).unwrap_or("activity log failed")))
}

// ----- the list page -------------------------------------------------------

#[derive(FromQueryResult)]
struct Aggregate {
    aggregate: Int,
}

/// Searchable/sortable list over the server's schedules (+ total count).
/// Sortables follow the resource: only_when_online, last_run_at, next_run_at.
async fn rows(p: &ServerPanel, lp: &ListParams) -> Result<(Vec<schedules::Model>, u64), WebError> {
    let b = p.st.db.get_database_backend();
    let dir = if lp.desc { "desc" } else { "asc" };
    let order = match lp.sort.as_deref() {
        Some("only_when_online") => format!("{{only_when_online}} {dir}, {{id}} asc"),
        Some("last_run_at") => format!("{{last_run_at}} {dir}"),
        Some("next_run_at") => format!("{{next_run_at}} {dir}"),
        _ => "{id} asc".to_string(),
    };
    let mut where_sql = "where {schedules.server_id} = ?".to_string();
    let mut vals = vec![p.server.id.into()];
    if !lp.search.is_empty() {
        where_sql.push_str(" and LOWER({name}) LIKE ?");
        vals.push(format!("%{}%", lp.search.to_lowercase()).into());
    }
    let mut sql = format!("select {{schedules}}.* from {{schedules}} {where_sql} order by {order}");
    if let Some((limit, offset)) = lp.limit_offset() {
        sql.push_str(&format!(" limit {limit} offset {offset}"));
    }
    let found = schedules::Model::find_by_statement(raw::stmt(b, &sql, vals.clone())).all(&p.st.db).await?;
    let count_sql = format!("select count(*) as {{aggregate}} from {{schedules}} {where_sql}");
    let total = Aggregate::find_by_statement(raw::stmt(b, &count_sql, vals))
        .one(&p.st.db)
        .await?
        .map(|a| a.aggregate.0.max(0) as u64)
        .unwrap_or(0);
    Ok((found, total))
}

/// `ScheduleStatus::getColor()`.
fn status_color(active: bool, processing: bool) -> &'static str {
    if !active {
        "danger"
    } else if processing {
        "warning"
    } else {
        "success"
    }
}

async fn render_list(p: ServerPanel, lp: ListParams) -> WebResult<Response> {
    let base = p.base_url();
    let url = format!("{base}/schedules");
    let opts = [10, 25, 50, 0];
    let (models, total) = rows(&p, &lp).await?;

    let can_update = p.can("schedule.update");
    let locale = p.locale();
    let t = |k: &str| p.t(&format!("server/schedule.{k}"));

    let mut table = Table::new("schedules", &url);
    table.columns = vec![
        Column::new("name", t("name")),
        Column::new("cron", t("cron")),
        Column::new("status", t("status")),
        Column::new("online_only", t("online_only")).sortable(),
        Column::new("last_run", t("last_run")).sortable(),
        Column::new("next_run", t("next_run")).sortable(),
    ];
    let now = Utc::now();
    for s in &models {
        let ago = |at: Option<Timestamp>| -> String {
            at.map(|c| c.0.and_utc())
                .map(|t| crate::views::diff_for_humans(&locale, t, now))
                .unwrap_or_else(|| t("never"))
        };
        // DateTimeColumn::since(); next_run only while the schedule is active.
        let status_badge = Cell::badge(
            t(&format!("schedule_status.{}", status_key(s))),
            status_color(s.is_active.0, s.is_processing.0),
        );
        let next_cell = if !s.is_active.0 { Cell::Empty } else { Cell::text(ago(s.next_run_at)) };
        let mut row = Row::new(s.id.0.to_string())
            .cell("name", Cell::text(s.name.clone()))
            .cell(
                "cron",
                Cell::text_desc(
                    format!(
                        "{} {} {} {} {}",
                        s.cron_minute, s.cron_hour, s.cron_day_of_month, s.cron_month, s.cron_day_of_week
                    ),
                    Some(s.timezone.clone()),
                ),
            )
            .cell("status", status_badge)
            .cell("online_only", Cell::Bool { value: s.only_when_online.0 })
            .cell("last_run", Cell::text(ago(s.last_run_at)))
            .cell("next_run", next_cell);

        // recordActions: View (when not editable), Edit, Delete, inline.
        if can_update {
            row.actions.push(Action::link(
                p.t("filament-actions::edit.single.label"),
                "edit",
                format!("{url}/{}/edit", s.id.0),
            ));
        } else {
            row.actions.push(Action::link(
                p.t("filament-actions::view.single.label"),
                "eye",
                format!("{url}/{}", s.id.0),
            ));
        }
        if p.can("schedule.delete") {
            row.actions.push(
                Action::post(p.t("filament-actions::delete.single.label"), "trash", format!("{url}/{}/delete", s.id.0))
                    .color("danger")
                    .confirm(p.t("filament-actions::modal.confirmation"))
                    .field("_back", lp.url(&url, |_| {})),
            );
        }
        table.rows.push(row);
    }
    table.empty_icon = "clock".into();
    table.empty_heading = t("title");
    table.searchable = true;
    table.finish(&lp, total, &opts, &locale);

    p.page(
        "schedules",
        "server/schedules.html",
        context! {
            title => t("title"),
            base => base,
            list_url => url,
            table => Mj::from_serialize(&table),
            can_import => p.can("schedule.create"),
        },
    )
}

fn parse_lp(q: Option<&str>) -> ListParams {
    ListParams::parse(q, &[10, 25, 50, 0], 10)
}

/// `GET /server/{server}/schedules`.
async fn index(p: ServerPanel) -> WebResult<Response> {
    p.authorize("schedule.read")?;
    let lp = parse_lp(p.query.as_deref());
    render_list(p, lp).await
}

// ----- sharing: export / import -------------------------------------------

/// PHP `Str::afterLast($haystack, $needle)` … `Str::before('…', '.json')` for
/// imported schedule names.
fn import_name(raw: &str) -> String {
    let hay = if let Some(idx) = raw.rfind("schedule-") { raw.split_at(idx + "schedule-".len()).1 } else { raw };
    let head = hay.split_once(".json").map(|(a, _)| a).unwrap_or(hay);
    headline(head)
}

/// PHP `Str::headline`.
fn headline(s: &str) -> String {
    s.split(['-', '_', ' ', '.'])
        .flat_map(split_camel)
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_camel(w: &str) -> Vec<&str> {
    let bytes = w.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    for i in 1..bytes.len() {
        if bytes[i].is_ascii_uppercase() && bytes[i - 1].is_ascii_lowercase() {
            parts.push(&w[start..i]);
            start = i;
        }
    }
    parts.push(&w[start..]);
    parts
}

/// PHP `Str::kebab($name)->lower()->trim()` for the export file name.
fn kebab(s: &str) -> String {
    let mut out = String::new();
    let prev = |out: &str| out.chars().last().is_none_or(|c| c == '-');
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c.is_alphabetic() || c.is_numeric() {
            if !prev(&out) && c.is_uppercase() {
                out.push('-');
            }
            for l in c.to_lowercase() {
                out.push(l);
            }
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// `ScheduleExporterService::handle()` output, pretty-printed.
#[derive(Serialize)]
struct ExportedTask<'a> {
    sequence_id: i64,
    action: &'a str,
    payload: &'a str,
    time_offset: i64,
    continue_on_failure: bool,
}

#[derive(Serialize)]
struct ExportedSchedule<'a> {
    name: &'a str,
    is_active: bool,
    only_when_online: bool,
    cron_minute: &'a str,
    cron_hour: &'a str,
    cron_day_of_month: &'a str,
    cron_month: &'a str,
    cron_day_of_week: &'a str,
    timezone: &'a str,
    tasks: Vec<ExportedTask<'a>>,
}

/// `GET …/export` — `ExportScheduleAction`: streamed download.
async fn export(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("schedule.read")?;
    let s = find(&p, id).await?;
    let ts = tasks_of(&p.st.db, s.id.0).await?;
    let doc = ExportedSchedule {
        name: &s.name,
        is_active: s.is_active.0,
        only_when_online: s.only_when_online.0,
        cron_minute: &s.cron_minute,
        cron_hour: &s.cron_hour,
        cron_day_of_month: &s.cron_day_of_month,
        cron_month: &s.cron_month,
        cron_day_of_week: &s.cron_day_of_week,
        timezone: &s.timezone,
        tasks: ts
            .iter()
            .map(|t| ExportedTask {
                sequence_id: t.sequence_id.0,
                action: &t.action,
                payload: &t.payload,
                time_offset: t.time_offset.0,
                continue_on_failure: t.continue_on_failure.0,
            })
            .collect(),
    };
    let mut body = serde_json::to_string_pretty(&doc).unwrap_or_default();
    body.push('\n');
    let mut resp = (axum::http::StatusCode::OK, body).into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));
    if let Ok(v) =
        header::HeaderValue::from_str(format!("attachment; filename=\"schedule-{}.json\"", kebab(&s.name)).as_str())
    {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// One import candidate: fetched URL bytes or an uploaded file.
struct ImportSource {
    data: Vec<u8>,
}

/// Parsed multipart request of the Import tab (`ImportScheduleAction.schema`):
/// the `urls` repeater flattened and the uploaded `files[]`.
struct ImportBody {
    urls: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
}

fn boundary(ct: &str) -> Option<&str> {
    ct.split(';').filter_map(|p| p.trim().strip_prefix("boundary=")).next().map(|b| b.trim_matches('"'))
}

/// Extract `(name, filename, data)` parts of a multipart body.
fn multipart_parts(boundary: &str, body: &[u8]) -> Vec<(String, Option<String>, Vec<u8>)> {
    let sep = format!("--{boundary}").into_bytes();
    let mut marks = Vec::new();
    let mut at = 0usize;
    while let Some(p) = find_bytes(&body[at..], &sep) {
        let head = at + p;
        marks.push(head);
        at = head + sep.len();
        if at >= body.len() {
            break;
        }
    }
    let mut out = Vec::new();
    for pair in marks.windows(2) {
        let (s, e) = (pair[0] + sep.len(), pair[1]);
        if e > s
            && let Some(part) = parse_part(&body[s..e])
        {
            out.push(part);
        }
    }
    if let Some(last) = marks.last() {
        let s = last + sep.len();
        if s < body.len()
            && let Some(part) = parse_part(&body[s..])
        {
            out.push(part);
        }
    }
    out
}

/// One `--boundary` section (`\r\n` + headers + blank line + data).
fn parse_part(chunk: &[u8]) -> Option<(String, Option<String>, Vec<u8>)> {
    let chunk = chunk.strip_prefix(b"\r\n").unwrap_or(chunk);
    if chunk.starts_with(b"--") || chunk.is_empty() {
        return None;
    }
    let headers_end = find_bytes(chunk, b"\r\n\r\n")?;
    let mut data = chunk[headers_end + 4..].to_vec();
    while data.last() == Some(&b'\n') || data.last() == Some(&b'\r') {
        data.pop();
    }
    let disp = find_line(&chunk[..headers_end], b"Content-Disposition:");
    let name = part_param(disp, "name=").unwrap_or_default();
    Some((name, part_param(disp, "filename="), data))
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len().max(1)).position(|w| w.starts_with(needle))
}

fn find_line<'a>(headers: &'a [u8], prefix: &'a [u8]) -> &'a [u8] {
    for line in headers.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(prefix) {
            return line;
        }
    }
    b""
}

/// The quoted value after `key=` in a Content-Disposition header.
fn part_param(disp: &[u8], key: &str) -> Option<String> {
    let idx = find_bytes(disp, key.as_bytes())?;
    let tail = &disp[idx + key.len()..];
    let tail = match tail.first() {
        Some(b'"') => &tail[1..],
        _ => tail,
    };
    let len = tail.iter().position(|b| matches!(b, b'"' | b';' | b'\r')).unwrap_or(tail.len());
    Some(String::from_utf8_lossy(&tail[..len]).into_owned())
}

impl FromRequest<AppState> for ImportBody {
    type Rejection = WebError;

    async fn from_request(req: Request, st: &AppState) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let bytes = to_bytes(body, 16 * 1024 * 1024).await.map_err(|_| WebError::PayloadTooLarge)?;
        let ct = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        let mut urls = Vec::new();
        let mut files = Vec::new();
        if let Some(b) = boundary(ct) {
            for (name, filename, data) in multipart_parts(b, &bytes) {
                match (name.as_str(), filename) {
                    (_, Some(file)) => files.push((file, data)),
                    ("_token", _) | ("_back", _) => {}
                    (_, None) if name.starts_with("urls[") && name.ends_with("[url]") => {
                        let url = String::from_utf8_lossy(&data).trim().to_string();
                        if !url.is_empty() {
                            urls.push(url);
                        }
                    }
                    _ => {}
                }
            }
        }
        let _ = st;
        // `array_merge(collect(urls)->flatten()->unique, files)` — URLs first,
        // de-duplicated preserving order.
        let mut uniq: Vec<String> = Vec::new();
        for u in urls {
            if !uniq.contains(&u) {
                uniq.push(u);
            }
        }
        Ok(ImportBody { urls: uniq, files })
    }
}

/// `(bool)`-cast semantics of Laravel's `boolean` model cast.
fn php_bool(v: Option<&serde_json::Value>) -> bool {
    v.is_some_and(|v| php_bool_val(v))
}

fn php_bool_val(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        serde_json::Value::String(s) => !s.is_empty() && s != "0",
        _ => false,
    }
}

/// `ScheduleImporterService::fromFile()` core: decode + transactional insert.
async fn import_one(p: &ServerPanel, src: ImportSource) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_slice(&src.data).map_err(|e| format!("Could not read JSON file: {e}"))?;
    let v = |k: &str| parsed.get(k).cloned();
    let s = |k: &str, default: &str| {
        v(k).and_then(|x| x.as_str().map(str::to_string)).unwrap_or_else(|| default.to_string())
    };

    let txn = p.st.db.begin().await.map_err(|e| e.to_string())?;
    let tz = v("timezone").and_then(|x| x.as_str().map(str::to_string)).unwrap_or_else(|| "UTC".to_string());
    let minute = s("cron_minute", "0");
    let hour = s("cron_hour", "0");
    let day_of_month = s("cron_day_of_month", "*");
    let month = s("cron_month", "*");
    let day_of_week = s("cron_day_of_week", "*");
    let next = next_run_date(&minute, &hour, &day_of_month, &month, &day_of_week, &tz)
        .map_err(|_| "The cron data provided does not evaluate to a valid expression.".to_string())?;
    let now = Timestamp(pelican_core::time::now());
    let created = pelican_db::write::insert(
        &txn,
        schedules::ActiveModel {
            name: Set(s("name", "")),
            cron_day_of_week: Set(day_of_week),
            cron_month: Set(month),
            cron_day_of_month: Set(day_of_month),
            cron_hour: Set(hour),
            cron_minute: Set(minute),
            timezone: Set(tz),
            is_active: Set(Bool(v("is_active").map(|x| php_bool(Some(&x))).unwrap_or(true))),
            is_processing: Set(Bool(false)),
            only_when_online: Set(SmallBool(v("only_when_online").map(|x| php_bool(Some(&x))).unwrap_or(false))),
            server_id: Set(p.server.id),
            next_run_at: Set(Some(Timestamp(next))),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut created_tasks: Vec<tasks::Model> = Vec::new();
    for task in v("tasks").and_then(|x| x.as_array().cloned()).unwrap_or_default() {
        let num = |k: &str, default: i64| -> i64 {
            match task.get(k) {
                Some(x @ serde_json::Value::Number(_)) => x.as_i64().unwrap_or(default),
                Some(serde_json::Value::String(sv)) => php_intval(sv),
                _ => default,
            }
        };
        let t = pelican_db::write::insert(
            &txn,
            tasks::ActiveModel {
                schedule_id: Set(created.id),
                sequence_id: Set(Int(num("sequence_id", 0))),
                action: Set(s_via(&task, "action")),
                payload: Set(s_via(&task, "payload")),
                time_offset: Set(Int(num("time_offset", 0))),
                is_queued: Set(Bool(false)),
                continue_on_failure: Set(SmallBool(task.get("continue_on_failure").is_some_and(php_bool_val))),

                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        created_tasks.push(t);
    }
    pelican_api::jobs::touch_schedule(&txn, created.id.0)
        .await
        .map_err(|e| e.errors.first().map(|o| o.detail.clone()).unwrap_or_else(|| e.status.to_string()))?;
    txn.commit().await.map_err(|e| e.to_string())?;
    // After the transaction: ScheduleImporterService → `Schedule::create` and
    // `Task::create` per task (created events).
    model_events::created(&p.st, &created).await;
    for t in &created_tasks {
        model_events::created(&p.st, t).await;
    }
    Ok(())
}

fn s_via(v: &serde_json::Value, k: &str) -> String {
    match v.get(k) {
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => (*b as i64).to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `POST …/import` — `ImportScheduleAction`: process every source, report
/// failed/successful names like the original notifications.
async fn import(p: ServerPanel, body: ImportBody) -> WebResult<Response> {
    p.authorize("schedule.create")?;
    let list_url = format!("{}/schedules", p.base_url());
    if body.urls.is_empty() && body.files.is_empty() {
        return Ok(p.redirect(&list_url));
    }

    // URL entries first (`array_merge(collect(urls)->…unique, files)`); github
    // blob links are rewritten to their raw counterpart before fetching.
    let mut success = Vec::new();
    let mut failed = Vec::new();
    for url in &body.urls {
        let name = import_name(&file_name_of(url));
        let fixed = if url.contains("github.com") { url.replacen("blob", "raw", 1) } else { url.clone() };
        let outcome = match fetch_url(&fixed).await {
            Ok(data) => import_one(&p, ImportSource { data }).await,
            Err(detail) => Err(detail),
        };
        match outcome {
            Ok(()) => success.push(name),
            Err(detail) => {
                tracing::warn!(%detail, "schedule import failed");
                failed.push(name);
            }
        }
    }
    for (filename, data) in &body.files {
        let name = import_name(filename);
        match import_one(&p, ImportSource { data: data.clone() }).await {
            Ok(()) => success.push(name),
            Err(detail) => {
                tracing::warn!(%detail, "schedule import failed");
                failed.push(name);
            }
        }
    }

    if !failed.is_empty() {
        p.flash_danger(p.t("server/schedule.import_action.import_failed"), Some(failed.join(", ")));
    }
    if !success.is_empty() {
        p.flash_success(format!("{}: {}", p.t("server/schedule.import_action.import_success"), success.join(", ")));
    }
    Ok(p.redirect(&list_url))
}

async fn fetch_url(url: &str) -> Result<Vec<u8>, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(1))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map(|b| b.into())
        .map_err(|e| e.to_string())
}

/// Last path segment carrying a file name (`basename($url)`).
fn file_name_of(url: &str) -> &str {
    let trimmed = url.split(['?', '#']).next().unwrap_or(url);
    trimmed.rsplit('/').find(|s| !s.is_empty()).unwrap_or(trimmed)
}

// ----- create / edit / view ------------------------------------------------

/// Submitted (or default) form values echoed back through re-renders.
#[derive(Clone)]
struct FormView {
    name: String,
    is_active: bool,
    only_when_online: bool,
    minute: String,
    hour: String,
    day_of_month: String,
    month: String,
    day_of_week: String,
    errors: Errors,
    cron_invalid: bool,
}

impl FormView {
    /// Resource defaults: minute `*/5`, everything else `*`, toggles checked.
    fn defaults() -> Self {
        FormView {
            name: String::new(),
            is_active: true,
            only_when_online: true,
            minute: "*/5".into(),
            hour: "*".into(),
            day_of_month: "*".into(),
            month: "*".into(),
            day_of_week: "*".into(),
            errors: Errors::default(),
            cron_invalid: false,
        }
    }

    fn from_model(m: &schedules::Model) -> Self {
        FormView {
            name: m.name.clone(),
            is_active: m.is_active.0,
            only_when_online: m.only_when_online.0,
            minute: m.cron_minute.clone(),
            hour: m.cron_hour.clone(),
            day_of_month: m.cron_day_of_month.clone(),
            month: m.cron_month.clone(),
            day_of_week: m.cron_day_of_week.clone(),
            errors: Errors::default(),
            cron_invalid: false,
        }
    }

    /// Echo the posted values (missing checkboxes become unchecked).
    fn capture(form: &Form) -> Self {
        let s = |k: &str| form.opt(k).unwrap_or("").to_string();
        FormView {
            name: s("name"),
            is_active: form.bool("is_active"),
            only_when_online: form.bool("only_when_online"),
            minute: s("cron_minute"),
            hour: s("cron_hour"),
            day_of_month: s("cron_day_of_month"),
            month: s("cron_month"),
            day_of_week: s("cron_day_of_week"),
            errors: Errors::default(),
            cron_invalid: false,
        }
    }

    /// `StoreScheduleRequest::rules()` under the model keys
    /// (`Schedule::$validationRules`) with Filament's lowercased labels.
    fn validate(&self, p: &ServerPanel) -> Errors {
        let json = serde_json::json!({
            "name": blank_null(&self.name),
            "cron_minute": blank_null(&self.minute),
            "cron_hour": blank_null(&self.hour),
            "cron_day_of_month": blank_null(&self.day_of_month),
            "cron_month": blank_null(&self.month),
            "cron_day_of_week": blank_null(&self.day_of_week),
            "is_active": self.is_active,
            "only_when_online": self.only_when_online,
        });
        let mut v = Validator::for_value(&json);
        v.attribute_name("name", &p.t("server/schedule.name").to_lowercase());
        v.attribute_name("cron_minute", &p.t("server/schedule.time.minute").to_lowercase());
        v.attribute_name("cron_hour", &p.t("server/schedule.time.hour").to_lowercase());
        v.attribute_name("cron_day_of_month", &p.t("server/schedule.time.day_of_month").to_lowercase());
        v.attribute_name("cron_month", &p.t("server/schedule.time.month").to_lowercase());
        v.attribute_name("cron_day_of_week", &p.t("server/schedule.time.day_of_week").to_lowercase());
        v.attribute_name("is_active", &p.t("server/schedule.enabled").to_lowercase());
        v.attribute_name("only_when_online", &p.t("server/schedule.only_online").to_lowercase());
        v.rules("name", &["required", "string", "max:255"]);
        v.rules("is_active", &["filled", "boolean"]);
        v.rules("only_when_online", &["boolean"]);
        for f in ["cron_minute", "cron_hour", "cron_day_of_month", "cron_month", "cron_day_of_week"] {
            v.rules(f, &["required", "string"]);
        }
        v.run();
        Errors::from_validator(&v)
    }

    /// `EditSchedule::mutateFormDataBeforeSave()` / `getNextRun()`; `true` when
    /// the expression evaluated.
    fn compute_next(&self, tz: &str) -> Option<chrono::NaiveDateTime> {
        next_run_date(&self.minute, &self.hour, &self.day_of_month, &self.month, &self.day_of_week, tz).ok()
    }
}

fn blank_null(s: &str) -> serde_json::Value {
    let t = s.trim();
    if t.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(t.to_string()) }
}

fn status_key(s: &schedules::Model) -> &'static str {
    if !s.is_active.0 {
        "inactive"
    } else if s.is_processing.0 {
        "processing"
    } else {
        "active"
    }
}

/// Next run date formatted in the viewer's timezone (`$nextRun->timezone(tz)`).
fn next_run_display(p: &ServerPanel, view: &FormView) -> String {
    match view.compute_next(&p.timezone()) {
        Some(next) => {
            let tz: chrono_tz::Tz = p.timezone().parse().unwrap_or(chrono_tz::UTC);
            next.and_utc().with_timezone(&tz).format("%Y-%m-%d %H:%M:%S").to_string()
        }
        None => p.t("server/schedule.invalid"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn render_form(p: &ServerPanel, mode: &str, view: FormView, dialog_state: DialogState) -> WebResult<Response> {
    let s = dialog_state.schedule.clone();
    let base = p.base_url();
    let tz = p.timezone();
    let can_update = p.can("schedule.update");
    let can_delete = p.can("schedule.delete");

    let mut task_list: Vec<tasks::Model> = Vec::new();
    let mut task_limit_reached = false;
    let sync_queue = p.st.config.env.string_or("QUEUE_CONNECTION", "database") == "sync";
    if s.is_some() {
        task_list = tasks_of(&p.st.db, s.as_ref().unwrap().id.0).await?;
        let limit = p.st.config.env.int_or("PANEL_PER_SCHEDULE_TASK_LIMIT", 10);
        task_limit_reached = task_list.len() as i64 >= limit;
    }
    let first_task_id = task_list.iter().map(|t| t.id.0).min();

    // The relation-manager table (ordered by sequence_id, reorderable via
    // per-row move actions).
    let mut tasks_table = Table::new("schedule-tasks", "");
    if !task_list.is_empty() {
        tasks_table.columns = vec![
            Column::new("action", p.t("server/schedule.tasks.actions.title")),
            Column::new("payload", p.t("server/schedule.tasks.payload")),
        ];
        if !sync_queue {
            tasks_table.columns.push(Column::new("time_offset", p.t("server/schedule.tasks.time_offset")));
        }
        tasks_table.columns.push(Column::new("continue_on_failure", p.t("server/schedule.tasks.continue_on_failure")));
        let mut ordered = task_list.clone();
        ordered.sort_by_key(|t| t.sequence_id.0);
        let sec_label =
            |n: i64| crate::i18n::trans_choice(&p.locale(), "server/schedule.tasks.seconds", (n.max(0)) as f64, &[]);
        for t in &ordered {
            let label = schema_label(p, &t.action).unwrap_or_else(|| t.action.clone());
            let mut row =
                Row::new(t.id.0.to_string()).cell("action", Cell::text(label)).cell("payload", payload_cell(p, t));
            if !sync_queue {
                let text = if first_task_id == Some(t.id.0) {
                    p.t("server/schedule.tasks.first_task")
                } else {
                    format!("{} {}", t.time_offset.0, sec_label(t.time_offset.0))
                };
                row = row.cell("time_offset", Cell::text(text));
            }
            row = row.cell("continue_on_failure", Cell::Bool { value: t.continue_on_failure.0 });
            if can_update {
                let turl = format!("{base}/schedules/{}/tasks/{}", s.as_ref().unwrap().id.0, t.id.0);
                row.actions.push(Action::link(
                    p.t("filament-actions::edit.single.label"),
                    "edit",
                    format!("{turl}/edit"),
                ));
                if ordered.len() > 1 {
                    row.actions.push(Action::post("", "chevron-up", format!("{turl}/move")).field("dir", "up"));
                    row.actions.push(Action::post("", "chevron-down", format!("{turl}/move")).field("dir", "down"));
                }
            }
            if p.can("schedule.delete") {
                row.group.push(
                    Action::post(
                        p.t("filament-actions::delete.single.label"),
                        "trash",
                        format!("{base}/schedules/{}/tasks/{}/delete", s.as_ref().unwrap().id.0, t.id.0),
                    )
                    .color("danger")
                    .confirm(p.t("filament-actions::modal.confirmation"))
                    .field("_back", dialog_state.back.clone()),
                );
            }
            tasks_table.rows.push(row);
        }
    }
    tasks_table.finish(&ListParams::default(), ordered_tasks_total(&task_list), &[10, 25, 50, 0], &p.locale());

    // Task dialogs: create + one per row, the requested one marked open.
    let create_errors = dialog_state.create_errors.clone();
    let mut dialogs = Vec::new();
    if mode != "create" && can_update {
        dialogs.push(TaskDialog::build(
            p,
            0,
            &dialog_state.create_action,
            &dialog_state.create_payload,
            dialog_state.create_offset.clone(),
            dialog_state.create_continue,
            first_task_id.is_none(),
            create_errors,
            dialog_state.open_create,
            "create-task",
        ));
        for t in &task_list {
            let tid = t.id.0;
            let open = dialog_state.open_edit == Some(tid);
            let errors = if open { dialog_state.edit_errors.clone() } else { Errors::default() };
            let d = TaskDialog::build(
                p,
                tid,
                &t.action,
                &t.payload,
                t.time_offset.0.to_string(),
                t.continue_on_failure.0,
                first_task_id == Some(tid),
                errors,
                open,
                &format!("task-edit-{tid}"),
            );
            dialogs.push(d);
        }
    }

    let status_value = s.as_ref().map(status_key).unwrap_or("active");
    let status_label = p.t(&format!("server/schedule.schedule_status.{status_value}"));
    let run_now_blocked = task_list.is_empty() || s.as_ref().is_some_and(|m| m.is_processing.0);
    let run_tooltip = if task_list.is_empty() {
        p.t("server/schedule.no_tasks")
    } else if s.as_ref().is_some_and(|m| m.is_processing.0) {
        p.t("server/schedule.schedule_status.processing")
    } else {
        p.t("server/schedule.run_now")
    };
    let action_url = match mode {
        "edit" => format!("{base}/schedules/{}/edit", s.as_ref().unwrap().id.0),
        "view" => String::new(),
        _ => format!("{base}/schedules/create"),
    };

    p.page(
        "schedules",
        "server/schedules_form.html",
        context! {
            title => dialog_state.title,
            mode => mode,
            base => base.clone(),
            action => action_url,
            record => Mj::from_serialize(ScheduleRow::of(s.as_ref())),
            s_id => s.as_ref().map(|m| m.id.0),
            v_name => view.name,
            v_is_active => view.is_active,
            v_only => view.only_when_online,
            v_minute => view.minute,
            v_hour => view.hour,
            v_dom => view.day_of_month,
            v_month => view.month,
            v_dow => view.day_of_week,
            v_errors => Mj::from_serialize(&view.errors),
            cron_invalid => view.cron_invalid,
            cron_next => next_run_display(p, &view),
            user_tz => tz.clone(),
            readonly => mode == "view",
            can_update => can_update,
            can_delete => can_delete,
            run_now_blocked => run_now_blocked,
            run_tooltip => run_tooltip,
            status_value => status_value,
            status_label => status_label,
            tasks_count => task_list.len() as i64,
            tasks_table => Mj::from_serialize(&tasks_table),
            task_limit_reached => task_limit_reached,
            dialogs => Mj::from_serialize(&dialogs),
            back => dialog_state.back,
        },
    )
}

fn ordered_tasks_total(list: &[tasks::Model]) -> u64 {
    list.len() as u64
}

#[derive(Serialize)]
struct ScheduleRow {
    id: i64,
    name: String,
    timezone: String,
}

impl ScheduleRow {
    fn of(m: Option<&schedules::Model>) -> Option<ScheduleRow> {
        m.map(|m| ScheduleRow { id: m.id.0, name: m.name.clone(), timezone: m.timezone.clone() })
    }
}

/// Payload column (`formatPayload`): multi-line payloads become badges,
/// `power` uppercases the action, an empty payload shows the placeholder.
fn payload_cell(p: &ServerPanel, t: &tasks::Model) -> Cell {
    let tooltip = ACTION_LABELS
        .iter()
        .find(|(a, _)| *a == t.action)
        .map(|(_, k)| p.t(k))
        .or_else(|| plugin_task(&t.action).and_then(|(_, pt)| pt.payload_label));
    if t.payload.is_empty() {
        return Cell::text(p.t("server/schedule.tasks.no_payload"));
    }
    match t.action.as_str() {
        "power" => Cell::Badge { text: ucfirst(&t.payload), color: "primary".into(), icon: None, tooltip, copy: None },
        _ => Cell::Badges { items: payload_items(&t.action, &t.payload), color: "primary".into() },
    }
}

fn ucfirst(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

/// One task-form dialog: the shared `action` select plus one live payload
/// control per schema kind (the inactive ones render disabled so exactly one
/// `payload` value is posted), `time_offset` and `continue_on_failure`.
#[derive(Serialize, Clone)]
struct TaskDialog {
    id: i64,
    dom_id: String,
    action: String,
    payload_command: String,
    payload_area: String,
    payload_power: String,
    /// The `action` select options (`TaskService::getMappings()`).
    options: Vec<(String, String)>,
    /// `disableOptionWhen`: a backup task needs `backup_limit > 0`
    /// (`CreateBackupSchema::canCreate`).
    backup_allowed: bool,
    offset: String,
    cont: bool,
    open: bool,
    edit: bool,
    errors: IndexMap<String, String>,
}

impl TaskDialog {
    #[allow(clippy::too_many_arguments)]
    fn build(
        p: &ServerPanel,
        id: i64,
        action: &str,
        payload: &str,
        offset: String,
        cont: bool,
        _first: bool,
        errors: Errors,
        open: bool,
        dom_id: &str,
    ) -> TaskDialog {
        TaskDialog {
            id,
            dom_id: dom_id.into(),
            action: action.into(),
            payload_command: match action {
                "command" => payload.into(),
                _ => String::new(),
            },
            payload_area: match action {
                "backup" | "delete_files" => payload.into(),
                _ => String::new(),
            },
            payload_power: match action {
                "power" => payload.into(),
                _ => ACTIONS.iter().find(|(a, _)| *a == "power").and_then(|(_, d)| *d).unwrap_or("restart").into(),
            },
            options: action_options(p),
            backup_allowed: p.server.backup_limit.0 > 0,
            offset,
            cont,
            open,
            edit: id != 0,
            errors: errors.0,
        }
    }
}

/// Mutable pieces carried between the render of a form page and its re-render
/// after a failed submit / deep-linked dialog.
struct DialogState {
    schedule: Option<schedules::Model>,
    title: String,
    back: String,
    open_create: bool,
    create_action: String,
    create_payload: String,
    create_offset: String,
    create_continue: bool,
    create_errors: Errors,
    open_edit: Option<i64>,
    edit_errors: Errors,
}

impl DialogState {
    fn fresh(title: String, back: String) -> Self {
        DialogState {
            schedule: None,
            title,
            back,
            open_create: false,
            create_action: ACTIONS.first().map(|(a, _)| *a).unwrap_or("command").into(),
            create_payload: String::new(),
            create_offset: "0".into(),
            create_continue: false,
            create_errors: Errors::default(),
            open_edit: None,
            edit_errors: Errors::default(),
        }
    }
}

/// The `action` select options of the task form (`TaskService::getMappings()`).
pub fn action_options(p: &ServerPanel) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = ACTIONS
        .iter()
        .map(|(id, _)| ((*id).to_string(), p.t(&format!("server/schedule.tasks.actions.{id}.title"))))
        .collect();
    // Task types of WASM plugins (`TaskService::register`).
    for (plugin, t) in pelican_api::sidecar::all_task_types() {
        out.push((t.id.clone(), crate::plugin_pages::label(&p.st, &plugin, &p.locale(), &t.name)));
    }
    out
}

/// Plugin task type by id: `(plugin, type)`.
fn plugin_task(action: &str) -> Option<(String, pelican_plugins::runtime::TaskType)> {
    pelican_api::sidecar::all_task_types().into_iter().find(|(_, t)| t.id == action)
}

/// `GET /schedules/create` — `CreateSchedule`.
async fn create_page(p: ServerPanel) -> WebResult<Response> {
    p.authorize("schedule.create")?;
    let title = p.t_with("filament-panels::resources/pages/create-record.title", &[("label", "Schedule")]);
    let state = DialogState::fresh(title.clone(), format!("{}/schedules", p.base_url()));
    render_form(&p, "create", FormView::defaults(), state).await
}

/// `POST /schedules/create` — validation, invalid-cron halt, insert and the
/// `server:schedule.create` activity.
async fn create_submit(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("schedule.create")?;
    let list_url = format!("{}/schedules", p.base_url());
    let view = FormView::capture(&form);
    let errors = view.validate(&p);
    if !errors.is_empty() {
        let title = p.t_with("filament-panels::resources/pages/create-record.title", &[("label", "Schedule")]);
        let mut state = DialogState::fresh(title, list_url.clone());
        state.back = list_url;
        return render_form(&p, "create", FormView { errors, ..view }, state).await;
    }
    let Some(next) = view.compute_next(&p.timezone()) else {
        // `ScheduleResource::getNextRun()` flashes the notification and halts.
        let title = p.t_with("filament-panels::resources/pages/create-record.title", &[("label", "Schedule")]);
        let state = DialogState::fresh(title, list_url.clone());
        return render_form(&p, "create", FormView { cron_invalid: true, ..view }, state).await;
    };

    let now = Timestamp(pelican_core::time::now());
    let am = schedules::ActiveModel {
        name: Set(view.name.clone()),
        cron_day_of_week: Set(view.day_of_week.clone()),
        cron_month: Set(view.month.clone()),
        cron_day_of_month: Set(view.day_of_month.clone()),
        cron_hour: Set(view.hour.clone()),
        cron_minute: Set(view.minute.clone()),
        timezone: Set(p.timezone()),
        is_active: Set(Bool(view.is_active)),
        is_processing: Set(Bool(false)),
        only_when_online: Set(SmallBool(view.only_when_online)),
        server_id: Set(p.server.id),
        next_run_at: Set(Some(Timestamp(next))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let created = pelican_db::write::insert(&p.st.db, am).await?;
    // CreateRecord → `Schedule::create($data)` (created event; no transaction).
    model_events::created(&p.st, &created).await;
    log_act(
        &p,
        "server:schedule.create",
        &[(morph::SCHEDULE, created.id.0)],
        vec![("name", created.name.clone().into())],
    )
    .await?;
    p.flash_success(p.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(p.redirect(&list_url))
}

/// The schedule behind view/edit requests.
async fn load_record(p: &ServerPanel, id: i64) -> Result<schedules::Model, WebError> {
    find(p, id).await
}

fn page_title(mode: &str, s: &Option<schedules::Model>) -> String {
    match mode {
        "create" => "Create Schedule".to_string(),
        _ => match s {
            Some(s) => s.name.clone(),
            None => "Schedule".to_string(),
        },
    }
}

/// `GET /schedules/{id}` — `ViewSchedule` (readonly schema + tasks manager).
async fn view_page(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("schedule.read")?;
    let s = load_record(&p, id).await?;
    let back = format!("{}/schedules/{}", p.base_url(), s.id.0);
    let mut state = DialogState::fresh(page_title("view", &Some(s.clone())), back);
    state.schedule = Some(s);
    render_form(&p, "view", FormView::from_model(state.schedule.as_ref().unwrap()), state).await
}

/// `GET /schedules/{id}/edit` — `EditSchedule`.
async fn edit_page(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = load_record(&p, id).await?;
    let back = format!("{}/schedules/{}/edit", p.base_url(), s.id.0);
    let mut state = DialogState::fresh(page_title("edit", &Some(s.clone())), back);
    state.schedule = Some(s);
    render_form(&p, "edit", FormView::from_model(state.schedule.as_ref().unwrap()), state).await
}

/// `POST /schedules/{id}/edit` — the same dirty-update the Client API
/// `update` performs, plus the `timezone` of `mutateFormDataBeforeSave`.
async fn edit_submit(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let back = format!("{}/schedules/{id}/edit", p.base_url());
    let s = load_record(&p, id).await?;
    let view = FormView::capture(&form);
    let errors = view.validate(&p);
    if !errors.is_empty() {
        let state = DialogState::fresh(s.name.clone(), back);
        return render_form(&p, "edit", FormView { errors, ..view }, state).await;
    }
    let Some(next) = view.compute_next(&s.timezone) else {
        let state = DialogState::fresh(s.name.clone(), back);
        return render_form(&p, "edit", FormView { cron_invalid: true, ..view }, state).await;
    };

    // Only dirty attributes are written (plus updated_at when anything is).
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
    set_if!(name, view.name.clone());
    set_if!(cron_day_of_week, view.day_of_week.clone());
    set_if!(cron_month, view.month.clone());
    set_if!(cron_day_of_month, view.day_of_month.clone());
    set_if!(cron_hour, view.hour.clone());
    set_if!(cron_minute, view.minute.clone());
    set_if!(timezone, p.timezone());
    set_if!(is_active, Bool(view.is_active));
    set_if!(only_when_online, SmallBool(view.only_when_online));
    if s.next_run_at.map(|n| n.0.format("%F %T").to_string()) != Some(next.format("%F %T").to_string()) {
        am.next_run_at = Set(Some(Timestamp(next)));
        dirty = true;
    }
    if s.is_active.0 != view.is_active && s.is_processing.0 {
        am.is_processing = Set(Bool(false));
        dirty = true;
    }
    if dirty {
        am.updated_at = Set(Some(Timestamp(pelican_core::time::now())));
        pelican_db::write::update(&p.st.db, &am).await?;
        // Dirty-update semantics (Client API `$schedule->update(...)` / EditRecord):
        // the `dirty` guard matches Eloquent isDirty.
        model_events::fire_id::<schedules::Entity>(&p.st, model_events::Action::Updated, id).await;
    }
    log_act(
        &p,
        "server:schedule.update",
        &[(morph::SCHEDULE, s.id.0)],
        vec![("name", view.name.clone().into()), ("active", view.is_active.into())],
    )
    .await?;
    p.flash_success(p.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(p.redirect(&back))
}

// ----- row actions ---------------------------------------------------------

/// `POST …/{id}/delete` — `DeleteAction` reused from the Client API handler
/// (authorization, delete + `server:schedule.delete` activity included).
async fn delete_schedule(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    let list_url = format!("{}/schedules", p.base_url());
    let back = form.back(&list_url);
    let ctx = api_ctx(&p)?;
    let bound = Bound { server: Some(p.server.clone()), schedule: Some(find(&p, id).await?), ..Default::default() };
    let bound = Arc::new(bound);
    let outcome =
        call(api::delete(axum::extract::State(p.st.clone()), Extension(ctx_with_bound(&ctx, bound))), |_| ()).await;
    notify_outcome(&p, outcome, p.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(p.redirect(&back))
}

/// Clone the ctx with a different `Bound` (api handlers read only Arc fields).
fn ctx_with_bound(ctx: &Ctx, bound: Arc<Bound>) -> Ctx {
    Ctx { bound, ..ctx.clone() }
}

fn notify_outcome<T>(p: &ServerPanel, outcome: ApiOutcome<T>, success: String) {
    match outcome {
        ApiOutcome::Done(_) => p.flash_success(success),
        ApiOutcome::Validation(_) => {}
        ApiOutcome::Failed(e) => {
            tracing::warn!(status=%e.status, detail=?first_detail(&e), "schedule api call failed");
            p.flash_danger(first_detail(&e).unwrap_or_else(|| pelican_api::error::GENERIC_500.into()), None);
        }
    }
}

/// `POST …/{id}/run` — `run_now` header action via `ProcessScheduleService`
/// (`api::execute`). No notification on success (the form is refilled in the
/// original); failures surface as a danger flash.
async fn run_now(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let back = form.back(&format!("{}/schedules/{id}/edit", p.base_url()));
    let ctx = api_ctx(&p)?;
    let bound =
        Arc::new(Bound { server: Some(p.server.clone()), schedule: Some(find(&p, id).await?), ..Default::default() });
    let outcome =
        call(api::execute(axum::extract::State(p.st.clone()), Extension(ctx_with_bound(&ctx, bound))), |_| ()).await;
    match outcome {
        ApiOutcome::Done(_) => {}
        ApiOutcome::Failed(e) => {
            tracing::warn!(status=%e.status, "schedule execution failed");
            p.flash_danger(first_detail(&e).unwrap_or_else(|| pelican_api::error::GENERIC_500.into()), None);
        }
        ApiOutcome::Validation(errors) => {
            tracing::warn!(errors = ?errors.0, "unexpected validation in execute");
        }
    }
    Ok(p.redirect(&back))
}

// ----- tasks (RelationManager) ---------------------------------------------

/// The `{task}` parameter next to `{id}` (RecordId covers the schedule only).
struct TaskParam(pub i64);

impl FromRequestParts<AppState> for TaskParam {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _st: &AppState) -> Result<Self, Self::Rejection> {
        let Path(params): Path<Vec<(String, String)>> =
            Path::from_request_parts(parts, _st).await.map_err(IntoResponse::into_response)?;
        params
            .iter()
            .find(|(k, _)| k == "task")
            .and_then(|(_, v)| v.parse().ok())
            .map(TaskParam)
            .ok_or_else(|| WebError::NotFound.into_response())
    }
}

/// Body of a task dialog submit translated to the API input (`In`), with the
/// Laravel middleware semantics applied per value (trim + empty → null).
fn task_input(form: &Form) -> Input {
    let action = blank_null(form.opt("action").unwrap_or(""));
    let payload = blank_null(form.opt("payload").unwrap_or(""));
    let offset = form.opt("time_offset").unwrap_or("").trim().to_string();
    let offset = if offset.is_empty() { "0".to_string() } else { offset };
    let cont = if form.bool("continue_on_failure") { "1".to_string() } else { "0".to_string() };
    Input::from_value(serde_json::json!({
        "action": action,
        "payload": payload,
        "time_offset": offset,
        "continue_on_failure": cont,
    }))
}

/// Local validation mirroring `StoreTaskRequest::rules()` for dialog errors
/// before the API handler re-validates.
fn validate_task_local(form: &Form, p: &ServerPanel) -> Errors {
    let json = serde_json::json!({
        "action": blank_null(form.opt("action").unwrap_or("")),
        "payload": blank_null(form.opt("payload").unwrap_or("")),
        "time_offset": form.opt("time_offset").map(|t| t.trim()).filter(|t| !t.is_empty()).map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
        "continue_on_failure": form.bool("continue_on_failure"),
    });
    let mut v = Validator::for_value(&json);
    v.attribute_name("action", &p.t("server/schedule.tasks.actions.title").to_lowercase());
    v.attribute_name("payload", &p.t("server/schedule.tasks.payload").to_lowercase());
    v.attribute_name("time_offset", &p.t("server/schedule.tasks.time_offset").to_lowercase());
    v.attribute_name("continue_on_failure", &p.t("server/schedule.tasks.continue_on_failure").to_lowercase());
    let mut actions: Vec<String> = ACTIONS.iter().map(|(a, _)| (*a).to_string()).collect();
    actions.extend(pelican_api::sidecar::all_task_types().into_iter().map(|(_, t)| t.id));
    let rule = format!("in:{}", actions.join(","));
    v.rules("action", &["required", rule.as_str()]);
    v.rules("payload", &["required_unless:action,backup", "string", "nullable"]);
    v.rules("time_offset", &["required", "numeric", "min:0", "max:900"]);
    v.rules("sequence_id", &["sometimes", "required", "numeric", "min:1"]);
    v.rules("continue_on_failure", &["sometimes", "required", "boolean"]);
    v.run();
    Errors::from_validator(&v)
}

async fn bound_full(p: &ServerPanel, sid: i64, tid: Option<i64>) -> Result<Arc<Bound>, WebError> {
    let s = find(p, sid).await?;
    let task = match tid {
        Some(id) => Some(
            tasks::Entity::find()
                .filter(tasks::Column::ScheduleId.eq(s.id))
                .filter(tasks::Column::Id.eq(Int(id)))
                .one(&p.st.db)
                .await?
                .ok_or(WebError::NotFound)?,
        ),
        None => None,
    };
    Ok(Arc::new(Bound { server: Some(p.server.clone()), schedule: Some(s), task, ..Default::default() }))
}

/// Per-row URL of the schedule page the tasks belong to.
fn schedule_back(p: &ServerPanel, sid: i64, mode_view: bool) -> String {
    if mode_view {
        format!("{}/schedules/{sid}", p.base_url())
    } else {
        format!("{}/schedules/{sid}/edit", p.base_url())
    }
}

/// `POST …/tasks/create` — `CreateAction` handled by the Client API
/// `task_store` (limit check, backup-limit guard, sequencing, activity).
async fn task_create(p: ServerPanel, RecordId(sid): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = find(&p, sid).await?;
    let input = task_input(&form);
    // Return to the page the dialog was submitted from (`_back` carries it).
    let back_default = schedule_back(&p, sid, false);
    let mut create = DialogState::fresh(s.name.clone(), form.back(&back_default));
    create.schedule = Some(s.clone());
    create.open_create = true;
    create.create_action = form.opt("action").unwrap_or("").to_string();
    create.create_payload = form.opt("payload").unwrap_or("").to_string();
    create.create_offset = form.opt("time_offset").unwrap_or("0").to_string();
    create.create_continue = form.bool("continue_on_failure");

    let local = validate_task_local(&form, &p);
    if !local.is_empty() {
        create.create_errors = local;
        return render_form(&p, "edit", FormView::from_model(&s), create).await;
    }

    let ctx = api_ctx(&p)?;
    let bound = bound_full(&p, sid, None).await?;
    let outcome = call(
        api::PANEL_FORM.scope(
            true,
            api::task_store(axum::extract::State(p.st.clone()), Extension(ctx_with_bound(&ctx, bound)), In(input)),
        ),
        |_| (),
    )
    .await;
    finish_task_call(&p, outcome, create, s).await
}

async fn finish_task_call(
    p: &ServerPanel,
    outcome: ApiOutcome<()>,
    mut state: DialogState,
    s: schedules::Model,
) -> WebResult<Response> {
    match outcome {
        ApiOutcome::Done(_) => Ok(p.redirect(&state.back)),
        ApiOutcome::Validation(errors) => {
            // Reopen the failing dialog with its field errors (stay in place).
            state.open_create = true;
            Ok(render_form_for_error(p, s, state, errors, None).await?)
        }
        ApiOutcome::Failed(e) => {
            tracing::warn!(status=%e.status, "task call failed");
            p.flash_danger(first_detail(&e).unwrap_or_else(|| pelican_api::error::GENERIC_500.into()), None);
            Ok(p.redirect(&state.back))
        }
    }
}

/// Re-render the form page with one task dialog open and its field errors.
async fn render_form_for_error(
    p: &ServerPanel,
    s: schedules::Model,
    state: DialogState,
    errors: Errors,
    open_edit: Option<i64>,
) -> WebResult<Response> {
    let mut state = state;
    state.open_edit = open_edit;
    if open_edit.is_some() {
        state.edit_errors = errors;
        state.open_create = false;
    } else {
        state.create_errors = errors;
        state.open_create = true;
    }
    render_form(p, "edit", FormView::from_model(&s), state).await
}

/// `GET …/tasks/{task}/edit` — opens one EditAction dialog (deep link).
async fn open_task_edit(p: ServerPanel, RecordId(sid): RecordId, TaskParam(tid): TaskParam) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = find(&p, sid).await?;
    let back = schedule_back(&p, sid, false);
    let mut state = DialogState::fresh(s.name.clone(), back);
    state.schedule = Some(s.clone());
    state.open_edit = Some(tid);
    render_form(&p, "edit", FormView::from_model(&s), state).await
}

/// `POST …/tasks/{task}/update` — EditAction through the Client API
/// `task_update` (sequence shifts, dirty checks, activity).
async fn task_update(
    p: ServerPanel,
    RecordId(sid): RecordId,
    TaskParam(tid): TaskParam,
    form: Form,
) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = find(&p, sid).await?;
    let back_default = schedule_back(&p, sid, false);
    let mut state = DialogState::fresh(s.name.clone(), form.back(&back_default));
    state.schedule = Some(s.clone());
    state.open_edit = Some(tid);

    let local = validate_task_local(&form, &p);
    if !local.is_empty() {
        state.edit_errors = local;
        return render_form(&p, "edit", FormView::from_model(&s), state).await;
    }
    let input = task_input(&form);
    let ctx = api_ctx(&p)?;
    let bound = bound_full(&p, sid, Some(tid)).await?;
    let outcome = call(
        api::PANEL_FORM.scope(
            true,
            api::task_update(axum::extract::State(p.st.clone()), Extension(ctx_with_bound(&ctx, bound)), In(input)),
        ),
        |_| (),
    )
    .await;
    finish_task_call(&p, outcome, state, s).await
}

/// `POST …/tasks/{task}/delete` — DeleteAction through the Client API
/// `task_delete` (`can(schedule.delete)` enforced there).
async fn task_delete(
    p: ServerPanel,
    RecordId(sid): RecordId,
    TaskParam(tid): TaskParam,
    form: Form,
) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = find(&p, sid).await?;
    let back = form.back(&schedule_back(&p, sid, false));
    let ctx = api_ctx(&p)?;
    let bound = bound_full(&p, sid, Some(tid)).await?;
    let outcome =
        call(api::task_delete(axum::extract::State(p.st.clone()), Extension(ctx_with_bound(&ctx, bound))), |_| ())
            .await;
    match outcome {
        ApiOutcome::Done(_) => {}
        ApiOutcome::Validation(e) => tracing::warn!(errors = ?e.0, "unexpected task-delete validation"),
        ApiOutcome::Failed(e) => {
            tracing::warn!(status=%e.status, "task deletion failed");
            p.flash_danger(first_detail(&e).unwrap_or_else(|| pelican_api::error::GENERIC_500.into()), None);
        }
    }
    let _ = s;
    Ok(p.redirect(&back))
}

/// Re-render the form page with one task dialog open and its field errors.

/// `POST …/tasks/{task}/move` — the table is `reorderable('sequence_id')`;
/// this port exposes per-row up/down moves that swap the adjacent tasks'
/// sequence ids and touch the schedule (same persisted order change).
async fn task_move(
    p: ServerPanel,
    RecordId(sid): RecordId,
    TaskParam(tid): TaskParam,
    form: Form,
) -> WebResult<Response> {
    p.authorize("schedule.update")?;
    let s = find(&p, sid).await?;
    let back = form.back(&schedule_back(&p, sid, false));
    let down = form.opt("dir") == Some("down");
    let mut rows = tasks_of(&p.st.db, s.id.0).await?;
    rows.sort_by_key(|t| (t.sequence_id.0, t.id.0));
    let pos = rows.iter().position(|t| t.id.0 == tid).ok_or(WebError::NotFound)?;
    if let Some(other) = if down { pos.checked_add(1) } else { pos.checked_sub(1) }.and_then(|o| rows.get(o)) {
        let a = &rows[pos];
        let now = Timestamp(pelican_core::time::now());
        let txn = p.st.db.begin().await?;
        // Filament reorderable table → `$record->save()` per moved row (updated
        // event when the sequence actually changes).
        let mut seq_changed = Vec::new();
        for pair in [(a.id.0, other.sequence_id), (other.id.0, a.sequence_id)] {
            tasks::Entity::update_many()
                .col_expr(tasks::Column::SequenceId, sea_orm::sea_query::Expr::value(pair.1))
                .col_expr(tasks::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
                .filter(tasks::Column::Id.eq(pair.0))
                .exec(&txn)
                .await?;
            if Some(pair.1) != rows.iter().find(|t| t.id.0 == pair.0).map(|t| t.sequence_id) {
                seq_changed.push(pair.0);
            }
        }
        pelican_api::jobs::touch_schedule(&txn, s.id.0)
            .await
            .map_err(|e| WebError::internal(e.errors.first().map(|o| o.detail.as_str()).unwrap_or("QueryException")))?;
        txn.commit().await?;
        for tid in &seq_changed {
            model_events::fire_id::<tasks::Entity>(&p.st, model_events::Action::Updated, *tid).await;
        }
        log_act(
            &p,
            "server:task.update",
            &[(morph::SCHEDULE, s.id.0), (morph::TASK, a.id.0)],
            vec![
                ("name", s.name.clone().into()),
                ("action", a.action.clone().into()),
                ("payload", a.payload.clone().into()),
            ],
        )
        .await?;
    }
    Ok(p.redirect(&back))
}

// ----- helpers referenced by templates/tests -------------------------------

/// `Task::getSchema()?.getName() ?? $task->action` — schema display names of
/// the registered actions.
fn schema_label(p: &ServerPanel, action: &str) -> Option<String> {
    ACTIONS
        .iter()
        .find(|(id, _)| *id == action)
        .map(|(id, _)| p.t(&format!("server/schedule.tasks.actions.{id}.title")))
        .or_else(|| {
            plugin_task(action).map(|(plugin, t)| crate::plugin_pages::label(&p.st, &plugin, &p.locale(), &t.name))
        })
}

/// Pure form of the payload column used by `payload_cell` and tests.
fn payload_items(action: &str, payload: &str) -> Vec<String> {
    if action == "power" || payload.is_empty() { Vec::new() } else { payload.lines().map(|l| l.to_string()).collect() }
}

// -----
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_names() {
        assert_eq!(kebab("Daily Restart"), "daily-restart");
        assert_eq!(kebab("My_Schedule name"), "my-schedule-name");
        assert_eq!(kebab("CamelCaseX"), "camel-case-x");
    }

    #[test]
    fn headline_names() {
        assert_eq!(headline("daily-restart"), "Daily Restart");
        assert_eq!(headline("every_night"), "Every Night");
        assert_eq!(import_name("https://host/x/y/schedule-daily-restart.json"), "Daily Restart");
        assert_eq!(import_name("schedule-Weekly Backup.json"), "Weekly Backup");
    }

    #[test]
    fn payload_display() {
        // `PowerActionSchema::formatPayload` uppercases the first letter.
        assert_eq!(ucfirst("restart"), "Restart");
        // Other schemas explode on newlines (badge list); empty → placeholder.
        assert_eq!(payload_items("delete_files", "a.txt\nb.txt"), ["a.txt", "b.txt"]);
        assert_eq!(payload_items("command", "say hi"), ["say hi"]);
        assert!(payload_items("command", "").is_empty());
        assert!(payload_items("power", "stop").is_empty());
    }

    #[test]
    fn multipart_extraction() {
        let body = concat!(
            "--B\r\nContent-Disposition: form-data; name=\"urls[0][url]\"\r\n\r\nhttps://example.com/x.json\r\n",
            "--B\r\nContent-Disposition: form-data; name=\"files[]\"; filename=\"schedule-daily.json\"\r\n\r\n{\"a\":1}\r\n",
            "--B--\r\n"
        );
        let parts = multipart_parts("B", body.as_bytes());
        assert_eq!(parts[0].0, "urls[0][url]");
        assert_eq!(parts[0].2, b"https://example.com/x.json");
        assert_eq!(parts[1].1.as_deref(), Some("schedule-daily.json"));
        assert_eq!(parts[1].2, b"{\"a\":1}");
    }

    /// The list template renders with the empty state (no server needed).
    #[test]
    fn list_template_renders() {
        let mut t = Table::new("schedules", "/server/x/schedules");
        t.finish(&ListParams::default(), 0, &[10, 25, 50, 0], "en");
        let ctx = context! {
            title => "Schedules",
            base => "/server/x",
            list_url => "/server/x/schedules",
            table => Mj::from_serialize(&t),
            can_import => true,
        };
        crate::views::render("server/schedules.html", ctx).expect("template renders");
    }

    /// The form page (create/edit/view) and one task dialog each.
    #[test]
    fn form_template_renders() {
        let mut tasks_table = Table::new("schedule-tasks", "");
        tasks_table.finish(&ListParams::default(), 0, &[10, 25, 50, 0], "en");
        let dialog = TaskDialog {
            id: 7,
            dom_id: "task-edit-7".into(),
            action: "command".into(),
            payload_command: "say hi".into(),
            payload_area: String::new(),
            payload_power: "restart".into(),
            options: vec![("command".into(), "Send Command".to_string())],
            backup_allowed: true,
            offset: "30".into(),
            cont: false,
            open: true,
            edit: true,
            errors: IndexMap::new(),
        };
        for mode in ["create", "edit", "view"] {
            let ctx = context! {
                title => "Schedule",
                mode => mode,
                action => "/server/x/schedules/create",
                record => Mj::from_serialize(ScheduleRow { id: 3, name: "Daily".into(), timezone: "UTC".into() }),
                s_id => 3,
                v_name => "Daily",
                v_is_active => true,
                v_only => false,
                v_minute => "*/5",
                v_hour => "*",
                v_dom => "*",
                v_month => "*",
                v_dow => "*",
                v_errors => Mj::from_serialize(&Errors::default()),
                cron_invalid => false,
                cron_next => "2026-01-01 00:00:00",
                user_tz => "UTC",
                readonly => mode == "view",
                can_update => true,
                can_delete => true,
                run_now_blocked => false,
                run_tooltip => "Run Now",
                status_value => "active",
                status_label => "Active",
                tasks_count => if mode == "create" { 0 } else { 1 },
                tasks_table => Mj::from_serialize(&tasks_table),
                task_limit_reached => false,
                dialogs => Mj::from_serialize(vec![dialog.clone()]),
                back => "/server/x/schedules/3/edit",
            };
            crate::views::render("server/schedules_form.html", ctx).expect("form template renders");
        }
    }
}
