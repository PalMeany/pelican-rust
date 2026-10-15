//! Database notifications (`->databaseNotifications()` on every panel, app/Providers/Filament/
//! PanelProvider.php): the topbar bell with the unread count and the slide-over list of
//! Filament's `DatabaseNotifications` component, polled every 30 s.

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::HeaderValue;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use minijinja::context;
use pelican_core::state::AppState;
use pelican_db::entities::notifications;
use pelican_db::types::{Int, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::Authed;
use crate::error::WebResult;

const TPL: &str = "components/notifications.html";
/// `simplePaginate(50, pageName: 'database-notifications-page')`.
const PER_PAGE: usize = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/_notifications", get(list))
        .route("/_notifications/trigger", get(trigger))
        .route("/_notifications/read-all", post(read_all))
        .route("/_notifications/clear", post(clear))
        .route("/_notifications/{id}/remove", post(remove))
        .route("/_notifications/{id}/actions/{index}", post(action))
}

#[derive(Serialize)]
struct Item {
    id: String,
    unread: bool,
    title: String,
    body: Option<String>,
    date: String,
    icon: Option<String>,
    color: String,
    actions: Vec<ActionView>,
}

#[derive(Serialize)]
struct ActionView {
    index: usize,
    label: String,
    link: bool,
    color: String,
    outlined: bool,
    new_tab: bool,
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<usize>,
}

/// `$user->notifications()->where('data->format', 'filament')`, latest first.
async fn user_notifications(a: &Authed) -> WebResult<Vec<notifications::Model>> {
    let rows = notifications::Entity::find()
        .filter(notifications::Column::NotifiableType.eq("user"))
        .filter(notifications::Column::NotifiableId.eq(Int(a.user.id())))
        .order_by_desc(notifications::Column::CreatedAt)
        .all(&a.st.db)
        .await?;
    Ok(rows.into_iter().filter(|n| n.data.0.get("format").and_then(Value::as_str) == Some("filament")).collect())
}

fn unread_count(rows: &[notifications::Model]) -> usize {
    rows.iter().filter(|n| n.read_at.is_none()).count()
}

/// `str($html)->sanitizeHtml()`.
fn sanitize(s: &str) -> String {
    ammonia::clean(s)
}

/// A Tabler icon for the notification's icon (Heroicons of Filament's defaults mapped), else none.
fn icon_name(icon: Option<&str>) -> Option<String> {
    let icon = icon?;
    let mapped = match icon {
        "heroicon-o-check-circle" | "heroicon-s-check-circle" => "circle-check",
        "heroicon-o-x-circle" | "heroicon-s-x-circle" => "circle-x",
        "heroicon-o-exclamation-circle" | "heroicon-s-exclamation-circle" => "alert-circle",
        "heroicon-o-exclamation-triangle" | "heroicon-s-exclamation-triangle" => "alert-triangle",
        "heroicon-o-information-circle" | "heroicon-s-information-circle" => "info-circle",
        other => other.trim_start_matches("tabler-"),
    };
    crate::assets::icon_key(mapped).map(str::to_string)
}

fn item(n: &notifications::Model, now: chrono::DateTime<chrono::Utc>, locale: &str) -> Item {
    let d = &n.data.0;
    let s = |k: &str| d.get(k).and_then(Value::as_str);
    let color = s("iconColor").or(s("status")).or(s("color")).unwrap_or("gray").to_string();
    let actions = d
        .get("actions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .enumerate()
                .filter(|(_, a)| !a.get("isDisabled").and_then(Value::as_bool).unwrap_or(false))
                .map(|(index, a)| ActionView {
                    index,
                    label: a.get("label").and_then(Value::as_str).unwrap_or_default().to_string(),
                    link: a.get("view").and_then(Value::as_str).is_some_and(|v| v.contains("link")),
                    color: a.get("color").and_then(Value::as_str).unwrap_or("primary").to_string(),
                    outlined: a.get("isOutlined").and_then(Value::as_bool).unwrap_or(false),
                    new_tab: a.get("shouldOpenUrlInNewTab").and_then(Value::as_bool).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    Item {
        id: n.id.clone(),
        unread: n.read_at.is_none(),
        title: sanitize(s("title").unwrap_or_default()),
        body: s("body").filter(|b| !b.is_empty()).map(sanitize),
        date: n
            .created_at
            .as_ref()
            .map(|t| crate::views::diff_for_humans(locale, t.0.and_utc(), now))
            .unwrap_or_default(),
        icon: icon_name(s("icon")),
        color,
        actions,
    }
}

/// The slide-over's content: header actions, one page of notifications, pagination.
fn panel_html(a: &Authed, rows: &[notifications::Model], page: usize) -> WebResult<String> {
    let now = chrono::Utc::now();
    let locale = a.locale();
    let start = (page - 1) * PER_PAGE;
    let items: Vec<Item> = rows.iter().skip(start).take(PER_PAGE).map(|n| item(n, now, &locale)).collect();
    let ctx = context! {
        items => items,
        unread => unread_count(rows),
        page => page,
        has_more => rows.len() > start + PER_PAGE,
        ..a.base()
    };
    Ok(crate::views::render_block(TPL, "panel", ctx)?)
}

fn trigger_html(a: &Authed, unread: usize, oob: bool) -> WebResult<String> {
    let ctx = context! { unread => unread, oob => oob, ..a.base() };
    Ok(crate::views::render_block(TPL, "trigger", ctx)?)
}

async fn list(a: Authed, Query(q): Query<PageQuery>) -> WebResult {
    let rows = user_notifications(&a).await?;
    let page = q.page.unwrap_or(1).max(1);
    Ok(Html(panel_html(&a, &rows, page)?).into_response())
}

async fn trigger(a: Authed) -> WebResult {
    let rows = user_notifications(&a).await?;
    Ok(Html(trigger_html(&a, unread_count(&rows), false)?).into_response())
}

/// The refreshed list plus the bell (out of band) after a change.
async fn refreshed(a: &Authed) -> WebResult {
    let rows = user_notifications(a).await?;
    let html = panel_html(a, &rows, 1)? + &trigger_html(a, unread_count(&rows), true)?;
    Ok(Html(html).into_response())
}

fn ids(rows: &[notifications::Model]) -> Vec<String> {
    rows.iter().map(|n| n.id.clone()).collect()
}

/// `markAllNotificationsAsRead`: `update(['read_at' => now()])` (with `updated_at`).
async fn read_all(a: Authed) -> WebResult {
    let rows = user_notifications(&a).await?;
    let unread: Vec<String> = rows.iter().filter(|n| n.read_at.is_none()).map(|n| n.id.clone()).collect();
    if !unread.is_empty() {
        let now = Timestamp(pelican_core::time::now());
        notifications::Entity::update_many()
            .col_expr(notifications::Column::ReadAt, Expr::value(Some(now)))
            .col_expr(notifications::Column::UpdatedAt, Expr::value(Some(now)))
            .filter(notifications::Column::Id.is_in(unread))
            .exec(&a.st.db)
            .await?;
    }
    refreshed(&a).await
}

/// `clearNotifications` (closes the slide-over).
async fn clear(a: Authed) -> WebResult {
    let rows = user_notifications(&a).await?;
    if !rows.is_empty() {
        notifications::Entity::delete_many().filter(notifications::Column::Id.is_in(ids(&rows))).exec(&a.st.db).await?;
    }
    let mut resp = refreshed(&a).await?;
    resp.headers_mut().insert("hx-trigger", HeaderValue::from_static(r#"{"close-modal":{"id":"notifications"}}"#));
    Ok(resp)
}

/// `removeNotification` (the notification's close button).
async fn remove(a: Authed, Path(id): Path<String>) -> WebResult {
    let rows = user_notifications(&a).await?;
    if rows.iter().any(|n| n.id == id) {
        notifications::Entity::delete_by_id(id).exec(&a.st.db).await?;
    }
    refreshed(&a).await
}

/// A notification action: `markAsRead()` / `markAsUnread()` as configured, then its URL.
async fn action(a: Authed, Path((id, index)): Path<(String, usize)>) -> WebResult {
    let rows = user_notifications(&a).await?;
    let Some(n) = rows.iter().find(|n| n.id == id) else {
        return Ok(a.redirect("/"));
    };
    let act =
        n.data.0.get("actions").and_then(Value::as_array).and_then(|l| l.get(index)).cloned().unwrap_or(Value::Null);
    let flag = |k: &str| act.get(k).and_then(Value::as_bool).unwrap_or(false);
    let read_at = match (flag("shouldMarkAsRead"), flag("shouldMarkAsUnread")) {
        (true, _) => Some(Some(Timestamp(pelican_core::time::now()))),
        (_, true) => Some(None),
        _ => None,
    };
    if let Some(read_at) = read_at {
        notifications::Entity::update_many()
            .col_expr(notifications::Column::ReadAt, Expr::value(read_at))
            .col_expr(notifications::Column::UpdatedAt, Expr::value(Some(Timestamp(pelican_core::time::now()))))
            .filter(notifications::Column::Id.eq(id))
            .exec(&a.st.db)
            .await?;
    }
    match act.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) {
        Some(url) => Ok(Redirect::to(url).into_response()),
        None => Ok(a.redirect("/")),
    }
}
