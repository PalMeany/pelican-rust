//! `GET /account/activity` and `GET /servers/{server}/activity` (ActivityLogController).

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::activity_logs;
use pelican_db::raw;
use sea_orm::{FromQueryResult, Value as DbValue};
use serde_json::Map;

use super::data::activity_item;
use super::servers::count;
use crate::envelope::{Includes, Pagination, list, per_page, query_appends, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input, parse_query};
use crate::json::PhpJson;
use crate::pipeline::Ctx;
use crate::query;

/// `ActivityLog::DISABLED_EVENTS`.
const DISABLED_EVENTS: &[&str] = &["server:file.upload"];

/// Shared listing: `base_from`/`base_where` select activity logs; adds the
/// event filter, timestamp sort and pagination exactly like QueryBuilder.
async fn listing(
    st: &AppState,
    ctx: &Ctx,
    uri: &OriginalUri,
    input: &Input,
    select: &str,
    from: &str,
    mut wheres: Vec<String>,
    mut values: Vec<DbValue>,
) -> ApiResult<Response> {
    let db = &st.db;
    let backend = db.get_database_backend();
    let includes = Includes::parse(input);
    let sorts = query::sorts(input, &["timestamp"])?;
    for (name, value) in query::filters(input, &["event"])? {
        if let Some((sql, vals)) = query::partial(backend, &format!("activity_logs.{name}"), &value) {
            wheres.push(sql);
            values.extend(vals);
        }
    }
    let where_sql = format!(" where {}", wheres.join(" and "));
    let mut order = String::new();
    if !sorts.is_empty() {
        let parts: Vec<String> = sorts
            .iter()
            .map(|(n, desc)| format!("{} {}", raw::q(backend, n), if *desc { "desc" } else { "asc" }))
            .collect();
        order = format!(" order by {}", parts.join(", "));
    }
    let query_input = Input::from_value(parse_query(uri.query().unwrap_or("")));
    let per = per_page(&query_input, 25, 100);
    let page = crate::paginate::resolve_page(input.get("page"));
    let total = count(db, backend, &format!("select {select} from {from}{where_sql}"), values.clone()).await?;
    let mut sql = format!("select {select} from {from}{where_sql}{order}");
    if per > 0 {
        sql.push_str(" limit ? offset ?");
        values.push(per.into());
        values.push(((page - 1) * per).into());
    }
    let rows = if total > 0 {
        activity_logs::Model::find_by_statement(raw::stmt(backend, &sql, values)).all(db).await?
    } else {
        Vec::new()
    };
    let mut data = Vec::with_capacity(rows.len());
    for r in &rows {
        data.push(activity_item(st, ctx, r, &includes).await?);
    }
    let p = Pagination {
        total,
        count: data.len() as i64,
        per_page: per,
        current_page: page,
        url: format!("{}{}", ctx.root, uri.path().trim_end_matches('/')),
        appends: query_appends(uri.query()),
    };
    let mut meta = Map::new();
    meta.insert("pagination".into(), p.meta());
    Ok(PhpJson::ok(with_meta(list(data), meta)).into_response())
}

fn not_in(col: &str, items: &[&str]) -> (String, Vec<DbValue>) {
    (format!("{col} not in ({})", vec!["?"; items.len()].join(", ")), items.iter().map(|s| DbValue::from(*s)).collect())
}

/// `GET /account/activity` — `$user->activity()` (morphToMany via subjects).
pub async fn account(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    uri: OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    let user = ctx.user()?;
    let (ni, ni_vals) = not_in("{activity_logs.event}", DISABLED_EVENTS);
    let mut values: Vec<DbValue> = vec![user.id().into(), "user".into()];
    values.extend(ni_vals);
    listing(
        &st,
        &ctx,
        &uri,
        &input,
        "{activity_logs}.*, {activity_log_subjects.subject_id} as {pivot_subject_id}, {activity_log_subjects.activity_log_id} as {pivot_activity_log_id}, {activity_log_subjects.subject_type} as {pivot_subject_type}",
        "{activity_logs} inner join {activity_log_subjects} on {activity_logs.id} = {activity_log_subjects.activity_log_id}",
        vec!["{activity_log_subjects.subject_id} = ?".into(), "{activity_log_subjects.subject_type} = ?".into(), ni],
        values,
    )
    .await
}

/// `GET /servers/{server}/activity` — `ActivityLog::whereHas('subjects', ...)`.
pub async fn server(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    uri: OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    if !ctx.gate(&st, "activity.read").await? {
        return Err(ApiError::unauthorized_action());
    }
    let server = ctx.bound.server()?;
    let mut wheres = vec![
        "exists (select * from {activity_log_subjects} where {activity_logs.id} = {activity_log_subjects.activity_log_id} and {subject_id} = ? and {subject_type} = ?)".to_string(),
    ];
    let mut values: Vec<DbValue> = vec![server.id.0.into(), "server".into()];
    let (ni, ni_vals) = not_in("{activity_logs.event}", DISABLED_EVENTS);
    wheres.push(ni);
    values.extend(ni_vals);
    if st.config.env.bool_or("APP_ACTIVITY_HIDE_ADMIN", false) {
        // ActivityLog::scopeHideAdminActivity — arrives with the admin roles work (M5).
        return Err(ApiError::internal("NotImplemented"));
    }
    listing(&st, &ctx, &uri, &input, "*", "{activity_logs}", wheres, values).await
}
