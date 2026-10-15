//! WASM plugin pages, web routes and navigation entries (ADR-0018): admin pages at
//! `/admin/plugins/{plugin}/page/{page}`, server pages at `/server/{server}/plugins/{plugin}/{page}`,
//! app pages at `/plugins/{plugin}/pages/{page}` and the plugin's web routes below
//! `/plugins/{plugin}/`. Page bodies are HTML fragments placed in the panel layout.

use axum::Router;
use axum::extract::{Path, Request};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use minijinja::context;
use pelican_api::plugin_routes::{server_json, user_json};
use pelican_core::state::AppState;
use pelican_plugins::runtime::{NavItem, Page, Panel, Request as PluginRequest, RouteScope};

use crate::admin::Admin;
use crate::ctx::{Authed, Web};
use crate::error::{WebError, WebResult};
use crate::server::ServerPanel;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/plugins/{plugin}/page/{page}", get(admin_page))
        .route("/server/{server}/plugins/{plugin}/{page}", get(server_page))
        .route("/plugins/{plugin}/{*rest}", any(web_route))
}

/// Label/title: `<plugin>::group.key` from the plugin, other keys from the panel.
pub fn label(st: &AppState, plugin: &str, locale: &str, key: &str) -> String {
    let cfg = pelican_api::application::plugins::config(st);
    pelican_plugins::trans_line(&cfg, plugin, locale, key, &[]).unwrap_or_else(|| crate::i18n::trans(locale, key, &[]))
}

fn find_page(plugin: &str, panel: Panel, page: &str) -> Option<Page> {
    let rt = pelican_plugins::runtime::global()?;
    rt.get(plugin)?.registration.pages.iter().find(|p| p.panel == panel && p.id == page).cloned()
}

/// Navigation entries of loaded plugins for a panel: `(plugin, item, page)`.
pub fn nav_items(panel: Panel) -> Vec<(String, NavItem, Option<Page>)> {
    let Some(rt) = pelican_plugins::runtime::global() else { return Vec::new() };
    let mut out = Vec::new();
    for l in rt.loaded() {
        for n in l.registration.navigation.iter().filter(|n| n.panel == panel) {
            let page = l.registration.pages.iter().find(|p| p.panel == panel && p.id == n.page).cloned();
            out.push((l.id.clone(), n.clone(), page));
        }
    }
    out.sort_by_key(|(_, n, _)| n.sort.unwrap_or(0));
    out
}

pub fn icon(name: Option<&str>) -> &'static str {
    name.and_then(crate::assets::icon_key).unwrap_or("packages")
}

fn request(web: &Web, req_path: &str, user: Option<String>, server: Option<String>) -> PluginRequest {
    PluginRequest {
        method: "GET".into(),
        path: req_path.to_string(),
        query: web.query.clone().unwrap_or_default(),
        headers: Vec::new(),
        body: Vec::new(),
        locale: web.locale(),
        user,
        server,
    }
}

async fn render(
    web: &Web,
    plugin: &str,
    panel: Panel,
    page: &Page,
    user: Option<String>,
    server: Option<String>,
) -> (String, Option<String>, Option<String>) {
    let title = label(&web.st, plugin, &web.locale(), &page.title);
    let Some(rt) = pelican_plugins::runtime::global() else { return (title, None, Some("Plugin not loaded".into())) };
    match rt.render_page(plugin, panel, &page.id, request(web, &format!("/{}", page.id), user, server)).await {
        Ok(Ok(html)) => (title, Some(html), None),
        Ok(Err(e)) => (title, None, Some(e)),
        Err(e) => (title, None, Some(e.to_string())),
    }
}

/// GET /admin/plugins/{plugin}/page/{page}.
async fn admin_page(admin: Admin, Path((plugin, page)): Path<(String, String)>) -> WebResult {
    let p = find_page(&plugin, Panel::Admin, &page).ok_or(WebError::NotFound)?;
    if let Some(perm) = &p.permission {
        admin.authorize(perm)?;
    }
    let user = user_json(&admin.user.user, admin.user.is_root_admin());
    let (title, html, error) = render(&admin, &plugin, Panel::Admin, &p, Some(user), None).await;
    admin.page(&format!("plugin:{plugin}:{page}"), "plugins/page.html", context! { title, html, error }).await
}

/// GET /server/{server}/plugins/{plugin}/{page}.
async fn server_page(p: ServerPanel, Path((_, plugin, page)): Path<(String, String, String)>) -> WebResult {
    let pg = find_page(&plugin, Panel::Server, &page).ok_or(WebError::NotFound)?;
    if let Some(perm) = &pg.permission {
        if !p.can(perm) {
            return Err(WebError::Forbidden);
        }
    }
    let user = user_json(&p.user.user, p.user.is_root_admin());
    let (title, html, error) = render(&p, &plugin, Panel::Server, &pg, Some(user), Some(server_json(&p.server))).await;
    p.page(&format!("plugin:{plugin}:{page}"), "plugins/page.html", context! { title, html, error })
}

/// `/plugins/{plugin}/*`: an app page (`pages/<id>`) or one of the plugin's web routes.
async fn web_route(web: Authed, Path((plugin, rest)): Path<(String, String)>, req: Request) -> Response {
    if let Some(page_id) = rest.strip_prefix("pages/") {
        if req.method() == axum::http::Method::GET {
            if let Some(page) = find_page(&plugin, Panel::App, page_id) {
                let user = user_json(&web.user.user, web.user.is_root_admin());
                let (title, html, error) = render(&web, &plugin, Panel::App, &page, Some(user), None).await;
                return web.render("plugins/page.html", context! { title, html, error }).into_response();
            }
        }
    }
    let user = user_json(&web.user.user, web.user.is_root_admin());
    let locale = web.locale();
    match pelican_api::plugin_routes::call(&plugin, RouteScope::Web, req, &rest, locale, Some(user), None).await {
        Ok(r) => r,
        Err(_) => WebError::NotFound.into_response(),
    }
}
