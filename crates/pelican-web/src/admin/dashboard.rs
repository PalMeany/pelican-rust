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
        crate::sidecar_proxy::embedded(&admin, &headers, "/admin/rs-widgets").await
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
