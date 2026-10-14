//! App panel (`/`): server list and profile (app/Filament/App, app/Filament/Pages/Auth).

mod servers;

use axum::Router;
use axum::routing::{get, post};
use pelican_core::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(servers::index)).route("/servers/{id}/power", post(servers::power))
}
