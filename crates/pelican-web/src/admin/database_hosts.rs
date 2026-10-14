//! Admin › Database Hosts (`app/Filament/Admin/Resources/DatabaseHosts/…`,
//! `app/Services/Databases/Hosts/*`, DatabasesRelationManager): list, create
//! wizard (`HostCreationService`), view/edit (`HostUpdateService`), delete and
//! the Databases relation manager as a tab on the record pages.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::application::database_hosts::sync_nodes;
use pelican_api::services::LogActivity;
use pelican_api::validation::{CustomRule, Validator, expand_wildcard};
use pelican_core::activity::morph;
use pelican_core::databases::{HostConnection, HostError};
use pelican_core::state::AppState;
use pelican_db::entities::{database_host_node, database_hosts, databases, nodes, servers};
use pelican_db::{Int, Timestamp};
use sea_orm::{
    ActiveValue::{Set, Unchanged},
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const BASE: &str = "/admin/database-hosts";
const SLUG: &str = "database-hosts";
const DBS_PER_PAGE: u64 = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/database-hosts", get(index))
        .route("/admin/database-hosts/create", get(create_form).post(create))
        .route("/admin/database-hosts/create/commands", post(commands))
        .route("/admin/database-hosts/{id}", get(view))
        .route("/admin/database-hosts/{id}/edit", get(edit_form).post(update))
        .route("/admin/database-hosts/{id}/delete", post(delete_host))
        .route("/admin/database-hosts/{id}/databases/{db}/rotate", post(rotate_database))
        .route("/admin/database-hosts/{id}/databases/{db}/delete", post(delete_database))
}

/// `DatabaseHostResource::getEloquentQuery()`: hosts linked to an accessible
/// node or with no linked node.
fn visible(admin: &Admin, node_ids: &[i64]) -> bool {
    node_ids.is_empty() || node_ids.iter().any(|n| admin.user.can_target_node(*n))
}

/// `DatabaseHostPolicy::before()`: a record is denied when any linked node is
/// not targetable.
fn record_allowed(admin: &Admin, node_ids: &[i64]) -> bool {
    node_ids.iter().all(|n| admin.user.can_target_node(*n))
}

async fn database_counts(st: &pelican_core::state::AppState, ids: &[i64]) -> Result<HashMap<i64, i64>, WebError> {
    let mut out: HashMap<i64, i64> = HashMap::new();
    if !ids.is_empty() {
        for d in
            databases::Entity::find().filter(databases::Column::DatabaseHostId.is_in(ids.to_vec())).all(&st.db).await?
        {
            *out.entry(d.database_host_id.0).or_insert(0) += 1;
        }
    }
    Ok(out)
}

/// Linked node ids of every host (`$host->nodes()->pluck('id')`).
async fn host_nodes(st: &pelican_core::state::AppState, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>, WebError> {
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    if !ids.is_empty() {
        for r in database_host_node::Entity::find()
            .filter(database_host_node::Column::DatabaseHostId.is_in(ids.to_vec()))
            .all(&st.db)
            .await?
        {
            out.entry(r.database_host_id.0).or_default().push(r.node_id.0);
        }
    }
    Ok(out)
}

/// `GET {BASE}` (ListDatabaseHosts).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList databaseHost")?;
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let opts = [5, 10, 25, 50, 0];
    let mut p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let all = database_hosts::Entity::find().order_by_asc(database_hosts::Column::Id).all(&admin.st.db).await?;
    let ids: Vec<i64> = all.iter().map(|h| h.id.0).collect();
    // `TextColumn::make('databases_count')->counts('databases')`.
    let counts = database_counts(&admin.st, &ids).await?;
    let rel = host_nodes(&admin.st, &ids).await?;
    // Filament searches by `$recordTitleAttribute` (name).
    let needle = p.search.to_lowercase();
    let visible_rows: Vec<&database_hosts::Model> = all
        .iter()
        .filter(|h| visible(&admin, rel.get(&h.id.0).map(|r| r.as_slice()).unwrap_or(&[])))
        .filter(|h| needle.is_empty() || h.name.to_lowercase().contains(&needle))
        .collect();
    let total = visible_rows.len() as u64;
    p.clamp(total);
    let page_rows: Vec<&&database_hosts::Model> = match p.limit_offset() {
        Some((limit, offset)) => visible_rows.iter().skip(offset as usize).take(limit as usize).collect(),
        None => visible_rows.iter().collect(),
    };
    let node_names: HashMap<i64, String> =
        nodes::Entity::find().all(&admin.st.db).await?.into_iter().map(|n| (n.id.0, n.name)).collect();
    let can_edit = admin.can("update databaseHost");
    let can_view = admin.can("view databaseHost");
    let mut table = Table::new("database-hosts", BASE);
    table.columns = vec![
        Column::new("name", t("table.name")),
        Column::new("host", t("table.host")),
        Column::new("port", t("table.port")),
        Column::new("username", t("table.username")),
        Column::new("databases", t("databases")),
        Column::new("nodes", t("nodes")),
    ];
    for h in page_rows.into_iter() {
        let h = *h;
        let node_ids = rel.get(&h.id.0).cloned().unwrap_or_default();
        // Row actions follow the record policy: Edit when allowed, else View.
        let url = if record_allowed(&admin, &node_ids) && can_edit {
            format!("{BASE}/{}/edit", h.id.0)
        } else if record_allowed(&admin, &node_ids) && can_view {
            format!("{BASE}/{}", h.id.0)
        } else {
            String::new()
        };
        let mut row = Row::new(h.id.0.to_string());
        if !url.is_empty() {
            row = row.url(url.clone());
        }
        row = row
            .cell("name", Cell::text(h.name.clone()))
            .cell("host", Cell::text(h.host.clone()))
            .cell("port", Cell::text(h.port.0.to_string()))
            .cell("username", Cell::text(h.username.clone()))
            .cell("databases", Cell::text(counts.get(&h.id.0).copied().unwrap_or(0).to_string()));
        if node_ids.is_empty() {
            row = row.cell("nodes", Cell::text(t("no_nodes")));
        } else {
            row = row.cell(
                "nodes",
                Cell::Badges {
                    items: node_ids.iter().filter_map(|n| node_names.get(n).cloned()).collect(),
                    color: "primary".into(),
                },
            );
        }
        if !url.is_empty() {
            row.actions.push(if can_edit {
                Action::link(admin.t("filament-actions::edit.single.label"), "pencil", url)
            } else {
                Action::link(admin.t("filament-actions::view.single.label"), "eye", url)
            });
        }
        table.rows.push(row);
    }
    table.searchable = true;
    table.empty_icon = "database".into();
    table.empty_heading = t("no_database_hosts");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create databaseHost").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => format!("{BASE}/create"),
        }
    });
    admin
        .page(
            SLUG,
            "admin/list.html",
            context! { title => t("model_label_plural"), table => Mj::from_serialize(&table), create => create },
        )
        .await
}

// ---------------------------------------------------------------------------
// Form data + validation
// ---------------------------------------------------------------------------

struct FormData {
    host: String,
    port: String,
    max_databases: String,
    name: String,
    username: String,
    /// Plain text password; `None` keeps the stored one on edit.
    password: Option<String>,
    node_ids: Vec<String>,
    different_server: bool,
}

impl FormData {
    fn from_model(h: &database_hosts::Model, node_ids: &[i64]) -> FormData {
        FormData {
            host: h.host.clone(),
            port: h.port.0.to_string(),
            max_databases: h.max_databases.map(|m| m.0.to_string()).unwrap_or_default(),
            name: h.name.clone(),
            username: h.username.clone(),
            password: None,
            node_ids: node_ids.iter().map(i64::to_string).collect(),
            different_server: false,
        }
    }

    fn from_form(f: &Form) -> FormData {
        FormData {
            host: f.get("host").trim().to_string(),
            port: f.get("port").trim().to_string(),
            max_databases: f.opt("max_databases").map(str::trim).unwrap_or("").to_string(),
            name: f.get("name").trim().to_string(),
            username: f.get("username").trim().to_string(),
            password: f.opt("password").map(str::trim).filter(|p| !p.is_empty()).map(str::to_string),
            node_ids: f.list("node_ids"),
            different_server: f.bool("different_server"),
        }
    }

    fn ctx(&self) -> Mj {
        context! {
            host => self.host, port => self.port, max_databases => self.max_databases,
            name => self.name, username => self.username,
            password => self.password.clone().unwrap_or_default(),
            node_ids => self.node_ids,
            different_server => self.different_server,
        }
    }
}

fn opt_int(s: &str) -> Option<Int> {
    s.trim().parse::<i64>().ok().map(Int)
}

/// PHP `(int)` cast of a validated numeric field.
fn php_int(s: &str) -> i64 {
    match s.trim().parse::<i64>() {
        Ok(v) => v,
        Err(_) => pelican_core::php::intval(s.trim()),
    }
}

/// Field rules of the Filament forms plus `DatabaseHost::$validationRules`
/// (per field the effective conjunction; the stricter limit wins).
async fn validate(admin: &Admin, data: &FormData, creating: bool) -> Result<Errors, WebError> {
    let input = json!({
        "name": data.name, "host": data.host, "port": data.port, "username": data.username,
        "password": data.password.clone().map(Value::from),
        "max_databases":
            if data.max_databases.is_empty() { Value::Null } else { Value::from(data.max_databases.clone()) },
        "node_ids": data.node_ids,
    });
    let mut v = Validator::for_value(&input);
    let lbl = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    for f in ["host", "port", "username", "password"] {
        v.attribute_name(f, &lbl(f));
    }
    v.attribute_name("name", &lbl("display_name"));
    v.attribute_name("max_databases", &lbl("max_database"));
    let nodes_lbl = lbl("linked_nodes");
    v.attribute_name("node_ids", &nodes_lbl);
    v.rules("name", &["required", "string", "max:60"]);
    v.rules("host", &["required", "string", "max:255"]);
    v.rules("port", &["required", "numeric", "min:0", "max:65535", "between:1,65535"]);
    v.rules("max_databases", &["nullable", "numeric"]);
    v.rules("username", &["required", "string", "max:32"]);
    if creating {
        v.rules("password", &["required", "string", "max:255"]);
    } else {
        v.rules("password", &["nullable", "string", "max:255"]);
    }
    v.rules("node_ids", &["nullable", "array"]);
    // exists:nodes,id per element ($validationRules['node_ids.*']).
    for path in expand_wildcard(&input, "node_ids.*") {
        v.attribute_name(&path, &nodes_lbl);
        let element = path
            .rsplit('.')
            .next()
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| data.node_ids.get(i))
            .cloned()
            .unwrap_or_default();
        let exists = match element.parse::<i64>() {
            Ok(id) => nodes::Entity::find_by_id(Int(id)).one(&admin.st.db).await?.is_some(),
            Err(_) => false,
        };
        v.custom(
            &path,
            CustomRule::precomputed("exists", (!exists).then(|| "The selected :attribute is invalid.".into())),
        );
    }
    v.run();
    Ok(Errors::from_validator(&v))
}

/// Deduplicated accessible subset of the submitted node ids (the select only
/// lists accessible nodes).
fn requested_node_ids(admin: &Admin, form: &Form) -> Vec<i64> {
    let mut out: Vec<i64> = Vec::new();
    for s in form.list("node_ids") {
        if let Ok(id) = s.trim().parse::<i64>() {
            if !out.contains(&id) && admin.user.can_target_node(id) {
                out.push(id);
            }
        }
    }
    out
}

/// A failed connection test rethrows the raw exception message like the pages
/// do (`PDOException` body inside the danger notification).
fn conn_err(e: HostError) -> String {
    match e {
        HostError::Decrypt => "DecryptException".into(),
        HostError::Query(e) => e.to_string(),
    }
}

/// `buildConnection()->getPdo()` against database `mysql`
/// (`DatabaseHost::buildConnection`, step 3 of `HostCreationService`).
async fn test_connection(st: &pelican_core::state::AppState, probe: &database_hosts::Model) -> Result<(), String> {
    match HostConnection::open(probe, &st.enc).await {
        Ok(conn) => {
            conn.close().await;
            Ok(())
        }
        Err(e) => Err(conn_err(e)),
    }
}

/// Fetch the host plus its linked node ids; a record-policy denial becomes
/// `Forbidden` (`DatabaseHostPolicy::before()`).
async fn find_host(admin: &Admin, id: i64) -> Result<(database_hosts::Model, Vec<i64>), WebError> {
    let h = database_hosts::Entity::find_by_id(Int(id)).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    let node_ids = host_nodes(&admin.st, &[id]).await?.remove(&id).unwrap_or_default();
    if !record_allowed(admin, &node_ids) {
        return Err(WebError::Forbidden);
    }
    Ok((h, node_ids))
}

// ---------------------------------------------------------------------------
// Create wizard
// ---------------------------------------------------------------------------

/// The hidden `panel_ip`: `127.0.0.1`, or — when the "different server" toggle
/// is on — `gethostbyname(<APP_URL without scheme/slashes>)` (the hostname
/// itself when it does not resolve, like PHP's `gethostbyname`).
async fn panel_ip_value(st: &pelican_core::state::AppState, different: bool) -> String {
    if !different {
        return "127.0.0.1".into();
    }
    let host = st.config.app_url.replace("http:", "").replace("https:", "").replace('/', "");
    if !host.is_empty() {
        if let Ok(mut addrs) = tokio::net::lookup_host((host.as_str(), 0u16)).await {
            if let Some(a) = addrs.next() {
                return a.ip().to_string();
            }
        }
    }
    host
}

/// Step-2 SQL commands generated after step 1 validates.
fn commands_for(username: &str, password: &str, panel_ip: &str) -> (String, String) {
    (
        format!("CREATE USER '{username}'@'{panel_ip}' IDENTIFIED BY '{password}';"),
        format!("GRANT ALL PRIVILEGES ON *.* TO '{username}'@'{panel_ip}' WITH GRANT OPTION;"),
    )
}

async fn node_options(admin: &Admin) -> Result<Vec<(String, String)>, WebError> {
    Ok(nodes::Entity::find()
        .order_by_asc(nodes::Column::Name)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .filter(|n| admin.user.can_target_node(n.id.0))
        .map(|n| (n.id.0.to_string(), n.name))
        .collect())
}

/// Setup block context: panel ip and the two generated commands.
async fn setup_context(st: &pelican_core::state::AppState, different: bool, username: String, password: String) -> Mj {
    let panel_ip = panel_ip_value(st, different).await;
    let (cu, ap) = commands_for(&username, &password, &panel_ip);
    context! { panel_ip => panel_ip, create_user => cu, assign_permissions => ap }
}

/// `GET {BASE}/create`.
async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create databaseHost")?;
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    // Wizard defaults: fixed `pelicanuser` + random `Str::password(16)`
    // (`Utilities::randomStringWithSpecialCharacters` here).
    let data = FormData {
        host: String::new(),
        port: "3306".into(),
        max_databases: String::new(),
        name: String::new(),
        username: "pelicanuser".into(),
        password: Some(pelican_core::databases::random_password(16)),
        node_ids: vec![],
        different_server: false,
    };
    render_create_form(&admin, &data, &t, &Errors::default()).await
}

async fn render_create_form(admin: &Admin, data: &FormData, t: impl Fn(&str) -> String, errors: &Errors) -> WebResult {
    let setup = setup_context(
        &admin.st,
        data.different_server,
        data.username.clone(),
        data.password.clone().unwrap_or_default(),
    )
    .await;
    // htmx attributes of the step-1 controls: they refresh the command block.
    let refresh_attrs = format!(
        "hx-post=\"{BASE}/create/commands\" hx-trigger=\"input changed delay:400ms, change\" \
         hx-target=\"#dh-setup-commands\" hx-swap=\"outerHTML\" hx-include=\"closest fieldset\""
    );
    admin
        .page(
            SLUG,
            "admin/database_hosts/create.html",
            context! {
                title => admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
                action => format!("{BASE}/create"),
                back_url => BASE,
                v => data.ctx(),
                errors => Mj::from_serialize(errors),
                node_options => node_options(admin).await?,
                refresh_attrs => refresh_attrs,
                ..setup
            },
        )
        .await
}

/// Livewire fragment equivalent: recompute the SQL commands and the hidden
/// panel ip from the current step-1 values while typing.
async fn commands(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create databaseHost")?;
    let setup = setup_context(
        &admin.st,
        form.bool("different_server"),
        form.get("username").trim().to_string(),
        form.opt("password").map(str::trim).unwrap_or("").to_string(),
    )
    .await;
    admin.render_block("admin/database_hosts/create.html", "setup_commands", setup)
}

/// POST `{BASE}/create` (`CreateRecord` + `HostCreationService::handle`):
/// insert, sync pivots, test the credentials — one transaction that rolls back
/// when the connection test fails.
async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create databaseHost")?;
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, true).await?;
    if !errors.is_empty() {
        return render_create_form(&admin, &data, &t, &errors).await;
    }
    let pw_plain = data.password.clone().unwrap_or_default();
    let now = Timestamp(pelican_core::time::now());
    let b = admin.st.db.get_database_backend();
    let txn = admin.st.db.begin().await?;
    let am = database_hosts::ActiveModel {
        name: Set(data.name.clone()),
        host: Set(data.host.clone()),
        port: Set(Int(php_int(&data.port))),
        username: Set(data.username.clone()),
        password: Set(admin.st.enc.encrypt_string(&pw_plain)),
        max_databases: Set(opt_int(&data.max_databases)),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&txn, &am).await?.ok_or_else(|| WebError::internal("no id"))?;
    sync_nodes(&txn, b, Int(id), &requested_node_ids(&admin, &form)).await.map_err(api_err)?;
    let probe = database_hosts::Entity::find_by_id(Int(id))
        .one(&txn)
        .await?
        .ok_or_else(|| WebError::internal("QueryException"))?;
    if let Err(msg) = test_connection(&admin.st, &probe).await {
        // PDOException handler: danger notification, rollback, stay on page.
        admin.flash_danger(t("error"), Some(msg));
        return render_create_form(&admin, &data, &t, &errors).await;
    }
    txn.commit().await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(BASE))
}

// ---------------------------------------------------------------------------
// View / edit / delete
// ---------------------------------------------------------------------------

fn page_url(admin: &Admin) -> String {
    match admin.query.as_deref() {
        Some(q) if !q.is_empty() => format!("{}?{}", admin.path, q),
        _ => admin.path.clone(),
    }
}

fn query_flag(query: Option<&str>, key: &str) -> Option<String> {
    form_urlencoded::parse(query.unwrap_or("").as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

#[allow(clippy::too_many_arguments)]
async fn render_record_page(
    admin: &Admin,
    h: &database_hosts::Model,
    node_ids: &[i64],
    data: &FormData,
    errors: &Errors,
    readonly: bool,
    active_tab: &str,
    dbs_page: u64,
    back_url: &str,
) -> WebResult {
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    // Header EditAction of ViewDatabaseHost; on the edit page we already
    // authorized the update ability.
    let edit_url = if readonly && admin.can("update databaseHost") && record_allowed(admin, node_ids) {
        Some(format!("{BASE}/{}/edit", h.id.0))
    } else {
        None
    };
    let count = dbs_count(&admin.st, h.id.0).await?;
    // Header DeleteAction (edit page only): disabled while the host has
    // databases.
    let can_delete = !readonly && record_allowed(admin, node_ids) && admin.can("delete databaseHost");
    let delete_disabled = count > 0;
    let delete_tooltip = if delete_disabled {
        t("delete_help")
    } else {
        admin.t("filament-actions::delete.single.modal.actions.delete.label")
    };
    let title = if readonly {
        admin.t_with("filament-panels::resources/pages/view-record.title", &[("label", &h.name)])
    } else {
        admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &h.name)])
    };
    // Relation manager visibility: `viewAny Database` (root admins only in
    // practice; docs/spec/ui.md §3507).
    let show_databases = admin.user.is_root_admin();
    let dbs_total = if show_databases { count.max(0) } else { 0 };
    let dbs_pages: u64 = ((dbs_total as u64 + DBS_PER_PAGE - 1) / DBS_PER_PAGE).max(1);
    let dbs_page = dbs_page.clamp(1, dbs_pages);
    let db_rows = if show_databases && active_tab == "databases" {
        load_databases(admin, h, dbs_page).await?
    } else {
        Vec::new()
    };
    let record_tab_active = active_tab != "databases";
    admin
        .page(
            SLUG,
            "admin/database_hosts/form.html",
            context! {
                title => title,
                action => format!("{BASE}/{}/edit", h.id.0),
                readonly => readonly,
                active_record => record_tab_active,
                record_tab_url => format!("{back_url}?tab=record"),
                databases_tab_url => format!("{back_url}?tab=databases"),
                show_databases => show_databases,
                db_rows => db_rows,
                dbs_total => dbs_total,
                dbs_page => dbs_page,
                dbs_prev => (dbs_page > 1).then(|| format!("{back_url}?tab=databases&dbs_page={}", dbs_page - 1)),
                dbs_next => (dbs_page < dbs_pages).then(|| format!("{back_url}?tab=databases&dbs_page={}", dbs_page + 1)),
                empty_heading => t("no_databases"),
                unlimited_table => admin.t("server/database.unlimited"),
                rotate_title => t("rotate_password"),
                record => context! { id => h.id.0, name => h.name.clone(), host => h.host.clone(), port => h.port.0 },
                edit_url => edit_url,
                can_delete => can_delete,
                delete_disabled => delete_disabled,
                delete_tooltip => delete_tooltip,
                back_url => back_url,
                node_options => node_options(admin).await?,
                v => data.ctx(),
                errors => Mj::from_serialize(errors),
            },
        )
        .await
}

async fn dbs_count(st: &pelican_core::state::AppState, id: i64) -> Result<i64, WebError> {
    Ok(database_counts(st, &[id]).await?.get(&id).copied().unwrap_or(0))
}

/// One row of the relation-manager table with everything the modal needs.
struct DbRow {
    id: i64,
    database: String,
    username: String,
    remote: String,
    max_conn: String,
    created_at: String,
    password: String,
    jdbc: String,
    server_name: Option<String>,
    server_url: Option<String>,
}

impl DbRow {
    fn ctx(self) -> Mj {
        context! {
            id => self.id, database => self.database, username => self.username, remote => self.remote,
            max_conn => self.max_conn, created_at => self.created_at, password => self.password,
            jdbc => self.jdbc, server_name => self.server_name, server_url => self.server_url,
        }
    }
}

/// PHP `urlencode()` (application/x-www-form-urlencoded: `-_.` and alnum kept,
/// space becomes `+`).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Rows of the `Databases` relation manager for one page.
async fn load_databases(admin: &Admin, h: &database_hosts::Model, page: u64) -> Result<Vec<Mj>, WebError> {
    let rows = databases::Entity::find()
        .order_by_asc(databases::Column::Id)
        .filter(databases::Column::DatabaseHostId.eq(Int(h.id.0)))
        .all(&admin.st.db)
        .await?;
    let slice: Vec<&databases::Model> =
        rows.iter().skip(((page - 1) * DBS_PER_PAGE) as usize).take(DBS_PER_PAGE as usize).collect();
    let server_ids: Vec<i64> = slice.iter().map(|d| d.server_id.0).collect();
    let mut srv: HashMap<i64, servers::Model> = HashMap::new();
    if !server_ids.is_empty() {
        for s in servers::Entity::find().filter(servers::Column::Id.is_in(server_ids)).all(&admin.st.db).await? {
            srv.insert(s.id.0, s);
        }
    }
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let address = format!("{}:{}", h.host, h.port.0);
    let mut out = Vec::new();
    for d in slice {
        // `$record->remote === '%' ? trans('admin/databasehost.anywhere') . ' ( % )'`.
        let remote = if d.remote == "%" { format!("{} ( % )", t("anywhere")) } else { d.remote.clone() };
        // Table column uses the `server/database.unlimited` key, as the original does.
        let max_conn = d
            .max_connections
            .filter(|c| c.0 != 0)
            .map(|c| c.0.to_string())
            .unwrap_or_else(|| admin.t("server/database.unlimited"));
        let password = admin.st.enc.decrypt_string_lenient(&d.password).unwrap_or_default();
        // `Database::jdbc` attribute.
        let jdbc = format!("jdbc:mysql://{}:{}@{}/{}", d.username, urlencode(&password), address, d.database);
        let server = srv.get(&d.server_id.0);
        // Column url shown only when the user may update that server.
        let server_url = server
            .and_then(|s| admin.user.can_update_server(s.node_id.0).then(|| s.id.0))
            .map(|sid| format!("/admin/servers/{sid}/edit"));
        out.push(DbRow {
            id: d.id.0,
            database: d.database.clone(),
            username: d.username.clone(),
            remote,
            max_conn,
            created_at: d.created_at.map(|ts| ts.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default(),
            password,
            jdbc,
            server_name: server.map(|s| s.name.clone()),
            server_url,
        });
    }
    Ok(out.into_iter().map(DbRow::ctx).collect())
}

/// `GET {BASE}/{id}` (ViewDatabaseHost, read-only).
async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view databaseHost")?;
    let (h, node_ids) = find_host(&admin, id).await?;
    let tab = query_flag(admin.query.as_deref(), "tab").unwrap_or_else(|| "record".into());
    let dbs_page: u64 = query_flag(admin.query.as_deref(), "dbs_page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let back = page_url(&admin);
    render_record_page(
        &admin,
        &h,
        &node_ids,
        &FormData::from_model(&h, &node_ids),
        &Errors::default(),
        true,
        &tab,
        dbs_page,
        &back,
    )
    .await
}

/// `GET {BASE}/{id}/edit` (EditDatabaseHost).
async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update databaseHost")?;
    let (h, node_ids) = find_host(&admin, id).await?;
    let tab = query_flag(admin.query.as_deref(), "tab").unwrap_or_else(|| "record".into());
    let dbs_page: u64 = query_flag(admin.query.as_deref(), "dbs_page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let back = page_url(&admin);
    render_record_page(
        &admin,
        &h,
        &node_ids,
        &FormData::from_model(&h, &node_ids),
        &Errors::default(),
        false,
        &tab,
        dbs_page,
        &back,
    )
    .await
}

/// POST `{BASE}/{id}/edit` (`EditRecord` + `HostUpdateService::handle`):
/// update in a transaction, re-test the connection, then sync the pivots.
async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update databaseHost")?;
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let (h, node_ids) = find_host(&admin, id).await?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, false).await?;
    let back = format!("{BASE}/{id}/edit");
    if !errors.is_empty() {
        return render_record_page(&admin, &h, &node_ids, &data, &errors, false, "record", 1, &back).await;
    }
    // An `empty()` provided password keeps the stored one ("0" included).
    let pw_new: Option<String> = data.password.clone().filter(|p| !pelican_core::php::empty(&json!(p)));
    let now = Timestamp(pelican_core::time::now());
    let b = admin.st.db.get_database_backend();
    let txn = admin.st.db.begin().await?;
    let mut am = database_hosts::ActiveModel {
        id: Unchanged(h.id),
        name: Set(data.name.clone()),
        host: Set(data.host.clone()),
        port: Set(Int(php_int(&data.port))),
        username: Set(data.username.clone()),
        max_databases: Set(opt_int(&data.max_databases)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    if let Some(p) = pw_new.clone() {
        am.password = Set(admin.st.enc.encrypt_string(&p));
    }
    pelican_db::write::update(&txn, &am).await?;
    // Probe model mirroring `$host->buildConnection()` after the update.
    let mut probe = h.clone();
    probe.name = data.name.clone();
    probe.host = data.host.clone();
    probe.port = Int(php_int(&data.port));
    probe.username = data.username.clone();
    probe.max_databases = opt_int(&data.max_databases);
    probe.updated_at = Some(now);
    if let Some(p) = pw_new.clone() {
        probe.password = admin.st.enc.encrypt_string(&p);
    }
    if let Err(msg) = test_connection(&admin.st, &probe).await {
        admin.flash_danger(t("error"), Some(msg));
        return render_record_page(&admin, &h, &node_ids, &data, &errors, false, "record", 1, &back).await;
    }
    txn.commit().await?;
    sync_nodes(&admin.st.db, b, Int(h.id.0), &requested_node_ids(&admin, &form)).await.map_err(api_err)?;
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&back))
}

/// Map an `ApiError` of reused pelican-api services onto web internals.
fn api_err(e: pelican_api::error::ApiError) -> WebError {
    WebError::internal(e.errors.into_iter().next().map(|o| o.detail).unwrap_or_else(|| "QueryException".into()))
}

/// POST `{BASE}/{id}/delete`: plain Eloquent delete of the host (header
/// `DeleteAction`; blocked while databases exist — the button is disabled and
/// the guard mirrors that).
async fn delete_host(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("delete databaseHost")?;
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let (h, _) = find_host(&admin, id).await?;
    if dbs_count(&admin.st, id).await? > 0 {
        admin.flash_danger(t("delete_help"), None);
        return Ok(admin.redirect(&format!("{BASE}/{id}/edit")));
    }
    database_hosts::Entity::delete_by_id(h.id).exec(&admin.st.db).await?;
    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(admin.redirect(BASE))
}

// ---------------------------------------------------------------------------
// Relation-manager actions (`RotateDatabasePasswordAction`, DeleteAction of
// the Databases table; root-admin gated — spec ui.md §3507)
// ---------------------------------------------------------------------------

/// The database must belong to this host (nested relation route binding).
async fn find_db(st: &pelican_core::state::AppState, host_id: i64, db_id: i64) -> Result<databases::Model, WebError> {
    databases::Entity::find()
        .filter(databases::Column::Id.eq(Int(db_id)))
        .filter(databases::Column::DatabaseHostId.eq(Int(host_id)))
        .one(&st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// POST `{BASE}/{id}/databases/{db}/rotate`:
/// `DatabaseManagementService::rotatePassword` + activity log + notifications.
async fn rotate_database(admin: Admin, Path((id, db_id)): Path<(i64, i64)>, form: Form) -> WebResult {
    // `DatabasePolicy::update` against a null tenant passes only for root admins
    // (docs/spec/ui.md §3507).
    if !admin.user.is_root_admin() {
        return Err(WebError::Forbidden);
    }
    let t = |k: &str| admin.t(&format!("admin/databasehost.{k}"));
    let (_, _) = find_host(&admin, id).await?;
    let d = find_db(&admin.st, id, db_id).await?;
    let h = database_hosts::Entity::find_by_id(d.database_host_id)
        .one(&admin.st.db)
        .await?
        .ok_or_else(|| WebError::internal("no host"))?;
    // Service body: new password saved, MySQL user recreated with grants.
    let txn_result: Result<(), WebError> = async {
        let txn = admin.st.db.begin().await?;
        let password = pelican_core::databases::random_password(24);
        let am = databases::ActiveModel {
            id: Unchanged(d.id),
            password: Set(admin.st.enc.encrypt_string(&password)),
            updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
            ..Default::default()
        };
        pelican_db::write::update(&txn, &am).await?;
        let conn = HostConnection::open(&h, &admin.st.enc).await.map_err(|e| WebError::internal(conn_err(e)))?;
        let ddl = async {
            conn.drop_user(&d).await?;
            conn.create_user(&d, &password).await?;
            conn.assign_user(&d).await?;
            conn.flush_privileges().await
        };
        let result = ddl.await;
        conn.close().await;
        result.map_err(|e| WebError::internal(conn_err(e)))?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    match txn_result {
        Ok(()) => {
            admin
                .activity("server:database.rotate-password")
                .subject(morph::DATABASE, d.id.0)
                .property("name", d.database.clone())
                .log(&admin.st)
                .await
                .map_err(api_err)?;
            admin.flash_success(t("rotated"));
        }
        Err(e) => {
            admin.flash_danger(t("rotate_error"), Some(e.to_string()));
        }
    }
    Ok(admin.redirect(&form.back(&format!("{BASE}/{id}/edit?tab=databases"))))
}

/// POST `{BASE}/{id}/databases/{db}/delete`: the default Filament `DeleteAction`
/// deletes only the `databases` row, not the remote database/user (spec ui.md
/// §3508 documents this asymmetry; replicated deliberately).
async fn delete_database(admin: Admin, Path((id, db_id)): Path<(i64, i64)>, form: Form) -> WebResult {
    if !admin.user.is_root_admin() {
        return Err(WebError::Forbidden);
    }
    let (_, _) = find_host(&admin, id).await?;
    let d = find_db(&admin.st, id, db_id).await?;
    databases::Entity::delete_by_id(d.id).exec(&admin.st.db).await?;
    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(admin.redirect(&form.back(&format!("{BASE}/{id}/edit?tab=databases"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_commands() {
        let (cu, ap) = commands_for("u1", "secret", "127.0.0.1");
        assert_eq!(cu, "CREATE USER 'u1'@'127.0.0.1' IDENTIFIED BY 'secret';");
        assert_eq!(ap, "GRANT ALL PRIVILEGES ON *.* TO 'u1'@'127.0.0.1' WITH GRANT OPTION;");
    }

    #[test]
    fn php_style_encoding() {
        assert_eq!(urlencode("abc-_.~ x+y"), "abc-_.%7E+x%2By");
    }

    #[test]
    fn int_casts() {
        assert_eq!(php_int("3306"), 3306);
        assert_eq!(php_int("3.9"), 3);
        assert_eq!(opt_int(""), None);
        assert_eq!(opt_int("12"), Some(Int(12)));
    }
}
