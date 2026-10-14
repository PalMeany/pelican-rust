//! Client API (`routes/api-client.php`, docs/spec/client-api.md).

mod account;
mod activity;
pub mod data;
mod servers;

use axum::Router;
use axum::routing::{MethodRouter, get, post};
use pelican_core::state::AppState;

pub fn routes(state: &AppState) -> Router<AppState> {
    let p = |m: MethodRouter<AppState>| {
        m.route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::pipeline::client))
    };
    Router::new()
        .route("/", p(get(servers::index)))
        .route("/permissions", p(get(servers::permissions)))
        .route("/account", p(get(account::index)))
        .route("/account/activity", p(get(activity::account)))
        .route("/servers/{server}", p(get(servers::view)))
        .route("/servers/{server}/websocket", p(get(servers::websocket)))
        .route("/servers/{server}/resources", p(get(servers::resources)))
        .route("/servers/{server}/activity", p(get(activity::server)))
        .route("/servers/{server}/command", p(post(servers::command)))
        .route("/servers/{server}/power", p(post(servers::power)))
}
