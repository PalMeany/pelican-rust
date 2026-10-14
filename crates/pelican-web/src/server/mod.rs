//! Server panel (`/server/{uuid_short}`, app/Filament/Server): tenant access,
//! subuser permissions, navigation and the console.

mod console;
mod databases;
mod files;

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
    Router::new().merge(console::routes()).merge(files::routes()).merge(databases::routes())
}

/// A request inside a server's panel: the tenant, its node and what the user may do there.
pub struct ServerPanel {
    pub authed: Authed,
    pub server: servers::Model,
    pub node: nodes::Model,
    /// `None` = every permission (owner, root admin, `update server` on the node).
    perms: Option<HashSet<String>>,
    pub transfer: bool,
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
        load(authed, &id).await.map_err(IntoResponse::into_response)
    }
}

async fn load(authed: Authed, id: &str) -> Result<ServerPanel, WebError> {
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
    Ok(ServerPanel { authed, server, node, perms, transfer })
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
        let mut add = |slug: &str, label: String, icon: &'static str, perm: Option<&str>| {
            if perm.is_none_or(|p| self.can(p)) {
                items.push(NavItem {
                    label,
                    url: if slug.is_empty() { base.clone() } else { format!("{base}/{slug}") },
                    icon,
                    active: slug == active,
                    badge: None,
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
        if self.user.gate_before("viewList webhook") {
            add("webhooks", self.t("admin/webhook.nav_title"), "webhook", None);
        }
        if self.user.is_admin() && self.user.can_view_server(self.node.id.0) {
            items.push(NavItem {
                label: self.t("server/console.open_in_admin"),
                url: format!("/admin/servers/{}/edit", self.server.id.0),
                icon: "arrow-back",
                active: false,
                badge: None,
            });
        }
        vec![NavGroup { label: None, items }]
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
