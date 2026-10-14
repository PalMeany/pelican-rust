//! PHP-compatible JSON responses (ADR-0006).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use laravel_compat::json::{self, JSON_PRETTY_PRINT, JSON_UNESCAPED_SLASHES};
use serde::Serialize;

/// `new JsonResponse($data, $status)` — json_encode flags 0 unless set.
pub struct PhpJson<T> {
    pub status: StatusCode,
    pub flags: u32,
    pub value: T,
}

impl<T: Serialize> PhpJson<T> {
    pub fn ok(value: T) -> Self {
        PhpJson { status: StatusCode::OK, flags: 0, value }
    }

    pub fn with_status(status: StatusCode, value: T) -> Self {
        PhpJson { status, flags: 0, value }
    }

    /// Laravel's exception handler `prepareJsonResponse` encoding.
    pub fn pretty(status: StatusCode, value: T) -> Self {
        PhpJson { status, flags: JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES, value }
    }
}

impl<T: Serialize> IntoResponse for PhpJson<T> {
    fn into_response(self) -> Response {
        match json::to_string_with_flags(&self.value, self.flags) {
            Ok(body) => {
                let mut r = (self.status, body).into_response();
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                r
            }
            Err(e) => {
                tracing::error!("json encoding failed: {e}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// `204` with empty body and no Content-Type (Symfony `Response::prepare`).
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// Fractal-compatible item envelope `{object, attributes}`.
#[derive(Serialize)]
pub struct Item<'a, A: Serialize> {
    pub object: &'a str,
    pub attributes: A,
}
