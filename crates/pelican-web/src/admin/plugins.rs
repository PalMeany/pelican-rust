//! Admin › Plugins (app/Filament/Admin/Resources/Plugins). The rows come from
//! the `plugins/` directory (`pelican_plugins::Config`, the Sushi model of the
//! original); lifecycle actions reuse `pelican_plugins::lifecycle` directly,
//! because the Filament jobs pick their own `enable` argument.
//!
//! Original background jobs (InstallPlugin/UpdatePlugin/UninstallPlugin) run
//! synchronously here with the same success/error notifications; "Settings" is
//! part of the WASM runtime work (ADR-0018) and is not implemented yet.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path as AxPath, Request};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::application::plugins as api;
use pelican_api::sidecar;
use pelican_core::state::AppState;
use pelican_plugins::{Category, Plugin, Status, compat, lifecycle, updates};
use serde_json::json;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::Form;
use crate::table::{Action, Cell, Column, ListParams, Row, Tab, Table};

const SLUG: &str = "plugins";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/plugins", get(index))
        .route("/admin/plugins/reorder", post(reorder))
        .route("/admin/plugins/{plugin}/action", post(action))
        .route("/admin/plugins/{plugin}/readme", get(readme))
        .route("/admin/plugins/{plugin}/settings", get(settings_form).post(settings_save))
        .route("/admin/plugins/import/file", post(import_file))
        .route("/admin/plugins/import/url", post(import_url))
}

/// The plugin service config with the panel's translator for its messages.
fn config(admin: &Admin) -> pelican_plugins::Config {
    let mut cfg = api::config(&admin.st);
    let locale = admin.locale();
    cfg.translate = std::sync::Arc::new(move |key: &str, replace: &[(&str, &str)]| {
        let params: Vec<(String, String)> = replace.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        crate::i18n::trans(&locale, key, &params)
    });
    cfg
}

/// `(strlen > 80) ? substr(..., 80).'...' : description`.
fn truncated(s: Option<&str>) -> Option<String> {
    let s = s?;
    Some(if s.len() > 80 {
        let mut end = 80.min(s.len());
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    } else {
        s.to_string()
    })
}

/// `Plugin::getReadme()`: `plugins/<dir>/README.md`.
fn readme_of(cfg: &pelican_plugins::Config, p: &Plugin) -> Option<String> {
    std::fs::read_to_string(cfg.root.join(&p.dir).join("README.md")).ok().filter(|r| !r.trim().is_empty())
}

/// Badge colours of `App\Enums\PluginStatus`.
fn status_color(status: Status) -> &'static str {
    match status {
        Status::NotInstalled => "gray",
        Status::Disabled => "warning",
        Status::Enabled => "success",
        Status::Errored | Status::Incompatible | Status::Crashed => "danger",
    }
}

/// Icons of `App\Enums\PluginStatus`.
fn status_icon(status: Status) -> &'static str {
    match status {
        Status::NotInstalled => "heart-off",
        Status::Disabled => "heart-x",
        Status::Enabled => "heart-check",
        Status::Errored => "heart-broken",
        Status::Incompatible => "heart-cancel",
        Status::Crashed => "heart-bolt",
    }
}

fn category_label(admin: &Admin, c: Category) -> String {
    admin.t(&format!("admin/plugin.category_enum.{}", c.as_str()))
}

fn tab_category(tab: &str) -> Option<Category> {
    match tab {
        "plugin" => Some(Category::Plugin),
        "theme" => Some(Category::Theme),
        "language" => Some(Category::Language),
        _ => None,
    }
}

/// `canEnable()`: Disabled or Crashed, and compatible with this panel version.
fn can_enable(cfg: &pelican_plugins::Config, p: &Plugin) -> bool {
    matches!(p.status, Status::Disabled | Status::Crashed) && compat::is_compatible(p, cfg.panel_version.as_deref())
}

fn views_escaped(s: &str) -> String {
    crate::views::escape(s)
}

/// `{plugin}` route binding: find after a re-scan, 404 when gone.
fn bound(cfg: &pelican_plugins::Config, id: &str) -> Result<Plugin, WebError> {
    let id = id.trim().to_lowercase();
    cfg.find(&id).map_err(|e| match e {
        pelican_plugins::Error::NotFound(_) => WebError::NotFound,
        other => WebError::internal(other.to_string()),
    })
}

/// `GET /admin/plugins` — ListPlugins: safe-mode banner, tabs with counts,
/// sortable/searchable table and per-row actions.
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList plugin")?;
    let cfg = config(&admin);
    // PluginService::loadPlugins() runs on every list request in the original.
    if !cfg.safe_mode {
        cfg.refresh_statuses();
    }

    let opts = [10, 25, 50, 0];
    let mut p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    p.tab = Some(p.tab.clone().unwrap_or_else(|| "all".into()));

    let mut all = cfg.plugins();
    // Default sort load_order; ties keep the scan order (the Sushi row order).
    all.sort_by_key(|pl| pl.load_order);

    // Counts per tab before search/sort/restrictions.
    let tab_counts: [usize; 3] = [
        all.iter().filter(|pl| pl.category == Category::Plugin).count(),
        all.iter().filter(|pl| pl.category == Category::Theme).count(),
        all.iter().filter(|pl| pl.category == Category::Language).count(),
    ];
    let total_all = all.len();

    if let Some(cat) = p.tab.as_deref().and_then(tab_category) {
        all.retain(|pl| pl.category == cat);
    }
    if !p.search.is_empty() {
        // The only searchable column is the name.
        let q = p.search.to_lowercase();
        all.retain(|pl| pl.name.to_lowercase().contains(&q));
    }
    match p.sort.as_deref() {
        Some("name") if p.desc => all.sort_by(|a, b| b.name.cmp(&a.name)),
        Some("name") => all.sort_by(|a, b| a.name.cmp(&b.name)),
        Some("author") if p.desc => all.sort_by(|a, b| b.author.cmp(&a.author)),
        Some("author") => all.sort_by(|a, b| a.author.cmp(&b.author)),
        Some("version") if p.desc => all.sort_by(|a, b| b.version.cmp(&a.version)),
        Some("version") => all.sort_by(|a, b| a.version.cmp(&b.version)),
        Some("category") if p.desc => all.sort_by_key(|b| std::cmp::Reverse(b.category.as_str())),
        Some("category") => all.sort_by_key(|b| b.category.as_str()),
        Some("status") if p.desc => all.sort_by_key(|b| std::cmp::Reverse(b.status.as_str())),
        Some("status") => all.sort_by_key(|b| b.status.as_str()),
        _ => {}
    }

    let total = all.len() as u64;
    let page: Vec<&Plugin> = match p.limit_offset() {
        Some((l, o)) => all.iter().skip(o as usize).take(l as usize).collect(),
        None => all.iter().collect(),
    };

    let t = |k: &str| admin.t(&format!("admin/plugin.{k}"));
    let can_update = admin.can("update plugin");
    let can_delete = admin.can("delete plugin");
    let show_category = p.tab.as_deref() == Some("all");

    let mut table = Table::new(SLUG, "/admin/plugins");
    table.columns = vec![
        Column::new("name", t("name")).sortable(),
        Column::new("author", t("author")).sortable().wide_only(),
        Column::new("version", t("version")).sortable(),
    ];
    if show_category {
        table.columns.push(Column::new("category", t("category")).sortable());
    }
    table.columns.push(Column::new("status", t("status")).sortable());

    for pl in page {
        let update_available = updates::update_available(&cfg, pl).await;
        let readme = readme_of(&cfg, pl);
        // Name column: description truncated to 80 characters and the versions
        // icon (green, or red with a tooltip when an update is available).
        let icon = if update_available {
            format!(
                r#"<span class="text-danger" title="{}">{}</span> "#,
                views_escaped(&t("update_available")),
                crate::views::icon_html("versions-off", "icon-sm")
            )
        } else {
            format!(r#"<span class="text-success">{}</span> "#, crate::views::icon_html("versions", "icon-sm"))
        };
        let name_cell = Cell::Html {
            html: format!(
                r#"<div><span data-plugin-row="{id}" style="display:inline-flex;align-items:center;gap:.25rem">{icon}<span>{name}</span></span>{desc}</div>"#,
                id = views_escaped(&pl.id),
                desc = truncated(pl.description.as_deref())
                    .map(|d| format!(r#"<div class="desc">{}</div>"#, views_escaped(&d)))
                    .unwrap_or_default(),
                name = views_escaped(&pl.name),
            ),
        };
        // ⚠ plugin.json does not declare an api_version, assuming 1.
        let warn = (pl.api_version.is_none()).then(|| t("api_version_missing"));
        let status_tooltip: String =
            [pl.status_message.clone(), warn].into_iter().flatten().collect::<Vec<_>>().join("\n");

        let mut row = Row::new(pl.id.clone()).cell("name", name_cell).cell("author", Cell::text(pl.author.clone()));
        if let Some(v) = &pl.version {
            row = row.cell("version", Cell::text(v.clone()));
        }
        if show_category {
            row = row.cell("category", Cell::badge(category_label(&admin, pl.category), "primary"));
        }
        row = row.cell(
            "status",
            Cell::Badge {
                text: t(&format!("status_enum.{}", pl.status.as_str())),
                color: status_color(pl.status).into(),
                icon: Some(status_icon(pl.status).into()),
                tooltip: (!status_tooltip.is_empty()).then_some(status_tooltip),
                copy: None,
            },
        );

        // View action: README slide-over link (or the plugin's website when
        // there is no README); the icon picks eye / eye-share accordingly.
        if readme.is_some() || pl.url.is_some() {
            let (url, icon_name) = match (&readme, pl.url.as_deref()) {
                (Some(_), _) => (format!("/admin/plugins/{}/readme", pl.id), "eye"),
                (None, Some(url)) => (url.to_string(), "eye-share"),
                _ => continue,
            };
            row.actions.push(Action::link(admin.t("filament-actions::view.single.label"), icon_name, url));
        }

        // Settings (HasPluginSettings): enabled WASM plugins that declare settings.
        let has_settings = pl.status == Status::Enabled
            && pelican_plugins::runtime::global()
                .and_then(|rt| rt.get(&pl.id))
                .is_some_and(|l| l.registration.settings);
        if can_update && has_settings {
            row.actions.push(Action::link(t("settings"), "settings", format!("/admin/plugins/{}/settings", pl.id)));
        }

        if can_update && pl.status == Status::NotInstalled {
            row.group.push(
                Action::post(t("install"), "terminal-2", format!("/admin/plugins/{}/action", pl.id))
                    .field("op", "install")
                    .color("success"),
            );
        }
        if can_update && update_available {
            row.group.push(
                Action::post(t("update"), "download", format!("/admin/plugins/{}/action", pl.id))
                    .field("op", "update")
                    .color("success"),
            );
        }
        if can_update && can_enable(&cfg, pl) {
            let mut a = Action::post(t("enable"), "check", format!("/admin/plugins/{}/action", pl.id))
                .field("op", "enable")
                .color("success");
            // Enabling a theme while another one is already enabled asks first.
            if pl.is_theme() && lifecycle::has_theme_enabled(&cfg) {
                a = a.confirm(t("enable_theme_modal.description"));
            }
            row.group.push(a);
        }
        if can_update && matches!(pl.status, Status::Enabled | Status::Incompatible) {
            row.group.push(
                Action::post(t("disable"), "x-mark", format!("/admin/plugins/{}/action", pl.id))
                    .field("op", "disable")
                    .color("warning"),
            );
        }
        if can_delete && matches!(pl.status, Status::NotInstalled | Status::Errored) {
            row.group.push(
                Action::post(
                    admin.t("filament-actions::delete.single.label"),
                    "trash",
                    format!("/admin/plugins/{}/action", pl.id),
                )
                .field("op", "delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
            );
        }
        if can_update && !matches!(pl.status, Status::NotInstalled | Status::Errored) {
            row.group.push(
                Action::post(t("uninstall"), "terminal-2", format!("/admin/plugins/{}/action", pl.id))
                    .field("op", "uninstall")
                    .color("danger")
                    .confirm(admin.t("filament-actions::modal.confirmation")),
            );
        }
        table.rows.push(row);
    }

    table.searchable = true;
    let tab = |key: &str, label: String, n: usize| Tab {
        key: key.into(),
        label,
        badge: Some(n as i64),
        url: String::new(),
        active: false,
    };
    table.tabs = vec![tab("all", admin.t("admin/plugin.all"), total_all)]
        .into_iter()
        .chain(
            [(Category::Plugin, tab_counts[0]), (Category::Theme, tab_counts[1]), (Category::Language, tab_counts[2])]
                .iter()
                .map(|c| tab(c.0.as_str(), category_label(&admin, c.0), c.1)),
        )
        .collect();
    table.empty_icon = "packages".into();
    table.empty_heading = t("no_plugins");
    table.finish(&p, total, &opts, &admin.locale());

    let back_url = table.current_url.clone();
    admin
        .page(
            SLUG,
            "admin/plugins/list.html",
            context! {
                title => t("model_label_plural"),
                table => Mj::from_serialize(&table),
                can_import => admin.can("create plugin"),
                import_from_file => t("import_from_file"),
                import_from_url => t("import_from_url"),
                file_label => t("file"),
                visit_website => t("visit_website"),
                close_label => admin.t("filament::components/modal.actions.close.label"),
                change_load_order => t("change_load_order"),
                apply_load_order => t("apply_load_order"),
                safe_mode => cfg.safe_mode,
                safe_mode_text => t("safe_mode_active"),
                back_url => back_url,
            },
        )
        .await
}

/// `reorderTable()` — `PluginService::updateLoadOrder($order)` for the full
/// sequence posted by drag-and-drop (`ids[]`).
async fn reorder(admin: Admin, form: Form) -> WebResult {
    admin.authorize("update plugin")?;
    let order: Vec<String> = form.list("ids").into_iter().filter(|id| !id.is_empty()).collect();
    if order.is_empty() {
        return Ok(admin.redirect("/admin/plugins"));
    }
    lifecycle::update_load_order(&config(&admin), &order).map_err(|_| WebError::Unprocessable)?;
    Ok(admin.redirect(&form.back("/admin/plugins")))
}

/// The row actions of `PluginResource`: every op answers with the notification
/// keys of the original and redirects back to the list.
async fn action(admin: Admin, AxPath(plugin): AxPath<String>, form: Form) -> WebResult<Response> {
    let cfg = config(&admin);
    if !cfg.safe_mode {
        cfg.refresh_statuses();
    }
    let back = form.back("/admin/plugins");
    let notify_err = |e: pelican_plugins::Error| e.to_string();
    let bad_state = || WebError::Unprocessable;

    match form.get("op") {
        "install" => {
            admin.authorize("update plugin")?;
            let p = bound(&cfg, &plugin)?;
            if p.status != Status::NotInstalled {
                return Err(bad_state());
            }
            // InstallPlugin: enable unless it is a theme and another theme is
            // already enabled.
            let enable = !p.is_theme() || !lifecycle::has_theme_enabled(&cfg);
            let done = match sidecar::delegate(&admin.st, &p, "install", json!({ "enable": enable })).await {
                Some(r) => r.map_err(pelican_plugins::Error::Message),
                None => pelican_api::application::plugins::lifecycle_hook(&cfg, &p, true)
                    .await
                    .map_err(pelican_plugins::Error::Message)
                    .and_then(|()| lifecycle::install(&cfg, &fresh(&cfg, &p), enable)),
            };
            match done {
                Ok(()) => admin.session.flash(
                    "success",
                    admin.t("admin/plugin.notifications.installed"),
                    Some(p.name.clone()),
                ),
                Err(e) => admin.flash_danger(admin.t("admin/plugin.notifications.install_error"), Some(notify_err(e))),
            }
        }
        "update" => {
            admin.authorize("update plugin")?;
            let p = bound(&cfg, &plugin)?;
            let updated = match sidecar::delegate(&admin.st, &p, "update", json!({})).await {
                Some(r) => r.map_err(pelican_plugins::Error::Message).map(|()| p.clone()),
                None => match lifecycle::update(&cfg, &fresh(&cfg, &p)).await {
                    Ok(u) if u.status != Status::NotInstalled => {
                        pelican_api::application::plugins::lifecycle_hook(&cfg, &u, true)
                            .await
                            .map(|()| u)
                            .map_err(pelican_plugins::Error::Message)
                    }
                    other => other,
                },
            };
            match updated {
                Ok(_) => {
                    admin.session.flash("success", admin.t("admin/plugin.notifications.updated"), Some(p.name.clone()))
                }
                Err(e) => admin.flash_danger(admin.t("admin/plugin.notifications.update_error"), Some(notify_err(e))),
            }
        }
        "enable" => {
            admin.authorize("update plugin")?;
            let p = fresh(&cfg, &bound(&cfg, &plugin)?);
            if !can_enable(&cfg, &p) {
                return Err(bad_state());
            }
            let done = match sidecar::delegate(&admin.st, &p, "enable", json!({})).await {
                Some(r) => r.map_err(pelican_plugins::Error::Message),
                None => lifecycle::enable(&cfg, &p),
            };
            match done {
                Ok(()) => admin.flash_success(admin.t("admin/plugin.notifications.enabled")),
                Err(e) => admin.flash_danger(notify_err(e), None),
            }
        }
        "disable" => {
            admin.authorize("update plugin")?;
            let p = fresh(&cfg, &bound(&cfg, &plugin)?);
            if !matches!(p.status, Status::Enabled | Status::Incompatible) {
                return Err(bad_state());
            }
            let done = match sidecar::delegate(&admin.st, &p, "disable", json!({})).await {
                Some(r) => r.map_err(pelican_plugins::Error::Message),
                None => lifecycle::disable(&cfg, &p),
            };
            match done {
                Ok(()) => admin.flash_success(admin.t("admin/plugin.notifications.disabled")),
                Err(e) => admin.flash_danger(notify_err(e), None),
            }
        }
        "delete" => {
            admin.authorize("delete plugin")?;
            let p = fresh(&cfg, &bound(&cfg, &plugin)?);
            if !matches!(p.status, Status::NotInstalled | Status::Errored) {
                return Err(bad_state());
            }
            match lifecycle::delete(&cfg, &p) {
                Ok(()) => admin.flash_success(admin.t("admin/plugin.notifications.deleted")),
                Err(e) => admin.flash_danger(notify_err(e), None),
            }
        }
        "uninstall" => {
            admin.authorize("update plugin")?;
            let p = fresh(&cfg, &bound(&cfg, &plugin)?);
            if matches!(p.status, Status::NotInstalled | Status::Errored) {
                return Err(bad_state());
            }
            let done = match sidecar::delegate(&admin.st, &p, "uninstall", json!({ "delete": false })).await {
                Some(r) => r.map_err(pelican_plugins::Error::Message),
                None => pelican_api::application::plugins::lifecycle_hook(&cfg, &p, false)
                    .await
                    .map_err(pelican_plugins::Error::Message)
                    .and_then(|()| lifecycle::uninstall(&cfg, &p, false)),
            };
            match done {
                Ok(()) => admin.session.flash(
                    "success",
                    admin.t("admin/plugin.notifications.uninstalled"),
                    Some(p.name.clone()),
                ),
                Err(e) => {
                    admin.flash_danger(admin.t("admin/plugin.notifications.uninstall_error"), Some(notify_err(e)))
                }
            }
        }
        _ => return Err(WebError::NotFound),
    }
    // Enable/disable/install/... take effect on the next request in the original; the
    // runtime reloads in the background.
    pelican_plugins::runtime::reload_global(&cfg);
    Ok(admin.redirect(&back))
}

/// Re-scan the manifest before acting on it (`Plugin::refreshRows()`).
fn fresh(cfg: &pelican_plugins::Config, p: &Plugin) -> Plugin {
    cfg.find(&p.id).unwrap_or_else(|_| p.clone())
}

/// `GET …/readme` — JSON for the slide-over: README html, title and website URL.
async fn readme(admin: Admin, AxPath(plugin): AxPath<String>) -> WebResult<Response> {
    admin.authorize("viewList plugin")?;
    let cfg = config(&admin);
    let p = bound(&cfg, &plugin)?;
    // The original renders the markdown through TextEntry->markdown(); until a
    // renderer lands in the workspace we show the raw file, escaped.
    let body = readme_of(&cfg, &p)
        .map(|md| format!(r#"<pre class="code-block">{}</pre>"#, crate::views::escape(md.trim_end())))
        .unwrap_or_default();
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(json!({
            "ok": true,
            "title": p.name,
            "html": body,
            "url": p.url,
        })),
    )
        .into_response())
}

// ----- Import (header actions) ------------------------------------------------

/// Largest upload body we buffer (the service enforces
/// PANEL_PLUGIN_MAX_IMPORT_SIZE itself, default 100 MB).
const MAX_UPLOAD: usize = 512 * 1024 * 1024;

/// One multipart part of the submitted form.
struct Part {
    name: String,
    filename: Option<String>,
    data: Vec<u8>,
}

/// The `boundary=…` parameter of a `multipart/form-data` header.
fn boundary(ct: &str) -> Option<&str> {
    ct.split(';').filter_map(|p| p.trim().strip_prefix("boundary=")).next().map(|b| b.trim_matches('"'))
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len().max(1)).position(|w| w.starts_with(needle))
}

/// The quoted value after `key=` of a Content-Disposition line.
fn part_param(disp: &str, key: &str) -> Option<String> {
    let idx = disp.find(key)? + key.len();
    let tail = disp[idx..].trim_start();
    let tail = tail.strip_prefix('"').unwrap_or(tail);
    let len = tail.find(['"', ';', '\r']).unwrap_or(tail.len());
    Some(tail[..len].to_string())
}

/// One `--boundary` section (headers + blank line + data); mirrors
/// `server/schedules.rs` so browser bodies parse the same way.
fn parse_part(chunk: &[u8]) -> Option<Part> {
    let chunk = chunk.strip_prefix(b"\r\n").unwrap_or(chunk);
    if chunk.starts_with(b"--") || chunk.is_empty() {
        return None;
    }
    let headers_end = find_bytes(chunk, b"\r\n\r\n")?;
    let mut data = chunk[headers_end + 4..].to_vec();
    while data.last() == Some(&b'\n') || data.last() == Some(&b'\r') {
        data.pop();
    }
    let headers = String::from_utf8_lossy(&chunk[..headers_end]).to_string();
    let disp = headers.lines().find(|l| l.starts_with("Content-Disposition:")).unwrap_or("");
    Some(Part {
        name: part_param(disp, "name=").unwrap_or_default(),
        filename: part_param(disp, "filename=").filter(|f| !f.is_empty()),
        data,
    })
}

/// Extract the parts of a multipart body.
fn multipart_parts(boundary: &str, body: &[u8]) -> Vec<Part> {
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
    out
}

/// After an import over an installed plugin: reinstall without changing its
/// enabled/disabled state (`reinstall_if_installed()` of the API controller).
async fn reinstall_if_installed(cfg: &pelican_plugins::Config, id: String) -> Result<String, String> {
    if lifecycle::is_installed(cfg, &id).map_err(|e| e.to_string())? {
        let p = cfg.find(&id).map_err(|e| e.to_string())?;
        pelican_api::application::plugins::lifecycle_hook(cfg, &p, true).await?;
        lifecycle::install(cfg, &p, false).map_err(|e| e.to_string())?;
    }
    pelican_plugins::runtime::reload_global(cfg);
    Ok(id)
}

/// `POST /admin/plugins/import/file` — Import from File: the zip goes into
/// `downloadPluginFromFile`; importing over an installed plugin reinstalls it
/// (`InstallPlugin` with `reinstall: true` = install without enabling).
async fn import_file(admin: Admin, req: Request) -> WebResult<Response> {
    admin.authorize("create plugin")?;
    let cfg = config(&admin);
    if !cfg.safe_mode {
        cfg.refresh_statuses();
    }
    let ct = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes = to_bytes(req.into_body(), MAX_UPLOAD).await.map_err(|_| WebError::PayloadTooLarge)?;
    let back = multipart_parts(boundary(&ct).unwrap_or(""), &bytes)
        .iter()
        .find(|part| part.name == "_back" && part.filename.is_none())
        .map(|part| String::from_utf8_lossy(&part.data).trim().to_string())
        .filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains('\\'))
        .unwrap_or_else(|| "/admin/plugins".into());

    let fail = |msg: Option<String>| -> WebResult<Response> {
        admin.flash_danger(admin.t("admin/plugin.notifications.import_failed"), msg);
        Ok(admin.redirect(&back))
    };
    let Some(bound) = boundary(&ct) else { return fail(None) };
    let Some(file) =
        multipart_parts(bound, &bytes).into_iter().find(|p| p.filename.as_deref().is_some_and(|f| !f.is_empty()))
    else {
        return fail(None);
    };
    // FileUpload's acceptedFileTypes validates against application/zip (+ the
    // compressed variants); finfo sees real zips through their PK signature.
    if !file.data.starts_with(b"PK") {
        return fail(None);
    }
    let tmp: PathBuf = std::env::temp_dir().join(format!("pelican-import-{}.zip", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &file.data).map_err(|e| WebError::internal(e.to_string()))?;
    let imported = lifecycle::import_zip(&cfg, Path::new(&tmp), None).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    let imported = match imported {
        Ok(id) => reinstall_if_installed(&cfg, id).await,
        Err(e) => Err(e),
    };
    match imported {
        Ok(_) => {
            admin.flash_success(admin.t("admin/plugin.notifications.imported"));
            Ok(admin.redirect(&back))
        }
        Err(msg) => fail(Some(msg)),
    }
}

/// `POST /admin/plugins/import/url` — Import from URL
/// (`required|url|ends_with:.zip`; no Hub checksum on manual imports).
async fn import_url(admin: Admin, form: Form) -> WebResult<Response> {
    admin.authorize("create plugin")?;
    let cfg = config(&admin);
    if !cfg.safe_mode {
        cfg.refresh_statuses();
    }
    let back = form.back("/admin/plugins");
    let fail = |msg: Option<String>| -> WebResult<Response> {
        admin.flash_danger(admin.t("admin/plugin.notifications.import_failed"), msg);
        Ok(admin.redirect(&back))
    };
    let url = form.opt("url").unwrap_or("").trim().to_string();
    if url.is_empty() {
        return fail(None);
    }
    let ends_with_zip = reqwest::Url::parse(&url)
        .map(|u| u.path().ends_with(".zip") && matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false);
    if !ends_with_zip {
        return fail(None);
    }
    let imported = match lifecycle::import_url(&cfg, &url, None, None).await.map_err(|e| e.to_string()) {
        Ok(id) => reinstall_if_installed(&cfg, id).await,
        Err(e) => Err(e),
    };
    match imported {
        Ok(_) => {
            admin.flash_success(admin.t("admin/plugin.notifications.imported"));
            Ok(admin.redirect(&back))
        }
        Err(msg) => fail(Some(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str =
        r#"{"id":"Demo","name":"Demo","author":"Me","category":"theme","namespace":"Me\\Demo","class":"DemoPlugin"}"#;

    /// A temporary plugins directory, removed with the test.
    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> TmpDir {
            let d = std::env::temp_dir().join(format!("pelican-web-plugins-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).expect("mkdir");
            TmpDir(d)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn truncates_descriptions_at_eighty_chars() {
        assert_eq!(truncated(Some("short")).as_deref(), Some("short"));
        assert_eq!(truncated(None), None);
        assert_eq!(truncated(Some(&"x".repeat(81))), Some(format!("{}...", "x".repeat(80))));
        // Multibyte boundaries do not panic.
        assert!(truncated(Some(&"ä".repeat(90))).is_some());
    }

    #[test]
    fn readmes_come_from_the_plugin_directory() {
        let tmp = TmpDir::new("readme");
        let c = test_cfg(tmp.0.clone());
        let dir = tmp.0.join("demo");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("plugin.json"), MANIFEST).expect("manifest");
        assert_eq!(readme_of(&c, &c.find("demo").expect("row")), None);
        std::fs::write(dir.join("README.md"), "# Hello").expect("readme");
        assert_eq!(readme_of(&c, &c.find("demo").expect("row")).as_deref(), Some("# Hello"));
    }

    fn test_cfg(root: PathBuf) -> pelican_plugins::Config {
        pelican_plugins::Config {
            root,
            dev_mode: false,
            safe_mode: false,
            max_import_size: 1024 * 1024,
            hub_url: "https://hub.pelican.dev".into(),
            hub_api_key: None,
            panel_version: None,
            panel_version_display: "canary".into(),
            production: true,
            translate: std::sync::Arc::new(pelican_plugins::english),
        }
    }

    /// A browser-shaped multipart body parses into named parts (import from file).
    #[test]
    fn multipart_parsing() {
        let body = b"--B\r\nContent-Disposition: form-data; name=\"_back\"\r\n\r\n/admin/plugins?tab=all\r\n--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"demo.zip\"\r\nContent-Type: application/zip\r\n\r\nPK\x03\x04<bin>\r\n--B--\r\n";
        let parts = multipart_parts("B", body);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "_back");
        assert_eq!(String::from_utf8_lossy(&parts[0].data), "/admin/plugins?tab=all");
        assert_eq!(parts[1].filename.as_deref(), Some("demo.zip"));
        assert_eq!(&parts[1].data[..2], b"PK");
        assert!(parts[1].data.ends_with(b"<bin>"));
        assert_eq!(boundary("multipart/form-data; boundary=\"quoted\""), Some("quoted"));
        assert_eq!(boundary("application/x-www-form-urlencoded"), None);
    }

    /// Only content with the zip signature passes the accepted-file check.
    #[test]
    fn zip_detection_needs_the_pk_header() {
        let body =
            b"--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\n\r\nnot a zip\r\n--B--\r\n";
        let part = &multipart_parts("B", body)[0];
        assert!(!part.data.starts_with(b"PK"));
    }

    /// The list template renders with representative state (tabs, cells,
    /// banners, dialogs) without template errors.
    #[test]
    fn list_template_renders() {
        let mut table = Table::new(SLUG, "/admin/plugins");
        table.columns = vec![
            Column::new("name", "Name").sortable(),
            Column::new("author", "Author").sortable().wide_only(),
            Column::new("version", "Version").sortable(),
            Column::new("category", "Category").sortable(),
            Column::new("status", "Status").sortable(),
        ];
        let mut row = Row::new("demo").cell(
            "name",
            Cell::Html {
                html:
                    r#"<div><span data-plugin-row="demo"><span>x</span>Demo</span><div class="desc">desc</div></div>"#
                        .into(),
            },
        );
        row = row.cell("author", Cell::text(String::from("Me"))).cell("version", Cell::text(String::from("1.0.0")));
        row = row.cell("category", Cell::badge(String::from("Theme"), "primary"));
        row = row.cell(
            "status",
            Cell::Badge {
                text: "Enabled".into(),
                color: "success".into(),
                icon: Some("heart-check".into()),
                tooltip: None,
                copy: None,
            },
        );
        row.actions.push(Action::link("View", "eye", "/admin/plugins/demo/readme"));
        row.group.push(
            Action::post("Disable", "x-mark", "/admin/plugins/demo/action").field("op", "disable").color("warning"),
        );
        table.rows.push(row);
        table.searchable = true;
        table.tabs = vec![
            Tab {
                key: "all".into(),
                label: "All".into(),
                badge: Some(1),
                url: "/admin/plugins?tab=all".into(),
                active: true,
            },
            Tab {
                key: "theme".into(),
                label: "Theme".into(),
                badge: Some(1),
                url: "/admin/plugins?tab=theme".into(),
                active: false,
            },
        ];
        table.finish(&ListParams::parse(Some("tab=all"), &[10, 25, 0], 10), 1, &[10, 25, 0], "en");
        let ctx = context! {
            title => "Plugins",
            table => Mj::from_serialize(&table),
            can_import => true,
            import_from_file => "Import from File",
            import_from_url => "Import from URL",
            file_label => "File",
            visit_website => "Visit Website",
            close_label => "Close",
            change_load_order => "Change load order",
            apply_load_order => "Apply load order",
            safe_mode => true,
            safe_mode_text => "Safe mode is on.",
            back_url => "/admin/plugins?tab=all",
        };
        crate::views::render("admin/plugins/list.html", ctx).expect("template renders");
    }
}

/// The plugin's settings form (`HasPluginSettings`), prefilled from `settings-data`.
async fn settings_page(
    admin: &Admin,
    plugin: &str,
    values: serde_json::Value,
    errors: crate::forms::Errors,
) -> WebResult {
    let rt = pelican_plugins::runtime::global().ok_or(WebError::NotFound)?;
    let loaded = rt.get(plugin).filter(|l| l.registration.settings).ok_or(WebError::NotFound)?;
    let fields = rt.settings_form(plugin).await.map_err(|e| WebError::internal(e.to_string()))?;
    let locale = admin.locale();
    let fields: Vec<minijinja::Value> = fields
        .iter()
        .map(|fd| {
            minijinja::context! {
                name => fd.name, kind => fd.kind, required => fd.required,
                label => crate::plugin_pages::label(&admin.st, plugin, &locale, &fd.label),
                help => fd.help.as_ref().map(|h| crate::plugin_pages::label(&admin.st, plugin, &locale, h)),
                options => fd.options.iter().map(|(v, l)| vec![v.clone(), l.clone()]).collect::<Vec<_>>(),
            }
        })
        .collect();
    let name = config(admin).find(&loaded.id).map(|p| p.name).unwrap_or_else(|_| loaded.id.clone());
    admin
        .page(
            "plugins",
            "admin/plugins/settings.html",
            minijinja::context! {
                title => format!("{name} — {}", admin.t("admin/plugin.settings")),
                plugin => plugin,
                fields => fields,
                values => minijinja::Value::from_serialize(&values),
                errors => minijinja::Value::from_serialize(&errors),
            },
        )
        .await
}

/// GET /admin/plugins/{plugin}/settings.
async fn settings_form(admin: Admin, AxPath(plugin): AxPath<String>) -> WebResult {
    admin.authorize("update plugin")?;
    let rt = pelican_plugins::runtime::global().ok_or(WebError::NotFound)?;
    let data = rt.settings_data(&plugin).await.map_err(|_| WebError::NotFound)?;
    let values = serde_json::from_str(&data).unwrap_or_else(|_| serde_json::json!({}));
    settings_page(&admin, &plugin, values, crate::forms::Errors::default()).await
}

/// POST /admin/plugins/{plugin}/settings: `saveSettings($data)`; the plugin's error
/// message is shown on the form.
async fn settings_save(admin: Admin, AxPath(plugin): AxPath<String>, form: Form) -> WebResult {
    admin.authorize("update plugin")?;
    let rt = pelican_plugins::runtime::global().ok_or(WebError::NotFound)?;
    let fields = rt.settings_form(&plugin).await.map_err(|_| WebError::NotFound)?;
    let mut data = serde_json::Map::new();
    for fd in &fields {
        let v = match fd.kind.as_str() {
            "toggle" => serde_json::Value::Bool(form.bool(&fd.name)),
            "number" => form
                .opt(&fd.name)
                .and_then(|n| n.trim().parse::<f64>().ok())
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::from(form.get(&fd.name)),
        };
        data.insert(fd.name.clone(), v);
    }
    let data = serde_json::Value::Object(data);
    match rt.save_settings(&plugin, &data.to_string()).await {
        Ok(Ok(())) => {
            admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
            Ok(admin.redirect(&format!("/admin/plugins/{plugin}/settings")))
        }
        Ok(Err(msg)) => {
            admin.flash_danger(msg, None);
            settings_page(&admin, &plugin, data, crate::forms::Errors::default()).await
        }
        Err(e) => Err(WebError::internal(e.to_string())),
    }
}
