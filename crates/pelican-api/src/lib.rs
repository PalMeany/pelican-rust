//! HTTP layer of pelican-rs (ADR-0001/0002): `/api/remote`, `/api/client`,
//! `/api/application`, with the original middleware order and error formats.

pub mod application;
pub mod binding;
pub mod client;
pub mod envelope;
pub mod error;
pub mod health;
pub mod input;
pub mod jobs;
pub mod json;
pub mod paginate;
pub mod passwords;
pub mod pipeline;
pub mod query;
pub mod remote;
pub mod resources;
pub mod services;
pub mod url_rule;
pub mod validation;
pub mod versions;

use axum::Router;
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;

use crate::error::ApiError;

/// The full API router (state applied).
pub fn router(state: AppState) -> Router {
    let client = client::routes(&state);
    let remote = remote::routes(&state);
    let application = application::routes(&state);
    let inner = Router::new()
        .nest("/api/client", client)
        .nest("/api/remote", remote)
        .nest("/api/application", application)
        .fallback(route_not_found)
        .with_state(state);
    // Error negotiation must see responses after axum finalised them (e.g. the
    // `Allow` header of 405s is added at the inner router's top level).
    Router::new().fallback_service(inner).layer(axum::middleware::from_fn(pipeline::negotiate_errors))
}

/// Laravel `NotFoundHttpException("The route <path> could not be found.")`.
async fn route_not_found(method: Method, uri: axum::http::Uri) -> Response {
    let _ = method;
    let path = uri.path().trim_matches('/');
    ApiError::not_found(format!("The route {path} could not be found.")).into_response()
}
