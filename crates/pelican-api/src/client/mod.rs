//! Client API (`routes/api-client.php`, docs/spec/client-api.md).

mod account;

use axum::Router;
use axum::routing::get;
use pelican_core::state::AppState;

pub fn routes(state: &AppState) -> Router<AppState> {
    let p = |m: axum::routing::MethodRouter<AppState>| {
        m.route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::pipeline::client))
    };
    Router::new().route("/account", p(get(account::index)))
}
