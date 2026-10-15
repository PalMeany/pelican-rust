//! Admin › Dashboard (app/Filament/Admin/Pages/Dashboard.php + Widgets/*): version,
//! development build notice, first-node hint, support and help sections.

use axum::Router;
use axum::routing::get;
use minijinja::context;
use pelican_api::versions;
use pelican_core::state::AppState;
use pelican_db::entities::nodes;
use sea_orm::{EntityTrait, PaginatorTrait};

use super::Admin;
use crate::error::WebResult;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin", get(index))
}

async fn index(admin: Admin, headers: axum::http::HeaderMap) -> WebResult {
    let st = &admin.st;
    let version = versions::current_panel_version();
    // UpdateWidget: a canary build is never compared, so it always reads "up to date".
    let latest = match versions::current_comparable_version() {
        Some(_) => versions::latest_panel_version(st).await,
        None => None,
    };
    let update = match (versions::current_comparable_version(), &latest) {
        (Some(_), None) => "unavailable",
        _ if versions::is_latest_panel(st).await => "latest",
        _ => "available",
    };
    let no_nodes = nodes::Entity::find().count(&st.db).await? == 0;
    // The widgets PHP plugins add to the dashboard (`/admin/rs-widgets` of the bridge).
    let widgets = if pelican_api::sidecar::manifest().is_some_and(|m| m.admin_widgets > 0) {
        plugin_widgets(&admin, &headers).await
    } else {
        None
    };
    let (sidecar, cookies) = match widgets {
        Some((t, c)) => (Some(t), c),
        None => (None, Vec::new()),
    };
    let mut resp = admin
        .page(
            "",
            "admin/dashboard.html",
            context! {
                title => admin.t_with("admin/dashboard.heading", &[("app", &st.config.app_name)]),
                subheading => admin.t_with("admin/dashboard.version", &[("version", &version)]),
                version => version,
                update => update,
                latest_version => latest,
                in_container => std::path::Path::new("/.dockerenv").exists(),
                canary => versions::current_comparable_version().is_none(),
                no_nodes => no_nodes,
                sidecar_head => sidecar.as_ref().map(|t| t.head.clone()),
                sidecar_main => sidecar.as_ref().map(|t| t.main.clone()),
                sidecar_tail => sidecar.as_ref().map(|t| t.tail.clone()),
            },
        )
        .await?;
    for c in cookies {
        resp.headers_mut().append(axum::http::header::SET_COOKIE, c);
    }
    Ok(resp)
}

/// Rendered plugin widgets per browser (their Livewire snapshots carry that browser's sidecar
/// CSRF token), reused for [`WIDGETS_KEEP`]; older than [`WIDGETS_FRESH`] they are shown at
/// once and rendered again in the background. Plugins' widgets may be slow (System Status
/// samples the CPU for a second) and the original renders them on every visit.
async fn plugin_widgets(
    admin: &Admin,
    headers: &axum::http::HeaderMap,
) -> Option<(crate::sidecar_proxy::Transplant, Vec<axum::http::HeaderValue>)> {
    use std::hash::{Hash, Hasher};
    const PATH: &str = "/admin/rs-widgets";
    // Per browser session (cookies themselves are re-encrypted on every response).
    let mut h = std::collections::hash_map::DefaultHasher::new();
    admin.session.csrf().hash(&mut h);
    let key = format!("admin-widgets:{}:{:x}", admin.user.id(), h.finish());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let cached = crate::cache::get(&key).and_then(|v| {
        let at = v.get("at")?.as_u64()?;
        let t = serde_json::from_value::<crate::sidecar_proxy::Transplant>(v.get("t")?.clone()).ok()?;
        Some((at, t))
    });
    if let Some((at, t)) = cached {
        if now.saturating_sub(at) >= WIDGETS_FRESH
            && REFRESHING.lock().map(|mut g| g.insert(key.clone())).unwrap_or(false)
        {
            let origin = crate::sidecar_proxy::Origin::of(admin, headers);
            tokio::spawn(async move {
                if let Some(t) = crate::sidecar_proxy::embedded_detached(origin, PATH.to_string()).await {
                    crate::cache::put(&key, WIDGETS_KEEP, serde_json::json!({ "at": now, "t": t }));
                }
                if let Ok(mut g) = REFRESHING.lock() {
                    g.remove(&key);
                }
            });
        }
        return Some((t, Vec::new()));
    }
    let (t, cookies) = crate::sidecar_proxy::embedded(admin, headers, PATH).await?;
    crate::cache::put(&key, WIDGETS_KEEP, serde_json::json!({ "at": now, "t": &t }));
    Some((t, cookies))
}

const WIDGETS_FRESH: u64 = 30;
const WIDGETS_KEEP: std::time::Duration = std::time::Duration::from_secs(600);
static REFRESHING: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());
