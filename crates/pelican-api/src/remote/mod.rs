//! Remote API used by Wings (`routes/api-remote.php`, docs/spec/remote-api.md).

mod activity;
mod backups;
mod servers;
mod sftp;
mod transfer;

use axum::Router;
use axum::routing::{get, post};
use pelican_core::state::AppState;
use pelican_db::entities::nodes;

use crate::error::ApiError;

pub fn routes(state: &AppState) -> Router<AppState> {
    let p = |m: axum::routing::MethodRouter<AppState>| {
        m.route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::pipeline::remote))
    };
    Router::new()
        .route("/sftp/auth", p(post(sftp::auth)))
        .route("/servers", p(get(servers::list)))
        .route("/servers/reset", p(post(servers::reset)))
        .route("/activity", p(post(activity::store)))
        .route("/servers/{server}", p(get(servers::show)))
        .route("/servers/{server}/install", p(get(servers::install_show).post(servers::install_store)))
        .route("/servers/{server}/transfer/failure", p(post(transfer::failure)))
        .route("/servers/{server}/transfer/success", p(post(transfer::success)))
        .route("/servers/{server}/container/status", p(post(servers::container_status)))
        .route("/backups/{backup}", p(get(backups::upload_parts).post(backups::status)))
        .route("/backups/{backup}/restore", p(post(backups::restore)))
}

/// `HttpForbiddenException('Requesting node does not have permission to access this server.')`.
pub fn node_forbidden() -> ApiError {
    ApiError::new(
        403,
        "HttpForbiddenException",
        "Requesting node does not have permission to access this server.",
        crate::error::Style::Pretty,
    )
}

pub fn node(ctx: &crate::pipeline::Ctx) -> Result<&nodes::Model, ApiError> {
    ctx.node.as_deref().ok_or_else(|| ApiError::internal("InvalidArgumentException"))
}
