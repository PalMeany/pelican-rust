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
mod roles;
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
        .merge(roles::routes())
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
        let mut server = Vec::new();
        if self.can("viewList node") {
            server.push(item("nodes", t("admin/node.nav_title"), "server-2", count(st, "nodes").await));
        }
        if self.can("viewList server") {
            server.push(item("servers", t("admin/server.nav_title"), "brand-docker", count(st, "servers").await));
        }
        if self.can("viewList egg") {
            server.push(item("eggs", t("admin/egg.nav_title"), "eggs", count(st, "eggs").await));
        }
        let mut user = Vec::new();
        if self.can("viewList user") {
            user.push(item("users", t("admin/user.nav_title"), "users", count(st, "users").await));
        }
        if self.can("viewList role") {
            user.push(item("roles", t("admin/role.nav_title"), "users-group", count(st, "roles").await));
        }
        let mut advanced = Vec::new();
        if self.can("viewList apiKey") {
            advanced.push(item("api-keys", t("admin/apikey.nav_title"), "key", None));
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
        if self.can("viewList mount") {
            advanced.push(item("mounts", t("admin/mount.nav_title"), "layers-linked", count(st, "mounts").await));
        }
        if self.can("view panelLog") {
            advanced.push(item("logs", t("admin/log.navigation.panel_logs"), "file-info", None));
        }
        if self.can("viewList plugin") {
            advanced.push(item("plugins", t("admin/plugin.nav_title"), "packages", None));
        }
        if self.can("viewList webhook") {
            advanced.push(item(
                "webhooks",
                t("admin/webhook.nav_title"),
                "webhook",
                count(st, "webhook_configurations").await,
            ));
        }
        let mut groups = vec![NavGroup { label: None, items: top }];
        for (label, items) in [
            (t("admin/dashboard.server"), server),
            (t("admin/dashboard.user"), user),
            (t("admin/dashboard.advanced"), advanced),
        ] {
            if !items.is_empty() {
                groups.push(NavGroup { label: Some(label), items });
            }
        }
        groups
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
            context! { ..ctx, ..context! { nav => Value::from_serialize(&nav), user_menu => user_menu, home_url => "/admin" } },
        )
    }
}

pub use crate::table::{bulk_delete_notify, bulk_ids};
