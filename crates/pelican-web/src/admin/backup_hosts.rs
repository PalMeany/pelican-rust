//! Admin › Backup Hosts (`app/Filament/Admin/Resources/BackupHosts`, model
//! `app/Models/BackupHost.php`, schemas `app/Extensions/BackupAdapter/**`):
//! list, create/view/edit form with the per-schema configuration section,
//! delete and the read-only Backups relation manager as a tab (docs/spec/ui.md §1489).

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::{model_events, validation::Validator};
use pelican_core::state::AppState;
use pelican_db::entities::{backup_host_node, backup_hosts, backups, nodes, servers};
use pelican_db::{Int, Json, Timestamp, raw};
use sea_orm::{
    ActiveValue::{NotSet, Set, Unchanged},
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use serde_json::{Map, Value};

use crate::views::diff_for_humans;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const BASE: &str = "/admin/backup-hosts";
const SLUG: &str = "backup-hosts";
const RM_PER_PAGE: u64 = 10;

/// `BackupAdapterService::getMappings()` of the registered default schemas
/// (`BackupAdapterServiceProvider`); the name is `Str::title(id)`.
const SCHEMAS: [(&str, &str); 2] = [("wings", "Wings"), ("s3", "S3")];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/backup-hosts", get(index))
        .route("/admin/backup-hosts/create", get(create_form).post(create))
        .route("/admin/backup-hosts/configuration", post(configuration_fragment))
        .route("/admin/backup-hosts/{id}", get(view))
        .route("/admin/backup-hosts/{id}/edit", get(edit_form).post(update))
        .route("/admin/backup-hosts/{id}/delete", post(delete_host))
}

/// `BackupHostPolicy::before()`: a record is denied when any linked node is
/// not targetable.
fn record_allowed(admin: &Admin, node_ids: &[i64]) -> bool {
    node_ids.iter().all(|n| admin.user.can_target_node(*n))
}

/// Linked node ids per host (`$host->nodes()->pluck('id')`).
async fn host_nodes(st: &pelican_core::state::AppState, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>, WebError> {
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    if !ids.is_empty() {
        for r in backup_host_node::Entity::find()
            .filter(backup_host_node::Column::BackupHostId.is_in(ids.to_vec()))
            .all(&st.db)
            .await?
        {
            out.entry(r.backup_host_id.0).or_default().push(r.node_id.0);
        }
    }
    Ok(out)
}

/// `$host->backups()->count()` per id.
async fn backup_counts(st: &pelican_core::state::AppState, ids: &[i64]) -> Result<HashMap<i64, i64>, WebError> {
    let mut out: HashMap<i64, i64> = HashMap::new();
    if !ids.is_empty() {
        for b in backups::Entity::find().filter(backups::Column::BackupHostId.is_in(ids.to_vec())).all(&st.db).await? {
            *out.entry(b.backup_host_id.0).or_insert(0) += 1;
        }
    }
    Ok(out)
}

/// `GET {BASE}` (ListBackupHosts).
/// `getModelLabel()` / `getPluralModelLabel()` (`trans_choice` of one key).
fn model_label(admin: &Admin, n: f64) -> String {
    crate::i18n::trans_choice(&admin.locale(), "admin/backuphost.model_label", n, &[])
}

async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList backupHost")?;
    let t = |k: &str| admin.t(&format!("admin/backuphost.{k}"));
    let opts = [10, 25, 50, 0];
    let mut p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    // No query scope in the original — every host is listed (spec ui.md §1489).
    let all = backup_hosts::Entity::find().order_by_asc(backup_hosts::Column::Id).all(&admin.st.db).await?;
    // Filament searches by `$recordTitleAttribute` (name).
    let needle = p.search.to_lowercase();
    let rows: Vec<&backup_hosts::Model> =
        all.iter().filter(|h| needle.is_empty() || h.name.to_lowercase().contains(&needle)).collect();
    let total = rows.len() as u64;
    p.clamp(total);
    let page_rows: Vec<&&backup_hosts::Model> = match p.limit_offset() {
        Some((limit, offset)) => rows.iter().skip(offset as usize).take(limit as usize).collect(),
        None => rows.iter().collect(),
    };
    let page_ids: Vec<i64> = page_rows.iter().map(|h| h.id.0).collect();
    let rel = host_nodes(&admin.st, &page_ids).await?;
    let counts = backup_counts(&admin.st, &page_ids).await?;
    let node_names: HashMap<i64, String> =
        nodes::Entity::find().all(&admin.st.db).await?.into_iter().map(|n| (n.id.0, n.name)).collect();
    let can_edit = admin.can("update backupHost");
    let can_view = admin.can("view backupHost");
    let mut table = Table::new("backup-hosts", BASE);
    table.columns = vec![
        Column::new("name", t("name")),
        Column::new("schema", t("schema")),
        Column::new("backups_count", t("backups")),
        Column::new("nodes", t("linked_nodes")),
    ];
    for h in page_rows.into_iter() {
        let h = *h;
        let node_ids = rel.get(&h.id.0).cloned().unwrap_or_default();
        let allowed = record_allowed(&admin, &node_ids);
        // Row actions follow the record policy: Edit when allowed, else View;
        // ViewAction is hidden when the user may edit.
        let url = if allowed && can_edit {
            format!("{BASE}/{}/edit", h.id.0)
        } else if allowed && can_view {
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
            // Schema badge shows the raw schema id, as the column has no color map.
            .cell("schema", Cell::badge(h.schema.clone(), "primary"))
            .cell("backups_count", Cell::text(counts.get(&h.id.0).copied().unwrap_or(0).to_string()));
        if node_ids.is_empty() {
            row = row.cell("nodes", Cell::text(t("all_nodes")));
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
    table.empty_icon = "file-zip".into();
    table.empty_heading = t("no_backup_hosts");
    table.empty_description = t("local_backups_only");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create backupHost").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &model_label(&admin, 1.0))]),
            url => format!("{BASE}/create"),
        }
    });
    admin
        .page(
            SLUG,
            "admin/list.html",
            context! { title => model_label(&admin, 2.0), table => Mj::from_serialize(&table), create => create },
        )
        .await
}

// ---------------------------------------------------------------------------
// Form data + validation
// ---------------------------------------------------------------------------

struct FormData {
    name: String,
    schema: String,
    node_ids: Vec<String>,
    region: String,
    key: String,
    secret: String,
    bucket: String,
    endpoint: String,
    path_style: bool,
}

impl FormData {
    fn empty() -> FormData {
        FormData {
            name: String::new(),
            schema: String::new(),
            node_ids: vec![],
            region: String::new(),
            key: String::new(),
            secret: String::new(),
            bucket: String::new(),
            endpoint: String::new(),
            path_style: false,
        }
    }

    fn from_model(m: &backup_hosts::Model) -> FormData {
        // The six editable keys come back out of the configuration JSON; keys
        // not editable here are kept on save via merge (see below).
        let cfg = m.configuration.as_ref().and_then(|c| c.0.as_object());
        let s = |key: &str| cfg.and_then(|o| o.get(key)).and_then(Value::as_str).unwrap_or_default().to_string();
        FormData {
            name: m.name.clone(),
            schema: m.schema.clone(),
            node_ids: vec![],
            region: s("region"),
            key: s("key"),
            secret: s("secret"),
            bucket: s("bucket"),
            endpoint: s("endpoint"),
            path_style: cfg.and_then(|o| o.get("use_path_style_endpoint")).and_then(Value::as_bool).unwrap_or(false),
        }
    }

    fn from_form(f: &Form) -> FormData {
        FormData {
            name: f.get("name").trim().to_string(),
            schema: f.get("schema").trim().to_string(),
            node_ids: f.list("node_ids"),
            region: f.get("configuration.region").to_string(),
            key: f.get("configuration.key").to_string(),
            secret: f.get("configuration.secret").to_string(),
            bucket: f.get("configuration.bucket").to_string(),
            endpoint: f.get("configuration.endpoint").to_string(),
            path_style: f.bool("configuration.use_path_style_endpoint"),
        }
    }

    fn ctx(&self) -> Mj {
        context! {
            name => self.name, schema => self.schema, node_ids => self.node_ids,
        }
    }

    /// Values of the S3 fields as displayed inside the configuration section.
    fn cfg_ctx(&self) -> Mj {
        context! {
            region => self.region, key => self.key, secret => self.secret,
            bucket => self.bucket, endpoint => self.endpoint, path_style => self.path_style,
        }
    }
}

/// The configuration JSON to store for an `s3` host: the submitted fields over
/// the stored ones (Filament dehydration only writes the bound paths, so keys
/// like `prefix` or `storage_class` survive an edit).
fn s3_configuration(data: &FormData, old: Option<&Value>) -> Value {
    let mut obj = match old.and_then(Value::as_object) {
        Some(o) => o.clone(),
        None => Map::new(),
    };
    for (k, v) in [
        ("region", data.region.as_str()),
        ("key", data.key.as_str()),
        ("secret", data.secret.as_str()),
        ("bucket", data.bucket.as_str()),
        ("endpoint", data.endpoint.as_str()),
    ] {
        obj.insert(k.into(), Value::from(v));
    }
    obj.insert("use_path_style_endpoint".into(), Value::from(data.path_style));
    Value::Object(obj)
}

/// Field rules of the Filament form plus `BackupHost::$validationRules`
/// (the conjunction per field; the effective values below the selected schema).
fn validate(admin: &Admin, data: &FormData) -> Result<Errors, WebError> {
    let input = serde_json::json!({
        "name": data.name,
        "schema": data.schema,
        "region": data.region, "key": data.key, "secret": data.secret,
        "bucket": data.bucket, "endpoint": data.endpoint,
    });
    let mut v = Validator::for_value(&input);
    let lbl = |k: &str| admin.t(&format!("admin/backuphost.{k}"));
    for f in ["name", "schema"] {
        v.attribute_name(f, &lbl(f).to_lowercase());
    }
    v.rules("name", &["required", "string", "max:255"]);
    v.rules("schema", &["required", "string", "max:255"]);
    if data.schema == "s3" {
        // Labels of the five required text inputs live under settings' S3 group.
        for f in ["region", "key", "secret", "bucket", "endpoint"] {
            v.attribute_name(f, &admin.t(&format!("admin/setting.backup.s3.{f}")));
            v.rules(f, &["required"]);
        }
    }
    v.run();
    Ok(Errors::from_validator(&v))
}

/// Deduplicated accessible subset of the submitted node ids (the select only
/// lists accessible nodes; pivot enforces one node → at most one host).
async fn requested_node_ids(admin: &Admin, form: &Form) -> Result<Vec<i64>, WebError> {
    let mut out: Vec<i64> = Vec::new();
    for s in form.list("node_ids") {
        if let Ok(id) = s.trim().parse::<i64>() {
            if !out.contains(&id) && admin.user.can_target_node(id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Replace the node pivots of a host (`$host->nodes()->sync($ids)`); the pivot
/// declares no extra columns so no timestamps are written (cf. `mounts` sync).
async fn sync_nodes<C: ConnectionTrait>(db: &C, host_id: i64, ids: &[i64]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(
        b,
        "delete from {backup_host_node} where {backup_host_id} = ?",
        vec![Int(host_id).into()],
    ))
    .await?;
    for id in ids {
        db.execute_raw(raw::stmt(
            b,
            "insert into {backup_host_node} ({node_id}, {backup_host_id}) values (?, ?)",
            vec![Int(*id).into(), Int(host_id).into()],
        ))
        .await?;
    }
    Ok(())
}

async fn find_host(admin: &Admin, id: i64) -> Result<(backup_hosts::Model, Vec<i64>), WebError> {
    let h = backup_hosts::Entity::find_by_id(Int(id)).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    let node_ids = host_nodes(&admin.st, &[id]).await?.remove(&id).unwrap_or_default();
    if !record_allowed(admin, &node_ids) {
        return Err(WebError::Forbidden);
    }
    Ok((h, node_ids))
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

// ---------------------------------------------------------------------------
// Record pages (create / view / edit) + Backups relation manager tab
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn render_record_page(
    admin: &Admin,
    record: Option<&backup_hosts::Model>,
    node_ids: &[i64],
    data: &FormData,
    errors: &Errors,
    readonly: bool,
    tab: &str,
    rm_page: u64,
    rm_search: &str,
    rm_sort_desc: Option<bool>,
) -> WebResult {
    let t = |k: &str| admin.t(&format!("admin/backuphost.{k}"));
    let title = match record {
        Some(h) if readonly => {
            admin.t_with("filament-panels::resources/pages/view-record.title", &[("label", &h.name)])
        }
        Some(h) => admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &h.name)]),
        None => {
            admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &model_label(admin, 1.0))])
        }
    };
    let action = match record {
        Some(h) => format!("{BASE}/{}/edit", h.id.0),
        None => format!("{BASE}/create"),
    };
    // Header EditAction of ViewBackupHost.
    let edit_url = if readonly && admin.can("update backupHost") && record_allowed(admin, node_ids) {
        Some(format!("{BASE}/{}/edit", record.map(|h| h.id.0).unwrap_or(0)))
    } else {
        None
    };
    // Header DeleteAction (edit page only): hidden while this is the only host,
    // disabled and relabelled while the host still has backups.
    let backups_total = match record {
        Some(h) => backup_counts(&admin.st, &[h.id.0]).await?.get(&h.id.0).copied().unwrap_or(0),
        None => 0,
    };
    let hosts_total = count_hosts(&admin.st).await?;
    let can_delete = !readonly && record.is_some() && admin.can("delete backupHost") && hosts_total > 1;
    let delete_disabled = backups_total > 0;
    let delete_label = if delete_disabled {
        t("delete_help")
    } else {
        admin.t("filament-actions::delete.single.modal.actions.delete.label")
    };
    // Base URL of the page (edit/view), rebuilt fresh for the tab links
    // (`persistTabInQueryString` keeps only the tab param).
    let base = match record {
        Some(h) if readonly => format!("{BASE}/{}", h.id.0),
        Some(h) => format!("{BASE}/{}/edit", h.id.0),
        None => String::new(),
    };
    let backups_base = format!("{}?tab=backups&rm_search={}", base, urlencode(rm_search));
    let show_backups = admin.user.is_root_admin(); // spec ui.md §1489: BackupPolicy denies others

    let mut rm_rows: Vec<Mj> = Vec::new();
    let mut rm_pages = 0u64;
    if show_backups && tab == "backups" {
        let b = load_backups(&admin, record.map(|h| h.id.0).unwrap_or(0), rm_page, rm_search, rm_sort_desc).await?;
        rm_rows = b.rows;
        rm_pages = b.pages;
    }
    let rm_page = rm_page.clamp(1, rm_pages.max(1));

    admin
        .page(
            SLUG,
            "admin/backup_hosts/form.html",
            context! {
                title => title,
                action => action,
                readonly => readonly,
                is_record => record.is_some(),
                active_record => tab != "backups",
                record_tab_url => format!("{base}?tab=record"),
                backups_tab_url => backups_base.clone(),
                show_backups => show_backups && record.is_some(),
                rm_rows => rm_rows,
                rm_pages => rm_pages,
                rm_page => rm_page,
                rm_search => rm_search,
                rm_prev =>
                    (tab == "backups" && rm_page > 1)
                        .then(|| format!("{backups_base}&rm_page={}", rm_page - 1)),
                rm_next => (rm_pages > rm_page)
                        .then(|| format!("{backups_base}&rm_page={}", rm_page + 1)),
                created_at_sort_url => {
                    // DateTimeColumn sortable: clicking toggles asc/desc.
                    let next = if rm_sort_desc == Some(true) { "asc" } else { "desc" };
                    format!("{backups_base}&rm_sort=created_at&rm_dir={next}")
                },
                rm_dir => match rm_sort_desc {
                    Some(true) => "desc",
                    Some(false) => "asc",
                    _ => "",
                },
                rm_created_label => admin.t("server/backup.created_at"),
                server_col_label => admin.t("admin/server.name"),
                empty_heading =>
                    admin.t_with("filament-tables::table.empty.heading", &[("model", "Backup")]),
                empty_description =>
                    admin.t_with("filament-tables::table.empty.description", &[("model", "Backup")]),
                empty_icon => "file-zip",
                status_in_progress => admin.t("server/backup.backup_status.in_progress"),
                status_successful => admin.t("server/backup.backup_status.successful"),
                status_failed => admin.t("server/backup.backup_status.failed"),
                bhs => data.schema,
                v => data.ctx(),
                cfg => data.cfg_ctx(),
                errors => Mj::from_serialize(errors),
                node_options => node_options(admin).await?,
                schema_options =>
                    SCHEMAS.iter().map(|(id, name)| ((*id).to_string(), (*name).to_string())).collect::<Vec<_>>(),
                hx_attrs => (!readonly)
                        .then(|| "hx-post=\"/admin/backup-hosts/configuration\" hx-trigger=\"change\" \
                                hx-target=\"#bh-configuration\" hx-swap=\"outerHTML\" hx-include=\"#record-form\""),
                edit_url => edit_url,
                can_delete => can_delete,
                delete_disabled => delete_disabled,
                delete_label => delete_label,
                delete_action => format!("{BASE}/{}/delete", record.map(|h| h.id.0).unwrap_or(0)),
                record_name => record.map(|h| h.name.clone()).unwrap_or_default(),
                save_label =>
                    if readonly { String::new() }
                    else if record.is_some() {
                        admin.t("filament-panels::resources/pages/edit-record.form.actions.save.label")
                    } else {
                        admin.t("filament-panels::resources/pages/create-record.form.actions.create.label")
                    },
            },
        )
        .await
}

/// Total number of hosts (`EditBackupHost` hides Delete while one remains).
async fn count_hosts(st: &pelican_core::state::AppState) -> Result<i64, WebError> {
    Ok(backup_hosts::Entity::find().all(&st.db).await?.len() as i64)
}

/// PHP `urlencode()` (application/x-www-form-urlencoded), as in `database_hosts`.
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

/// First value of `key` in the request query string.
fn query_flag(query: Option<&str>, key: &str) -> Option<String> {
    form_urlencoded::parse(query.unwrap_or("").as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

// ---------------------------------------------------------------------------
// Relation manager rows (BackupsRelationManager)
// ---------------------------------------------------------------------------

/// One rendered row of the Backups tab plus the pagination totals.
struct BackupsPage {
    rows: Vec<Mj>,
    pages: u64,
}

#[allow(clippy::too_many_arguments)]
async fn load_backups(
    admin: &Admin,
    host_id: i64,
    page: u64,
    search: &str,
    sort_desc: Option<bool>,
) -> Result<BackupsPage, WebError> {
    let locale = admin.locale();
    let now = chrono::Utc::now();
    let mut bs = backups::Entity::find()
        .order_by_asc(backups::Column::Id)
        .filter(backups::Column::BackupHostId.eq(Int(host_id)))
        .all(&admin.st.db)
        .await?;
    if let Some(desc) = sort_desc {
        // Sortable DateTimeColumn: order the current result set by created_at
        // (nulls first, then id).
        bs.sort_by_key(|b| (b.created_at.map(|t| t.0), b.id.0));
        if desc {
            bs.reverse();
        }
    }
    let server_ids: Vec<i64> = bs.iter().map(|b| b.server_id.0).collect();
    let srv: HashMap<i64, servers::Model> = if server_ids.is_empty() {
        HashMap::new()
    } else {
        servers::Entity::find()
            .filter(servers::Column::Id.is_in(server_ids))
            .all(&admin.st.db)
            .await?
            .into_iter()
            .map(|s| (s.id.0, s))
            .collect()
    };
    let needle = search.to_lowercase();
    let filtered: Vec<&backups::Model> = bs
        .iter()
        .filter(|b| {
            if needle.is_empty() {
                return true;
            }
            if b.name.to_lowercase().contains(&needle) {
                return true;
            }
            srv.get(&b.server_id.0).is_some_and(|s| s.name.to_lowercase().contains(&needle))
        })
        .collect();
    let total = filtered.len() as u64;
    let pages = total.div_ceil(RM_PER_PAGE).max(1);
    let page = page.clamp(1, pages);
    let slice: Vec<&&backups::Model> =
        filtered.iter().skip(((page - 1) * RM_PER_PAGE) as usize).take(RM_PER_PAGE as usize).collect();
    let in_progress = admin.t("server/backup.backup_status.in_progress");
    let successful = admin.t("server/backup.backup_status.successful");
    let failed = admin.t("server/backup.backup_status.failed");
    let rows = slice
        .into_iter()
        .map(|b| {
            // Status badge mapping shared with the client backups list.
            let (status, color) = if pending(b) {
                (in_progress.clone(), "primary")
            } else if b.is_successful.0 {
                (successful.clone(), "success")
            } else {
                (failed.clone(), "danger")
            };
            let server = srv.get(&b.server_id.0);
            // Server link shown only when the user may update that server.
            let server_url = server
                .and_then(|s| admin.user.can_update_server(s.node_id.0).then(|| s.id.0))
                .map(|sid| format!("/admin/servers/{sid}/edit"));
            context! {
                server_name => server.map(|s| s.name.clone()).unwrap_or_default(),
                server_url => server_url,
                name => b.name.clone(),
                bytes => b.bytes.0,
                created_at =>
                    b.created_at
                        .map(|ts| ts.0.and_utc())
                        .map(|ts| diff_for_humans(&locale, ts, now))
                        .unwrap_or_default(),
                status => status,
                status_color => color,
                locked => b.is_locked.0,
            }
        })
        .collect();
    Ok(BackupsPage { rows, pages })
}

/// True while a backup has not completed yet (shared client-page semantics).
fn pending(b: &backups::Model) -> bool {
    b.completed_at.is_none()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET {BASE}/create`.
async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create backupHost")?;
    render_record_page(&admin, None, &[], &FormData::empty(), &Errors::default(), false, "record", 1, "", None).await
}

/// `GET {BASE}/{id}` (ViewBackupHost, read-only).
async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view backupHost")?;
    let (h, node_ids) = find_host(&admin, id).await?;
    let mut data = FormData::from_model(&h);
    data.node_ids = node_ids.iter().map(i64::to_string).collect();
    render_from_query(&admin, &h, &node_ids, data, Errors::default(), true).await
}

/// `GET {BASE}/{id}/edit` (EditBackupHost).
async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update backupHost")?;
    let (h, node_ids) = find_host(&admin, id).await?;
    let mut data = FormData::from_model(&h);
    data.node_ids = node_ids.iter().map(i64::to_string).collect();
    render_from_query(&admin, &h, &node_ids, data, Errors::default(), false).await
}

/// Reads the tab / relation-manager state from the request query string.
async fn render_from_query(
    admin: &Admin,
    h: &backup_hosts::Model,
    node_ids: &[i64],
    data: FormData,
    errors: Errors,
    readonly: bool,
) -> WebResult {
    let query = |key: &str| query_flag(admin.query.as_deref(), key).unwrap_or_default();
    let tab = match query("tab").as_str() {
        "backups" => "backups".to_string(),
        _ => "record".to_string(),
    };
    let rm_page: u64 = query("rm_page").parse().unwrap_or(1);
    let rm_search = query("rm_search");
    let rm_sort_desc = match query("rm_sort").as_str() {
        "created_at" => Some(query("rm_dir") == "desc"),
        _ => None,
    };
    render_record_page(admin, Some(h), node_ids, &data, &errors, readonly, &tab, rm_page, &rm_search, rm_sort_desc)
        .await
}

/// `{path}?{query}` — the base URL links point back to.
fn page_url(admin: &Admin) -> String {
    match admin.query.as_deref() {
        Some(q) if !q.is_empty() => format!("{}?{}", admin.path, q),
        _ => admin.path.clone(),
    }
}

/// POST `{BASE}/configuration`: Livewire fragment equivalent — re-renders the
/// Configuration section for the newly selected schema keeping typed values.
async fn configuration_fragment(admin: Admin, form: Form) -> WebResult {
    if !(admin.can("create backupHost") || admin.can("update backupHost")) {
        return Err(WebError::Forbidden);
    }
    let data = FormData::from_form(&form);
    admin.render_block(
        "admin/backup_hosts/form.html",
        "configuration",
        context! { bhs => data.schema, cfg => data.cfg_ctx(), readonly => false, errors => Mj::from_serialize(&Errors::default()) },
    )
}

/// POST `{BASE}/create`: insert (+ timestamps), then sync the node pivots.
async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create backupHost")?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data)?;
    if !errors.is_empty() {
        return render_record_page(&admin, None, &[], &data, &errors, false, "record", 1, "", None).await;
    }
    let now = Timestamp(pelican_core::time::now());
    // `configuration` stays null unless the s3 schema was chosen (no form
    // component binds it otherwise).
    let configuration = (data.schema == "s3").then(|| Json(s3_configuration(&data, None)));
    let txn = admin.st.db.begin().await?;
    let am = backup_hosts::ActiveModel {
        name: Set(data.name.clone()),
        schema: Set(data.schema.clone()),
        configuration: Set(configuration),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&txn, &am).await?.ok_or_else(|| WebError::internal("no id"))?;
    sync_nodes(&txn, id, &requested_node_ids(&admin, &form).await?).await?;
    txn.commit().await?;
    // CreateRecord → `BackupHost::create($data)` (created event).
    model_events::fire_id::<backup_hosts::Entity>(&admin.st, model_events::Action::Created, id).await;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("{BASE}/{id}/edit")))
}

/// POST `{BASE}/{id}/edit`: update, then sync the node pivots.
async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update backupHost")?;
    let (h, node_ids) = find_host(&admin, id).await?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data)?;
    if !errors.is_empty() {
        return render_from_query(&admin, &h, &node_ids, data, errors, false).await;
    }
    let mut am = backup_hosts::ActiveModel {
        id: Unchanged(h.id),
        name: Set(data.name.clone()),
        schema: Set(data.schema.clone()),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    if data.schema == "s3" {
        am.configuration = Set(Some(Json(s3_configuration(&data, h.configuration.as_ref().map(|c| &c.0)))));
    } else {
        // Fields of another schema are not bound here: keep the stored value.
        am.configuration = NotSet;
    }
    // EditRecord → `$record->fill($data)->save()`; the write always touches
    // `updated_at`, so treat it as dirty (updated event).
    let changed = pelican_db::write::update(&admin.st.db, &am).await?;
    sync_nodes(&admin.st.db, h.id.0, &requested_node_ids(&admin, &form).await?).await?;
    if changed {
        model_events::fire_id::<backup_hosts::Entity>(&admin.st, model_events::Action::Updated, h.id.0).await;
    }
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&page_url(&admin)))
}

/// POST `{BASE}/{id}/delete`: plain Eloquent delete; blocked while backups
/// remain (`DeleteAction` disabled state mirrors it) — the pivots cascade.
async fn delete_host(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("delete backupHost")?;
    let t = |k: &str| admin.t(&format!("admin/backuphost.{k}"));
    let (h, _) = find_host(&admin, id).await?;
    if backup_counts(&admin.st, &[id]).await?.get(&id).copied().unwrap_or(0) > 0 {
        admin.flash_danger(t("delete_help"), None);
        return Ok(admin.redirect(&form.back(&format!("{BASE}/{id}/edit"))));
    }
    let txn = admin.st.db.begin().await?;
    txn.execute_raw(raw::stmt(
        txn.get_database_backend(),
        "delete from {backup_host_node} where {backup_host_id} = ?",
        vec![Int(id).into()],
    ))
    .await?;
    backup_hosts::Entity::delete_by_id(h.id).exec(&txn).await?;
    txn.commit().await?;
    // DeleteAction → `$record->delete()` (deleted event; `h` was read before).
    model_events::deleted(&admin.st, &h).await;
    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(admin.redirect(BASE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The s3 configuration keeps keys it does not manage (`prefix`,
    /// `storage_class`) and stores the toggle as a real boolean.
    #[test]
    fn s3_configuration_merges_stored_keys() {
        let data = FormData {
            name: "h".into(),
            schema: "s3".into(),
            node_ids: vec![],
            region: "eu".into(),
            key: "k".into(),
            secret: "s".into(),
            bucket: "b".into(),
            endpoint: "https://e".into(),
            path_style: true,
        };
        let old: Value = serde_json::json!({"region": "old", "prefix": "p", "storage_class": "STANDARD"});
        let cfg = s3_configuration(&data, Some(&old));
        assert_eq!(cfg["region"], "eu");
        assert_eq!(cfg["use_path_style_endpoint"], true);
        assert_eq!(cfg["prefix"], "p");
        assert_eq!(cfg["storage_class"], "STANDARD");
    }

    #[test]
    fn query_params_roundtrip() {
        assert_eq!(urlencode("a b&c"), "a+b%26c");
        assert_eq!(query_flag(Some("?tab=backups&rm_page=2"), "rm_page").as_deref(), Some("2"));
        assert_eq!(query_flag(None, "tab"), None);
    }

    /// The record template renders in every mode (create / edit with the
    /// backups tab / view) without template errors.
    #[test]
    fn form_template_renders() {
        for (readonly, is_record, tab) in
            [(false, false, "record"), (false, true, "record"), (false, true, "backups"), (true, true, "record")]
        {
            let ctx = context! {
                title => "Backup Host",
                action => "/admin/backup-hosts/1/edit",
                readonly => readonly,
                is_record => is_record,
                active_record => (tab == "record"),
                record_tab_url => "/admin/backup-hosts/1/edit?tab=record",
                backups_tab_url => "/admin/backup-hosts/1/edit?tab=backups",
                show_backups => is_record,
                rm_rows => vec![context! {
                    server_name => "srv", server_url => "/admin/servers/1/edit", name => "b1",
                    bytes => 1536_i64, created_at => "5 minutes ago",
                    status => "Successful", status_color => "success", locked => true,
                }],
                rm_pages => 1u64, rm_page => 1u64, rm_search => "", rm_prev => None::<String>, rm_next => None::<String>,
                created_at_sort_url => "?tab=backups&rm_sort=created_at&rm_dir=desc",
                rm_dir => "", rm_created_label => "Created at", server_col_label => "Name",
                empty_heading => "No Backup", empty_description => "Create a Backup to get started.",
                empty_icon => "file-zip",
                status_in_progress => "In Progress", status_successful => "Successful", status_failed => "Failed",
                bhs => if tab == "record" { "" } else { "s3" },
                v => FormData { name: "Local".into(), schema: "wings".into(), node_ids: vec![], ..FormData::empty() }.ctx(),
                cfg => FormData::empty().cfg_ctx(),
                errors => Mj::from_serialize(&Errors::default()),
                node_options => Vec::<(String, String)>::new(),
                schema_options => SCHEMAS.iter().map(|(id, name)| ((*id).to_string(), (*name).to_string())).collect::<Vec<_>>(),
                hx_attrs => (!readonly)
                    .then(|| "hx-post=\"/admin/backup-hosts/configuration\" hx-target=\"#bh-configuration\""),
                edit_url => (is_record && readonly).then(|| "/admin/backup-hosts/1/edit"),
                can_delete => (is_record && !readonly),
                delete_disabled => false, delete_label => "Delete", delete_action => "/admin/backup-hosts/1/delete",
                record_name => "Local",
                save_label => "Save",
            };
            crate::views::render("admin/backup_hosts/form.html", ctx).expect("template renders");
        }
    }
}
