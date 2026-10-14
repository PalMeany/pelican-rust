//! Client API (`routes/api-client.php`, docs/spec/client-api.md).

mod account;
mod activity;
mod backups;
pub mod data;
mod servers;
mod startup;
mod subusers;

use axum::Router;
use axum::routing::{MethodRouter, delete, get, post, put};
use pelican_core::state::AppState;

pub fn routes(state: &AppState) -> Router<AppState> {
    let p = |m: MethodRouter<AppState>| {
        m.route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::pipeline::client))
    };
    Router::new()
        .route("/", p(get(servers::index)))
        .route("/permissions", p(get(servers::permissions)))
        .route("/account", p(get(account::index)))
        .route("/account/username", p(put(account::update_username)))
        .route("/account/email", p(put(account::update_email)))
        .route("/account/password", p(put(account::update_password)))
        .route("/account/api-keys", p(get(account::api_keys_index).post(account::api_keys_store)))
        .route("/account/api-keys/{identifier}", p(delete(account::api_keys_delete)))
        .route("/account/ssh-keys", p(get(account::ssh_keys_index).post(account::ssh_keys_store)))
        .route("/account/ssh-keys/{fingerprint}", p(delete(account::ssh_keys_delete)))
        .route("/account/activity", p(get(activity::account)))
        .route("/servers/{server}", p(get(servers::view)))
        .route("/servers/{server}/websocket", p(get(servers::websocket)))
        .route("/servers/{server}/resources", p(get(servers::resources)))
        .route("/servers/{server}/activity", p(get(activity::server)))
        .route("/servers/{server}/command", p(post(servers::command)))
        .route("/servers/{server}/power", p(post(servers::power)))
        .route("/servers/{server}/users", p(get(subusers::index).post(subusers::store)))
        .route("/servers/{server}/backups", p(get(backups::index).post(backups::store)))
        .route("/servers/{server}/startup", p(get(startup::index)))
        .route("/servers/{server}/startup/variable", p(put(startup::update)))
        .route("/servers/{server}/backups/{backup}", p(get(backups::view).delete(backups::delete)))
        .route("/servers/{server}/backups/{backup}/download", p(get(backups::download)))
        .route("/servers/{server}/backups/{backup}/rename", p(put(backups::rename)))
        .route("/servers/{server}/backups/{backup}/lock", p(post(backups::lock)))
        .route("/servers/{server}/backups/{backup}/restore", p(post(backups::restore)))
        .route("/servers/{server}/users/{user}", p(get(subusers::view).post(subusers::update).delete(subusers::delete)))
}
