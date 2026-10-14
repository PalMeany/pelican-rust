//! CSRF protection like Laravel's `VerifyCsrfToken`: every non-safe request
//! carries the session token in `_token` (urlencoded forms) or `X-CSRF-TOKEN`.

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::WebError;
use crate::session::Session;

/// Largest urlencoded body we buffer to look for `_token`.
const MAX_FORM: usize = 16 * 1024 * 1024;

fn form_token(body: &[u8]) -> Option<String> {
    form_urlencoded::parse(body).find(|(k, _)| k == "_token").map(|(_, v)| v.into_owned())
}

pub async fn layer(req: Request, next: Next) -> Result<Response, WebError> {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(next.run(req).await);
    }
    let Some(session) = req.extensions().get::<Session>().cloned() else {
        return Err(WebError::PageExpired);
    };
    let expected = session.csrf();
    let header_token = req.headers().get("x-csrf-token").and_then(|v| v.to_str().ok()).map(str::to_string);
    if let Some(t) = header_token {
        return if !expected.is_empty() && laravel_compat::hashing::constant_time_eq(t.as_bytes(), expected.as_bytes()) {
            Ok(next.run(req).await)
        } else {
            Err(WebError::PageExpired)
        };
    }
    let urlencoded = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
    if !urlencoded {
        return Err(WebError::PageExpired);
    }
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, MAX_FORM).await.map_err(|_| WebError::PayloadTooLarge)?;
    let ok = form_token(&bytes).is_some_and(|t| {
        !expected.is_empty() && laravel_compat::hashing::constant_time_eq(t.as_bytes(), expected.as_bytes())
    });
    if !ok {
        return Err(WebError::PageExpired);
    }
    Ok(next.run(Request::from_parts(parts, Body::from(bytes))).await)
}
