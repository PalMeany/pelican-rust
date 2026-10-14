//! Client API (`routes/api-client.php`, docs/spec/client-api.md).

mod account;

use axum::Router;
use axum::routing::get;
use pelican_core::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/account", get(account::index))
}
