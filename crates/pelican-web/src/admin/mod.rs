//! Admin panel (`/admin`, app/Filament/Admin): access check, navigation,
//! resources. Permissions follow the spatie names (`viewList mount`, …).

mod api_keys;
mod backup_hosts;
mod dashboard;
mod database_hosts;
mod eggs;
mod health;
mod logs;
mod mounts;
mod nodes;
mod plugins;
mod roles;
mod search;
mod server_relations;
mod server_transfer;
mod servers;
mod servers_create;
mod settings;
mod users;
mod webhooks;

use axum::Router;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use minijinja::{Value, context};
use pelican_core::state::AppState;
use pelican_db::raw;
use sea_orm::FromQueryResult;
use serde::Serialize;

use crate::ctx::Authed;
use crate::error::{WebError, WebResult};

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(api_keys::routes())
        .merge(backup_hosts::routes())
        .merge(dashboard::routes())
        .merge(database_hosts::routes())
        .merge(eggs::routes())
        .merge(health::routes())
        .merge(logs::routes())
        .merge(mounts::routes())
        .merge(nodes::routes())
        .merge(plugins::routes())
        .merge(roles::routes())
        .merge(search::routes())
        .merge(servers_create::routes())
        .merge(servers::routes())
        .merge(settings::routes())
        .merge(server_relations::routes())
        .merge(server_transfer::routes())
        .merge(users::routes())
        .merge(webhooks::routes())
}

/// A request to the admin panel (`canAccessPanel(admin)` = `isAdmin()`).
pub struct Admin(pub Authed);

impl std::ops::Deref for Admin {
    type Target = Authed;
    fn deref(&self) -> &Authed {
        &self.0
    }
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let authed = Authed::from_request_parts(parts, st).await?;
        if !authed.user.is_admin() {
            return Err(WebError::Forbidden.into_response());
        }
        Ok(Admin(authed))
    }
}

#[derive(Serialize)]
pub struct NavItem {
    pub label: String,
    pub url: String,
    pub icon: &'static str,
    pub active: bool,
    pub badge: Option<String>,
    /// `getNavigationBadgeColor()` / `getNavigationBadgeTooltip()`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge_color: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge_tooltip: Option<String>,
}

#[derive(Serialize)]
pub struct NavGroup {
    pub label: Option<String>,
    pub items: Vec<NavItem>,
}

#[derive(FromQueryResult)]
struct Count {
    aggregate: pelican_db::Int,
}

/// The relation managers PHP plugins add to an admin resource, for one record: the bridge's
/// `/admin/rs-relations` page cut out for an edit page, and the sidecar's cookies.
pub async fn plugin_relations(
    admin: &Admin,
    headers: &axum::http::HeaderMap,
    resource: &str,
    record: i64,
) -> (Option<crate::sidecar_proxy::Transplant>, Vec<axum::http::HeaderValue>) {
    if !pelican_api::sidecar::manifest().is_some_and(|m| m.admin_relations.get(resource).is_some_and(|n| *n > 0)) {
        return (None, Vec::new());
    }
    match crate::sidecar_proxy::embedded(
        admin,
        headers,
        &format!("/admin/rs-relations?resource={resource}&record={record}"),
    )
    .await
    {
        Some((t, c)) => (Some(t), c),
        None => (None, Vec::new()),
    }
}

/// A plugin relation manager's tab on an edit page (`data-rs-relation` of the bridge's
/// relations page): it comes first among the record's relation tabs, as in the original.
#[derive(serde::Deserialize, Serialize)]
pub struct PluginRelationTab {
    title: String,
    icon: Option<String>,
    badge: Option<serde_json::Value>,
    badge_color: Option<String>,
}

pub fn plugin_relation_tabs(main: &str) -> Vec<PluginRelationTab> {
    use base64::Engine;
    const ATTR: &str = "data-rs-relation=\"";
    let mut out = Vec::new();
    let mut rest = main;
    while let Some(i) = rest.find(ATTR) {
        rest = &rest[i + ATTR.len()..];
        let Some(end) = rest.find('"') else { break };
        if let Some(tab) = base64::engine::general_purpose::STANDARD
            .decode(&rest[..end])
            .ok()
            .and_then(|b| serde_json::from_slice::<PluginRelationTab>(&b).ok())
        {
            out.push(tab);
        }
        rest = &rest[end..];
    }
    out
}

/// `ApiKeyResource::getNavigationBadge()`: application keys, hidden at zero.
async fn application_key_count(st: &AppState) -> Option<String> {
    let b = st.db.get_database_backend();
    let sql = "select count(*) as {aggregate} from {api_keys} where {key_type} = ?";
    let n = Count::find_by_statement(raw::stmt(b, sql, vec![2.into()])).one(&st.db).await.ok().flatten()?.aggregate.0;
    (n > 0).then(|| n.to_string())
}

/// Lay a menu out as the original panel builds it for this user (`layout` from the PHP sidecar,
/// covering its plugins' items too): its groups and items in its order, its badges where ours
/// have none. Items it does not list keep their groups, after the rest.
pub fn arrange(groups: Vec<NavGroup>, layout: &[pelican_api::sidecar::LayoutGroup]) -> Vec<NavGroup> {
    if layout.is_empty() {
        return groups;
    }
    let path =
        |url: &str| crate::server::canonical_path(url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/'));
    let mut pool: Vec<(Option<String>, Option<NavItem>)> =
        groups.into_iter().flat_map(|g| g.items.into_iter().map(move |i| (g.label.clone(), Some(i)))).collect();
    let mut out: Vec<NavGroup> = Vec::new();
    for lg in layout {
        let mut items = Vec::new();
        for li in &lg.items {
            let want = path(&li.path);
            let found = pool.iter_mut().find(|(_, i)| i.as_ref().is_some_and(|i| path(&i.url) == want));
            if let Some(mut item) = found.and_then(|(_, i)| i.take()) {
                if item.badge.is_none() {
                    item.badge = match &li.badge {
                        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
                        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
                        _ => None,
                    };
                    item.badge_color = li.badge_color.as_deref().and_then(|c| {
                        ["danger", "warning", "success", "info", "primary", "gray"].into_iter().find(|k| *k == c)
                    });
                }
                items.push(item);
            }
        }
        if !items.is_empty() {
            out.push(NavGroup { label: lg.label.clone().filter(|l| !l.is_empty()), items });
        }
    }
    for (label, item) in pool {
        let Some(item) = item else { continue };
        match out.iter_mut().find(|g| g.label == label) {
            Some(g) => g.items.push(item),
            None => out.push(NavGroup { label, items: vec![item] }),
        }
    }
    out
}

async fn count(st: &AppState, table: &str) -> Option<String> {
    let b = st.db.get_database_backend();
    let sql = format!("select count(*) as {{aggregate}} from {{{table}}}");
    let n = Count::find_by_statement(raw::stmt(b, &sql, vec![])).one(&st.db).await.ok().flatten()?.aggregate.0;
    (n > 0).then(|| n.to_string())
}

impl Admin {
    /// Gate check: root admin or the spatie permission (`DefaultAdminPolicies`).
    pub fn can(&self, ability: &str) -> bool {
        self.user.gate_before(ability)
    }

    pub fn authorize(&self, ability: &str) -> Result<(), WebError> {
        if self.can(ability) { Ok(()) } else { Err(WebError::Forbidden) }
    }

    /// Sidebar of the admin panel; `active` is the resource slug.
    pub async fn nav(&self, active: &str) -> Vec<NavGroup> {
        let st = &self.st;
        let t = |k: &str| self.t(k);
        let item = |slug: &str, label: String, icon: &'static str, badge: Option<String>| NavItem {
            label,
            url: if slug.is_empty() { "/admin".into() } else { format!("/admin/{slug}") },
            icon,
            active: slug == active,
            badge,
            badge_color: None,
            badge_tooltip: None,
        };
        let mut top = Vec::new();
        top.push(item("", t("filament-panels::pages/dashboard.title"), "layout-dashboard", None));
        if self.can("view settings") {
            top.push(item("settings", t("admin/setting.title"), "settings", None));
        }
        // The original's order (resources discovered by name, pages first within a group).
        if self.can("viewList plugin") {
            // getNavigationBadge(): the plugin count, hidden at zero.
            let n = pelican_api::application::plugins::config(st).plugins().len();
            top.push(item("plugins", t("admin/plugin.nav_title"), "packages", (n > 0).then(|| n.to_string())));
        }
        let mut server = Vec::new();
        if self.can("viewList egg") {
            server.push(item("eggs", t("admin/egg.nav_title"), "eggs", count(st, "eggs").await));
        }
        if self.can("viewList node") {
            server.push(item("nodes", t("admin/node.nav_title"), "server-2", count(st, "nodes").await));
        }
        if self.can("viewList server") {
            server.push(item("servers", t("admin/server.nav_title"), "brand-docker", count(st, "servers").await));
        }
        let mut user = Vec::new();
        if self.can("viewList role") {
            user.push(item("roles", t("admin/role.nav_title"), "users-group", count(st, "roles").await));
        }
        if self.can("viewList user") {
            user.push(item("users", t("admin/user.nav_title"), "users", count(st, "users").await));
        }
        let mut advanced = Vec::new();
        if self.can("view health") {
            // Health::getNavigationIcon/Badge/BadgeColor/BadgeTooltip from the stored results.
            let results = pelican_api::health::latest();
            let icon = match &results {
                None => "heart-question",
                Some(r) if r.contains_failing() => "heart-exclamation",
                Some(_) => "heart-check",
            };
            let mut it = item("health", t("admin/health.title"), icon, None);
            if let Some(r) = &results {
                let failed = r.failed();
                if !failed.is_empty() {
                    it.badge = Some(failed.len().to_string());
                    it.badge_color = Some("danger");
                    let names = failed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ");
                    it.badge_tooltip = Some(self.t_with("admin/health.checks.failed", &[("checks", &names)]));
                }
            }
            advanced.push(it);
        }
        if self.can("viewList apiKey") {
            // ApiKeyResource::getEloquentQuery(): application keys.
            advanced.push(item("api-keys", t("admin/apikey.nav_title"), "key", application_key_count(st).await));
        }
        if self.can("viewList backupHost") {
            advanced.push(item(
                "backup-hosts",
                crate::i18n::trans_choice(&self.locale(), "admin/backuphost.model_label", 2.0, &[]),
                "file-zip",
                count(st, "backup_hosts").await,
            ));
        }
        if self.can("viewList databaseHost") {
            advanced.push(item(
                "database-hosts",
                t("admin/databasehost.nav_title"),
                "database",
                count(st, "database_hosts").await,
            ));
        }
        if self.can("viewList mount") {
            advanced.push(item("mounts", t("admin/mount.nav_title"), "layers-linked", count(st, "mounts").await));
        }
        if self.can("viewList webhook") {
            advanced.push(item(
                "webhooks",
                t("admin/webhook.nav_title"),
                "webhook",
                count(st, "webhook_configurations").await,
            ));
        }
        if self.can("view panelLog") {
            advanced.push(item("logs", t("admin/log.navigation.panel_logs"), "file-info", None));
        }
        // Plugin pages (ADR-0018): into the named core group, else a group of their own.
        let mut custom: Vec<(String, Vec<NavItem>)> = Vec::new();
        for (plugin, n, page) in crate::plugin_pages::nav_items(pelican_plugins::runtime::Panel::Admin) {
            if page.as_ref().and_then(|p| p.permission.as_deref()).is_some_and(|perm| !self.can(perm)) {
                continue;
            }
            let entry = NavItem {
                label: crate::plugin_pages::label(st, &plugin, &self.locale(), &n.label),
                url: format!("/admin/plugins/{plugin}/page/{}", n.page),
                icon: crate::plugin_pages::icon(n.icon.as_deref()),
                active: active == format!("plugin:{plugin}:{}", n.page),
                badge: None,
                badge_color: None,
                badge_tooltip: None,
            };
            match n.group.as_deref().map(str::to_lowercase).as_deref() {
                None => top.push(entry),
                Some("server") => server.push(entry),
                Some("user") => user.push(entry),
                Some("advanced") => advanced.push(entry),
                Some(_) => {
                    let name =
                        crate::plugin_pages::label(st, &plugin, &self.locale(), n.group.as_deref().unwrap_or(""));
                    match custom.iter_mut().find(|(g, _)| *g == name) {
                        Some((_, items)) => items.push(entry),
                        None => custom.push((name, vec![entry])),
                    }
                }
            }
        }
        // PHP plugins' Filament pages, served by the sidecar (ADR-0020).
        for n in crate::sidecar_proxy::nav(st, "admin", self.user.id(), None).await {
            let entry = NavItem {
                url: n.path.clone(),
                icon: crate::sidecar_proxy::icon(n.icon.as_deref()),
                active: active == n.path || active.starts_with(&format!("{}/", n.path)),
                label: n.label.clone(),
                badge: None,
                badge_color: None,
                badge_tooltip: None,
            };
            let group = n.group.as_deref().map(str::to_lowercase);
            match group.as_deref() {
                None => top.push(entry),
                Some(g) if g == t("admin/dashboard.server").to_lowercase() => server.push(entry),
                Some(g) if g == t("admin/dashboard.user").to_lowercase() => user.push(entry),
                Some(g) if g == t("admin/dashboard.advanced").to_lowercase() => advanced.push(entry),
                Some(_) => {
                    let name = n.group.clone().unwrap_or_default();
                    match custom.iter_mut().find(|(g, _)| *g == name) {
                        Some((_, items)) => items.push(entry),
                        None => custom.push((name, vec![entry])),
                    }
                }
            }
        }
        if crate::models::nav_mode(st, &self.user.user) == "topbar" {
            // With top navigation Eggs, Nodes, Servers and Users have no group and Roles moves to
            // Advanced (`*Resource::getNavigationGroup()`); ungrouped resources follow the pages by name.
            let take = |v: &mut Vec<NavItem>, slug: &str| {
                let url = format!("/admin/{slug}");
                v.iter().position(|i| i.url == url).map(|p| v.remove(p))
            };
            let (eggs, nodes, servers) =
                (take(&mut server, "eggs"), take(&mut server, "nodes"), take(&mut server, "servers"));
            let (roles, users) = (take(&mut user, "roles"), take(&mut user, "users"));
            let at = top.iter().position(|i| i.url == "/admin/plugins").unwrap_or_else(|| {
                top.iter().rposition(|i| i.url == "/admin" || i.url == "/admin/settings").map_or(0, |p| p + 1)
            });
            let plugins = (top.get(at).is_some_and(|i| i.url == "/admin/plugins")).then(|| top.remove(at));
            let tail = top.split_off(at);
            top.extend([eggs, nodes, plugins, servers, users].into_iter().flatten());
            top.extend(tail);
            if let Some(roles) = roles {
                let at = advanced
                    .iter()
                    .position(|i| i.url == "/admin/webhooks" || i.url == "/admin/logs")
                    .unwrap_or(advanced.len());
                advanced.insert(at, roles);
            }
        }
        // Groups the panel does not register (plugins') sort with the first registered one.
        let mut groups = vec![NavGroup { label: None, items: top }];
        for (label, items) in std::iter::once((t("admin/dashboard.server"), server))
            .chain(custom)
            .chain([(t("admin/dashboard.user"), user), (t("admin/dashboard.advanced"), advanced)])
        {
            if !items.is_empty() {
                groups.push(NavGroup { label: Some(label), items });
            }
        }
        let layout = pelican_api::sidecar::page_extras(st, "admin", Some(self.user.id()), None).await;
        arrange(groups, &layout.layout)
    }

    /// API request context for calling Application API services as this user
    /// (activity actor/ip, `Server::saved` hooks).
    pub fn api_ctx(&self, bound: pelican_api::binding::Bound) -> pelican_api::pipeline::Ctx {
        pelican_api::pipeline::Ctx {
            ip: self.ip.parse().unwrap_or(std::net::IpAddr::from([127, 0, 0, 1])),
            user: Some(std::sync::Arc::new(self.user.clone())),
            bound: std::sync::Arc::new(bound),
            node: None,
            root: self.st.config.app_url.clone(),
            subject: None,
        }
    }

    /// Render an admin page with the sidebar and the "exit admin" menu item.
    pub async fn page(&self, active: &str, template: &str, ctx: Value) -> WebResult {
        let nav = self.nav(active).await;
        let user_menu = vec![context! { label => self.t("profile.exit_admin"), url => "/", icon => "arrow-back" }];
        self.render(
            template,
            context! {
                ..ctx,
                ..context! {
                    nav => Value::from_serialize(&nav), user_menu => user_menu, home_url => "/admin",
                    global_search => true, sidebar_collapsible => true,
                }
            },
        )
    }
}

pub use crate::table::{bulk_delete_notify, bulk_ids};
