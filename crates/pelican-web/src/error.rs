//! Error pages: the panel's own views (`resources/views/errors/*.blade.php`, Filament's simple
//! layout) for 400/401/403/404/500/503, Laravel's minimal ones for the rest.

use std::sync::RwLock;

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

/// The panel's name for error pages, which render without a request (set with each router build).
static BRAND: RwLock<Option<String>> = RwLock::new(None);

pub(crate) fn set_brand(app_name: &str) {
    if let Ok(mut g) = BRAND.write() {
        *g = Some(app_name.to_string());
    }
}

/// The panel's error view for a status: title, subtitle and icon (`errors/{code}.blade.php`).
fn panel_view(code: u16) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match code {
        400 => ("Bad request", "", "exclamation-circle"),
        401 => ("Unauthorized", "You are not authorized to access this resource.", "exclamation-circle"),
        // AuthorizationException's message.
        403 => ("Forbidden", "This action is unauthorized.", "exclamation-circle"),
        404 => ("Not found", "The requested resource was not found.", "zoom-question"),
        500 => ("Server Error", "Something went wrong.", "bug"),
        503 => ("Maintenance", "This site is currently down for maintenance.", "traffic-cone"),
        _ => return None,
    })
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, "web request failed");
        }
        let code = status.as_u16();
        let app_name = BRAND.read().ok().and_then(|g| g.clone()).unwrap_or_else(|| "Pelican".into());
        let footer_name = if app_name.eq_ignore_ascii_case("pelican") {
            app_name.clone()
        } else {
            format!("{app_name} - Powered by Pelican")
        };
        let ctx = match panel_view(code) {
            Some((title, subtitle, icon_name)) => context! {
                code, title, subtitle, icon_name, panel => true, app_name, footer_name,
                year => chrono::Utc::now().format("%Y").to_string(), locale => "en",
            },
            None => context! { code, title => self.title(), locale => "en" },
        };
        let body = crate::views::render("errors/error.html", ctx)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(e: WebError) -> String {
        let r = e.into_response();
        String::from_utf8(axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn panel_and_minimal_error_pages_render() {
        let b = body(WebError::NotFound).await;
        assert!(b.contains("404 | Not found") && b.contains("app-footer"), "{b}");
        let b = body(WebError::Internal("x".into())).await;
        assert!(b.contains("500 | Server Error") && b.contains("Something went wrong."), "{b}");
        let b = body(WebError::PageExpired).await;
        assert!(b.contains("error-minimal") && b.contains("Page Expired"), "{b}");
    }
}
