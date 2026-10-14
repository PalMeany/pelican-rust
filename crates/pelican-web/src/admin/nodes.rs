//! Admin › Nodes (app/Filament/Admin/Resources/Nodes): list with health and
//! browser reachability, create, edit tabs (overview, basic, advanced, config
//! file with auto-deploy and token reset, diagnostics), allocations and servers.

use std::time::Duration;

use axum::Json as AxJson;
use axum::Router;
use axum::extract::Path;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::validation::Validator;
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, api_keys, mountables, mounts, nodes, servers};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder};
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::server_info;
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Table};

const SLUG: &str = "nodes";
const TABS: [&str; 5] = ["overview", "basic_settings", "advanced_settings", "config_file", "diagnostics"];
const DEFAULT_DAEMON_BASE: &str = "/var/lib/pelican/volumes";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/nodes", get(index))
        .route("/admin/nodes/create", get(create_form).post(create))
        .route("/admin/nodes/{id}", get(view))
        .route("/admin/nodes/{id}/edit", get(edit).post(save))
        .route("/admin/nodes/{id}/stats", get(stats))
        .route("/admin/nodes/{id}/action", post(action))
        .route("/admin/nodes/{id}/deploy-command", get(deploy_command))
        .route("/admin/nodes/{id}/allocations/create", post(allocation_create))
        .route("/admin/nodes/{id}/allocations/{aid}/{op}", post(allocation_op))
}

async fn find(admin: &Admin, id: i64) -> Result<nodes::Model, WebError> {
    if !admin.user.can_target_node(id) {
        return Err(WebError::NotFound);
    }
    nodes::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)
}

fn tags_of(n: &nodes::Model) -> Vec<String> {
    match n.tags.as_ref().map(|j| &j.0) {
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str().map(String::from)).collect(),
        Some(Value::Object(m)) => m.values().filter_map(|t| t.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => serde_json::from_str::<Vec<String>>(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `Node::getConnectionAddress()`.
fn connection_address(n: &nodes::Model) -> String {
    format!("{}://{}:{}", n.scheme, n.fqdn, n.daemon_connect.0)
}

/// `GET /admin/nodes` (ListNodes).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList node")?;
    let st = &admin.st;
    let opts = [10, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let mut all: Vec<nodes::Model> = nodes::Entity::find()
        .order_by_asc(nodes::Column::Id)
        .all(&st.db)
        .await?
        .into_iter()
        .filter(|n| admin.user.can_target_node(n.id.0))
        .collect();
    if !p.search.is_empty() {
        let q = p.search.to_lowercase();
        all.retain(|n| {
            n.name.to_lowercase().contains(&q)
                || n.fqdn.to_lowercase().contains(&q)
                || n.uuid.as_deref().unwrap_or("").contains(&q)
        });
    }
    if let Some(tag) = p.filter("tag") {
        all.retain(|n| tags_of(n).iter().any(|t| t == tag));
    }
    let counts: Vec<(i64, i64)> = {
        let b = st.db.get_database_backend();
        #[derive(sea_orm::FromQueryResult)]
        struct C {
            node_id: Int,
            c: Int,
        }
        C::find_by_statement(raw::stmt(
            b,
            "select {node_id}, count(*) as {c} from {servers} group by {node_id}",
            vec![],
        ))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.node_id.0, c.c.0))
        .collect()
    };
    let servers_of = |id: i64| counts.iter().find(|(n, _)| *n == id).map(|(_, c)| *c).unwrap_or(0);
    let desc = p.desc;
    match p.sort.as_deref() {
        Some("name") => all.sort_by(|a, b| a.name.cmp(&b.name)),
        Some("fqdn") => all.sort_by(|a, b| a.fqdn.cmp(&b.fqdn)),
        Some("servers") => all.sort_by_key(|n| servers_of(n.id.0)),
        _ => {}
    }
    if desc && p.sort.is_some() {
        all.reverse();
    }
    let total = all.len() as u64;
    let page: Vec<nodes::Model> = match p.limit_offset() {
        Some((l, o)) => all.iter().skip(o as usize).take(l as usize).cloned().collect(),
        None => all.clone(),
    };
    let infos = futures::future::join_all(page.iter().map(|n| server_info::node_system_information(st, n))).await;
    let t = |k: &str| admin.t(k);
    let can_edit = admin.can("update node");
    let mut table = Table::new("nodes", "/admin/nodes");
    table.columns = vec![
        Column::new("health", t("admin/node.table.health")).center(),
        Column::new("reachable", t("admin/node.table.reachable")).center(),
        Column::new("name", t("admin/node.table.name")).sortable(),
        Column::new("fqdn", t("admin/node.table.address")).sortable().wide_only(),
        Column::new("ssl", t("admin/node.table.ssl")).center().wide_only(),
        Column::new("public", t("admin/node.table.public")).center().wide_only(),
        Column::new("servers", t("admin/node.table.servers")).sortable(),
    ];
    for (n, info) in page.iter().zip(infos.iter()) {
        let health = match info.get("exception") {
            Some(e) => Cell::Icon {
                icon: "heart-off".into(),
                color: "danger".into(),
                tooltip: Some(format!("Error connecting to node! {}", e.as_str().unwrap_or_default())),
            },
            None => Cell::Icon {
                icon: "heartbeat".into(),
                color: "success".into(),
                tooltip: Some(info.get("version").and_then(Value::as_str).unwrap_or_default().to_string()),
            },
        };
        let url = if can_edit { format!("/admin/nodes/{}/edit", n.id.0) } else { format!("/admin/nodes/{}", n.id.0) };
        let mut row = Row::new(n.id.0.to_string())
            .url(url.clone())
            .cell("health", health)
            .cell(
                "reachable",
                Cell::Html {
                    html: format!(
                        r#"<span class="node-reach" data-url="{}">…</span>"#,
                        html_escape(&connection_address(n))
                    ),
                },
            )
            .cell("name", Cell::text(n.name.clone()))
            .cell("fqdn", Cell::text(n.fqdn.clone()))
            .cell(
                "ssl",
                Cell::icon(
                    if n.scheme == "https" { "lock" } else { "lock-open-off" },
                    if n.scheme == "https" { "success" } else { "danger" },
                ),
            )
            .cell(
                "public",
                Cell::icon(
                    if n.public.0 { "eye-check" } else { "eye-cancel" },
                    if n.public.0 { "success" } else { "danger" },
                ),
            )
            .cell("servers", Cell::text(servers_of(n.id.0).to_string()));
        row.actions.push(if can_edit {
            Action::link(t("filament-actions::edit.single.label"), "pencil", url)
        } else {
            Action::link(t("filament-actions::view.single.label"), "eye", url)
        });
        table.rows.push(row);
    }
    let mut tags: Vec<String> = nodes::Entity::find().all(&st.db).await?.iter().flat_map(tags_of).collect();
    tags.sort();
    tags.dedup();
    table.searchable = true;
    if !tags.is_empty() {
        table.filters = vec![Filter {
            name: "tag".into(),
            label: t("admin/node.tags"),
            options: tags.into_iter().map(|t| (t.clone(), t)).collect(),
            value: p.filter("tag").unwrap_or("").into(),
        }];
    }
    table.empty_icon = "server-2".into();
    table.empty_heading = t("admin/node.no_nodes");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create node").then(|| {
        context! { label => admin.t_with("filament-actions::create.single.label", &[("label", &t("admin/node.model_label"))]), url => "/admin/nodes/create" }
    });
    let strings = json!({
        "checking": t("admin/node.connectivity.checking"), "offline": t("admin/node.connectivity.offline"),
        "online": t("admin/node.connectivity.online"),
    });
    admin
        .page(
            SLUG,
            "admin/nodes/list.html",
            context! { title => t("admin/node.model_label_plural"), table => Mj::from_serialize(&table), create => create, strings => strings.to_string() },
        )
        .await
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Values of the basic + advanced settings forms.
#[derive(Default)]
struct NodeForm {
    name: String,
    fqdn: String,
    connection: String,
    daemon_connect: String,
    daemon_listen: String,
    public: bool,
    maintenance_mode: bool,
    tags: String,
    upload_size: String,
    daemon_base: String,
    daemon_sftp: String,
    daemon_sftp_alias: String,
    memory: String,
    memory_overallocate: String,
    disk: String,
    disk_overallocate: String,
    cpu: String,
    cpu_overallocate: String,
    description: String,
}

impl NodeForm {
    fn defaults(secure: bool) -> NodeForm {
        NodeForm {
            connection: if secure { "https".into() } else { "http".into() },
            daemon_connect: "8080".into(),
            daemon_listen: "8080".into(),
            public: true,
            upload_size: "256".into(),
            daemon_base: DEFAULT_DAEMON_BASE.into(),
            daemon_sftp: "2022".into(),
            memory: "0".into(),
            memory_overallocate: "0".into(),
            disk: "0".into(),
            disk_overallocate: "0".into(),
            cpu: "0".into(),
            cpu_overallocate: "0".into(),
            ..Default::default()
        }
    }

    fn from_model(n: &nodes::Model) -> NodeForm {
        NodeForm {
            name: n.name.clone(),
            fqdn: n.fqdn.clone(),
            connection: if n.scheme == "http" {
                "http".into()
            } else if n.behind_proxy.0 {
                "https_proxy".into()
            } else {
                "https".into()
            },
            daemon_connect: n.daemon_connect.0.to_string(),
            daemon_listen: n.daemon_listen.0.to_string(),
            public: n.public.0,
            maintenance_mode: n.maintenance_mode.0,
            tags: tags_of(n).join(", "),
            upload_size: n.upload_size.0.to_string(),
            daemon_base: n.daemon_base.clone(),
            daemon_sftp: n.daemon_sftp.0.to_string(),
            daemon_sftp_alias: n.daemon_sftp_alias.clone().unwrap_or_default(),
            memory: n.memory.0.to_string(),
            memory_overallocate: n.memory_overallocate.0.to_string(),
            disk: n.disk.0.to_string(),
            disk_overallocate: n.disk_overallocate.0.to_string(),
            cpu: n.cpu.0.to_string(),
            cpu_overallocate: n.cpu_overallocate.0.to_string(),
            description: n.description.clone().unwrap_or_default(),
        }
    }

    fn from_form(f: &Form) -> NodeForm {
        let lim = |u: &str, k: &str| if f.get(u) == "1" { "0".to_string() } else { f.get(k).trim().to_string() };
        NodeForm {
            name: f.get("name").trim().into(),
            fqdn: f.get("fqdn").trim().into(),
            connection: f.get("connection").into(),
            daemon_connect: f.get("daemon_connect").trim().into(),
            daemon_listen: f.get("daemon_listen").trim().into(),
            public: f.get("public") == "1",
            maintenance_mode: f.get("maintenance_mode") == "1",
            tags: f.get("tags").into(),
            upload_size: f.get("upload_size").trim().into(),
            daemon_base: f.get("daemon_base").trim().into(),
            daemon_sftp: f.get("daemon_sftp").trim().into(),
            daemon_sftp_alias: f.get("daemon_sftp_alias").trim().into(),
            memory: lim("unlimited_memory", "memory"),
            memory_overallocate: if f.get("unlimited_memory") == "1" {
                "0".into()
            } else {
                f.get("memory_overallocate").trim().into()
            },
            disk: lim("unlimited_disk", "disk"),
            disk_overallocate: if f.get("unlimited_disk") == "1" {
                "0".into()
            } else {
                f.get("disk_overallocate").trim().into()
            },
            cpu: lim("unlimited_cpu", "cpu"),
            cpu_overallocate: if f.get("unlimited_cpu") == "1" {
                "0".into()
            } else {
                f.get("cpu_overallocate").trim().into()
            },
            description: f.get("description").into(),
        }
    }

    fn ctx(&self) -> Mj {
        context! {
            name => self.name, fqdn => self.fqdn, connection => self.connection, daemon_connect => self.daemon_connect,
            daemon_listen => self.daemon_listen, public => if self.public { "1" } else { "0" },
            maintenance_mode => if self.maintenance_mode { "1" } else { "0" }, tags => self.tags,
            upload_size => self.upload_size, daemon_base => self.daemon_base, daemon_sftp => self.daemon_sftp,
            daemon_sftp_alias => self.daemon_sftp_alias, memory => self.memory, memory_overallocate => self.memory_overallocate,
            disk => self.disk, disk_overallocate => self.disk_overallocate, cpu => self.cpu, cpu_overallocate => self.cpu_overallocate,
            description => self.description,
        }
    }

    fn tag_list(&self) -> Vec<String> {
        self.tags.split(',').map(str::trim).filter(|t| !t.is_empty()).map(String::from).collect()
    }

    /// Field rules + `Node::$validationRules`.
    fn validate(&self, admin: &Admin) -> Errors {
        let data = json!({
            "name": self.name, "fqdn": self.fqdn, "daemon_connect": self.daemon_connect, "daemon_listen": self.daemon_listen,
            "upload_size": self.upload_size, "daemon_base": self.daemon_base, "daemon_sftp": self.daemon_sftp,
            "daemon_sftp_alias": if self.daemon_sftp_alias.is_empty() { Value::Null } else { Value::String(self.daemon_sftp_alias.clone()) },
            "memory": self.memory, "memory_overallocate": self.memory_overallocate, "disk": self.disk,
            "disk_overallocate": self.disk_overallocate, "cpu": self.cpu, "cpu_overallocate": self.cpu_overallocate,
            "connection": self.connection,
        });
        let mut v = Validator::for_value(&data);
        for (k, label) in [
            ("name", "admin/node.display_name"),
            ("fqdn", "admin/node.domain"),
            ("daemon_connect", "admin/node.port"),
            ("daemon_listen", "admin/node.listen_port"),
            ("upload_size", "admin/node.upload_limit"),
            ("daemon_base", "admin/node.daemon_base"),
            ("daemon_sftp", "admin/node.sftp_port"),
            ("memory", "admin/node.memory_limit"),
            ("disk", "admin/node.disk_limit"),
            ("cpu", "admin/node.cpu_limit"),
        ] {
            v.attribute_name(k, &admin.t(label).to_lowercase());
        }
        v.rules("name", &["required", "string", "min:1", "max:100"]);
        v.rules("fqdn", &["required", "string", "max:255"]);
        v.rules("connection", &["required", "in:http,https,https_proxy"]);
        v.rules("daemon_connect", &["required", "integer", "between:1,65535"]);
        v.rules("daemon_listen", &["required", "integer", "between:1,65535"]);
        v.rules("upload_size", &["required", "integer", "min:1"]);
        v.rules("daemon_base", &["required", "regex:/^([\\/][\\d\\w.\\-\\/]+)$/"]);
        v.rules("daemon_sftp", &["required", "integer", "between:1,65535"]);
        v.rules("daemon_sftp_alias", &["nullable", "string"]);
        v.rules("memory", &["required", "numeric", "min:0"]);
        v.rules("memory_overallocate", &["required", "numeric", "min:-1", "max:100"]);
        v.rules("disk", &["required", "numeric", "min:0"]);
        v.rules("disk_overallocate", &["required", "numeric", "min:-1", "max:100"]);
        v.rules("cpu", &["required", "numeric", "min:0"]);
        v.rules("cpu_overallocate", &["required", "numeric", "min:-1", "max:100"]);
        v.run();
        Errors::from_validator(&v)
    }
}

fn int(s: &str) -> i64 {
    s.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0)
}

/// `Node::getConfiguration()`.
async fn configuration(admin: &Admin, n: &nodes::Model, redacted: bool) -> Result<Value, WebError> {
    let st = &admin.st;
    let pivots = mountables::Entity::find()
        .filter(mountables::Column::MountableType.eq("node"))
        .filter(mountables::Column::MountableId.eq(n.id.0))
        .all(&st.db)
        .await?;
    let ids: Vec<i64> = pivots.iter().map(|p| p.mount_id.0).collect();
    let ms = if ids.is_empty() {
        Vec::new()
    } else {
        mounts::Entity::find().filter(mounts::Column::Id.is_in(ids)).all(&st.db).await?
    };
    let token = if redacted {
        "**********".to_string()
    } else {
        st.enc.decrypt_string_lenient(&n.daemon_token).unwrap_or_default()
    };
    let fqdn = n.fqdn.to_lowercase();
    Ok(json!({
        "debug": false,
        "uuid": n.uuid,
        "token_id": if redacted { json!("**********") } else { json!(n.daemon_token_id) },
        "token": token,
        "api": {
            "host": "0.0.0.0",
            "port": n.daemon_listen.0,
            "ssl": {
                "enabled": !n.behind_proxy.0 && n.scheme == "https",
                "cert": format!("/etc/letsencrypt/live/{fqdn}/fullchain.pem"),
                "key": format!("/etc/letsencrypt/live/{fqdn}/privkey.pem"),
            },
            "upload_limit": n.upload_size.0,
        },
        "system": { "data": n.daemon_base, "sftp": { "bind_port": n.daemon_sftp.0 } },
        "allowed_mounts": ms.iter().map(|m| json!(m.source)).collect::<Vec<_>>(),
        "remote": st.config.app_url,
    }))
}

async fn render(
    admin: &Admin,
    node: Option<&nodes::Model>,
    form: &NodeForm,
    errors: &Errors,
    readonly: bool,
) -> WebResult {
    let st = &admin.st;
    let t = |k: &str| admin.t(k);
    let secure = st.config.app_url.starts_with("https://");
    let tab = admin
        .query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "tab").map(|(_, v)| v.into_owned()))
        .filter(|t| TABS.contains(&t.as_str()))
        .unwrap_or_else(|| if node.is_some() { "overview".into() } else { "basic_settings".into() });
    let mut extra = context! {};
    if let Some(n) = node {
        let info = server_info::node_system_information(st, n).await;
        let config = configuration(admin, n, readonly).await?;
        let yaml = egg_format::yaml::dump_empty_as_sequence(&config, 4);
        let allocs = allocations::Entity::find()
            .filter(allocations::Column::NodeId.eq(n.id.0))
            .order_by_asc(allocations::Column::Ip)
            .order_by_asc(allocations::Column::Port)
            .all(&st.db)
            .await?;
        let node_servers = servers::Entity::find()
            .filter(servers::Column::NodeId.eq(n.id.0))
            .order_by_asc(servers::Column::Id)
            .all(&st.db)
            .await?;
        let server_name = |id: Option<Int>| {
            id.and_then(|i| node_servers.iter().find(|s| s.id == i)).map(|s| (s.id.0, s.name.clone()))
        };
        extra = context! {
            node => context! {
                id => n.id.0, uuid => n.uuid.clone(), name => n.name.clone(),
                has_servers => !node_servers.is_empty(),
            },
            info => context! {
                version => info.get("version").and_then(Value::as_str).map(String::from).unwrap_or_else(|| t("admin/node.unknown")),
                cpu_count => info.get("cpu_count").and_then(Value::as_i64).unwrap_or(0),
                architecture => info.get("architecture").and_then(Value::as_str).map(String::from).unwrap_or_else(|| t("admin/node.unknown")),
                kernel => info.get("kernel_version").and_then(Value::as_str).map(String::from).unwrap_or_else(|| t("admin/node.unknown")),
            },
            config_yaml => yaml,
            allocations => allocs.iter().map(|a| context! {
                id => a.id.0, ip => a.ip.clone(), port => a.port.0, alias => a.ip_alias.clone().unwrap_or_default(),
                notes => a.notes.clone().unwrap_or_default(), server => server_name(a.server_id).map(|(id, name)| context! { id => id, name => name }),
            }).collect::<Vec<_>>(),
            servers => node_servers.iter().map(|s| context! { id => s.id.0, name => s.name.clone(), uuid_short => s.uuid_short.clone() }).collect::<Vec<_>>(),
            node_ips => server_info::node_ips(st, n).await.into_iter().map(|i| vec![i.clone(), i]).collect::<Vec<_>>(),
            diagnostics => admin.session.data().extra.get(&format!("node_diag_{}", n.id.0)).and_then(Value::as_str).map(String::from),
        };
    }
    let title = match node {
        Some(n) if readonly => {
            admin.t_with("filament-panels::resources/pages/view-record.title", &[("label", &n.name)])
        }
        Some(n) => admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &n.name)]),
        None => admin
            .t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("admin/node.model_label"))]),
    };
    admin
        .page(
            SLUG,
            "admin/nodes/form.html",
            context! {
                ..extra,
                ..context! {
                    title => title,
                    tab => tab,
                    creating => node.is_none(),
                    readonly => readonly,
                    can_delete => node.is_some() && !readonly && admin.can("delete node"),
                    secure => secure,
                    v => form.ctx(),
                    errors => Mj::from_serialize(errors),
                    binary => st.config.use_binary_prefix,
                }
            },
        )
        .await
}

async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create node")?;
    let secure = admin.st.config.app_url.starts_with("https://");
    render(&admin, None, &NodeForm::defaults(secure), &Errors::default(), false).await
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view node")?;
    let n = find(&admin, id).await?;
    render(&admin, Some(&n), &NodeForm::from_model(&n), &Errors::default(), true).await
}

async fn edit(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update node")?;
    let n = find(&admin, id).await?;
    render(&admin, Some(&n), &NodeForm::from_model(&n), &Errors::default(), false).await
}

fn scheme_of(connection: &str) -> (&'static str, bool) {
    match connection {
        "http" => ("http", false),
        "https_proxy" => ("https", true),
        _ => ("https", false),
    }
}

/// `POST /admin/nodes/create` (CreateNode → `Node::create`).
async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create node")?;
    let data = NodeForm::from_form(&form);
    let errors = data.validate(&admin);
    if !errors.is_empty() {
        return render(&admin, None, &data, &errors, false).await;
    }
    let st = &admin.st;
    let (scheme, proxy) = scheme_of(&data.connection);
    let now = Timestamp(pelican_core::time::now());
    let listen = if proxy { int(&data.daemon_listen) } else { int(&data.daemon_connect) };
    let am = nodes::ActiveModel {
        uuid: Set(Some(uuid::Uuid::new_v4().to_string())),
        public: Set(SmallBool(data.public)),
        name: Set(data.name.clone()),
        description: Set(None),
        fqdn: Set(data.fqdn.clone()),
        scheme: Set(scheme.into()),
        behind_proxy: Set(Bool(proxy)),
        maintenance_mode: Set(Bool(data.maintenance_mode)),
        memory: Set(Int(int(&data.memory))),
        memory_overallocate: Set(Int(int(&data.memory_overallocate))),
        disk: Set(Int(int(&data.disk))),
        disk_overallocate: Set(Int(int(&data.disk_overallocate))),
        cpu: Set(Int(int(&data.cpu))),
        cpu_overallocate: Set(Int(int(&data.cpu_overallocate))),
        upload_size: Set(Int(int(&data.upload_size))),
        daemon_token_id: Set(Some(pelican_wings::jwt::str_random(16))),
        daemon_token: Set(st.enc.encrypt_string(&pelican_wings::jwt::str_random(64))),
        daemon_listen: Set(Int(listen)),
        daemon_sftp: Set(Int(int(&data.daemon_sftp))),
        daemon_sftp_alias: Set((!data.daemon_sftp_alias.is_empty()).then(|| data.daemon_sftp_alias.clone())),
        daemon_base: Set(data.daemon_base.clone()),
        daemon_connect: Set(Int(int(&data.daemon_connect))),
        tags: Set(Some(Json(json!(data.tag_list())))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&st.db, &am).await?.ok_or_else(|| WebError::internal("no id"))?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("/admin/nodes/{id}/edit?tab=config_file")))
}

/// POST `/api/update` with the node's configuration (`DaemonSystemRepository::update`).
async fn push_config(admin: &Admin, conn: &pelican_wings::Node, n: &nodes::Model) -> bool {
    let Ok(body) = configuration(admin, n, false).await else { return false };
    let call = pelican_wings::client::Call::new(pelican_wings::client::Method::POST, "/api/update").json(body);
    admin.st.wings.send(conn, call).await.is_ok()
}

/// `POST /admin/nodes/{id}/edit`: EditNode::save + afterSave.
async fn save(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update node")?;
    let n = find(&admin, id).await?;
    let data = NodeForm::from_form(&form);
    let errors = data.validate(&admin);
    if !errors.is_empty() {
        return render(&admin, Some(&n), &data, &errors, false).await;
    }
    let st = &admin.st;
    let (scheme, proxy) = scheme_of(&data.connection);
    // mutateFormDataBeforeSave(): without a proxy Wings listens on the connect port.
    let listen = if proxy { int(&data.daemon_listen) } else { int(&data.daemon_connect) };
    let am = nodes::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(n.id),
        public: Set(SmallBool(data.public)),
        name: Set(data.name.clone()),
        fqdn: Set(data.fqdn.clone()),
        scheme: Set(scheme.into()),
        behind_proxy: Set(Bool(proxy)),
        maintenance_mode: Set(Bool(data.maintenance_mode)),
        memory: Set(Int(int(&data.memory))),
        memory_overallocate: Set(Int(int(&data.memory_overallocate))),
        disk: Set(Int(int(&data.disk))),
        disk_overallocate: Set(Int(int(&data.disk_overallocate))),
        cpu: Set(Int(int(&data.cpu))),
        cpu_overallocate: Set(Int(int(&data.cpu_overallocate))),
        upload_size: Set(Int(int(&data.upload_size))),
        daemon_listen: Set(Int(listen)),
        daemon_sftp: Set(Int(int(&data.daemon_sftp))),
        daemon_sftp_alias: Set((!data.daemon_sftp_alias.is_empty()).then(|| data.daemon_sftp_alias.clone())),
        daemon_base: Set(data.daemon_base.clone()),
        daemon_connect: Set(Int(int(&data.daemon_connect))),
        tags: Set(Some(Json(json!(data.tag_list())))),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    pelican_db::write::update(&st.db, &am).await?;
    let fresh = nodes::Entity::find_by_id(id).one(&st.db).await?.ok_or(WebError::NotFound)?;
    // afterSave(): only Wings-relevant changes are pushed.
    let changed = fresh.fqdn != n.fqdn
        || fresh.scheme != n.scheme
        || fresh.behind_proxy != n.behind_proxy
        || fresh.upload_size != n.upload_size
        || fresh.daemon_listen != n.daemon_listen
        || fresh.daemon_sftp != n.daemon_sftp
        || fresh.daemon_sftp_alias != n.daemon_sftp_alias
        || fresh.daemon_base != n.daemon_base
        || fresh.daemon_connect != n.daemon_connect;
    let ok = if changed {
        match st.node_conn(&fresh) {
            Ok(conn) => push_config(&admin, &conn, &fresh).await,
            Err(_) => false,
        }
    } else {
        true
    };
    if ok {
        admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    } else {
        admin.session.flash(
            "warning",
            admin.t_with("admin/node.error_connecting", &[("node", &fresh.name)]),
            Some(admin.t("admin/node.error_connecting_description")),
        );
    }
    let tab = form.opt("_tab").filter(|t| TABS.contains(t)).unwrap_or("basic_settings");
    Ok(admin.redirect(&format!("/admin/nodes/{id}/edit?tab={tab}")))
}

/// `GET /admin/nodes/{id}/stats`: `Node::statistics()` for the overview charts.
async fn stats(admin: Admin, Path(id): Path<i64>) -> Result<Response, WebError> {
    admin.authorize("view node")?;
    let n = find(&admin, id).await?;
    Ok(AxJson(server_info::node_statistics(&admin.st, &n).await).into_response())
}

/// `GET /admin/nodes/{id}/deploy-command?docker=0|1` (NodeAutoDeployService).
async fn deploy_command(admin: Admin, Path(id): Path<i64>) -> Result<Response, WebError> {
    admin.authorize("update node")?;
    let n = find(&admin, id).await?;
    let st = &admin.st;
    let docker = admin.query.as_deref().unwrap_or("").contains("docker=1");
    // An application key whose permissions are exactly {node: READ}, else create one.
    let keys = api_keys::Entity::find()
        .filter(api_keys::Column::KeyType.eq(2))
        .order_by_asc(api_keys::Column::Id)
        .all(&st.db)
        .await?;
    let found = keys
        .into_iter()
        .find(|k| k.permissions.as_ref().and_then(|p| p.0.get("node")).and_then(Value::as_i64) == Some(1));
    let (identifier, secret) = match found {
        Some(k) => (k.identifier.clone().unwrap_or_default(), st.enc.decrypt_string(&k.token).unwrap_or_default()),
        None => {
            let identifier = format!("papp_{}", pelican_wings::jwt::str_random(11));
            let secret = pelican_wings::jwt::str_random(32);
            let now = Timestamp(pelican_core::time::now());
            let am = api_keys::ActiveModel {
                user_id: Set(Some(admin.user.user.id)),
                key_type: Set(Int(2)),
                identifier: Set(Some(identifier.clone())),
                token: Set(st.enc.encrypt_string(&secret)),
                memo: Set(Some("Automatically generated node deployment key.".into())),
                allowed_ips: Set(Json(json!([]))),
                permissions: Set(Some(Json(json!({ "node": 1 })))),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            pelican_db::write::insert_id(&st.db, &am).await?;
            (identifier, secret)
        }
    };
    let secure = st.config.app_url.starts_with("https://");
    let cmd = format!(
        "{} wings configure --panel-url {} --token {}{} --node {}{}",
        if docker {
            r#"docker compose exec -it $(docker ps --filter "name=wings" --format "{{.Names}}")"#
        } else {
            "sudo"
        },
        st.config.app_url,
        identifier,
        secret,
        n.id.0,
        if secure { "" } else { " --allow-insecure" }
    );
    Ok(AxJson(json!({ "command": cmd })).into_response())
}

/// Header and config/diagnostics actions.
async fn action(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let n = find(&admin, id).await?;
    let st = &admin.st;
    match form.get("op") {
        "delete" => {
            admin.authorize("delete node")?;
            let used = servers::Entity::find().filter(servers::Column::NodeId.eq(id)).one(&st.db).await?.is_some();
            if used {
                admin.flash_danger(admin.t("admin/node.node_has_servers"), None);
                return Ok(admin.redirect(&format!("/admin/nodes/{id}/edit")));
            }
            nodes::Entity::delete_by_id(n.id).exec(&st.db).await?;
            admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
            return Ok(admin.redirect("/admin/nodes"));
        }
        "reset-token" => {
            admin.authorize("update node")?;
            // NodeUpdateService(reset): new token, configuration pushed with the old credentials.
            let old_conn = st.node_conn(&n).map_err(WebError::internal)?;
            let token = pelican_wings::jwt::str_random(64);
            let token_id = pelican_wings::jwt::str_random(16);
            let am = nodes::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(n.id),
                daemon_token: Set(st.enc.encrypt_string(&token)),
                daemon_token_id: Set(Some(token_id)),
                updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
                ..Default::default()
            };
            let txn = sea_orm::TransactionTrait::begin(&st.db).await?;
            pelican_db::write::update(&txn, &am).await?;
            let fresh = nodes::Entity::find_by_id(id).one(&txn).await?.ok_or(WebError::NotFound)?;
            let Ok(body) = configuration(&admin, &fresh, false).await else { return Err(WebError::internal("config")) };
            let call = pelican_wings::client::Call::new(pelican_wings::client::Method::POST, "/api/update").json(body);
            let warn = || {
                admin.session.flash(
                    "warning",
                    admin.t_with("admin/node.error_connecting", &[("node", &n.name)]),
                    Some(admin.t("admin/node.error_connecting_description")),
                )
            };
            // NodeUpdateService: stored even when Wings is unreachable; other failures roll back.
            match st.wings.send(&old_conn, call).await {
                Ok(_) => {
                    txn.commit().await?;
                    admin.flash_success(admin.t("admin/node.token_reset"));
                }
                Err(pelican_wings::WingsError::Connection(_)) => {
                    txn.commit().await?;
                    warn();
                }
                Err(_) => {
                    txn.rollback().await?;
                    warn();
                }
            }
            return Ok(admin.redirect(&format!("/admin/nodes/{id}/edit?tab=config_file")));
        }
        "diagnostics-pull" => {
            admin.authorize("update node")?;
            let lines = form.get("log_lines").parse::<i64>().unwrap_or(200).clamp(100, 500);
            let q = [
                ("log_lines", lines.to_string()),
                ("include_endpoints", if form.bool("include_endpoints") { "true".into() } else { "false".into() }),
                ("include_logs", if form.bool("include_logs") { "true".into() } else { "false".into() }),
            ];
            let mut call =
                pelican_wings::client::Call::new(pelican_wings::client::Method::GET, "/api/diagnostics").query(&q);
            call.timeout = Some(Duration::from_secs(5));
            call.throw = false;
            match st.node_conn(&n) {
                Ok(conn) => match st.wings.send(&conn, call).await {
                    Ok(r) if r.status == 404 => {
                        admin.session.flash("warning", admin.t("admin/node.diagnostics.404"), None)
                    }
                    Ok(r) => {
                        let body = String::from_utf8_lossy(&r.body).into_owned();
                        admin.session.update(|d| {
                            d.extra.insert(format!("node_diag_{id}"), json!(body));
                        });
                        admin.flash_success(admin.t("admin/node.diagnostics.logs_pulled"));
                    }
                    Err(e) => admin.flash_danger(
                        admin.t_with("admin/node.error_connecting", &[("node", &n.name)]),
                        Some(e.to_string()),
                    ),
                },
                Err(e) => admin.flash_danger(
                    admin.t_with("admin/node.error_connecting", &[("node", &n.name)]),
                    Some(e.to_string()),
                ),
            }
            return Ok(admin.redirect(&format!("/admin/nodes/{id}/edit?tab=diagnostics")));
        }
        "diagnostics-clear" => {
            admin.session.update(|d| {
                d.extra.remove(&format!("node_diag_{id}"));
            });
            return Ok(admin.redirect(&format!("/admin/nodes/{id}/edit?tab=diagnostics")));
        }
        "refresh-ips" => {
            server_info::forget_node_ips(id);
            return Ok(admin.redirect(&format!("/admin/nodes/{id}/edit#relations")));
        }
        _ => Err(WebError::Unprocessable),
    }
}

/// AllocationsRelationManager › CreateAction (`AssignmentService`).
async fn allocation_create(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update node")?;
    let n = find(&admin, id).await?;
    let ip = form.get("allocation_ip").trim().to_string();
    let ports: Vec<Value> = form
        .get("allocation_ports")
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| Value::String(p.to_string()))
        .collect();
    let alias = form.opt("allocation_alias").map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    if !ip.is_empty() && !ports.is_empty() {
        match pelican_api::application::nodes::assign_allocations(&admin.st, n.id, &ip, &ports, alias, None, false)
            .await
        {
            Ok(_) => admin.flash_success(admin.t("filament-actions::create.single.notifications.created.title")),
            Err(e) => admin.flash_danger(
                admin.t("admin/server.notifications.create_failed"),
                e.errors.first().map(|o| o.detail.clone()),
            ),
        }
    }
    Ok(admin.redirect(&format!("/admin/nodes/{id}/edit#relations")))
}

/// Inline columns (ip select, alias, notes) and delete of unassigned allocations.
async fn allocation_op(admin: Admin, Path((id, aid, op)): Path<(i64, i64, String)>, form: Form) -> WebResult {
    admin.authorize("update node")?;
    let n = find(&admin, id).await?;
    let st = &admin.st;
    let a = allocations::Entity::find_by_id(aid).one(&st.db).await?.ok_or(WebError::NotFound)?;
    if a.node_id != n.id {
        return Err(WebError::NotFound);
    }
    let base = allocations::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(a.id),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    match op.as_str() {
        "ip" => {
            let ip = form.get("ip").trim().to_string();
            if ip.parse::<std::net::IpAddr>().is_ok() {
                pelican_db::write::update(&st.db, &allocations::ActiveModel { ip: Set(ip), ..base }).await?;
            }
        }
        "alias" => {
            let v = form.get("ip_alias").trim().to_string();
            pelican_db::write::update(
                &st.db,
                &allocations::ActiveModel { ip_alias: Set((!v.is_empty()).then_some(v)), ..base },
            )
            .await?;
        }
        "notes" => {
            let v = form.get("notes").trim().to_string();
            pelican_db::write::update(
                &st.db,
                &allocations::ActiveModel { notes: Set((!v.is_empty()).then_some(v)), ..base },
            )
            .await?;
        }
        "delete" => {
            if a.server_id.is_some() {
                admin.flash_danger(admin.t("exceptions.allocations.server_using"), None);
            } else {
                allocations::Entity::delete_by_id(a.id).exec(&st.db).await?;
                admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
            }
        }
        _ => return Err(WebError::Unprocessable),
    }
    Ok(admin.redirect(&format!("/admin/nodes/{id}/edit#relations")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn yaml_config_shape() {
        let v =
            serde_json::json!({"debug": false, "allowed_mounts": [], "api": {"ssl": {"enabled": true}, "port": 8080}});
        let y = egg_format::yaml::dump_empty_as_sequence(&v, 4);
        assert!(y.contains("debug: false"));
        assert!(y.contains("allowed_mounts: []"));
        assert!(y.contains("api:\n  ssl:\n    enabled: true\n  port: 8080"));
    }
}
