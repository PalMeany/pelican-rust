//! Server panel › Activity (app/Filament/Server/Resources/Activities): the
//! tenant's activity log listing with event/actor filters, timestamp sorting
//! and a per-entry detail page standing in for the record `ViewAction`
//! modal; labels and metadata come from `crate::activity`.

use std::collections::{HashMap, HashSet};

use axum::Router;
use axum::routing::get;
use chrono::Utc;
use minijinja::{Value as Mj, context};
use pelican_core::activity::morph;
use pelican_core::auth;
use pelican_core::state::AppState;
use pelican_db::entities::{activity_log_subjects, activity_logs, subusers, users};
use pelican_db::{Int, raw};
use sea_orm::Value as DbValue;
use sea_orm::{ColumnTrait, EntityTrait, FromQueryResult, QueryFilter};

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Table};
use crate::views::diff_for_humans;

pub fn routes() -> Router<AppState> {
    Router::new().route("/server/{server}/activity", get(index)).route("/server/{server}/activity/{id}", get(detail))
}

/// `ActivityLog::DISABLED_EVENTS`.
const DISABLED_EVENTS: &[&str] = &["server:file.upload"];

/// Shared scoping of every query on this page (`ActivityResource::
/// getEloquentQuery`): subject on this server, disabled events hidden and,
/// when configured, admin activity hidden unless the actor belongs to the
/// server. Also applies the two select filters (`event`, `actor_id`).
///
/// The `{users}` join is always present so its placeholder value can come
/// first in `vals` (SQL text order); it changes no results.
struct Scope {
    from: String,
    wheres: String,
    vals: Vec<DbValue>,
}

fn scope(p: &ServerPanel, event: Option<&str>, actor_id: Option<i64>) -> Scope {
    let mut vals: Vec<DbValue> = vec!["user".into()];
    let join = " left join {users} on {users.id} = {activity_logs.actor_id} and {activity_logs.actor_type} = ?";
    // whereHas('subjects', ... subject_id / subject_type)
    let mut wheres = vec![
        "exists (select * from {activity_log_subjects} where \
         {activity_log_subjects.activity_log_id} = {activity_logs.id} and \
         {activity_log_subjects.subject_id} = ? and {activity_log_subjects.subject_type} = ?)"
            .to_string(),
    ];
    vals.push(p.server.id.into());
    vals.push(morph::SERVER.into());
    // DISABLED_EVENTS
    wheres.push(format!("{{activity_logs.event}} not in ({})", vec!["?"; DISABLED_EVENTS.len()].join(", ")));
    vals.extend(DISABLED_EVENTS.iter().map(|&e| e.into()));
    if p.st.config.env.bool_or("APP_ACTIVITY_HIDE_ADMIN", false) {
        // ActivityLog::scopeHideAdminActivity.
        wheres.push(
            "({users.id} is null or {users.id} not in (select {model_has_roles.model_id} from \
             {model_has_roles} where {model_has_roles.model_type} = ?) or {users.id} in \
             (select {subusers.user_id} from {subusers} where {subusers.server_id} = ?) \
             or {users.id} = ?)"
                .to_string(),
        );
        vals.push("user".into());
        vals.push(p.server.id.into());
        vals.push(p.server.owner_id.into());
    }
    if let Some(e) = event.filter(|e| !e.is_empty()) {
        wheres.push("{activity_logs.event} = ?".into());
        vals.push(e.to_string().into());
    }
    if let Some(a) = actor_id {
        wheres.push("{activity_logs.actor_id} = ?".into());
        vals.push(a.into());
    }
    Scope { from: format!("{{activity_logs}}{join}"), wheres: wheres.join(" and "), vals }
}

#[derive(FromQueryResult)]
struct Aggregate {
    aggregate: Int,
}

/// Page rows + total count; default sort is `timestamp` desc like Filament.
async fn rows(p: &ServerPanel, lp: &ListParams) -> Result<(Vec<activity_logs::Model>, u64), WebError> {
    let event = lp.filter("event").map(str::to_string);
    let actor_id = lp.filter("actor_id").and_then(|v| v.parse::<i64>().ok());
    let b = p.st.db.get_database_backend();
    let s = scope(p, event.as_deref(), actor_id);
    let order = match lp.sort.as_deref() {
        Some("timestamp") => format!("{{activity_logs.timestamp}} {}", if lp.desc { "desc" } else { "asc" }),
        _ => "{activity_logs.timestamp} desc".to_string(),
    };
    let mut sql = format!("select {{activity_logs}}.* from {} where {} order by {}", s.from, s.wheres, order);
    let mut vals = s.vals.clone();
    if let Some((limit, offset)) = lp.limit_offset() {
        sql.push_str(&format!(" limit {limit} offset {offset}"));
        vals.push((limit as i64).into());
        vals.push((offset as i64).into());
    }
    let logs = activity_logs::Model::find_by_statement(raw::stmt(b, &sql, vals)).all(&p.st.db).await?;
    let total = Aggregate::find_by_statement(raw::stmt(
        b,
        &format!("select count(*) as {{aggregate}} from {} where {}", s.from, s.wheres),
        s.vals,
    ))
    .one(&p.st.db)
    .await?
    .map(|a| a.aggregate.0.max(0) as u64)
    .unwrap_or(0);
    Ok((logs, total))
}

// Filter options are computed like the original SelectFilter closures from the
// (unfiltered) tenant-scoped table query.

/// Distinct events of the server's log (`SelectFilter 'event'`).
async fn event_options(p: &ServerPanel) -> Result<Vec<(String, String)>, WebError> {
    let b = p.st.db.get_database_backend();
    let s = scope(p, None, None);
    #[derive(FromQueryResult)]
    struct OptEvent {
        event: String,
    }
    let sql = format!(
        "select distinct {{activity_logs.event}} from {} where {} order by {{activity_logs.event}}",
        s.from, s.wheres
    );
    Ok(OptEvent::find_by_statement(raw::stmt(b, &sql, s.vals))
        .all(&p.st.db)
        .await?
        .into_iter()
        .map(|o| (o.event.clone(), o.event))
        .collect())
}

/// Actors behind the server's entries with a user row (`SelectFilter
/// 'actor_id'`: usernames keyed by id, empty names dropped).
async fn actor_options(p: &ServerPanel) -> Result<Vec<(String, String)>, WebError> {
    let b = p.st.db.get_database_backend();
    let s = scope(p, None, None);
    #[derive(FromQueryResult)]
    struct OptActor {
        actor_id: Int,
        username: String,
    }
    let sql = format!(
        "select distinct {{activity_logs.actor_id}}, {{users.username}} from {} where {} \
         and {{activity_logs.actor_type}} = 'user' and {{users.username}} <> '' \
         order by {{users.username}}",
        s.from, s.wheres
    );
    Ok(OptActor::find_by_statement(raw::stmt(b, &sql, s.vals))
        .all(&p.st.db)
        .await?
        .into_iter()
        .map(|o| (o.actor_id.0.to_string(), o.username))
        .collect())
}

/// `ActivityLog::getIcon()` resolved against the found actors.
fn entry_icon(log: &activity_logs::Model, actor: Option<&users::Model>) -> &'static str {
    if log.api_key_id.is_some() {
        "api"
    } else if actor.is_some() {
        "user"
    } else if log.actor_id.is_none() {
        "device-desktop"
    } else {
        "user-off"
    }
}

/// Whether the viewer may see this actor's email: admins plus owner/subuser
/// actors of this server (the rule used by the list column and ViewAction).
fn shows_email(p: &ServerPanel, sub_ids: &HashSet<i64>, log: &activity_logs::Model) -> bool {
    p.user.is_admin() || log.actor_id.map(|a| a.0 == p.server.owner_id.0 || sub_ids.contains(&a.0)).unwrap_or(false)
}

/// Whether the viewer may edit the acting user (`can('update', $actor)` =
/// spatie `update user` via DefaultAdminPolicies and `canTarget`: non-root
/// admins cannot target root admins, UserPolicy).
fn can_edit_actor(p: &ServerPanel, roots: &HashSet<i64>, actor_id: Option<Int>, resolved: bool) -> bool {
    resolved
        && actor_id
            .is_some_and(|a| p.user.gate_before("update user") && (p.user.is_root_admin() || !roots.contains(&a.0)))
}

/// Owner and subuser ids of this server (email visibility set).
async fn member_ids(p: &ServerPanel) -> Result<HashSet<i64>, WebError> {
    let subs = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(p.server.id))
        .all(&p.st.db)
        .await?
        .into_iter()
        .map(|s| s.user_id.0)
        .collect();
    Ok(subs)
}

/// Acting users referenced by the rows plus which of them are root admins;
/// the root lookup runs only when needed (`need_roots`, the `update user`
/// edit links).
async fn actors_with_roots(
    p: &ServerPanel,
    ids: &[i64],
    need_roots: bool,
) -> Result<(HashMap<i64, users::Model>, HashSet<i64>), WebError> {
    let b = p.st.db.get_database_backend();
    let found: Vec<users::Model> = if ids.is_empty() {
        Vec::new()
    } else {
        users::Entity::find().filter(users::Column::Id.is_in(ids.to_vec())).all(&p.st.db).await?
    };
    if !need_roots || found.is_empty() {
        return Ok((found.into_iter().map(|u| (u.id.0, u)).collect(), HashSet::new()));
    }
    #[derive(FromQueryResult)]
    struct RootId {
        model_id: Int,
    }
    let marks = vec!["?"; found.len()].join(", ");
    let mut vals: Vec<DbValue> = vec!["user".into(), auth::ROOT_ADMIN_ROLE.into()];
    vals.extend(found.iter().map(|u| u.id.into()));
    let roots: HashSet<i64> = RootId::find_by_statement(raw::stmt(
        b,
        &format!(
            "select {{model_has_roles.model_id}} from {{model_has_roles}} inner join {{roles}} \
             on {{roles.id}} = {{model_has_roles.role_id}} where {{model_has_roles.model_type}} = ? \
             and {{roles.name}} = ? and {{model_has_roles.model_id}} in ({marks})"
        ),
        vals,
    ))
    .all(&p.st.db)
    .await?
    .into_iter()
    .map(|r| r.model_id.0)
    .collect();
    Ok((found.into_iter().map(|u| (u.id.0, u)).collect(), roots))
}

/// `GET /server/{server}/activity` (ListActivities).
async fn index(p: ServerPanel) -> WebResult {
    p.authorize("activity.read")?;
    let locale = p.locale();
    let see_ips = p.user.gate_before("seeIps activityLog");
    let lp = ListParams::parse(p.query.as_deref(), &[25, 50], 25);

    let (logs, total) = rows(&p, &lp).await?;
    let mut lp = lp;
    lp.clamp(total);

    // Actors of the current page.
    let ids: Vec<i64> = logs
        .iter()
        .filter(|l| l.actor_type.as_deref() == Some("user"))
        .filter_map(|l| l.actor_id.map(|a| a.0))
        .collect();
    let (actors, _roots) = actors_with_roots(&p, &ids, false).await?;
    let sub_ids = member_ids(&p).await?;

    let t_event = p.t("server/activity.event");
    let t_user = p.t("server/activity.user");
    let t_timestamp = p.t("server/activity.timestamp");
    let system_label = p.t("server/activity.system");
    let deleted_user = p.t("server/activity.deleted_user");

    let base = p.base_url();
    let view_label = p.t("filament-actions::view.single.label");
    let mut table = Table::new("activity", &format!("{base}/activity"));
    table.columns = vec![
        Column::new("event", &t_event),
        Column::new("user", &t_user),
        Column::new("timestamp", t_timestamp).sortable(),
    ];
    for log in &logs {
        let actor =
            if log.actor_type.as_deref() == Some("user") { log.actor_id.and_then(|a| actors.get(&a.0)) } else { None };
        let props = log.properties.0.clone();
        let label = crate::activity::label(&locale, &log.event, &props);
        let tooltip_ip = see_ips.then(|| log.ip.clone()).filter(|ip| !ip.is_empty());

        let user_text = match (&actor, log.actor_id) {
            (Some(u), _) => {
                let mut text = u.username.clone();
                if shows_email(&p, &sub_ids, log) {
                    text.push_str(&format!(" ({})", u.email));
                }
                text
            }
            (None, Some(_)) => deleted_user.clone(),
            (None, None) => system_label.clone(),
        };

        let ts = log.timestamp.0.and_utc();
        let url = format!("{base}/activity/{}", log.id.0);

        let mut row = Row::new(log.id.0.to_string())
            .cell(
                "event",
                // TextColumn::html(): the label is trusted markup (properties are strip_tags'ed),
                // with the icon and the raw event name as description.
                Cell::Html {
                    html: format!(
                        r#"<span class="row" style="gap:0.5rem;flex-wrap:nowrap;align-items:flex-start">{}<span>{}<br><span class="muted small">{}</span></span></span>"#,
                        crate::views::icon_html(entry_icon(log, actor), "icon-sm"),
                        label,
                        crate::views::escape(&log.event)
                    ),
                },
            )
            .cell("user", Cell::Text { text: user_text, desc: None, mono: false, copy: None, tooltip: tooltip_ip })
            .cell(
                "timestamp",
                Cell::Text {
                    text: diff_for_humans(&locale, ts, Utc::now()),
                    desc: None,
                    mono: false,
                    copy: None,
                    tooltip: Some(log.timestamp.0.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                },
            );
        row.actions.push(Action::link(view_label.clone(), "eye", url));
        table.rows.push(row);
    }

    let ev_value = lp.filter("event").unwrap_or("").to_string();
    let actor_value = lp.filter("actor_id").unwrap_or("").to_string();
    table.filters = vec![
        Filter { name: "event".into(), label: t_event.clone(), options: event_options(&p).await?, value: ev_value },
        Filter {
            name: "actor_id".into(),
            label: t_user.clone(),
            options: actor_options(&p).await?,
            value: actor_value,
        },
    ];
    table.empty_icon = "stack".into();
    table.empty_heading = p.t_with("filament-tables::table.empty.heading", &[("model", "Activity")]);
    table.finish(&lp, total, &[25, 50], &locale);

    p.page(
        "activity",
        "server/activity.html",
        context! {
            title => p.t("server/activity.title"),
            base => base,
            table => Mj::from_serialize(&table),
        },
    )
}

/// PHP `Arr::dot()` over the raw properties for the detail page's KeyValue
/// list (`->formatStateUsing(fn ($state) => Arr::dot($state))`).
fn arr_dot(prefix: &str, v: &serde_json::Value, out: &mut Vec<(String, String)>) {
    fn scalar(v: &serde_json::Value) -> String {
        use serde_json::Value;
        match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        }
    }
    match v {
        // Non-empty nested containers flatten (`is_array($value) && $value`).
        serde_json::Value::Object(m) if !m.is_empty() => {
            for (k, v) in m {
                arr_dot(&join_key(prefix, k), v, out);
            }
        }
        serde_json::Value::Array(a) if !a.is_empty() => {
            for (i, v) in a.iter().enumerate() {
                arr_dot(&join_key(prefix, &i.to_string()), v, out);
            }
        }
        other => out.push((prefix.to_string(), scalar(other))),
    }
}

fn join_key(prefix: &str, k: &str) -> String {
    if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") }
}

/// `GET /server/{server}/activity/{id}` — the ViewAction schema: event, user
/// (with email and IP), timestamp and the dotted properties.
async fn detail(p: ServerPanel, RecordId(id): RecordId) -> WebResult {
    p.authorize("activity.read")?;
    let db = &p.st.db;
    // The entry must be one of this server's subjects (`getEloquentQuery`).
    let scoped = activity_log_subjects::Entity::find()
        .filter(activity_log_subjects::Column::ActivityLogId.eq(Int(id)))
        .filter(activity_log_subjects::Column::SubjectId.eq(p.server.id))
        .filter(activity_log_subjects::Column::SubjectType.eq(morph::SERVER))
        .one(db)
        .await?
        .is_some();
    if !scoped {
        return Err(WebError::NotFound);
    }
    let log = activity_logs::Entity::find_by_id(Int(id)).one(db).await?.ok_or(WebError::NotFound)?;
    if DISABLED_EVENTS.contains(&log.event.as_str()) {
        return Err(WebError::NotFound);
    }

    let locale = p.locale();
    let see_ips = p.user.gate_before("seeIps activityLog");
    let props = log.properties.0.clone();
    let label = crate::activity::label(&locale, &log.event, &props);

    let (actors, roots) = actors_with_roots(
        &p,
        &[log.actor_id.filter(|_| log.actor_type.as_deref() == Some("user")).map(|a| a.0).unwrap_or(0)],
        true,
    )
    .await?;
    let sub_ids = member_ids(&p).await?;
    let actor =
        if log.actor_type.as_deref() == Some("user") { log.actor_id.and_then(|a| actors.get(&a.0)) } else { None };

    let mut user_text = match (&actor, log.actor_id) {
        (Some(u), _) => {
            let mut text = u.username.clone();
            if shows_email(&p, &sub_ids, &log) {
                text.push_str(&format!(" ({})", u.email));
            }
            text
        }
        (None, Some(_)) => p.t("server/activity.deleted_user"),
        (None, None) => p.t("server/activity.system"),
    };
    if see_ips && !log.ip.is_empty() {
        user_text.push_str(&format!(" - {}", log.ip));
    }
    // Hint action 'hint_edit' on the user field.
    let edit_url = log
        .actor_id
        .filter(|_| can_edit_actor(&p, &roots, log.actor_id, actor.is_some()))
        .map(|a| format!("/admin/users/{}/edit", a.0));

    let mut metadata: Vec<(String, String)> = Vec::new();
    arr_dot("", &props, &mut metadata);
    let metadata = metadata.iter().map(|(k, v)| context! { key => k, value => v }).collect::<Vec<_>>();

    p.page(
        "activity",
        "server/activity_view.html",
        context! {
            title => p.t("server/activity.title"),
            base => p.base_url(),
            event_icon => entry_icon(&log, actor),
            event => log.event.clone(),
            label => label,
            actor => user_text,
            timestamp => log.timestamp.0.and_utc().to_rfc3339(),
            edit_url => edit_url,
            has_metadata => !metadata.is_empty(),
            metadata => metadata,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dots_nested_properties() {
        let mut out = Vec::new();
        arr_dot("", &json!({"files": ["a.txt"], "useragent": "UA"}), &mut out);
        assert_eq!(
            out,
            vec![("files.0".to_string(), "a.txt".to_string()), ("useragent".to_string(), "UA".to_string())]
        );
    }

    #[test]
    fn dots_empty_arrays_as_leaves() {
        let mut out = Vec::new();
        arr_dot("", &json!({"nested": {"x": 1}, "empty": {}, "list": []}), &mut out);
        assert_eq!(
            out,
            vec![
                ("nested.x".to_string(), "1".to_string()),
                ("empty".to_string(), "{}".to_string()),
                ("list".to_string(), "[]".to_string())
            ]
        );
    }

    #[test]
    fn disabled_events_match_the_model() {
        assert_eq!(DISABLED_EVENTS, &["server:file.upload"]);
    }
}
