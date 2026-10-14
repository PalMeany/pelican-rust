//! Application API (`routes/api-application.php`, docs/spec/application-api.md).

pub mod data;
pub mod database_hosts;
mod eggs;
mod mounts;
pub mod nodes;
mod roles;
pub mod servers;
mod servers_write;
mod users;
pub(crate) mod webhooks;

use axum::Router;
use axum::routing::{MethodRouter, delete, get, patch, post};
use pelican_core::state::AppState;
use serde_json::{Map, Value, json};

use crate::envelope::{Pagination, list, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::Input;
use crate::pipeline::Ctx;

/// `AdminAcl` bits (app/Services/Acl/Api/AdminAcl.php).
pub const READ: i64 = 1;
pub const WRITE: i64 = 2;

const KEY_TYPE_ACCOUNT: i64 = 1;
const KEY_TYPE_APPLICATION: i64 = 2;

fn key_permission(ctx: &Ctx, resource: &str) -> i64 {
    let Some(key) = ctx.user.as_ref().and_then(|u| u.api_key()) else { return 0 };
    key.permissions
        .as_ref()
        .and_then(|p| p.0.get(resource))
        .map(|v| pelican_core::php::intval(&pelican_core::php::string(v)))
        .unwrap_or(0)
}

impl Ctx {
    /// `ApplicationApiRequest::authorize()` (§1.6): root admins with an account
    /// key pass, everyone else needs the key's ACL bit (WRITE does not imply READ).
    pub fn acl(&self, resource: &str, permission: i64) -> ApiResult<()> {
        let user = self.user()?;
        let Some(key) = user.api_key() else { return Err(ApiError::unauthorized_action()) };
        if user.is_root_admin() && key.key_type.0 == KEY_TYPE_ACCOUNT {
            return Ok(());
        }
        if key_permission(self, resource) & permission != 0 { Ok(()) } else { Err(ApiError::unauthorized_action()) }
    }

    /// `IncludeContext::allowsAdmin($resource)` (§1.9).
    pub fn allows_admin(&self, resource: &str) -> bool {
        let Some(user) = self.user.as_ref() else { return false };
        let Some(key) = user.api_key() else { return false };
        match key.key_type.0 {
            KEY_TYPE_ACCOUNT => user.is_root_admin(),
            KEY_TYPE_APPLICATION => key_permission(self, resource) & READ != 0,
            _ => false,
        }
    }
}

/// `per_page` from the query string only, PHP semantics: falsy → 15,
/// non-numeric → TypeError, leading-numeric ("10abc") → warning → ErrorException.
pub fn per_page(query: &Value, default: i64) -> ApiResult<i64> {
    let raw = match query.get("per_page") {
        None | Some(Value::Null) => return Ok(default),
        Some(v) => pelican_core::php::string(v),
    };
    if pelican_core::php::is_numeric_str(&raw) {
        let n = raw.trim().parse::<f64>().unwrap_or(0.0) as i64;
        return Ok(if n == 0 { 15 } else { n });
    }
    let rest = raw.trim_start().trim_start_matches(['-', '+']);
    let leading = rest.starts_with(|c: char| c.is_ascii_digit())
        || (rest.starts_with('.') && rest[1..].starts_with(|c: char| c.is_ascii_digit()));
    Err(ApiError::internal(if leading { "ErrorException" } else { "TypeError" }))
}

/// A paginated listing: `LIMIT/OFFSET` clause and the `meta.pagination` block.
pub struct Page {
    pub per_page: i64,
    pub page: i64,
}

impl Page {
    pub fn new(input: &Input, default: i64) -> ApiResult<Page> {
        let per_page = per_page(&input.query, default)?;
        let page = crate::paginate::resolve_page(input.get("page"));
        Ok(Page { per_page, page })
    }

    /// ` limit N offset M` (no limit for a negative page size).
    pub fn sql(&self) -> String {
        let offset = ((self.page - 1) * self.per_page).max(0);
        if self.per_page >= 0 {
            format!(" limit {} offset {offset}", self.per_page)
        } else {
            format!(" offset {offset}")
        }
    }

    /// `{"object":"list","data":[...],"meta":{"pagination":...}}`; links carry only `page`.
    pub fn envelope(&self, ctx: &Ctx, path: &str, total: i64, data: Vec<Value>) -> Value {
        let p = Pagination {
            total,
            count: data.len() as i64,
            per_page: self.per_page,
            current_page: self.page,
            url: format!("{}{}", ctx.root, path.trim_end_matches('/')),
            appends: Vec::new(),
        };
        let mut meta = Map::new();
        meta.insert("pagination".into(), p.meta());
        with_meta(list(data), meta)
    }
}

/// `meta.resource` = `route(...)` on APP_URL.
pub fn resource_meta(st: &AppState, path: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("resource".into(), json!(format!("{}/api/application/{path}", st.config.app_url.trim_end_matches('/'))));
    m
}

pub fn routes(state: &AppState) -> Router<AppState> {
    let p = |m: MethodRouter<AppState>| {
        m.route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::pipeline::application))
    };
    Router::new()
        .route("/users", p(get(users::index).post(users::store)))
        .route("/users/external/{external_id}", p(get(users::view_external)))
        .route("/users/{user}", p(get(users::view).patch(users::update).delete(users::delete)))
        .route("/users/{user}/roles/assign", p(patch(users::assign_roles)))
        .route("/users/{user}/roles/remove", p(patch(users::remove_roles)))
        .route("/nodes", p(get(nodes::index).post(nodes::store)))
        .route("/nodes/deployable", p(get(nodes::deployable)))
        .route("/nodes/{node}", p(get(nodes::view).patch(nodes::update).delete(nodes::delete)))
        .route("/nodes/{node}/configuration", p(get(nodes::config)))
        .route("/nodes/{node}/allocations", p(get(nodes::allocations_index).post(nodes::allocations_store)))
        .route("/nodes/{node}/allocations/{allocation}", p(delete(nodes::allocations_delete)))
        .route("/servers", p(get(servers::index).post(servers_write::store)))
        .route("/servers/external/{external_id}", p(get(servers::view_external)))
        .route("/servers/{server}", p(get(servers::view).delete(servers::delete)))
        .route("/servers/{server}/{force}", p(delete(servers::delete)))
        .route("/servers/{server}/details", p(patch(servers::details)))
        .route("/servers/{server}/build", p(patch(servers_write::build)))
        .route("/servers/{server}/startup", p(patch(servers_write::startup)))
        .route("/servers/{server}/transfer", p(post(servers_write::transfer)))
        .route("/servers/{server}/suspend", p(post(servers::suspend)))
        .route("/servers/{server}/unsuspend", p(post(servers::unsuspend)))
        .route("/servers/{server}/reinstall", p(post(servers::reinstall)))
        .route("/servers/{server}/transfer/cancel", p(post(servers::transfer_cancel)))
        .route("/servers/{server}/databases", p(get(servers::databases_index).post(servers::databases_store)))
        .route(
            "/servers/{server}/databases/{database}",
            p(get(servers::databases_view).delete(servers::databases_delete)),
        )
        .route("/servers/{server}/databases/{database}/reset-password", p(post(servers::databases_reset_password)))
        .route("/eggs", p(get(eggs::index)))
        .route("/eggs/import", p(post(eggs::import)))
        .route("/eggs/{egg}", p(get(eggs::view).delete(eggs::delete)))
        .route("/eggs/{egg}/export", p(get(eggs::export)))
        .route("/eggs/uuid/{egg_uuid}", p(delete(eggs::delete)))
        .route("/webhooks", p(get(webhooks::index).post(webhooks::store)))
        .route("/webhooks/types", p(get(webhooks::types)))
        .route("/webhooks/events", p(get(webhooks::events)))
        .route("/webhooks/{webhook}", p(get(webhooks::view).patch(webhooks::update).delete(webhooks::delete)))
        .route("/webhooks/{webhook}/test", p(post(webhooks::test)))
        .route("/database-hosts", p(get(database_hosts::index).post(database_hosts::store)))
        .route(
            "/database-hosts/{database_host}",
            p(get(database_hosts::view).patch(database_hosts::update).delete(database_hosts::delete)),
        )
        .route("/mounts", p(get(mounts::index).post(mounts::store)))
        .route("/mounts/{mount}", p(get(mounts::view).patch(mounts::update).delete(mounts::delete)))
        .route("/mounts/{mount}/eggs", p(get(mounts::get_eggs).post(mounts::add_eggs)))
        .route("/mounts/{mount}/nodes", p(get(mounts::get_nodes).post(mounts::add_nodes)))
        .route("/mounts/{mount}/servers", p(get(mounts::get_servers).post(mounts::add_servers)))
        .route("/mounts/{mount}/eggs/{egg_id}", p(delete(mounts::delete_egg)))
        .route("/mounts/{mount}/nodes/{node_id}", p(delete(mounts::delete_node)))
        .route("/mounts/{mount}/servers/{server_id}", p(delete(mounts::delete_server)))
        .route("/roles", p(get(roles::index).post(roles::store)))
        .route("/roles/{role}", p(get(roles::view).patch(roles::update).delete(roles::delete)))
}
