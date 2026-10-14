//! HTTP layer of pelican-rs (ADR-0001/0002): `/api/remote`, `/api/client`,
//! `/api/application`, with the original middleware order and error formats.

pub mod binding;
pub mod client;
pub mod error;
pub mod json;
pub mod pipeline;
pub mod resources;

use axum::Router;
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;

use crate::error::ApiError;

/// The full API router (state applied).
pub fn router(state: AppState) -> Router {
    let client = client::routes().route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        pipeline::client,
    ));
    Router::new()
        .nest("/api/client", client)
        .fallback(route_not_found)
        .layer(axum::middleware::from_fn(pipeline::negotiate_errors))
        .with_state(state)
}

/// Laravel `NotFoundHttpException("The route <path> could not be found.")`.
async fn route_not_found(method: Method, uri: axum::http::Uri) -> Response {
    let _ = method;
    let path = uri.path().trim_matches('/');
    ApiError::not_found(format!("The route {path} could not be found.")).into_response()
}
