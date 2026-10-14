//! App panel (`/`): server list and profile (app/Filament/App, app/Filament/Pages/Auth).

use axum::Router;
use axum::routing::get;
use minijinja::context;
use pelican_core::state::AppState;

use crate::ctx::Authed;
use crate::error::WebResult;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(index))
}

async fn index(web: Authed) -> WebResult {
    web.render("app/servers.html", context! {})
}
