//! The admin panel's global search (`GlobalSearchModalPlugin` over Filament's resource global
//! search, `HasGlobalSearch`): the records of every resource the user can list whose title
//! attribute (Eggs: name, tags, uuid, id) contains each word of the query, at most 50 per
//! resource, grouped under the resource's plural label in resource (class name) order.

use axum::Router;
use axum::routing::get;
use minijinja::context;
use pelican_core::state::AppState;
use pelican_db::raw;
use sea_orm::{DbBackend, FromQueryResult};
use serde::Serialize;

use super::Admin;
use crate::error::WebResult;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/global-search", get(search))
}

/// `HasGlobalSearch::$globalSearchResultsLimit`.
const LIMIT: usize = 50;

/// A searchable resource: table, title attribute, searched attributes, URL slug and the model
/// name of its policy abilities; `pivot` = (table, owner column) of resources scoped to the
/// user's nodes like `getEloquentQuery()` (on an accessible node or on none).
struct Resource {
    label: &'static str,
    table: &'static str,
    title: &'static str,
    attrs: &'static [&'static str],
    slug: &'static str,
    model: &'static str,
    pivot: Option<(&'static str, &'static str)>,
}

const RESOURCES: &[Resource] = &[
    Resource {
        label: "admin/backuphost.model_label",
        table: "backup_hosts",
        title: "name",
        attrs: &["name"],
        slug: "backup-hosts",
        model: "backupHost",
        pivot: Some(("backup_host_node", "backup_host_id")),
    },
    Resource {
        label: "admin/databasehost.model_label_plural",
        table: "database_hosts",
        title: "name",
        attrs: &["name"],
        slug: "database-hosts",
        model: "databaseHost",
        pivot: None,
    },
    Resource {
        label: "admin/egg.model_label_plural",
        table: "eggs",
        title: "name",
        attrs: &["name", "tags", "uuid", "id"],
        slug: "eggs",
        model: "egg",
        pivot: None,
    },
    Resource {
        label: "admin/mount.model_label_plural",
        table: "mounts",
        title: "name",
        attrs: &["name"],
        slug: "mounts",
        model: "mount",
        pivot: Some(("mount_node", "mount_id")),
    },
    Resource {
        label: "admin/node.model_label_plural",
        table: "nodes",
        title: "name",
        attrs: &["name"],
        slug: "nodes",
        model: "node",
        pivot: None,
    },
    // Plugins (a Sushi model) sit here, see `plugins()`.
    Resource {
        label: "admin/role.model_label_plural",
        table: "roles",
        title: "name",
        attrs: &["name"],
        slug: "roles",
        model: "role",
        pivot: None,
    },
    Resource {
        label: "admin/server.model_label_plural",
        table: "servers",
        title: "name",
        attrs: &["name"],
        slug: "servers",
        model: "server",
        pivot: None,
    },
    Resource {
        label: "admin/user.model_label_plural",
        table: "users",
        title: "username",
        attrs: &["username"],
        slug: "users",
        model: "user",
        pivot: None,
    },
    Resource {
        label: "admin/webhook.model_label_plural",
        table: "webhook_configurations",
        title: "name",
        attrs: &["name"],
        slug: "webhooks",
        model: "webhook",
        pivot: None,
    },
];

#[derive(Serialize)]
struct Hit {
    title: String,
    url: String,
}

#[derive(Serialize)]
struct Group {
    label: String,
    results: Vec<Hit>,
}

#[derive(FromQueryResult)]
struct Row {
    id: pelican_db::Int,
    title: Option<String>,
    node_id: Option<pelican_db::Int>,
}

#[derive(FromQueryResult)]
struct PivotRow {
    owner: pelican_db::Int,
    node_id: pelican_db::Int,
}

/// `GET /admin/global-search?q=` — the results list of the search modal (htmx).
async fn search(admin: Admin) -> WebResult {
    let q = admin
        .query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()))
        .unwrap_or_default();
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let mut groups = Vec::new();
    if !words.is_empty() {
        for r in RESOURCES {
            if r.table == "roles" {
                plugins(&admin, &words, &mut groups);
            }
            if !admin.can(&format!("viewList {}", r.model)) {
                continue;
            }
            let results = find(&admin, r, &words).await?;
            if !results.is_empty() {
                let label = if r.label.ends_with(".model_label") {
                    crate::i18n::trans_choice(&admin.locale(), r.label, 2.0, &[])
                } else {
                    admin.t(r.label)
                };
                groups.push(Group { label, results });
            }
        }
    }
    admin.render(
        "admin/global_search.html",
        context! { searched => !words.is_empty(), groups => minijinja::Value::from_serialize(&groups) },
    )
}

/// One resource's matches (`getGlobalSearchResults`), linked as `getGlobalSearchResultUrl()`:
/// the view page if the user may view the record, else the edit page, else left out.
async fn find(admin: &Admin, r: &Resource, words: &[String]) -> WebResult<Vec<Hit>> {
    let st = &admin.st;
    let backend = st.db.get_database_backend();
    let column = |a: &str| match backend {
        DbBackend::Postgres => format!("lower(cast({{{a}}} as text))"),
        DbBackend::MySql if a == "id" => format!("lower(cast({{{a}}} as char))"),
        _ => format!("lower({{{a}}})"),
    };
    let mut wheres = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for w in words {
        wheres.push(format!(
            "({})",
            r.attrs.iter().map(|a| format!("{} like ?", column(a))).collect::<Vec<_>>().join(" or ")
        ));
        for _ in r.attrs {
            vals.push(format!("%{w}%").into());
        }
    }
    let node = if r.table == "servers" { "{node_id}" } else { "null" };
    let sql = format!(
        "select {{id}} as {{id}}, {{{title}}} as {{title}}, {node} as {{node_id}} from {{{table}}} where {} order by {{id}}",
        wheres.join(" and "),
        title = r.title,
        table = r.table,
    );
    let mut rows = Row::find_by_statement(raw::stmt(backend, &sql, vals)).all(&st.db).await?;
    // getEloquentQuery(): nodes and servers on the user's nodes, mounts and backup hosts on one of
    // them or on none.
    match (r.table, r.pivot) {
        ("nodes", _) => rows.retain(|row| admin.user.can_target_node(row.id.0)),
        ("servers", _) => rows.retain(|row| row.node_id.as_ref().is_some_and(|n| admin.user.can_target_node(n.0))),
        (_, Some((pivot, owner))) if !admin.user.is_root_admin() && !rows.is_empty() => {
            let sql = format!("select {{{owner}}} as {{owner}}, {{node_id}} as {{node_id}} from {{{pivot}}}");
            let links = PivotRow::find_by_statement(raw::stmt(backend, &sql, vec![])).all(&st.db).await?;
            rows.retain(|row| {
                let nodes: Vec<i64> = links.iter().filter(|l| l.owner.0 == row.id.0).map(|l| l.node_id.0).collect();
                nodes.is_empty() || nodes.iter().any(|n| admin.user.can_target_node(*n))
            });
        }
        _ => {}
    }
    rows.truncate(LIMIT);
    let perms = |row: &Row| match (r.table, &row.node_id) {
        ("servers", Some(n)) => (admin.user.can_view_server(n.0), admin.user.can_update_server(n.0)),
        _ => (admin.can(&format!("view {}", r.model)), admin.can(&format!("update {}", r.model))),
    };
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let url = match perms(&row) {
                (true, _) => format!("/admin/{}/{}", r.slug, row.id.0),
                (false, true) => format!("/admin/{}/{}/edit", r.slug, row.id.0),
                _ => return None,
            };
            Some(Hit { title: row.title.unwrap_or_default(), url })
        })
        .collect())
}

/// PluginResource (no view or edit page): the list with the record's table action.
fn plugins(admin: &Admin, words: &[String], groups: &mut Vec<Group>) {
    if !admin.can("viewList plugin") {
        return;
    }
    let action = if admin.can("view plugin") {
        "view"
    } else if admin.can("update plugin") {
        "edit"
    } else {
        return;
    };
    let results: Vec<Hit> = pelican_api::application::plugins::config(&admin.st)
        .plugins()
        .into_iter()
        .filter(|p| {
            let name = p.name.to_lowercase();
            words.iter().all(|w| name.contains(w.as_str()))
        })
        .take(LIMIT)
        .map(|p| Hit {
            url: format!("/admin/plugins?tableAction={action}&tableActionRecord={}", urlencoding(&p.id)),
            title: p.name,
        })
        .collect();
    if !results.is_empty() {
        groups.push(Group { label: admin.t("admin/plugin.model_label_plural"), results });
    }
}

fn urlencoding(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
