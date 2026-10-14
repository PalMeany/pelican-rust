//! Middleware pipelines of the API route groups, in the original order
//! (client-api.md §1, application-api.md, remote-api.md):
//!
//! client:      bindings → stateful → auth:sanctum → IsValidJson → TrackAPIKey →
//!              AuthenticateIPAccess → RequireClientApiKey → throttle:api.client →
//!              subject/server-access → route throttle
//! application: bindings → stateful → auth:sanctum → IsValidJson → TrackAPIKey →
//!              AuthenticateIPAccess → AuthenticateApplicationUser → throttle:api.application
//! remote:      bindings → DaemonAuthenticate

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, MatchedPath, RawPathParams, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use pelican_core::activity::{Activity, morph};
use pelican_core::auth::{self, AuthUser, Credential, KEY_TYPE_APPLICATION};
use pelican_core::net::{IpRange, client_ip};
use pelican_core::rate_limit::Throttle;
use pelican_core::state::AppState;
use pelican_db::entities::users;
use sea_orm::EntityTrait;

use crate::binding::{self, Bound};
use crate::error::{ApiError, ApiResult, Kind};

/// Per-request context stored in request extensions.
#[derive(Clone)]
pub struct Ctx {
    pub ip: IpAddr,
    pub user: Option<Arc<AuthUser>>,
    pub bound: Arc<Bound>,
    pub node: Option<Arc<pelican_db::entities::nodes::Model>>,
    /// Laravel `Request::url()` base: `{scheme}://{host}`.
    pub root: String,
}

impl Ctx {
    pub fn user(&self) -> ApiResult<&AuthUser> {
        self.user.as_deref().ok_or_else(ApiError::unauthenticated)
    }

    pub fn api_key_id(&self) -> Option<i64> {
        self.user.as_ref().and_then(|u| u.api_key()).map(|k| k.id.0)
    }

    /// Activity entry with ip, actor and api key filled like ActivityLogService.
    pub fn activity(&self, event: &str) -> Activity {
        let mut a = Activity::new(event, self.ip.to_string()).api_key(self.api_key_id());
        if let Some(u) = &self.user {
            a = a.actor(morph::USER, u.id());
        }
        a
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Client,
    Application,
    Remote,
}

fn request_root(headers: &HeaderMap, trusted_peer: bool) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let proto = trusted_peer
        .then(|| {
            headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
        })
        .flatten()
        .unwrap_or("http");
    format!("{proto}://{host}")
}

fn peer_ip(req: &Request) -> IpAddr {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::from([127, 0, 0, 1]))
}

pub async fn client(
    st: State<AppState>,
    mp: Option<MatchedPath>,
    params: RawPathParams,
    req: Request,
    next: Next,
) -> Response {
    run(st, Group::Client, mp, params, req, next).await
}

pub async fn application(
    st: State<AppState>,
    mp: Option<MatchedPath>,
    params: RawPathParams,
    req: Request,
    next: Next,
) -> Response {
    run(st, Group::Application, mp, params, req, next).await
}

pub async fn remote(
    st: State<AppState>,
    mp: Option<MatchedPath>,
    params: RawPathParams,
    req: Request,
    next: Next,
) -> Response {
    run(st, Group::Remote, mp, params, req, next).await
}

async fn run(
    State(st): State<AppState>,
    group: Group,
    mp: Option<MatchedPath>,
    params: RawPathParams,
    req: Request,
    next: Next,
) -> Response {
    let template = mp
        .as_ref()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    match pipeline(&st, group, &template, params, req, next).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn pipeline(
    st: &AppState,
    group: Group,
    template: &str,
    params: RawPathParams,
    mut req: Request,
    next: Next,
) -> ApiResult<Response> {
    let peer = peer_ip(&req);
    let ip = client_ip(
        peer,
        req.headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok()),
        &st.config.trusted_proxies,
    );
    let trusted_peer = ip != peer
        || st
            .config
            .trusted_proxies
            .iter()
            .any(|t| t == "*" || t == "**");
    let root = request_root(req.headers(), trusted_peer);
    let now = pelican_core::time::now();

    // 1. Route-model binding (SubstituteBindings runs first).
    let bound = binding::resolve(st, group, &params).await?;

    if group == Group::Remote {
        let node = daemon_authenticate(st, req.headers()).await?;
        req.extensions_mut().insert(Ctx {
            ip,
            user: None,
            bound: Arc::new(bound),
            node: Some(Arc::new(node)),
            root,
        });
        return Ok(next.run(req).await);
    }

    // 2-3. auth:sanctum (session support arrives with the web UI, ADR-0009).
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_token);
    let Some(token) = token else {
        return Err(ApiError::unauthenticated());
    };
    let Some(key) = auth::find_api_key(&st.db, &st.enc, &token, now).await? else {
        return Err(ApiError::unauthenticated());
    };
    let Some(user_row) = (match key.user_id {
        Some(uid) => users::Entity::find_by_id(uid).one(&st.db).await?,
        None => None,
    }) else {
        return Err(ApiError::unauthenticated());
    };
    auth::touch_last_used(&st.db, &key, now).await?;
    let user = AuthUser::load(&st.db, user_row, Credential::ApiKey(key.clone())).await?;

    // 4. IsValidJson
    let req = valid_json(req).await?;

    // 6. AuthenticateIPAccess
    let allowed: Vec<String> = match &key.allowed_ips.0 {
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    if !allowed.is_empty() {
        let mut ok = false;
        for entry in &allowed {
            match IpRange::parse(entry) {
                Some(r) if r.contains(&ip) => {
                    ok = true;
                    break;
                }
                Some(_) => {}
                None => return Err(ApiError::internal("InvalidArgumentException")),
            }
        }
        if !ok {
            Activity::new("auth:ip-blocked", ip.to_string())
                .api_key(Some(key.id.0))
                .actor(morph::USER, user.id())
                .subject(morph::USER, user.id())
                .subject(morph::API_KEY, key.id.0)
                .property(
                    "identifier",
                    key.identifier
                        .clone()
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                )
                .save(&st.db)
                .await?;
            return Err(ApiError::access_denied(format!(
                "This IP address ({ip}) does not have permission to access the API using these credentials."
            )));
        }
    }

    // 7. RequireClientApiKey / AuthenticateApplicationUser
    match group {
        Group::Client if key.key_type.0 == KEY_TYPE_APPLICATION => {
            return Err(ApiError::access_denied(
                "You are attempting to use an application API key on an endpoint that requires a client API key.",
            ));
        }
        Group::Application if !user.is_admin() => {
            return Err(ApiError::access_denied(
                "This account does not have permission to access the API.",
            ));
        }
        _ => {}
    }

    // 8. throttle:api.client / api.application, keyed by user uuid.
    let (name, limit) = match group {
        Group::Client => ("api.client", st.config.rate_limits.client),
        _ => ("api.application", st.config.rate_limits.application),
    };
    let unix = chrono::Utc::now().timestamp();
    let throttle = st.limiter.attempt(
        &format!("{name}|{}", user.user.uuid),
        limit.max,
        limit.decay_seconds(),
        unix,
    );
    if let Throttle::Exceeded {
        limit,
        retry_after,
        reset_at,
    } = throttle
    {
        return Err(too_many(limit, retry_after, reset_at));
    }

    let mut req = req;
    req.extensions_mut().insert(Ctx {
        ip,
        user: Some(Arc::new(user)),
        bound: Arc::new(bound),
        node: None,
        root,
    });
    let _ = template;
    let mut resp = next.run(req).await;
    if let Throttle::Allowed { limit, remaining } = throttle {
        add_rate_limit_headers(resp.headers_mut(), limit, remaining);
    }
    Ok(resp)
}

pub fn too_many(limit: u32, retry_after: i64, reset_at: i64) -> ApiError {
    ApiError::new(
        429,
        "ThrottleRequestsException",
        "Too Many Attempts.",
        crate::error::Style::Pretty,
    )
    .with_header("x-ratelimit-limit", limit)
    .with_header("x-ratelimit-remaining", 0)
    .with_header("retry-after", retry_after)
    .with_header("x-ratelimit-reset", reset_at)
}

/// `ThrottleRequests::addHeaders`: an outer limiter does not overwrite a more
/// restrictive (or equal) remaining count already set by an inner one.
pub fn add_rate_limit_headers(h: &mut HeaderMap, limit: u32, remaining: i64) {
    if let Some(existing) = h
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        if existing <= remaining {
            return;
        }
    }
    h.insert("x-ratelimit-limit", HeaderValue::from(limit));
    h.insert("x-ratelimit-remaining", HeaderValue::from(remaining));
}

/// `IsValidJson`: malformed JSON bodies with a JSON content type → 400.
async fn valid_json(req: Request) -> ApiResult<Request> {
    let is_json = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("/json") || ct.contains("+json"));
    if !is_json {
        return Ok(req);
    }
    let (parts, body) = req.into_parts();
    let bytes: Bytes = body
        .collect()
        .await
        .map_err(|_| ApiError::bad_request("Invalid request body."))?
        .to_bytes();
    if !(bytes.is_empty() || bytes.as_ref() == b"0") {
        if let Err(msg) = php_json_decode_check(&bytes) {
            return Err(ApiError::bad_request(format!(
                "The JSON data passed in the request appears to be malformed: {msg}"
            )));
        }
    }
    Ok(Request::from_parts(parts, Body::from(bytes)))
}

/// Error message of `json_decode(..., 512, JSON_THROW_ON_ERROR)` (`json_last_error_msg`).
fn php_json_decode_check(bytes: &[u8]) -> Result<(), &'static str> {
    if std::str::from_utf8(bytes).is_err() {
        return Err("Malformed UTF-8 characters, possibly incorrectly encoded");
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(_) => Ok(()),
        Err(e) => {
            let m = e.to_string();
            if m.contains("control character") {
                Err("Control character error, possibly incorrectly encoded")
            } else if m.contains("recursion limit") {
                Err("Maximum stack depth exceeded")
            } else {
                Err("Syntax error")
            }
        }
    }
}

/// `DaemonAuthenticate` (remote-api.md §1).
async fn daemon_authenticate(
    st: &AppState,
    headers: &HeaderMap,
) -> ApiResult<pelican_db::entities::nodes::Model> {
    use pelican_db::entities::nodes;
    use sea_orm::{ColumnTrait, QueryFilter};
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(auth::bearer_token);
    let Some(token) = token else {
        return Err(ApiError::http(
            401,
            "Access to this endpoint must include an Authorization header.",
        )
        .with_header("www-authenticate", "Bearer"));
    };
    let parts: Vec<&str> = token.split('.').collect();
    // PHP empty(): "" and "0" are empty.
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || *p == "0") {
        return Err(ApiError::bad_request(
            "The Authorization header provided was not in a valid format.",
        ));
    }
    let node = nodes::Entity::find()
        .filter(nodes::Column::DaemonTokenId.eq(parts[0]))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    let secret = st
        .enc
        .decrypt_string_lenient(&node.daemon_token)
        .map_err(|_| ApiError::internal("DecryptException"))?;
    if !laravel_compat::hashing::constant_time_eq(secret.as_bytes(), parts[1].as_bytes()) {
        return Err(ApiError::access_denied(
            "You are not authorized to access this resource.",
        ));
    }
    Ok(node)
}

/// Laravel only renders JSON errors when the client expects JSON; otherwise
/// guests are redirected to the login page (client-api.md §6.1).
pub async fn negotiate_errors(req: Request, next: Next) -> Response {
    let expects_json = expects_json(req.headers());
    let root = request_root(req.headers(), true);
    let referer = req
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_head = req.method() == Method::HEAD;
    let mut resp = next.run(req).await;
    if !expects_json {
        if let Some(kind) = resp.extensions().get::<Kind>().cloned() {
            match kind {
                Kind::Unauthenticated => return redirect(&format!("{root}/login")),
                Kind::Validation => return redirect(&referer.unwrap_or_else(|| root.clone())),
                Kind::Other => {}
            }
        }
    }
    if is_head {
        *resp.body_mut() = Body::empty();
    }
    resp
}

/// `expectsJson()` = (ajax && !pjax && acceptsAnyContentType) || wantsJson.
pub fn expects_json(h: &HeaderMap) -> bool {
    let accept = h
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let first = accept
        .split(',')
        .next()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let wants_json = first.contains("/json") || first.contains("+json");
    let ajax = h.get("x-requested-with").and_then(|v| v.to_str().ok()) == Some("XMLHttpRequest");
    let pjax = h.contains_key("x-pjax");
    let any = accept.is_empty() || first == "*/*" || first == "*";
    (ajax && !pjax && any) || wants_json
}

/// Symfony `RedirectResponse` (302 with the meta-refresh HTML body).
pub fn redirect(url: &str) -> Response {
    let esc = url
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&#039;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        "<!DOCTYPE html>\n<html>\n    <head>\n        <meta charset=\"UTF-8\" />\n        <meta http-equiv=\"refresh\" content=\"0;url='{esc}'\" />\n\n        <title>Redirecting to {esc}</title>\n    </head>\n    <body>\n        Redirecting to <a href=\"{esc}\">{esc}</a>.\n    </body>\n</html>"
    );
    let mut r = (axum::http::StatusCode::FOUND, body).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(url) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}
