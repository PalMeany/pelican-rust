//! Error rendering of `app/Exceptions/Handler.php` (client-api.md §6).

use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::json::PhpJson;

/// Byte encoding of the error body (client-api.md §6.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `prepareJsonResponse`: pretty print + unescaped slashes.
    Pretty,
    /// `response()->json()` / `new JsonResponse`: flags 0.
    Compact,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidationMeta {
    pub source_field: String,
    pub rule: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorObject {
    pub code: String,
    /// HTTP status as a string.
    pub status: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ValidationMeta>,
}

#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub errors: Vec<ErrorObject>,
    pub style: Style,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// Without `Accept: application/json` Laravel redirects guests to the login page.
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Unauthenticated,
    Validation,
    Other,
}

pub const GENERIC_500: &str =
    "An unexpected error was encountered while processing this request, please try again.";

impl ApiError {
    pub fn new(status: u16, code: &str, detail: impl Into<String>, style: Style) -> Self {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError {
            status,
            errors: vec![ErrorObject {
                code: code.into(),
                status: status.as_u16().to_string(),
                detail: detail.into(),
                meta: None,
            }],
            style,
            headers: Vec::new(),
            kind: Kind::Other,
        }
    }

    /// `AuthenticationException('Unauthenticated.')` — compact.
    pub fn unauthenticated() -> Self {
        let mut e = Self::new(
            401,
            "AuthenticationException",
            "Unauthenticated.",
            Style::Compact,
        );
        e.kind = Kind::Unauthenticated;
        e
    }

    /// Route-model binding failure (`ModelNotFoundException` → NotFoundHttpException).
    pub fn model_not_found() -> Self {
        Self::new(
            404,
            "NotFoundHttpException",
            "The requested resource could not be found on the server.",
            Style::Pretty,
        )
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(404, "NotFoundHttpException", detail, Style::Pretty)
    }

    pub fn access_denied(detail: impl Into<String>) -> Self {
        Self::new(403, "AccessDeniedHttpException", detail, Style::Pretty)
    }

    /// FormRequest `authorize()` false.
    pub fn unauthorized_action() -> Self {
        Self::access_denied("This action is unauthorized.")
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(400, "BadRequestHttpException", detail, Style::Pretty)
    }

    pub fn http(status: u16, detail: impl Into<String>) -> Self {
        Self::new(status, "HttpException", detail, Style::Pretty)
    }

    /// Any non-HTTP exception: generic message, class basename as code.
    pub fn internal(code: &str) -> Self {
        Self::new(500, code, GENERIC_500, Style::Pretty)
    }

    /// `DisplayException` (status 400 by default) — compact.
    pub fn display(code: &str, status: u16, detail: impl Into<String>) -> Self {
        Self::new(status, code, detail, Style::Compact)
    }

    /// 422 with one error object per (field, rule, message).
    pub fn validation(errors: Vec<(String, String, String)>) -> Self {
        let errors = errors
            .into_iter()
            .map(|(field, rule, message)| ErrorObject {
                code: "ValidationException".into(),
                status: "422".into(),
                detail: message,
                meta: Some(ValidationMeta {
                    source_field: field,
                    rule,
                }),
            })
            .collect();
        ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            errors,
            style: Style::Compact,
            headers: Vec::new(),
            kind: Kind::Validation,
        }
    }

    pub fn with_header(mut self, name: &'static str, value: impl ToString) -> Self {
        if let Ok(v) = HeaderValue::from_str(&value.to_string()) {
            self.headers.push((HeaderName::from_static(name), v));
        }
        self
    }
}

#[derive(Serialize)]
struct Body<'a> {
    errors: &'a [ErrorObject],
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Body {
            errors: &self.errors,
        };
        let mut resp = match self.style {
            Style::Pretty => PhpJson::pretty(self.status, body).into_response(),
            Style::Compact => PhpJson::with_status(self.status, body).into_response(),
        };
        for (k, v) in self.headers {
            resp.headers_mut().insert(k, v);
        }
        resp.extensions_mut().insert(self.kind);
        resp
    }
}

impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        tracing::error!("database error: {e}");
        ApiError::internal("QueryException")
    }
}

impl From<pelican_core::auth::AuthError> for ApiError {
    fn from(e: pelican_core::auth::AuthError) -> Self {
        match e {
            pelican_core::auth::AuthError::Db(d) => d.into(),
            pelican_core::auth::AuthError::Decrypt(_) => ApiError::internal("DecryptException"),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
