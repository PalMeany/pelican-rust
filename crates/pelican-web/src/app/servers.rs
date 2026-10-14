//! Server list of the app panel (app/Filament/App/Resources/Servers/Pages/ListServers.php).

use std::collections::HashMap;

use axum::extract::Path;
use axum::response::Response;
use minijinja::{Value as MjValue, context};
use pelican_core::auth::AuthUser;
use pelican_core::permissions::has_server_permission;
use pelican_db::entities::{allocations, eggs, nodes, servers, users};
use pelican_db::raw;
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, QueryFilter, Value};
use serde::Serialize;
use serde_json::json;

use crate::ctx::{Authed, Web};
use crate::error::{WebError, WebResult};
use crate::forms::Form;
use crate::models;
use crate::server_info::{self, ContainerStatus};
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Tab, Table, threshold_color};

/// `User::accessibleServers()` as a `where` fragment over `servers`.
fn accessible(user: &AuthUser) -> (String, Vec<Value>) {
    let me = user.id();
    let direct = "({servers.owner_id} = ? or exists (select 1 from {subusers} where {subusers.server_id} = {servers.id} and {subusers.user_id} = ?))";
    if user.gate_before("viewList server") {
        match &user.node_ids {
            // accessibleNodes() is every node: every server matches.
            None => ("1 = 1".into(), vec![]),
            Some(ids) if ids.is_empty() => (direct.into(), vec![me.into(), me.into()]),
            Some(ids) => {
                let marks = vec!["?"; ids.len()].join(", ");
                let mut vals: Vec<Value> = vec![me.into(), me.into()];
                vals.extend(ids.iter().map(|i| Value::from(*i)));
                (format!("({direct} or {{servers.node_id}} in ({marks}))"), vals)
            }
        }
    } else {
        (direct.into(), vec![me.into(), me.into()])
    }
}

#[derive(FromQueryResult)]
struct Aggregate {
    aggregate: pelican_db::Int,
}

async fn count(web: &Web, b: DbBackend, wheres: &[String], vals: &[Value]) -> Result<u64, WebError> {
    let sql = format!("select count(*) as {{aggregate}} from {{servers}} where {}", wheres.join(" and "));
    let row = Aggregate::find_by_statement(raw::stmt(b, &sql, vals.to_vec())).one(&web.st.db).await?;
    Ok(row.map(|r| r.aggregate.0.max(0) as u64).unwrap_or(0))
}

#[derive(Serialize)]
struct Card {
    name: String,
    url: String,
    description: Option<String>,
    uptime: String,
    condition_label: String,
    condition_icon: &'static str,
    condition_color: &'static str,
    icon: Option<String>,
    address: String,
    cpu: Cell,
    memory: Cell,
    disk: Cell,
    actions: Vec<Action>,
}

struct Usage {
    cpu: Cell,
    memory: Cell,
    disk: Cell,
}

fn progress(current: f64, max: f64, label: String) -> Cell {
    let percent = if max > 0.0 { current / max * 100.0 } else { 0.0 };
    Cell::Progress { percent: (percent * 100.0).round() / 100.0, color: threshold_color(current, max).into(), label }
}

/// The three ProgressBarColumns (cpu / memory / disk) of a server.
async fn usage(web: &Web, s: &servers::Model, node: &nodes::Model, details: &serde_json::Value) -> Usage {
    let binary = web.st.config.use_binary_prefix;
    let stats = server_info::node_statistics(&web.st, node).await;
    let cpu_now = server_info::resource(details, "cpu_absolute");
    let cpu_max = if s.cpu.0 == 0 {
        let info = server_info::node_system_information(&web.st, node).await;
        info.get("cpu_count").and_then(|c| c.as_f64()).unwrap_or(0.0) * 100.0
    } else {
        s.cpu.0 as f64
    };
    let fmt_pct = |v: f64| format!("{}%", v.round() as i64);
    let cpu_label = format!(
        "{} / {}",
        fmt_pct(cpu_now),
        if s.cpu.0 == 0 { server_info::UNLIMITED.to_string() } else { fmt_pct(s.cpu.0 as f64) }
    );
    let mem_now = server_info::resource(details, "memory_bytes");
    let mem_limit = server_info::limit_bytes(s.memory.0, binary);
    let mem_max =
        if s.memory.0 == 0 { stats.get("memory_total").and_then(|v| v.as_f64()).unwrap_or(0.0) } else { mem_limit };
    let mem_label = format!(
        "{} / {}",
        server_info::format_bytes_gib(mem_now, 2, binary),
        if s.memory.0 == 0 {
            server_info::UNLIMITED.to_string()
        } else {
            server_info::format_bytes_gib(mem_limit, 2, binary)
        }
    );
    let disk_now = server_info::resource(details, "disk_bytes");
    let disk_limit = server_info::limit_bytes(s.disk.0, binary);
    let disk_max =
        if s.disk.0 == 0 { stats.get("disk_total").and_then(|v| v.as_f64()).unwrap_or(0.0) } else { disk_limit };
    let disk_label = format!(
        "{} / {}",
        server_info::format_bytes_gib(disk_now, 2, binary),
        if s.disk.0 == 0 {
            server_info::UNLIMITED.to_string()
        } else {
            server_info::format_bytes_gib(disk_limit, 2, binary)
        }
    );
    Usage {
        cpu: progress(cpu_now, cpu_max, cpu_label),
        memory: progress(mem_now, mem_max, mem_label),
        disk: progress(disk_now, disk_max, disk_label),
    }
}

/// `ListServers::getPowerActionGroup()` for one server.
async fn power_actions(
    web: &Web,
    user: &AuthUser,
    s: &servers::Model,
    status: ContainerStatus,
    conflict: bool,
) -> Result<Vec<Action>, WebError> {
    if conflict {
        return Ok(Vec::new());
    }
    let db = &web.st.db;
    let url = format!("/servers/{}/power", s.uuid_short);
    let mut out = Vec::new();
    let label = |k: &str| web.t(&format!("server/console.power_actions.{k}"));
    if status.is_startable() && has_server_permission(db, user, s, "control.start").await? {
        out.push(Action::post(label("start"), "player-play-filled", &url).color("primary").field("action", "start"));
    }
    if status.is_restartable() && has_server_permission(db, user, s, "control.restart").await? {
        out.push(Action::post(label("restart"), "reload", &url).field("action", "restart"));
    }
    let can_stop = has_server_permission(db, user, s, "control.stop").await?;
    if can_stop && status.is_stoppable() && !status.is_killable() {
        out.push(Action::post(label("stop"), "player-stop-filled", &url).color("danger").field("action", "stop"));
    }
    if can_stop && status.is_killable() {
        out.push(Action::post(label("kill"), "alert-square", &url).color("danger").field("action", "kill"));
    }
    Ok(out)
}

/// `GET /`.
pub async fn index(web: Authed) -> WebResult {
    let st = web.st.clone();
    let user = web.user.clone();
    let me = user.id();
    let b = st.db.get_database_backend();
    let custom = models::customization(&st, &user.user);
    let grid = custom.get("dashboard_layout").and_then(|v| v.as_str()) == Some("grid");
    let options: &[u64] = if grid { &[10, 20, 30, 40] } else { &[10, 20, 50, 100] };
    let stored = custom.get("servers_per_page").and_then(|v| v.as_u64()).unwrap_or(0);
    let default = if options.contains(&stored) {
        stored
    } else if grid {
        10
    } else {
        20
    };
    let mut p = ListParams::parse(web.query.as_deref(), options, default);
    // updatedTableRecordsPerPage(): remember an explicit, offered choice.
    let asked = web.query.as_deref().unwrap_or("").split('&').any(|kv| kv.starts_with("per_page="));
    if asked && p.per_page != stored && options.contains(&p.per_page) {
        save_customization(&web, &user.user, "servers_per_page", json!(p.per_page)).await?;
    }
    let tab = p.tab.clone().unwrap_or_else(|| "my".into());
    p.tab = Some(tab.clone());

    let (acc_sql, acc_vals) = accessible(&user);
    let mine = |v: &mut Vec<Value>| v.push(me.into());
    let base_where = vec![acc_sql.clone()];
    let mut my_vals = acc_vals.clone();
    mine(&mut my_vals);
    let my_count = count(&web, b, &[acc_sql.clone(), "{servers.owner_id} = ?".into()], &my_vals).await?;
    let other_count = count(&web, b, &[acc_sql.clone(), "{servers.owner_id} <> ?".into()], &my_vals).await?;
    let all_count = count(&web, b, &base_where, &acc_vals).await?;

    let mut wheres = base_where.clone();
    let mut vals = acc_vals.clone();
    match tab.as_str() {
        "my" => {
            wheres.push("{servers.owner_id} = ?".into());
            mine(&mut vals);
        }
        "other" => {
            wheres.push("{servers.owner_id} <> ?".into());
            mine(&mut vals);
        }
        _ => {}
    }
    if !p.search.is_empty() {
        wheres.push("LOWER({servers.name}) LIKE ?".into());
        vals.push(format!("%{}%", p.search.to_lowercase()).into());
    }
    if let Some(egg) = p.filter("egg").and_then(|v| v.parse::<i64>().ok()) {
        wheres.push("{servers.egg_id} = ?".into());
        vals.push(egg.into());
    }
    if tab != "my" {
        if let Some(owner) = p.filter("owner").and_then(|v| v.parse::<i64>().ok()) {
            wheres.push("{servers.owner_id} = ?".into());
            vals.push(owner.into());
        }
    }
    let total = count(&web, b, &wheres, &vals).await?;
    p.clamp(total);
    let order = match p.sort.as_deref() {
        Some("name") => format!("{{servers.name}} {}", if p.desc { "desc" } else { "asc" }),
        _ => "{servers.id} asc".into(),
    };
    let mut sql = format!("select {{servers}}.* from {{servers}} where {} order by {order}", wheres.join(" and "));
    if let Some((limit, offset)) = p.limit_offset() {
        sql.push_str(&format!(" limit {limit} offset {offset}"));
    }
    let rows = servers::Model::find_by_statement(raw::stmt(b, &sql, vals)).all(&st.db).await?;

    // Relations used by the columns.
    let ids = |f: fn(&servers::Model) -> Option<i64>| rows.iter().filter_map(f).collect::<Vec<_>>();
    let node_map: HashMap<i64, nodes::Model> = nodes::Entity::find()
        .filter(nodes::Column::Id.is_in(ids(|s| Some(s.node_id.0))))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|n| (n.id.0, n))
        .collect();
    let egg_map: HashMap<i64, eggs::Model> = eggs::Entity::find()
        .filter(eggs::Column::Id.is_in(ids(|s| Some(s.egg_id.0))))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|e| (e.id.0, e))
        .collect();
    let alloc_map: HashMap<i64, allocations::Model> = allocations::Entity::find()
        .filter(allocations::Column::Id.is_in(ids(|s| s.allocation_id.map(|a| a.0))))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|a| (a.id.0, a))
        .collect();
    let transfers: Vec<i64> = if rows.is_empty() {
        Vec::new()
    } else {
        let marks = vec!["?"; rows.len()].join(", ");
        let q = format!(
            "select {{server_id}} as {{aggregate}} from {{server_transfers}} where {{successful}} is null and {{server_id}} in ({marks})"
        );
        Aggregate::find_by_statement(raw::stmt(b, &q, rows.iter().map(|s| Value::from(s.id.0)).collect()))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|a| a.aggregate.0)
            .collect()
    };

    // Wings lookups run concurrently (each is cached for 15 s).
    let details: Vec<serde_json::Value> = futures::future::join_all(rows.iter().map(|s| {
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

    let locale = web.locale();
    let none = web.t("server/dashboard.none");
    let mut table = Table::new("servers", "/");
    let mut cards = Vec::new();
    for (s, d) in rows.iter().zip(details.iter()) {
        let Some(node) = node_map.get(&s.node_id.0) else { continue };
        let (cond_label, cond_icon, cond_color) = server_info::condition(s, d, &locale);
        let status = server_info::status(d);
        let conflict = server_info::in_conflict_state(s, node, transfers.contains(&s.id.0));
        let actions = power_actions(&web, &user, s, status, conflict).await?;
        let icon = models::icon_url(&st, "server", &s.uuid).or_else(|| {
            egg_map.get(&s.egg_id.0).and_then(|e| e.uuid.as_deref()).and_then(|u| models::icon_url(&st, "egg", u))
        });
        let address = s.allocation_id.and_then(|a| alloc_map.get(&a.0)).map(models::display_address);
        let uptime = match server_info::ServerState::parse(s.status.as_deref()) {
            Some(state) => state.label().to_string(),
            None => {
                let ms = server_info::resource(d, "uptime");
                if ms == 0.0 { ContainerStatus::Offline.label(&locale) } else { server_info::format_uptime(ms) }
            }
        };
        let u = usage(&web, s, node, d).await;
        let url = format!("/server/{}", s.uuid_short);
        if grid {
            cards.push(Card {
                name: s.name.clone(),
                url,
                description: s.description.clone().filter(|d| !d.is_empty()).map(|d| limit_words(&d, 40)),
                uptime,
                condition_label: cond_label,
                condition_icon: cond_icon,
                condition_color: cond_color,
                icon,
                address: address.unwrap_or_else(|| none.clone()),
                cpu: u.cpu,
                memory: u.memory,
                disk: u.disk,
                actions,
            });
        } else {
            let mut row = Row::new(s.uuid_short.clone())
                .url(url)
                .cell("icon", Cell::Image { src: icon.unwrap_or_default() })
                .cell(
                    "condition",
                    Cell::Badge {
                        text: cond_label,
                        color: cond_color.into(),
                        icon: Some(cond_icon.into()),
                        tooltip: Some(uptime),
                        copy: None,
                    },
                )
                .cell("name", Cell::text_desc(s.name.clone(), s.description.clone()))
                .cell(
                    "address",
                    Cell::Badge {
                        text: address.clone().unwrap_or_else(|| "None".into()),
                        color: "gray".into(),
                        icon: None,
                        tooltip: None,
                        copy: address,
                    },
                )
                .cell("cpu", u.cpu)
                .cell("memory", u.memory)
                .cell("disk", u.disk);
            row.group = actions;
            row.group_icon = Some("power".into());
            table.rows.push(row);
        }
    }

    table.columns = vec![
        Column::new("icon", ""),
        Column::new("condition", web.t("server/dashboard.status")),
        Column::new("name", web.t("server/dashboard.title")).sortable(),
        Column::new("address", "").wide_only(),
        Column::new("cpu", ""),
        Column::new("memory", ""),
        Column::new("disk", ""),
    ];
    table.searchable = true;
    table.poll = (!grid).then(|| "15s".into());
    table.tabs = vec![
        Tab {
            key: "my".into(),
            label: web.t("server/dashboard.tabs.my"),
            badge: Some(my_count as i64),
            url: String::new(),
            active: false,
        },
        Tab {
            key: "other".into(),
            label: web.t("server/dashboard.tabs.other"),
            badge: Some(other_count as i64),
            url: String::new(),
            active: false,
        },
        Tab {
            key: "all".into(),
            label: web.t("server/dashboard.tabs.all"),
            badge: Some(all_count as i64),
            url: String::new(),
            active: false,
        },
    ];
    table.filters = filters(&web, b, &acc_sql, &acc_vals, &p, &tab).await?;
    table.empty_icon = "brand-docker".into();
    table.empty_heading =
        web.t(if tab == "my" { "server/dashboard.empty_own" } else { "server/dashboard.empty_other" });
    table.finish(&p, total, options, &locale);
    web.render(
        "app/servers.html",
        context! {
            table => MjValue::from_serialize(&table),
            grid => grid,
            cards => MjValue::from_serialize(&cards),
        },
    )
}

/// `Str::limit($text, $limit, preserveWords: true)`.
fn limit_words(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let cut: String = s.chars().take(limit).collect();
    let trimmed = match cut.rfind(' ') {
        Some(i) if s.chars().nth(limit) != Some(' ') => cut[..i].to_string(),
        _ => cut,
    };
    format!("{}...", trimmed.trim_end())
}

/// SelectFilters `egg` and `owner` limited to values present in the accessible servers.
async fn filters(
    web: &Web,
    b: DbBackend,
    acc_sql: &str,
    acc_vals: &[Value],
    p: &ListParams,
    tab: &str,
) -> Result<Vec<Filter>, WebError> {
    let db = &web.st.db;
    let egg_sql = format!(
        "select * from {{eggs}} where {{id}} in (select {{servers.egg_id}} from {{servers}} where {acc_sql}) order by {{name}}"
    );
    let egg_rows = eggs::Model::find_by_statement(raw::stmt(b, &egg_sql, acc_vals.to_vec())).all(db).await?;
    let mut out = vec![Filter {
        name: "egg".into(),
        label: "Egg".into(),
        options: egg_rows.iter().map(|e| (e.id.0.to_string(), e.name.clone())).collect(),
        value: p.filter("egg").unwrap_or("").into(),
    }];
    if tab != "my" {
        let user_sql = format!(
            "select * from {{users}} where {{id}} in (select {{servers.owner_id}} from {{servers}} where {acc_sql}) order by {{username}}"
        );
        let user_rows = users::Model::find_by_statement(raw::stmt(b, &user_sql, acc_vals.to_vec())).all(db).await?;
        out.push(Filter {
            name: "owner".into(),
            label: "Owner".into(),
            options: user_rows.iter().map(|u| (u.id.0.to_string(), u.username.clone())).collect(),
            value: p.filter("owner").unwrap_or("").into(),
        });
    }
    Ok(out)
}

/// `User::setCustomization()`.
pub async fn save_customization(
    web: &Web,
    user: &users::Model,
    key: &str,
    value: serde_json::Value,
) -> Result<(), WebError> {
    let mut stored = match user.customization.as_ref().map(|j| &j.0) {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
        Some(serde_json::Value::Object(m)) => serde_json::Value::Object(m.clone()),
        _ => json!({}),
    };
    if let Some(m) = stored.as_object_mut() {
        m.insert(key.into(), value);
    }
    let b = web.st.db.get_database_backend();
    let sql = if b == DbBackend::Postgres { "{customization} = CAST(? AS json)" } else { "{customization} = ?" };
    web.st
        .db
        .execute_raw(raw::stmt(
            b,
            &format!("update {{users}} set {sql}, {{updated_at}} = ? where {{id}} = ?"),
            vec![
                laravel_compat::json::to_string(&stored).map_err(WebError::internal)?.into(),
                pelican_core::time::now().into(),
                user.id.0.into(),
            ],
        ))
        .await?;
    Ok(())
}

/// `POST /servers/{uuid_short}/power` (`ListServers::powerAction`).
pub async fn power(web: Authed, Path(id): Path<String>, form: Form) -> WebResult<Response> {
    let action = form.get("action").to_string();
    let permission = match action.as_str() {
        "start" => "control.start",
        "restart" => "control.restart",
        "stop" | "kill" => "control.stop",
        _ => return Err(WebError::Unprocessable),
    };
    let st = &web.st;
    let server = servers::Entity::find()
        .filter(servers::Column::UuidShort.eq(id))
        .one(&st.db)
        .await?
        .ok_or(WebError::NotFound)?;
    if !has_server_permission(&st.db, &web.user, &server, permission).await? {
        return Err(WebError::Forbidden);
    }
    let node = nodes::Entity::find_by_id(server.node_id.0).one(&st.db).await?.ok_or(WebError::NotFound)?;
    let transfer = pelican_db::entities::server_transfers::Entity::find()
        .filter(pelican_db::entities::server_transfers::Column::ServerId.eq(server.id.0))
        .filter(pelican_db::entities::server_transfers::Column::Successful.is_null())
        .one(&st.db)
        .await?
        .is_some();
    if server_info::in_conflict_state(&server, &node, transfer) {
        return Err(WebError::Forbidden);
    }
    let sent = match st.node_conn(&node) {
        Ok(conn) => st.wings.power(&conn, &server.uuid, &action).await.is_ok(),
        Err(_) => false,
    };
    if sent {
        web.session.flash(
            "success",
            web.t("server/dashboard.power_actions"),
            Some(web.t_with("server/dashboard.power_action_sent", &[("action", &action), ("name", &server.name)])),
        );
        server_info::forget_details(&server.uuid);
    } else {
        web.flash_danger(web.t_with("exceptions.node.error_connecting", &[("node", &node.name)]), None);
    }
    Ok(web.redirect(&form.back("/")))
}
