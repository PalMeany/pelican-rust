//! Error pages (Laravel's default 403/404/419/500 views).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("Not Found")]
    NotFound,
    #[error("Forbidden")]
    Forbidden,
    #[error("Page Expired")]
    PageExpired,
    #[error("Unprocessable Content")]
    Unprocessable,
    #[error("Content Too Large")]
    PayloadTooLarge,
    #[error("Too Many Requests")]
    TooManyRequests,
    #[error("database: {0}")]
    Db(#[from] sea_orm::DbErr),
    #[error("template: {0}")]
    Template(#[from] minijinja::Error),
    #[error("{0}")]
    Internal(String),
}

impl WebError {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        WebError::Internal(e.to_string())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            WebError::NotFound => StatusCode::NOT_FOUND,
            WebError::Forbidden => StatusCode::FORBIDDEN,
            WebError::PageExpired => StatusCode::from_u16(419).unwrap_or(StatusCode::BAD_REQUEST),
            WebError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            WebError::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
            WebError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            WebError::NotFound => "Not Found",
            WebError::Forbidden => "Forbidden",
            WebError::PageExpired => "Page Expired",
            WebError::PayloadTooLarge => "Content Too Large",
            WebError::Unprocessable => "Unprocessable Content",
            WebError::TooManyRequests => "Too Many Requests",
            _ => "Server Error",
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "web request failed");
        }
        let body = crate::views::render(
            "errors/error.html",
            context! { code => status.as_u16(), title => self.title(), locale => "en" },
        )
        .unwrap_or_else(|_| format!("{} | {}", status.as_u16(), self.title()));
        let mut resp = (status, Html(body)).into_response();
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, private"));
        resp
    }
}

pub type WebResult<T = Response> = Result<T, WebError>;

pub async fn not_found() -> WebError {
    WebError::NotFound
}
