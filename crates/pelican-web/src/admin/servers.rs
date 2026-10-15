//! Admin › Servers (app/Filament/Admin/Resources/Servers): list grouped by
//! node/owner/egg, edit tabs (information, environment, egg, mounts,
//! actions), server actions and the inline primary allocation select.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::{
    binding::Bound,
    model_events,
    validation::{CustomRule, Validator},
};
use pelican_core::state::AppState;
use pelican_db::entities::{
    allocations, egg_variables, eggs, mountables, mounts, nodes, server_variables, servers, users,
};
use pelican_db::{Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    TransactionTrait,
};
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::server_info::{self, ServerState};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};
use crate::variables::{self, VariableField};

const SLUG: &str = "servers";
const TABS: [&str; 5] = ["information", "environment_configuration", "egg", "mounts", "actions"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/servers", get(index))
        .route("/admin/servers/{id}", get(view))
        .route("/admin/servers/{id}/edit", get(edit).post(save))
        .route("/admin/servers/{id}/primary", post(set_primary))
        .route("/admin/servers/{id}/action", post(action))
        .route("/admin/servers/{id}/icon/url", post(icon_url))
        .route("/admin/servers/{id}/icon/file", post(icon_file))
        .route("/admin/servers/{id}/icon/delete", post(icon_delete))
}

// UploadIcon / DeleteIcon on the edit page (the server panel's actions, `update server`).

async fn icon_url(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let (s, _) = find(&admin, id).await?;
    if !admin.user.can_update_server(s.node_id.0) {
        return Err(WebError::Forbidden);
    }
    let url = form.get("icon_url").trim().to_string();
    if url.is_empty() {
        admin.session.flash("warning", admin.t("admin/egg.import.no_icon"), None);
    } else {
        match crate::server::settings::fetch_icon_from_url(&admin.st, "server", &admin.locale(), &url, &s.uuid).await {
            Ok(()) => admin.flash_success(admin.t("admin/egg.import.icon_updated")),
            Err(msg) => admin.flash_danger(msg, None),
        }
    }
    Ok(admin.redirect(&format!("/admin/servers/{id}/edit")))
}

async fn icon_file(admin: Admin, Path(id): Path<i64>, req: axum::extract::Request) -> WebResult {
    let (s, _) = find(&admin, id).await?;
    if !admin.user.can_update_server(s.node_id.0) {
        return Err(WebError::Forbidden);
    }
    Ok(crate::server::settings::upload_icon_file(&admin.st, "server", &admin.locale(), req, &s.uuid).await)
}

async fn icon_delete(admin: Admin, Path(id): Path<i64>) -> WebResult {
    let (s, _) = find(&admin, id).await?;
    if !admin.user.can_update_server(s.node_id.0) {
        return Err(WebError::Forbidden);
    }
    crate::server::settings::delete_icon(&admin.st, "server", &s.uuid).await;
    admin.flash_success(admin.t("admin/egg.import.icon_deleted"));
    Ok(admin.redirect(&format!("/admin/servers/{id}/edit")))
}

#[derive(FromQueryResult)]
struct Count {
    aggregate: Int,
}

async fn find(admin: &Admin, id: i64) -> Result<(servers::Model, nodes::Model), WebError> {
    let s = servers::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    if !admin.user.can_target_node(s.node_id.0) {
        return Err(WebError::NotFound);
    }
    let n = nodes::Entity::find_by_id(s.node_id.0).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    Ok((s, n))
}

/// `GET /admin/servers` (ListServers): grouped by node by default.
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList server")?;
    let st = &admin.st;
    let b = st.db.get_database_backend();
    let opts = [10, 25, 50, 100, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let group = match p.filter("group") {
        Some(g @ ("node" | "owner" | "egg" | "none")) => g.to_string(),
        _ => "node".into(),
    };
    let mut wheres: Vec<String> = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    if let Some(ids) = &admin.user.node_ids {
        if !admin.user.is_root_admin() {
            let marks = vec!["?"; ids.len().max(1)].join(", ");
            wheres.push(format!("{{servers.node_id}} in ({marks})"));
            if ids.is_empty() {
                vals.push(sea_orm::Value::BigInt(None));
            }
            vals.extend(ids.iter().map(|i| sea_orm::Value::from(*i)));
        }
    }
    if !p.search.is_empty() {
        let like = format!("%{}%", p.search.to_lowercase());
        wheres.push(
            "(LOWER({servers.name}) LIKE ? OR LOWER({servers.uuid}) LIKE ? OR LOWER({nodes.name}) LIKE ? OR LOWER({eggs.name}) LIKE ? OR LOWER({users.username}) LIKE ?)"
                .into(),
        );
        for _ in 0..5 {
            vals.push(like.clone().into());
        }
    }
    let from = "{servers} left join {nodes} on {nodes.id} = {servers.node_id} left join {eggs} on {eggs.id} = {servers.egg_id} left join {users} on {users.id} = {servers.owner_id}";
    let where_sql = if wheres.is_empty() { String::new() } else { format!(" where {}", wheres.join(" and ")) };
    let total = Count::find_by_statement(raw::stmt(
        b,
        &format!("select count(*) as {{aggregate}} from {from}{where_sql}"),
        vals.clone(),
    ))
    .one(&st.db)
    .await?
    .map(|c| c.aggregate.0.max(0) as u64)
    .unwrap_or(0);
    let dir = if p.desc { "desc" } else { "asc" };
    let mut order = match group.as_str() {
        "node" => vec!["{nodes.name} asc".to_string()],
        "owner" => vec!["{users.username} asc".to_string()],
        "egg" => vec!["{eggs.name} asc".to_string()],
        _ => vec![],
    };
    match p.sort.as_deref() {
        Some("name") => order.push(format!("{{servers.name}} {dir}")),
        Some("node") => order.push(format!("{{nodes.name}} {dir}")),
        Some("egg") => order.push(format!("{{eggs.name}} {dir}")),
        Some("owner") => order.push(format!("{{users.username}} {dir}")),
        _ => {}
    }
    order.push("{servers.id} asc".into());
    let mut sql = format!("select {{servers}}.* from {from}{where_sql} order by {}", order.join(", "));
    if let Some((l, o)) = p.limit_offset() {
        sql.push_str(&format!(" limit {l} offset {o}"));
    }
    let rows = servers::Model::find_by_statement(raw::stmt(b, &sql, vals)).all(&st.db).await?;

    let node_map: HashMap<i64, nodes::Model> =
        nodes::Entity::find().all(&st.db).await?.into_iter().map(|n| (n.id.0, n)).collect();
    let egg_map: HashMap<i64, eggs::Model> =
        eggs::Entity::find().all(&st.db).await?.into_iter().map(|e| (e.id.0, e)).collect();
    let owner_ids: Vec<i64> = rows.iter().map(|s| s.owner_id.0).collect();
    let user_map: HashMap<i64, users::Model> = users::Entity::find()
        .filter(users::Column::Id.is_in(owner_ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|u| (u.id.0, u))
        .collect();
    let server_ids: Vec<i64> = rows.iter().map(|s| s.id.0).collect();
    let mut allocs: HashMap<i64, Vec<allocations::Model>> = HashMap::new();
    for a in allocations::Entity::find()
        .filter(allocations::Column::ServerId.is_in(server_ids))
        .order_by_asc(allocations::Column::Id)
        .all(&st.db)
        .await?
    {
        if let Some(s) = a.server_id {
            allocs.entry(s.0).or_default().push(a);
        }
    }
    let details: Vec<Value> = futures::future::join_all(rows.iter().map(|s| {
        let st = st.clone();
        let node = node_map.get(&s.node_id.0).cloned();
        async move {
            match node {
                Some(n) => server_info::details(&st, s, &n).await,
                None => json!({ "state": "missing" }),
            }
        }
    }))
    .await;

    let t = |k: &str| admin.t(k);
    let locale = admin.locale();
    let can_update = admin.can("update server");
    let mut table = Table::new("servers", "/admin/servers");
    table.columns = vec![
        Column::new("condition", t("admin/server.condition")),
        Column::new("name", t("admin/server.name")).sortable(),
    ];
    if group != "node" {
        table.columns.push(Column::new("node", t("admin/server.node")).sortable());
    }
    if group != "egg" {
        table.columns.push(Column::new("egg", t("admin/server.egg")).sortable());
    }
    if group != "owner" {
        table.columns.push(Column::new("owner", t("admin/user.username")).sortable());
    }
    table.columns.push(Column::new("allocation", t("admin/server.primary_allocation")));
    let mut last_group: Option<i64> = None;
    for (s, d) in rows.iter().zip(details.iter()) {
        let node = node_map.get(&s.node_id.0);
        let egg = egg_map.get(&s.egg_id.0);
        let owner = user_map.get(&s.owner_id.0);
        let (label, icon, color) = server_info::condition(s, d, &locale);
        let key = match group.as_str() {
            "node" => Some(s.node_id.0),
            "owner" => Some(s.owner_id.0),
            "egg" => Some(s.egg_id.0),
            _ => None,
        };
        let heading = (key.is_some() && key != last_group).then(|| match group.as_str() {
            "node" => (
                node.map(|n| n.name.clone()).unwrap_or_default(),
                node.and_then(|n| n.description.clone()).map(|d| d.chars().take(150).collect()),
            ),
            "owner" => (owner.map(|u| u.username.clone()).unwrap_or_default(), owner.map(|u| u.email.clone())),
            _ => (
                egg.map(|e| e.name.clone()).unwrap_or_default(),
                egg.and_then(|e| e.description.clone()).map(|d| d.chars().take(150).collect()),
            ),
        });
        last_group = key;
        let server_allocs = allocs.get(&s.id.0).cloned().unwrap_or_default();
        let allocation_cell = if can_update {
            Cell::Select {
                url: format!("/admin/servers/{}/primary", s.id.0),
                field: "allocation_id".into(),
                options: server_allocs.iter().map(|a| (a.id.0.to_string(), crate::models::address(a))).collect(),
                value: s.allocation_id.map(|a| a.0.to_string()).unwrap_or_default(),
                disabled: server_allocs.len() <= 1,
                placeholder: (server_allocs.len() <= 1 && s.allocation_id.is_none()).then(|| t("admin/server.none")),
            }
        } else {
            Cell::text(
                server_allocs
                    .iter()
                    .find(|a| Some(a.id) == s.allocation_id)
                    .map(crate::models::address)
                    .unwrap_or_else(|| t("admin/server.none")),
            )
        };
        let url =
            if can_update { format!("/admin/servers/{}/edit", s.id.0) } else { format!("/admin/servers/{}", s.id.0) };
        let mut row = Row::new(s.id.0.to_string())
            .url(url.clone())
            .cell(
                "condition",
                Cell::Badge { text: label, color: color.into(), icon: Some(icon.into()), tooltip: None, copy: None },
            )
            .cell("name", Cell::text(s.name.clone()))
            .cell("node", Cell::text(node.map(|n| n.name.clone()).unwrap_or_default()))
            .cell("egg", Cell::text(egg.map(|e| e.name.clone()).unwrap_or_default()))
            .cell("owner", Cell::text(owner.map(|u| u.username.clone()).unwrap_or_default()))
            .cell("allocation", allocation_cell);
        row.heading = heading;
        row.actions.push(Action::link(t("admin/server.console"), "terminal", format!("/server/{}", s.uuid_short)));
        row.actions.push(if can_update {
            Action::link(t("filament-actions::edit.single.label"), "pencil", url)
        } else {
            Action::link(t("filament-actions::view.single.label"), "eye", url)
        });
        table.rows.push(row);
    }
    table.searchable = true;
    table.filters = vec![crate::table::Filter {
        name: "group".into(),
        label: t("filament-tables::table.grouping.fields.group.label"),
        options: vec![
            ("node".into(), t("admin/server.node")),
            ("owner".into(), t("admin/user.username")),
            ("egg".into(), t("admin/server.egg")),
            ("none".into(), "—".into()),
        ],
        value: group.clone(),
    }];
    table.empty_icon = "brand-docker".into();
    table.empty_heading = t("admin/server.no_servers");
    table.finish(&p, total, &opts, &locale);
    let create = admin.can("create server").then(|| {
        context! { label => admin.t_with("filament-actions::create.single.label", &[("label", &t("admin/server.model_label"))]), url => "/admin/servers/create" }
    });
    admin
        .page(SLUG, "admin/list.html", context! { title => t("admin/server.model_label_plural"), table => Mj::from_serialize(&table), create => create })
        .await
}

/// `POST /admin/servers/{id}/primary` (SelectColumn `allocation_id`).
async fn set_primary(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, _) = find(&admin, id).await?;
    let Ok(alloc) = form.get("allocation_id").parse::<i64>() else {
        return Ok(admin.redirect(&form.back("/admin/servers")));
    };
    let owned =
        allocations::Entity::find_by_id(alloc).one(&admin.st.db).await?.is_some_and(|a| a.server_id == Some(s.id));
    if owned && s.allocation_id.map(|a| a.0) != Some(alloc) {
        update_columns(&admin, id, &["allocation_id"], vec![alloc.into()]).await?;
        // SelectColumn save → `$server->update(['allocation_id' => …])`.
        model_events::fire_id::<servers::Entity>(&admin.st, model_events::Action::Updated, id).await;
    }
    Ok(admin.redirect(&form.back("/admin/servers")))
}

async fn update_columns(admin: &Admin, id: i64, cols: &[&str], mut vals: Vec<sea_orm::Value>) -> Result<(), WebError> {
    let b = admin.st.db.get_database_backend();
    let mut sets: Vec<String> = cols.iter().map(|c| format!("{{{c}}} = ?")).collect();
    sets.push("{updated_at} = ?".into());
    vals.push(pelican_core::time::now().into());
    vals.push(id.into());
    admin
        .st
        .db
        .execute_raw(raw::stmt(b, &format!("update {{servers}} set {} where {{id}} = ?", sets.join(", ")), vals))
        .await?;
    Ok(())
}

/// `DaemonServerRepository::sync()`; a failure becomes the "error connecting" warning.
async fn sync_wings(admin: &Admin, s: &servers::Model) -> bool {
    let node = nodes::Entity::find_by_id(s.node_id.0).one(&admin.st.db).await.ok().flatten();
    let ok = match node.as_ref().map(|n| admin.st.node_conn(n)) {
        Some(Ok(conn)) => admin.st.wings.sync_server(&conn, &s.uuid).await.is_ok(),
        _ => false,
    };
    if !ok {
        admin.session.flash(
            "warning",
            admin.t_with(
                "admin/server.notifications.error_connecting",
                &[("node", &node.map(|n| n.name).unwrap_or_default())],
            ),
            Some(admin.t("admin/server.notifications.error_connecting_description")),
        );
    }
    ok
}

/// `ensureVariablesExist()` + egg variables in sort order with the server's values.
pub async fn variable_fields(
    admin: &Admin,
    server_id: Option<i64>,
    egg_id: i64,
) -> Result<Vec<VariableField>, WebError> {
    let vars = egg_variables::Entity::find()
        .filter(egg_variables::Column::EggId.eq(egg_id))
        .order_by_asc(egg_variables::Column::Sort)
        .order_by_asc(egg_variables::Column::Id)
        .all(&admin.st.db)
        .await?;
    let values: HashMap<i64, String> = match server_id {
        Some(sid) => server_variables::Entity::find()
            .filter(server_variables::Column::ServerId.eq(sid))
            .all(&admin.st.db)
            .await?
            .into_iter()
            .map(|v| (v.variable_id.0, v.variable_value))
            .collect(),
        None => HashMap::new(),
    };
    Ok(vars
        .iter()
        .map(|v| {
            let mut f = VariableField::new(v, values.get(&v.id.0).map(String::as_str));
            // Admins may edit every variable.
            f.editable = true;
            f
        })
        .collect())
}

fn json_list(v: Option<&Json>) -> Vec<String> {
    match v.map(|j| &j.0) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::Object(m)) => m.values().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// `docker_images` / `startup_commands`: `display name => value` maps (or plain lists).
fn named_options(v: Option<&Json>) -> Vec<(String, String)> {
    match v.map(|j| &j.0) {
        Some(Value::Object(m)) => {
            m.iter().map(|(k, v)| (v.as_str().unwrap_or_default().to_string(), k.clone())).collect()
        }
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| (s.to_string(), s.to_string()))).collect(),
        _ => Vec::new(),
    }
}

/// Mounts allowed for a node+egg (`getMountCheckboxList`).
pub async fn mount_options(admin: &Admin, node_id: i64, egg_id: i64) -> Result<Vec<mounts::Model>, WebError> {
    let all = mounts::Entity::find().order_by_asc(mounts::Column::Id).all(&admin.st.db).await?;
    let pivots = mountables::Entity::find().all(&admin.st.db).await?;
    Ok(all
        .into_iter()
        .filter(|m| {
            let nodes: Vec<i64> = pivots
                .iter()
                .filter(|p| p.mount_id == m.id && p.mountable_type == "node")
                .map(|p| p.mountable_id.0)
                .collect();
            let eggs: Vec<i64> = pivots
                .iter()
                .filter(|p| p.mount_id == m.id && p.mountable_type == "egg")
                .map(|p| p.mountable_id.0)
                .collect();
            (nodes.is_empty() || nodes.contains(&node_id)) && (eggs.is_empty() || eggs.contains(&egg_id))
        })
        .collect())
}

struct Ctx<'a> {
    s: &'a servers::Model,
    node: &'a nodes::Model,
    readonly: bool,
}

async fn render_edit(
    admin: &Admin,
    c: Ctx<'_>,
    form: Option<&Form>,
    errors: &Errors,
    headers: Option<&axum::http::HeaderMap>,
) -> WebResult {
    let st = &admin.st;
    let s = c.s;
    let egg = eggs::Entity::find_by_id(s.egg_id.0).one(&st.db).await?.ok_or(WebError::NotFound)?;
    let owners = users::Entity::find().order_by_asc(users::Column::Username).all(&st.db).await?;
    let other_eggs: Vec<(String, String)> = eggs::Entity::find()
        .order_by_asc(eggs::Column::Name)
        .all(&st.db)
        .await?
        .into_iter()
        .filter(|e| e.id != egg.id)
        .map(|e| (e.id.0.to_string(), e.name))
        .collect();
    let mut vars = variable_fields(admin, Some(s.id.0), egg.id.0).await?;
    let attached: Vec<i64> = mountables::Entity::find()
        .filter(mountables::Column::MountableType.eq("server"))
        .filter(mountables::Column::MountableId.eq(s.id.0))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|m| m.mount_id.0)
        .collect();
    let mut mount_list: Vec<Mj> = mount_options(admin, c.node.id.0, egg.id.0)
        .await?
        .into_iter()
        .map(|m| context! { id => m.id.0, name => m.name, desc => format!("{} -> {}", m.source, m.target), checked => attached.contains(&m.id.0) })
        .collect();
    // Re-render after a failed save: keep what was submitted.
    let get = |k: &str, cur: String| form.map(|f| f.get(k).to_string()).unwrap_or(cur);
    if let Some(f) = form {
        for v in &mut vars {
            if let Some(val) = f.opt(&format!("var_{}", v.id)) {
                v.value = val.to_string();
            }
        }
        let chosen = f.list("mounts");
        mount_list = mount_list
            .into_iter()
            .map(|m| {
                let id = m.get_attr("id").map(|v| v.to_string()).unwrap_or_default();
                context! { ..m, ..context! { checked => chosen.contains(&id) } }
            })
            .collect();
    }
    let details = server_info::details(st, s, c.node).await;
    let (cond_label, cond_icon, cond_color) = server_info::condition(s, &details, &admin.locale());
    let images = named_options(egg.docker_images.as_ref());
    let startups = named_options(egg.startup_commands.as_ref());
    let labels: Vec<(String, String)> = match s.docker_labels.as_ref().map(|j| &j.0) {
        Some(Value::Object(m)) => {
            m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()
        }
        _ => Vec::new(),
    };
    let tab = admin
        .query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "tab").map(|(_, v)| v.into_owned()))
        .filter(|t| TABS.contains(&t.as_str()))
        .unwrap_or_else(|| "information".into());
    let accessible_nodes =
        nodes::Entity::find().all(&st.db).await?.into_iter().filter(|n| admin.user.can_target_node(n.id.0)).count();
    let own_icon = crate::models::icon_url(st, "server", &s.uuid);
    let has_own_icon = own_icon.is_some();
    let icon = own_icon.or_else(|| egg.uuid.as_deref().and_then(|u| crate::models::icon_url(st, "egg", u)));
    // Relation managers PHP plugins add to servers (`ServerResource::registerCustomRelations()`).
    let (plugin_rel, cookies) = match headers {
        Some(h) => super::plugin_relations(admin, h, "servers", s.id.0).await,
        None => (None, Vec::new()),
    };
    let mut resp = admin
        .page(
            SLUG,
            "admin/servers/edit.html",
            context! {
                title => admin.t_with(if c.readonly { "filament-panels::resources/pages/view-record.title" } else { "filament-panels::resources/pages/edit-record.title" }, &[("label", &s.name)]),
                tab => tab,
                readonly => c.readonly,
                can_delete => !c.readonly && admin.can("delete server"),
                can_force_delete => crate::cache::get(&format!("servers.{}.canForceDelete", s.uuid)).and_then(|v| v.as_bool()).unwrap_or(false),
                s => context! {
                    id => s.id.0, uuid => s.uuid.clone(), uuid_short => s.uuid_short.clone(),
                    name => get("name", s.name.clone()), description => get("description", s.description.clone().unwrap_or_default()),
                    external_id => get("external_id", s.external_id.clone().unwrap_or_default()),
                    owner_id => get("owner_id", s.owner_id.0.to_string()),
                    node => c.node.name.clone(),
                    cpu => get("cpu", s.cpu.0.to_string()), memory => get("memory", s.memory.0.to_string()),
                    disk => get("disk", s.disk.0.to_string()), swap => get("swap", s.swap.0.to_string()),
                    io => s.io.0, threads => get("threads", s.threads.clone().unwrap_or_default()),
                    oom_killer => get("oom_killer", if s.oom_killer.0 { "1".into() } else { "0".into() }),
                    allocation_limit => get("allocation_limit", s.allocation_limit.map(|v| v.0).unwrap_or(0).to_string()),
                    database_limit => get("database_limit", s.database_limit.map(|v| v.0).unwrap_or(0).to_string()),
                    backup_limit => get("backup_limit", s.backup_limit.0.to_string()),
                    image => get("image", s.image.clone()),
                    startup => get("startup", s.startup.clone()),
                    skip_scripts => get("skip_scripts", if s.skip_scripts.0 { "1".into() } else { "0".into() }),
                    egg => egg.name.clone(), egg_id => egg.id.0,
                    suspended => s.status.as_deref() == Some("suspended"),
                    failed_install => matches!(ServerState::parse(s.status.as_deref()), Some(ServerState::InstallFailed | ServerState::ReinstallFailed)),
                    conflict => server_info::in_conflict_state(s, c.node, pelican_core::server_state::active_transfer(&st.db, s.id.0).await?.is_some()),
                    icon => icon,
                    own_icon => has_own_icon,
                },
                condition => context! { label => cond_label, icon => cond_icon, color => cond_color },
                // The name's `hint_random` action: "{egg name, kebab}-{random word}" (a few to pick from).
                random_names => (0..12).map(|_| {
                    let prefix = egg.name.to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
                    if prefix.is_empty() { crate::words::random_word().to_string() } else { format!("{prefix}-{}", crate::words::random_word()) }
                }).collect::<Vec<_>>(),
                dice_face => rand::random_range(1..=6),
                owners => owners.iter().map(|u| vec![u.id.0.to_string(), format!("{} ({})", u.username, u.email)]).collect::<Vec<_>>(),
                images => images.iter().map(|(v, l)| vec![v.clone(), l.clone()]).collect::<Vec<_>>(),
                image_custom => !images.iter().any(|(v, _)| *v == s.image),
                startups => startups.iter().map(|(v, l)| vec![v.clone(), l.clone()]).collect::<Vec<_>>(),
                startup_custom => !startups.iter().any(|(v, _)| *v == s.startup),
                labels => labels,
                variables => Mj::from_serialize(&vars),
                mounts => mount_list,
                other_eggs => other_eggs,
                can_transfer => accessible_nodes > 1,
                binary => st.config.use_binary_prefix,
                errors => Mj::from_serialize(errors),
                rel => super::server_relations::context(admin, s, c.node).await?,
                sidecar_head => plugin_rel.as_ref().map(|t| t.head.clone()),
                sidecar_main => plugin_rel.as_ref().map(|t| t.main.clone()),
                plugin_tabs => Mj::from_serialize(plugin_rel.as_ref().map(|t| super::plugin_relation_tabs(&t.main)).unwrap_or_default()),
                sidecar_tail => plugin_rel.as_ref().map(|t| t.tail.clone()),
            },
        )
        .await?;
    for c in cookies {
        resp.headers_mut().append(axum::http::header::SET_COOKIE, c);
    }
    Ok(resp)
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view server")?;
    let (s, n) = find(&admin, id).await?;
    render_edit(&admin, Ctx { s: &s, node: &n, readonly: true }, None, &Errors::default(), None).await
}

async fn edit(admin: Admin, Path(id): Path<i64>, headers: axum::http::HeaderMap) -> WebResult {
    admin.authorize("update server")?;
    let (s, n) = find(&admin, id).await?;
    render_edit(&admin, Ctx { s: &s, node: &n, readonly: false }, None, &Errors::default(), Some(&headers)).await
}

fn num(form: &Form, unlimited_key: &str, key: &str) -> String {
    if form.get(unlimited_key) == "1" { "0".into() } else { form.get(key).trim().to_string() }
}

/// `POST /admin/servers/{id}/edit`: EditServer::save + afterSave (Wings sync when needed).
async fn save(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, n) = find(&admin, id).await?;
    let st = &admin.st;
    let swap = match form.get("swap_support") {
        "unlimited" => "-1".to_string(),
        "disabled" => "0".to_string(),
        _ => form.get("swap").trim().to_string(),
    };
    let threads = if form.get("cpu_pinning") == "1" { form.get("threads").trim().to_string() } else { String::new() };
    let data = json!({
        "name": form.get("name").trim(),
        "owner_id": form.get("owner_id"),
        "description": form.get("description"),
        "external_id": form.opt("external_id").map(str::trim).filter(|v| !v.is_empty()),
        "cpu": num(&form, "unlimited_cpu", "cpu"),
        "memory": num(&form, "unlimited_memory", "memory"),
        "disk": num(&form, "unlimited_disk", "disk"),
        "swap": swap,
        "threads": if threads.is_empty() { Value::Null } else { Value::String(threads.clone()) },
        "allocation_limit": form.get("allocation_limit").trim(),
        "database_limit": form.get("database_limit").trim(),
        "backup_limit": form.get("backup_limit").trim(),
        "image": form.get("image").trim(),
        "startup": form.get("startup").trim(),
    });
    let mut v = Validator::for_value(&data);
    for (k, label) in [
        ("name", "admin/server.name"),
        ("owner_id", "admin/server.owner"),
        ("cpu", "admin/server.cpu_limit"),
        ("memory", "admin/server.memory_limit"),
        ("disk", "admin/server.disk_limit"),
        ("swap", "admin/server.swap"),
        ("image", "admin/server.image"),
        ("startup", "admin/server.startup_cmd"),
        ("threads", "admin/server.threads"),
    ] {
        v.attribute_name(k, &admin.t(label).to_lowercase());
    }
    // Field rules + Server::$validationRules.
    v.rules("name", &["required", "string", "min:1", "max:255"]);
    v.rules("owner_id", &["required", "integer"]);
    v.rules("external_id", &["sometimes", "nullable", "string", "between:1,255"]);
    v.rules("cpu", &["required", "numeric", "min:0"]);
    v.rules("memory", &["required", "numeric", "min:0"]);
    v.rules("disk", &["required", "numeric", "min:0"]);
    v.rules("swap", &["required", "numeric", "min:-1"]);
    v.rules("threads", &["nullable", "regex:/^[0-9-,]+$/"]);
    v.rules("allocation_limit", &["required", "integer", "min:0"]);
    v.rules("database_limit", &["required", "integer", "min:0"]);
    v.rules("backup_limit", &["required", "integer", "min:0"]);
    v.rules("image", &["required", "string", "max:255"]);
    v.rules("startup", &["required", "string"]);
    let owner_exists = match form.get("owner_id").parse::<i64>() {
        Ok(o) => users::Entity::find_by_id(o).one(&st.db).await?.is_some(),
        Err(_) => false,
    };
    v.custom(
        "owner_id",
        CustomRule::precomputed("exists", (!owner_exists).then(|| "The selected :attribute is invalid.".into())),
    );
    if let Some(ext) = data["external_id"].as_str() {
        let taken = servers::Entity::find()
            .filter(servers::Column::ExternalId.eq(ext))
            .filter(servers::Column::Id.ne(id))
            .one(&st.db)
            .await?
            .is_some();
        v.custom(
            "external_id",
            CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
        );
    }
    v.run();
    let mut errors = Errors::from_validator(&v);
    let vars = variable_fields(&admin, Some(id), s.egg_id.0).await?;
    for (field, msg) in variables::validate(&vars, |f| form.get(&format!("var_{}", f.id)).to_string()) {
        errors.add(&field, msg);
    }
    if !errors.is_empty() {
        return render_edit(&admin, Ctx { s: &s, node: &n, readonly: false }, Some(&form), &errors, None).await;
    }

    let int = |k: &str| data[k].as_str().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as i64;
    let labels: serde_json::Map<String, Value> = form
        .list("label_key")
        .into_iter()
        .zip(form.list("label_value"))
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| (k.trim().to_string(), Value::String(v)))
        .collect();
    let owner_id = form.get("owner_id").parse::<i64>().unwrap_or(s.owner_id.0);
    let new = servers::Model {
        name: data["name"].as_str().unwrap_or_default().to_string(),
        owner_id: Int(owner_id),
        description: Some(form.get("description").to_string()),
        external_id: data["external_id"].as_str().map(String::from),
        cpu: Int(int("cpu")),
        memory: Int(int("memory")),
        disk: Int(int("disk")),
        swap: Int(int("swap")),
        threads: (!threads.is_empty()).then_some(threads),
        oom_killer: SmallBool(form.get("oom_killer") == "1"),
        allocation_limit: Some(Int(int("allocation_limit"))),
        database_limit: Some(Int(int("database_limit"))),
        backup_limit: Int(int("backup_limit")),
        image: data["image"].as_str().unwrap_or_default().to_string(),
        startup: data["startup"].as_str().unwrap_or_default().to_string(),
        skip_scripts: pelican_db::Bool(form.get("skip_scripts") == "1"),
        docker_labels: Some(Json(Value::Object(labels))),
        ..s.clone()
    };
    // Wings-relevant changes (afterSave excludes these keys).
    let wings_changed = new.cpu != s.cpu
        || new.memory != s.memory
        || new.disk != s.disk
        || new.swap != s.swap
        || new.threads != s.threads
        || new.oom_killer != s.oom_killer
        || new.image != s.image
        || new.startup != s.startup
        || new.docker_labels != s.docker_labels;
    let txn = st.db.begin().await?;
    let am = servers::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(s.id),
        name: Set(new.name.clone()),
        owner_id: Set(new.owner_id),
        description: Set(new.description.clone()),
        external_id: Set(new.external_id.clone()),
        cpu: Set(new.cpu),
        memory: Set(new.memory),
        disk: Set(new.disk),
        swap: Set(new.swap),
        threads: Set(new.threads.clone()),
        oom_killer: Set(new.oom_killer),
        allocation_limit: Set(new.allocation_limit),
        database_limit: Set(new.database_limit),
        backup_limit: Set(new.backup_limit),
        image: Set(new.image.clone()),
        startup: Set(new.startup.clone()),
        skip_scripts: Set(new.skip_scripts),
        docker_labels: Set(new.docker_labels.clone()),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    // EditRecord → `$record->update($data)`; the event fires only when a
    // column actually changed (Eloquent isDirty).
    let server_changed = new.name != s.name
        || new.owner_id != s.owner_id
        || new.description != s.description
        || new.external_id != s.external_id
        || new.cpu != s.cpu
        || new.memory != s.memory
        || new.disk != s.disk
        || new.swap != s.swap
        || new.threads != s.threads
        || new.oom_killer != s.oom_killer
        || new.allocation_limit != s.allocation_limit
        || new.database_limit != s.database_limit
        || new.backup_limit != s.backup_limit
        || new.image != s.image
        || new.startup != s.startup
        || new.skip_scripts != s.skip_scripts
        || new.docker_labels != s.docker_labels;
    pelican_db::write::update(&txn, &am).await?;
    let mut vars_changed = false;
    let mut var_created: Vec<i64> = Vec::new();
    let mut var_updated: Vec<i64> = Vec::new();
    for f in &vars {
        let value = form.get(&format!("var_{}", f.id)).to_string();
        if value != f.value {
            vars_changed = true;
        }
        let b = txn.get_database_backend();
        let now = pelican_core::time::now();
        let updated = txn
            .execute_raw(raw::stmt(
                b,
                "update {server_variables} set {variable_value} = ?, {updated_at} = ? where {server_id} = ? and {variable_id} = ?",
                vec![value.clone().into(), now.into(), id.into(), f.id.into()],
            ))
            .await?
            .rows_affected();
        if updated == 0 {
            let am = server_variables::ActiveModel {
                server_id: Set(Int(id)),
                variable_id: Set(Int(f.id)),
                variable_value: Set(value),
                created_at: Set(Some(Timestamp(now))),
                updated_at: Set(Some(Timestamp(now))),
                ..Default::default()
            };
            // Repeater relationship save → `ServerVariable::create` per row.
            if let Some(new_id) = pelican_db::write::insert_id(&txn, &am).await? {
                var_created.push(new_id);
            }
        } else if value != f.value {
            // The event needs the ServerVariable's own id, not the egg
            // variable's (`$sv->update([...])` per repeater row).
            match server_variables::Entity::find()
                .filter(server_variables::Column::ServerId.eq(Int(id)))
                .filter(server_variables::Column::VariableId.eq(Int(f.id)))
                .one(&txn)
                .await?
                .map(|m| m.id.0)
            {
                Some(sv_id) => var_updated.push(sv_id),
                None => continue,
            }
        }
    }
    // Mounts (CheckboxList relationship sync).
    let allowed: Vec<i64> = mount_options(&admin, n.id.0, s.egg_id.0).await?.into_iter().map(|m| m.id.0).collect();
    let chosen: Vec<i64> =
        form.list("mounts").iter().filter_map(|m| m.parse().ok()).filter(|m| allowed.contains(m)).collect();
    let b = txn.get_database_backend();
    txn.execute_raw(raw::stmt(
        b,
        "delete from {mountables} where {mountable_type} = ? and {mountable_id} = ?",
        vec!["server".into(), id.into()],
    ))
    .await?;
    for m in &chosen {
        txn.execute_raw(raw::stmt(
            b,
            "insert into {mountables} ({mountable_type}, {mountable_id}, {mount_id}) values (?, ?, ?)",
            vec!["server".into(), id.into(), (*m).into()],
        ))
        .await?;
    }
    if new.owner_id != s.owner_id {
        let ctx = admin.api_ctx(Bound::default());
        pelican_api::services::server_saved(st, &ctx, &txn, &new)
            .await
            .map_err(|e| WebError::internal(format!("{e:?}")))?;
    }
    txn.commit().await?;
    // eloquent events (order: server, then the variable rows).
    if server_changed {
        model_events::fire_id::<servers::Entity>(st, model_events::Action::Updated, id).await;
    }
    for vid in var_updated {
        model_events::fire_id::<server_variables::Entity>(st, model_events::Action::Updated, vid).await;
    }
    for vid in var_created {
        model_events::fire_id::<server_variables::Entity>(st, model_events::Action::Created, vid).await;
    }
    if wings_changed || vars_changed {
        if sync_wings(&admin, &new).await {
            admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
        }
    } else {
        admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    }
    let tab = form.opt("_tab").filter(|t| TABS.contains(t)).unwrap_or("information");
    Ok(admin.redirect(&format!("/admin/servers/{id}/edit?tab={tab}")))
}

/// `POST /admin/servers/{id}/action`: header/actions-tab buttons.
async fn action(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let (s, n) = find(&admin, id).await?;
    let st = &admin.st;
    let op = form.get("op").to_string();
    let back = format!("/admin/servers/{id}/edit?tab=actions");
    let node_err = || admin.t_with("admin/server.notifications.error_connecting", &[("node", &n.name)]);
    let mut bound = Bound::default();
    bound.server = Some(s.clone());
    let ctx = admin.api_ctx(bound);
    match op.as_str() {
        "toggle-install" => {
            admin.authorize("update server")?;
            let state = ServerState::parse(s.status.as_deref());
            if matches!(state, Some(ServerState::InstallFailed | ServerState::ReinstallFailed)) {
                match pelican_api::application::servers::reinstall_server(st, &ctx, s.clone()).await {
                    Ok(()) => admin.flash_success(admin.t("admin/server.notifications.reinstall_started")),
                    Err(_) => {
                        admin.flash_danger(admin.t("admin/server.notifications.reinstall_failed"), Some(node_err()))
                    }
                }
            } else if state == Some(ServerState::Suspended) {
                admin.flash_danger(admin.t("admin/server.notifications.install_toggle_failed"), None);
            } else {
                // ToggleInstallService: installed → installing, otherwise →
                // installed (null); `$server->save()` → updated event.
                let status: Option<String> = server_info::is_installed(&s).then(|| "installing".into());
                update_columns(&admin, id, &["status"], vec![status.into()]).await?;
                model_events::fire_id::<servers::Entity>(st, model_events::Action::Updated, id).await;
                admin.flash_success(admin.t("admin/server.notifications.install_toggled"));
            }
        }
        "suspend" | "unsuspend" => {
            admin.authorize("update server")?;
            match pelican_api::application::servers::set_suspension(st, &ctx, s.clone(), op == "suspend").await {
                Ok(()) => admin.flash_success(admin.t(if op == "suspend" {
                    "admin/server.notifications.server_suspended"
                } else {
                    "admin/server.notifications.server_unsuspended"
                })),
                Err(_) => admin.session.flash(
                    "warning",
                    admin.t("admin/server.notifications.server_suspension"),
                    Some(node_err()),
                ),
            }
        }
        "reinstall" => {
            admin.authorize("update server")?;
            match pelican_api::application::servers::reinstall_server(st, &ctx, s.clone()).await {
                Ok(()) => admin.flash_success(admin.t("admin/server.notifications.reinstall_started")),
                Err(_) => admin.flash_danger(admin.t("admin/server.notifications.reinstall_failed"), Some(node_err())),
            }
        }
        "change-egg" => {
            admin.authorize("update server")?;
            let Ok(egg_id) = form.get("egg_id").parse::<i64>() else { return Ok(admin.redirect(&back)) };
            change_egg(&admin, &s, egg_id, form.bool("keep_old_variables")).await?;
            return Ok(admin.redirect(&format!("/admin/servers/{id}/edit?tab=egg")));
        }
        "delete" | "force-delete" => {
            admin.authorize("delete server")?;
            let force = op == "force-delete";
            match pelican_api::application::servers::delete_server(st, &ctx, &s, force).await {
                Ok(()) => {
                    crate::cache::forget(&format!("servers.{}.canForceDelete", s.uuid));
                    return Ok(admin.redirect("/admin/servers"));
                }
                Err(_) if !force => {
                    crate::cache::put(
                        &format!("servers.{}.canForceDelete", s.uuid),
                        std::time::Duration::from_secs(300),
                        Value::Bool(true),
                    );
                    admin.session.flash(
                        "warning",
                        admin.t("admin/server.notifications.error_server_delete"),
                        Some(admin.t("admin/server.notifications.error_server_delete_body")),
                    );
                    return Ok(admin.redirect(&format!("/admin/servers/{id}/edit")));
                }
                Err(_) => {
                    crate::cache::forget(&format!("servers.{}.canForceDelete", s.uuid));
                    return Ok(admin.redirect(&format!("/admin/servers/{id}/edit")));
                }
            }
        }
        _ => return Err(WebError::Unprocessable),
    }
    Ok(admin.redirect(&back))
}

/// `EggChangerService::handle()`.
async fn change_egg(admin: &Admin, s: &servers::Model, egg_id: i64, keep: bool) -> Result<(), WebError> {
    if egg_id == s.egg_id.0 {
        return Ok(());
    }
    let st = &admin.st;
    let egg = eggs::Entity::find_by_id(egg_id).one(&st.db).await?.ok_or(WebError::NotFound)?;
    let old: HashMap<String, String> = if keep {
        let vars =
            egg_variables::Entity::find().filter(egg_variables::Column::EggId.eq(s.egg_id.0)).all(&st.db).await?;
        let values =
            server_variables::Entity::find().filter(server_variables::Column::ServerId.eq(s.id.0)).all(&st.db).await?;
        values
            .into_iter()
            .filter_map(|v| {
                vars.iter().find(|e| e.id == v.variable_id).map(|e| (e.env_variable.clone(), v.variable_value))
            })
            .collect()
    } else {
        HashMap::new()
    };
    let txn = st.db.begin().await?;
    let image = json_list(egg.docker_images.as_ref()).into_iter().next().unwrap_or_default();
    let startup = json_list(egg.startup_commands.as_ref()).into_iter().next().unwrap_or_default();
    let b = txn.get_database_backend();
    txn.execute_raw(raw::stmt(
        b,
        "update {servers} set {egg_id} = ?, {image} = ?, {startup} = ?, {updated_at} = ? where {id} = ?",
        vec![egg_id.into(), image.into(), startup.into(), pelican_core::time::now().into(), s.id.0.into()],
    ))
    .await?;
    txn.execute_raw(raw::stmt(b, "delete from {server_variables} where {server_id} = ?", vec![s.id.0.into()])).await?;
    let now = Timestamp(pelican_core::time::now());
    let mut created_vars: Vec<i64> = Vec::new();
    for v in egg_variables::Entity::find().filter(egg_variables::Column::EggId.eq(egg_id)).all(&txn).await? {
        let am = server_variables::ActiveModel {
            server_id: Set(s.id),
            variable_id: Set(v.id),
            variable_value: Set(old.get(&v.env_variable).cloned().unwrap_or(v.default_value.clone())),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        };
        if let Some(new_id) = pelican_db::write::insert_id(&txn, &am).await? {
            created_vars.push(new_id);
        }
    }
    txn.commit().await?;
    // EggChangerService: `$server->forceFill([...])->saveOrFail()` (updated) and
    // `ServerVariable::create([...])` per egg variable (created; the mass
    // `ServerVariable::where(...)->delete()` fires nothing).
    model_events::fire_id::<servers::Entity>(st, model_events::Action::Updated, s.id.0).await;
    for vid in created_vars {
        model_events::fire_id::<server_variables::Entity>(st, model_events::Action::Created, vid).await;
    }
    Ok(())
}
