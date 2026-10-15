//! Server panel (`/server/{uuid_short}`, app/Filament/Server): tenant access,
//! subuser permissions, navigation and the console.

mod activity;
mod backups;
mod console;
mod databases;
mod features;
mod files;
mod mounts;
mod network;
mod schedules;
mod settings;
mod startup;
mod users;
mod webhooks;

use std::collections::HashSet;

use axum::Router;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use minijinja::{Value, context};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, nodes, servers, subusers};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;

use crate::admin::{NavGroup, NavItem};
use crate::ctx::Authed;
use crate::error::{WebError, WebResult};

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(activity::routes())
        .merge(backups::routes())
        .merge(console::routes())
        .merge(databases::routes())
        .merge(features::routes())
        .merge(files::routes())
        .merge(mounts::routes())
        .merge(network::routes())
        .merge(schedules::routes())
        .merge(settings::routes())
        .merge(startup::routes())
        .merge(users::routes())
        .merge(webhooks::routes())
        .route("/server/{server}/console", axum::routing::get(original_path))
        .route("/server/{server}/subusers", axum::routing::get(original_path))
        .route("/server/{server}/subusers/{*rest}", axum::routing::get(original_path))
        .route("/server/{server}/allocations", axum::routing::get(original_path))
        .route("/server/{server}/activities", axum::routing::get(original_path))
}

/// The original's paths for pages ours name differently (`/console`, `/subusers`, `/allocations`,
/// `/activities`), as bookmarks, plugins and stored notifications link them.
pub fn canonical_path(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    // ["", "server", "<id>", "<page>", ...]
    if segs.len() < 4 || segs[1] != "server" {
        return path.to_string();
    }
    let page = match segs[3] {
        "console" if segs.len() == 4 => "",
        "subusers" => "users",
        "allocations" => "network",
        "activities" => "activity",
        _ => return path.to_string(),
    };
    let mut out = format!("/server/{}", segs[2]);
    if !page.is_empty() {
        out.push('/');
        out.push_str(page);
    }
    for rest in &segs[4..] {
        out.push('/');
        out.push_str(rest);
    }
    out
}

async fn original_path(uri: axum::http::Uri) -> Response {
    let mut to = canonical_path(uri.path());
    if let Some(q) = uri.query() {
        to.push('?');
        to.push_str(q);
    }
    axum::response::Redirect::to(&to).into_response()
}

/// A request inside a server's panel: the tenant, its node and what the user may do there.
pub struct ServerPanel {
    pub authed: Authed,
    pub server: servers::Model,
    pub node: nodes::Model,
    /// `None` = every permission (owner, root admin, `update server` on the node).
    perms: Option<HashSet<String>>,
    pub transfer: bool,
    /// `WebhookResource::getNavigationBadge()`: this server's webhooks (None when hidden).
    webhooks: Option<u64>,
    /// PHP plugins' pages of the server panel this user may open (ADR-0020).
    sidecar_nav: Vec<pelican_api::sidecar::NavEntry>,
    /// The servers of the sidebar's server switcher.
    tenants: Vec<TenantView>,
    /// The original's menu for this user and server (order, badges), from the PHP sidecar.
    layout: Vec<pelican_api::sidecar::LayoutGroup>,
}

impl std::ops::Deref for ServerPanel {
    type Target = Authed;
    fn deref(&self) -> &Authed {
        &self.authed
    }
}

impl FromRequestParts<AppState> for ServerPanel {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let authed = Authed::from_request_parts(parts, st).await?;
        let Path(params): Path<Vec<(String, String)>> =
            Path::from_request_parts(parts, st).await.map_err(|_| WebError::NotFound.into_response())?;
        let id = params.iter().find(|(k, _)| k == "server").map(|(_, v)| v.clone()).unwrap_or_default();
        let panel = load(authed, &id).await.map_err(IntoResponse::into_response)?;
        // BlockAccessInConflict::canAccess() on the page this request belongs to.
        let section = parts.uri.path().split('/').nth(3).unwrap_or_default();
        if CONFLICT_BLOCKED.contains(&section) && panel.conflict() {
            return Err(WebError::Forbidden.into_response());
        }
        Ok(panel)
    }
}

/// The `{id}` segment of `/server/{server}/…/{id}` routes (`Path<i64>` would see both parameters).
pub struct RecordId(pub i64);

impl<S: Send + Sync> FromRequestParts<S> for RecordId {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &S) -> Result<Self, Self::Rejection> {
        let Path(params): Path<Vec<(String, String)>> =
            Path::from_request_parts(parts, st).await.map_err(|_| WebError::NotFound.into_response())?;
        params
            .iter()
            .find(|(k, _)| k == "id")
            .and_then(|(_, v)| v.parse().ok())
            .map(RecordId)
            .ok_or_else(|| WebError::NotFound.into_response())
    }
}

/// Pages using `BlockAccessInConflict` (resources and every `ServerFormPage`).
const CONFLICT_BLOCKED: &[&str] =
    &["files", "backups", "schedules", "users", "databases", "network", "startup", "settings", "mounts"];

pub(crate) async fn load(authed: Authed, id: &str) -> Result<ServerPanel, WebError> {
    let db = &authed.st.db;
    let server =
        servers::Entity::find().filter(servers::Column::UuidShort.eq(id)).one(db).await?.ok_or(WebError::NotFound)?;
    let node = nodes::Entity::find_by_id(server.node_id.0).one(db).await?.ok_or(WebError::NotFound)?;
    let user = &authed.user;
    let owner = server.owner_id.0 == user.id();
    let sub = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(server.id.0))
        .filter(subusers::Column::UserId.eq(user.id()))
        .one(db)
        .await?;
    // User::canAccessTenant(): view policy, owner, or subuser.
    if !(owner || sub.is_some() || user.can_view_server(node.id.0)) {
        return Err(WebError::NotFound);
    }
    // User::checkPermission(): owner / update-server admins have every permission.
    let perms = if owner || user.can_update_server(node.id.0) {
        None
    } else {
        let list = match sub.and_then(|s| s.permissions).map(|j| j.0) {
            Some(serde_json::Value::Array(a)) => a,
            Some(serde_json::Value::Object(m)) => m.into_iter().map(|(_, v)| v).collect(),
            _ => Vec::new(),
        };
        Some(list.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
    };
    let transfer = pelican_core::server_state::active_transfer(db, server.id.0).await?.is_some();
    let webhooks = if authed.user.gate_before("viewList webhook") {
        use sea_orm::PaginatorTrait;
        Some(
            pelican_db::entities::webhook_configurations::Entity::find()
                .filter(pelican_db::entities::webhook_configurations::Column::ServerId.eq(server.id))
                .filter(pelican_db::entities::webhook_configurations::Column::DeletedAt.is_null())
                .count(db)
                .await?,
        )
    } else {
        None
    };
    let sidecar_nav =
        crate::sidecar_proxy::nav(&authed.st, "server", authed.user.id(), Some(server.uuid_short.as_str())).await;
    let layout = pelican_api::sidecar::page_extras(
        &authed.st,
        "server",
        Some(authed.user.id()),
        Some(server.uuid_short.as_str()),
    )
    .await
    .layout
    .clone();
    let tenants = tenants(&authed).await?;
    Ok(ServerPanel { authed, server, node, perms, transfer, webhooks, sidecar_nav, tenants, layout })
}

/// A server in the sidebar's server switcher (Filament's tenant menu).
#[derive(Serialize, Clone)]
pub struct TenantView {
    name: String,
    uuid_short: String,
    initials: String,
}

impl TenantView {
    fn of(s: &servers::Model) -> Self {
        TenantView { name: s.name.clone(), uuid_short: s.uuid_short.clone(), initials: crate::ctx::avatar(&s.name) }
    }
}

/// `User::getTenants()`: `accessibleServers()`, in id order.
async fn tenants(authed: &Authed) -> Result<Vec<TenantView>, WebError> {
    use sea_orm::FromQueryResult;
    let (acc, vals) = crate::app::servers::accessible(&authed.user);
    let b = authed.st.db.get_database_backend();
    let sql = format!("select * from {{servers}} where {acc} order by {{servers.id}}");
    let rows = servers::Model::find_by_statement(pelican_db::raw::stmt(b, &sql, vals)).all(&authed.st.db).await?;
    Ok(rows.iter().map(TenantView::of).collect())
}

#[derive(Serialize)]
struct ServerView {
    id: i64,
    uuid: String,
    uuid_short: String,
    name: String,
    description: Option<String>,
}

impl ServerPanel {
    /// `$user->can(SubuserPermission::X, $server)`.
    pub fn can(&self, permission: &str) -> bool {
        match &self.perms {
            None => true,
            Some(p) => {
                p.contains(permission)
                    || (permission == "websocket.connect" && self.user.can_view_server(self.node.id.0))
            }
        }
    }

    /// Activity entry with the server as subject (`ServerSubject` middleware).
    pub fn activity_for_server(&self, event: &str) -> pelican_core::activity::Activity {
        self.activity(event).subject(pelican_core::activity::morph::SERVER, self.server.id.0)
    }

    pub fn authorize(&self, permission: &str) -> Result<(), WebError> {
        if self.can(permission) { Ok(()) } else { Err(WebError::Forbidden) }
    }

    /// `isInConflictState()`.
    pub fn conflict(&self) -> bool {
        crate::server_info::in_conflict_state(&self.server, &self.node, self.transfer)
    }

    pub fn base_url(&self) -> String {
        format!("/server/{}", self.server.uuid_short)
    }

    fn nav(&self, active: &str) -> Vec<NavGroup> {
        let base = self.base_url();
        let mut items = Vec::new();
        let conflict = self.conflict();
        let mut add = |slug: &str, label: String, icon: &'static str, perm: Option<&str>| {
            if perm.is_none_or(|p| self.can(p)) && !(conflict && CONFLICT_BLOCKED.contains(&slug)) {
                items.push(NavItem {
                    label,
                    url: if slug.is_empty() { base.clone() } else { format!("{base}/{slug}") },
                    icon,
                    active: slug == active,
                    badge: None,
                    badge_color: None,
                    badge_tooltip: None,
                });
            }
        };
        add("", self.t("server/console.title"), "terminal-2", None);
        add("files", self.t("server/file.title"), "files", Some("file.read"));
        add("backups", self.t("server/backup.title"), "file-zip", Some("backup.read"));
        add("schedules", self.t("server/schedule.title"), "clock", Some("schedule.read"));
        add("users", self.t("server/user.title"), "users", Some("user.read"));
        add("databases", self.t("server/database.title"), "database", Some("database.read"));
        add("network", self.t("server/network.title"), "network", Some("allocation.read"));
        add("activity", self.t("server/activity.title"), "stack", Some("activity.read"));
        add("mounts", self.t("server/mount.title"), "layers-linked", Some("mount.read"));
        add("startup", self.t("server/startup.title"), "player-play", Some("startup.read"));
        add("settings", self.t("server/setting.title"), "settings", None);
        if self.webhooks.is_some() {
            add("webhook", self.t("admin/webhook.nav_title"), "webhook", None);
            if let Some(item) = items.last_mut().filter(|i| i.icon == "webhook") {
                item.badge = self.webhooks.filter(|n| *n > 0).map(|n| n.to_string());
            }
        }
        for (plugin, n, page) in crate::plugin_pages::nav_items(pelican_plugins::runtime::Panel::Server) {
            if page.as_ref().and_then(|p| p.permission.as_deref()).is_some_and(|perm| !self.can(perm)) {
                continue;
            }
            items.push(NavItem {
                label: crate::plugin_pages::label(&self.st, &plugin, &self.locale(), &n.label),
                url: format!("{base}/plugins/{plugin}/{}", n.page),
                icon: crate::plugin_pages::icon(n.icon.as_deref()),
                active: active == format!("plugin:{plugin}:{}", n.page),
                badge: None,
                badge_color: None,
                badge_tooltip: None,
            });
        }
        // PHP plugins' pages of the tenant-scoped server panel (ADR-0020): `/server/{tenant}/<slug>`,
        // in their navigation groups after the core's items. `active` is the page's path there.
        let mut groups: Vec<NavGroup> = Vec::new();
        for n in &self.sidecar_nav {
            let rest = n.path.strip_prefix("/server").unwrap_or(&n.path);
            let url = format!("{base}{rest}");
            let item = NavItem {
                label: n.label.clone(),
                active: active == url || active.starts_with(&format!("{url}/")),
                url,
                icon: crate::sidecar_proxy::icon(n.icon.as_deref()),
                badge: None,
                badge_color: None,
                badge_tooltip: None,
            };
            match n.group.as_deref().filter(|g| !g.is_empty()) {
                None => items.push(item),
                Some(g) => match groups.iter_mut().find(|grp| grp.label.as_deref() == Some(g)) {
                    Some(grp) => grp.items.push(item),
                    None => groups.push(NavGroup { label: Some(g.to_string()), items: vec![item] }),
                },
            }
        }
        if self.user.is_admin() && self.user.can_view_server(self.node.id.0) {
            items.push(NavItem {
                label: self.t("server/console.open_in_admin"),
                url: format!("/admin/servers/{}/edit", self.server.id.0),
                icon: "arrow-back",
                active: false,
                badge: None,
                badge_color: None,
                badge_tooltip: None,
            });
        }
        let mut nav = vec![NavGroup { label: None, items }];
        nav.extend(groups);
        crate::admin::arrange(nav, &self.layout)
    }

    /// Render a server panel page (sidebar, conflict banner, user menu).
    pub fn page(&self, active: &str, template: &str, ctx: Value) -> WebResult {
        let mut banners = Vec::new();
        if let Some(msg) = conflict_message(self) {
            banners.push(context! { status => "warning", title => self.t("admin/log.warning"), body => msg });
        }
        let user_menu = vec![context! { label => self.t("profile.server_list"), url => "/", icon => "brand-docker" }];
        let view = ServerView {
            id: self.server.id.0,
            uuid: self.server.uuid.clone(),
            uuid_short: self.server.uuid_short.clone(),
            name: self.server.name.clone(),
            description: self.server.description.clone(),
        };
        self.render(
            template,
            context! {
                ..ctx,
                ..context! {
                    nav => Value::from_serialize(self.nav(active)),
                    user_menu => user_menu,
                    banners => banners,
                    home_url => "/",
                    server => Value::from_serialize(&view),
                    server_base => self.base_url(),
                    tenant => Value::from_serialize(TenantView::of(&self.server)),
                    tenants => Value::from_serialize(&self.tenants),
                }
            },
        )
    }

    pub async fn allocation(&self) -> Result<Option<allocations::Model>, WebError> {
        Ok(match self.server.allocation_id {
            Some(id) => allocations::Entity::find_by_id(id.0).one(&self.st.db).await?,
            None => None,
        })
    }
}

/// `ServerStateConflictException` message (exceptions.server.*), None when not in conflict.
fn conflict_message(p: &ServerPanel) -> Option<String> {
    use crate::server_info::ServerState;
    if !p.conflict() {
        return None;
    }
    let state = ServerState::parse(p.server.status.as_deref());
    let key = if state == Some(ServerState::Suspended) {
        "suspended"
    } else if p.node.maintenance_mode.0 {
        "maintenance"
    } else if !crate::server_info::is_installed(&p.server) {
        "marked_as_failed"
    } else if state == Some(ServerState::RestoringBackup) {
        "restoring_backup"
    } else if p.transfer {
        "transferring"
    } else {
        "state_conflict"
    };
    Some(p.t(&format!("exceptions.server.{key}")))
}

/// PHP `rawurlencode`.
pub fn rawurlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `Server::getSftpUrl()` for the signed-in user.
pub fn sftp_url(p: &ServerPanel, directory: Option<&str>) -> String {
    let dir = directory
        .map(|d| {
            let parts: Vec<String> = d.trim_matches('/').split('/').map(rawurlencode).collect();
            let joined = format!("/{}", parts.join("/"));
            if joined != "/" { format!("{joined}/") } else { joined }
        })
        .filter(|d| d != "/")
        .unwrap_or_default();
    let fqdn = p.node.daemon_sftp_alias.clone().unwrap_or_else(|| p.node.fqdn.clone());
    format!(
        "sftp://{}.{}@{}:{}{}",
        rawurlencode(&p.user.user.username),
        p.server.uuid_short,
        fqdn,
        p.node.daemon_sftp.0,
        dir
    )
}
