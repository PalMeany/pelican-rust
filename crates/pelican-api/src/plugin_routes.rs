//! Routes registered by WASM plugins (ADR-0018):
//! `/api/client/plugins/{plugin}/*`, `/api/client/servers/{server}/plugins/{plugin}/*` and
//! `/api/application/plugins/{plugin}/api/*`. The host authenticates (client key or session,
//! application key with the `plugin` permission) before the plugin sees the request.

use axum::Extension;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use pelican_core::state::AppState;
use pelican_db::entities::{servers, users};
use pelican_plugins::runtime::{Header, RouteScope};
use serde_json::json;

use crate::application::{READ, WRITE};
use crate::error::{ApiError, ApiResult};
use crate::pipeline::Ctx;

/// Every method routed to one handler (`any()` is a fallback, so it can't carry the
/// pipeline's `route_layer`).
pub fn all_methods<H, T>(h: H) -> axum::routing::MethodRouter<AppState>
where
    H: axum::handler::Handler<T, AppState> + Clone,
    T: 'static,
{
    axum::routing::get(h.clone()).post(h.clone()).put(h.clone()).patch(h.clone()).delete(h)
}

/// The `user` document plugins receive.
pub fn user_json(u: &users::Model, root_admin: bool) -> String {
    json!({
        "id": u.id.0, "uuid": u.uuid, "username": u.username, "email": u.email,
        "language": u.language, "timezone": u.timezone, "root_admin": root_admin,
    })
    .to_string()
}

/// The `server` document plugins receive.
pub fn server_json(s: &servers::Model) -> String {
    json!({
        "id": s.id.0, "uuid": s.uuid, "identifier": s.uuid_short, "name": s.name,
        "description": s.description, "owner_id": s.owner_id.0, "node_id": s.node_id.0,
        "egg_id": s.egg_id.0, "status": s.status,
    })
    .to_string()
}

/// `/items/{id}` style patterns; a trailing `*` matches the rest.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let got: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    for (i, p) in pat.iter().enumerate() {
        if *p == "*" {
            return true;
        }
        match got.get(i) {
            Some(g) if (p.starts_with('{') && p.ends_with('}')) || p == g => {}
            _ => return false,
        }
    }
    pat.len() == got.len()
}

/// Whether the plugin registered `(scope, method, path)`.
pub fn registered(id: &str, scope: RouteScope, method: &str, path: &str) -> bool {
    pelican_plugins::runtime::global().and_then(|rt| rt.get(id)).is_some_and(|l| {
        l.registration.routes.iter().any(|r| {
            r.scope == scope
                && (r.method == "*" || r.method.eq_ignore_ascii_case(method))
                && path_matches(&r.path, path)
        })
    })
}

const DROPPED_HEADERS: &[&str] = &["authorization", "cookie", "x-csrf-token", "host", "content-length"];

/// Call the plugin with a request below its mount point.
pub async fn call(
    id: &str,
    scope: RouteScope,
    req: Request,
    rest: &str,
    locale: String,
    user: Option<String>,
    server: Option<String>,
) -> Result<Response, StatusCode> {
    let rt = pelican_plugins::runtime::global().ok_or(StatusCode::NOT_FOUND)?;
    let method = req.method().as_str().to_string();
    let path = format!("/{}", rest.trim_start_matches('/'));
    if !registered(id, scope, &method, &path) {
        return Err(StatusCode::NOT_FOUND);
    }
    let query = req.uri().query().unwrap_or("").to_string();
    let headers: Vec<Header> = req
        .headers()
        .iter()
        .filter(|(k, _)| !DROPPED_HEADERS.contains(&k.as_str()))
        .map(|(k, v)| Header { name: k.to_string(), value: v.to_str().unwrap_or("").to_string() })
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 8 << 20).await.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let request =
        pelican_plugins::runtime::Request { method, path, query, headers, body: body.to_vec(), locale, user, server };
    let resp = rt.handle(id, request).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut out = Response::new(Body::from(resp.body));
    *out.status_mut() = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    for h in resp.headers {
        let name = h.name.to_ascii_lowercase();
        if matches!(name.as_str(), "set-cookie" | "transfer-encoding" | "connection" | "content-length") {
            continue;
        }
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(&h.value)) {
            out.headers_mut().append(k, v);
        }
    }
    Ok(out)
}

fn not_found(status: StatusCode) -> ApiError {
    if status == StatusCode::NOT_FOUND { ApiError::model_not_found() } else { ApiError::internal("PluginException") }
}

fn rest_of(req: &Request, marker: &str) -> String {
    let path = req.uri().path();
    path.split_once(marker)
        .map(|(_, r)| r.split_once('/').map(|(_, rest)| rest).unwrap_or(""))
        .unwrap_or("")
        .to_string()
}

/// `/api/client/plugins/{plugin}/*` and `/api/client/servers/{server}/plugins/{plugin}/*`.
pub async fn client(State(_st): State<AppState>, Extension(ctx): Extension<Ctx>, req: Request) -> ApiResult<Response> {
    let user = ctx.user()?;
    let id = ctx.bound.param("plugin").unwrap_or_default().to_string();
    let server = ctx.bound.server().ok().map(server_json);
    let rest = rest_of(&req, &format!("/plugins/{id}"));
    let (locale, user_doc) = (user.user.language.clone(), user_json(&user.user, user.is_root_admin()));
    call(&id, RouteScope::Client, req, &rest, locale, Some(user_doc), server).await.map_err(not_found)
}

/// `/api/application/plugins/{plugin}/api/*` (`plugin` read for GET/HEAD, write otherwise).
pub async fn application(
    State(_st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    req: Request,
) -> ApiResult<Response> {
    let write = !matches!(req.method().as_str(), "GET" | "HEAD");
    ctx.acl("plugin", if write { WRITE } else { READ })?;
    let user = ctx.user()?;
    let id = ctx.bound.param("plugin").unwrap_or_default().to_string();
    let rest = rest_of(&req, &format!("/plugins/{id}/api"));
    let (locale, user_doc) = (user.user.language.clone(), user_json(&user.user, user.is_root_admin()));
    call(&id, RouteScope::Application, req, &rest, locale, Some(user_doc), None).await.map_err(not_found)
}

#[cfg(test)]
mod tests {
    use super::path_matches;

    #[test]
    fn patterns() {
        assert!(path_matches("/count", "/count"));
        assert!(path_matches("/items/{id}", "/items/5"));
        assert!(!path_matches("/items/{id}", "/items"));
        assert!(path_matches("/files/*", "/files/a/b"));
        assert!(!path_matches("/count", "/count/x"));
    }
}
