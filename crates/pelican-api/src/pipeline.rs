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
use crate::services::LogActivity;

/// Per-request context stored in request extensions.
#[derive(Clone)]
pub struct Ctx {
    pub ip: IpAddr,
    pub user: Option<Arc<AuthUser>>,
    pub bound: Arc<Bound>,
    pub node: Option<Arc<pelican_db::entities::nodes::Model>>,
    /// Laravel `Request::url()` base: `{scheme}://{host}`.
    pub root: String,
    /// Default activity subject (`AccountSubject` / `ServerSubject`).
    pub subject: Option<(&'static str, i64)>,
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
        if let Some((kind, id)) = self.subject {
            a = a.subject(kind, id);
        }
        a
    }

    /// `HAS(perm)`: `$user->can($perm, $server)` for the bound server.
    pub async fn can(&self, st: &AppState, permission: &str) -> ApiResult<bool> {
        let user = self.user()?;
        let server = self.bound.server()?;
        Ok(pelican_core::permissions::has_server_permission(&st.db, user, server, permission).await?)
    }

    /// `GATE(perm)`: Gate with array arguments (include resolvers, activity).
    pub async fn gate(&self, st: &AppState, permission: &str) -> ApiResult<bool> {
        let user = self.user()?;
        let server = self.bound.server()?;
        Ok(pelican_core::permissions::gate_server_permission(&st.db, user, server, permission).await?)
    }

    /// FormRequest `authorize()` with a `permission()`: 403 when missing.
    pub async fn authorize(&self, st: &AppState, permission: &str) -> ApiResult<()> {
        if self.can(st, permission).await? { Ok(()) } else { Err(ApiError::unauthorized_action()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Client,
    Application,
    Remote,
}

/// `EnsureStatefulRequests::fromFrontend()`: the session cookie, or Sanctum's check of
/// Referer/Origin against `sanctum.stateful` (SANCTUM_STATEFUL_DOMAINS, by default local
/// hosts plus APP_URL's host and port).
fn stateful(st: &AppState, headers: &HeaderMap) -> bool {
    let cookies = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if cookies.split(';').any(|c| c.trim().split('=').next() == Some(st.config.session_cookie.as_str())) {
        return true;
    }
    let Some(from) = headers
        .get(header::REFERER)
        .or_else(|| headers.get(header::ORIGIN))
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    else {
        return false;
    };
    let mut domain = from.replacen("https://", "", 1).replacen("http://", "", 1);
    if !domain.ends_with('/') {
        domain.push('/');
    }
    let domains = match st.config.env.string("SANCTUM_STATEFUL_DOMAINS") {
        Some(list) => list,
        None => {
            // `Sanctum::currentApplicationUrlWithPort()`: APP_URL's host and explicit port.
            let rest = st.config.app_url.split_once("://").map_or(st.config.app_url.as_str(), |(_, r)| r);
            let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
            let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
            let current = if authority.is_empty() { String::new() } else { format!(",{authority}") };
            format!("localhost,localhost:3000,127.0.0.1,127.0.0.1:8000,::1{current}")
        }
    };
    // `Str::is("<domain>/*", ...)`: the pattern's prefix up to the wildcard.
    domains.split(',').map(str::trim).filter(|d| !d.is_empty()).any(|d| domain.starts_with(&format!("{d}/")))
}

/// The web session of a stateful request (pelican-rs sessions, ADR-0013): CSRF check for
/// unsafe methods (`X-CSRF-TOKEN`, as `VerifyCsrfToken`; a missing or unknown session has no
/// matching token: 419), then the signed-in user, if any.
async fn session_user(
    st: &AppState,
    headers: &HeaderMap,
    method: &Method,
    now: chrono::NaiveDateTime,
) -> ApiResult<Option<users::Model>> {
    use pelican_db::entities::sessions;
    use sea_orm::{ColumnTrait, QueryFilter};
    let name = &st.config.session_cookie;
    let id = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').map(str::trim).find_map(|c| c.strip_prefix(name.as_str())?.strip_prefix('=')))
        .filter(|v| v.len() == 40 && v.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_string);
    let unix = now.and_utc().timestamp();
    let lifetime = st.config.env.int_or("SESSION_LIFETIME", 120) * 60;
    let mut session: Option<(String, serde_json::Value)> = None;
    if let Some(id) = id {
        if let Some(row) = sessions::Entity::find_by_id(id.clone()).one(&st.db).await? {
            use base64::Engine;
            let data = base64::engine::general_purpose::STANDARD
                .decode(&row.payload)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
            if let Some(data) = data {
                let remember = data.get("remember").and_then(serde_json::Value::as_bool).unwrap_or(false);
                if remember || row.last_activity.0 >= unix - lifetime {
                    session = Some((id, data));
                }
            }
        }
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        let sent = headers.get("x-csrf-token").and_then(|v| v.to_str().ok());
        let expected = session.as_ref().and_then(|(_, d)| d.get("csrf")).and_then(serde_json::Value::as_str);
        if sent.is_none() || sent != expected {
            return Err(ApiError::http(419, "CSRF token mismatch."));
        }
    }
    let Some((id, data)) = session else { return Ok(None) };
    let Some(uid) = data.get("user_id").and_then(serde_json::Value::as_i64) else { return Ok(None) };
    // StartSession saves the session on every request: keep it alive.
    sessions::Entity::update_many()
        .col_expr(sessions::Column::LastActivity, sea_orm::sea_query::Expr::value(pelican_db::Int(unix)))
        .filter(sessions::Column::Id.eq(id))
        .exec(&st.db)
        .await?;
    Ok(users::Entity::find_by_id(uid).one(&st.db).await?)
}

fn request_root(headers: &HeaderMap, trusted_peer: bool) -> String {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    let proto = trusted_peer
        .then(|| headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()))
        .flatten()
        .unwrap_or("http");
    format!("{proto}://{host}")
}

fn peer_ip(req: &Request) -> IpAddr {
    req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()).unwrap_or(IpAddr::from([127, 0, 0, 1]))
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
    let template = mp.as_ref().map(|m| m.as_str().to_string()).unwrap_or_default();
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
    let ip =
        client_ip(peer, req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()), &st.config.trusted_proxies);
    let trusted_peer = ip != peer || st.config.trusted_proxies.iter().any(|t| t == "*" || t == "**");
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
            subject: None,
        });
        return Ok(next.run(req).await);
    }

    // 2-3. EnsureStatefulRequests + auth:sanctum: a request from the panel's own front end
    // (session cookie, or a stateful domain in Referer/Origin) runs with the web session first
    // (CSRF on unsafe methods; its user = Sanctum TransientToken), otherwise the bearer key.
    let session_user = if stateful(st, req.headers()) {
        let (headers, method) = (req.headers().clone(), req.method().clone());
        session_user(st, &headers, &method, now).await?
    } else {
        None
    };
    let user = match session_user {
        Some(u) => AuthUser::load(&st.db, u, Credential::Session).await?,
        None => {
            let token =
                req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(auth::bearer_token);
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
            AuthUser::load(&st.db, user_row, Credential::ApiKey(key)).await?
        }
    };

    // 4. IsValidJson
    let req = valid_json(req).await?;

    // 6. AuthenticateIPAccess (API keys; a TransientToken passes).
    let allowed: Vec<String> = match user.api_key().map(|k| &k.allowed_ips.0) {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    if let Some(key) = user.api_key().filter(|_| !allowed.is_empty()) {
        let mut ok = false;
        for entry in &allowed {
            match IpRange::try_parse(entry) {
                Ok(r) if r.contains(&ip) => {
                    ok = true;
                    break;
                }
                Ok(_) => {}
                Err(pelican_core::net::IpParseError::InvalidFormat) => return Err(ApiError::internal("IpException")),
                Err(pelican_core::net::IpParseError::Other(_)) => return Err(ApiError::internal("NetworkException")),
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
                    key.identifier.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
                )
                .log(&st)
                .await?;
            return Err(ApiError::access_denied(format!(
                "This IP address ({ip}) does not have permission to access the API using these credentials."
            )));
        }
    }

    // 7. RequireClientApiKey / AuthenticateApplicationUser
    match group {
        Group::Client if user.api_key().is_some_and(|k| k.key_type.0 == KEY_TYPE_APPLICATION) => {
            return Err(ApiError::access_denied(
                "You are attempting to use an application API key on an endpoint that requires a client API key.",
            ));
        }
        Group::Application if !user.is_admin() => {
            return Err(ApiError::access_denied("This account does not have permission to access the API."));
        }
        _ => {}
    }

    // 8. throttle:api.client / api.application, keyed by user uuid.
    let (name, limit) = match group {
        Group::Client => ("api.client", st.config.rate_limits.client),
        _ => ("api.application", st.config.rate_limits.application),
    };
    let unix = chrono::Utc::now().timestamp();
    let throttle = st.limiter.attempt(&format!("{name}|{}", user.user.uuid), limit.max, limit.decay_seconds(), unix);
    if let Throttle::Exceeded { limit, retry_after, reset_at } = throttle {
        return Err(too_many(limit, retry_after, reset_at));
    }

    let mut req = req;
    let method = req.method().clone();
    let user = Arc::new(user);
    let mut ctx = Ctx { ip, user: Some(user.clone()), bound: Arc::new(bound), node: None, root, subject: None };
    let inner = async {
        if group == Group::Client { client_route_checks(st, &mut ctx, &method, template, unix).await } else { Ok(None) }
    };
    let mut resp = match inner.await {
        Err(e) => e.into_response(),
        Ok(route_throttle) => {
            req.extensions_mut().insert(ctx);
            let mut r = next.run(req).await;
            if let Some(Throttle::Allowed { limit, remaining }) = route_throttle {
                add_rate_limit_headers(r.headers_mut(), limit, remaining);
            }
            r
        }
    };
    if let Throttle::Allowed { limit, remaining } = throttle {
        add_rate_limit_headers(resp.headers_mut(), limit, remaining);
    }
    Ok(resp)
}

/// `ResourceLimit` (app/Enums/ResourceLimit.php): per-server throttles by route.
fn resource_limit(
    st: &AppState,
    method: &Method,
    template: &str,
) -> Option<(&'static str, pelican_core::config::RateLimit)> {
    let rl = &st.config.rate_limits;
    let t = template.strip_prefix("/api/client/servers/{server}").unwrap_or("");
    match (method.as_str(), t) {
        ("GET", "/websocket") => Some(("Websocket", rl.websocket)),
        ("POST", "/network/allocations") => Some(("AllocationCreate", rl.default)),
        ("POST", "/backups/{backup}/restore") => Some(("BackupRestore", rl.backup_restore)),
        ("POST", "/databases") => Some(("DatabaseCreate", rl.database_create)),
        ("POST", "/schedules") => Some(("ScheduleCreate", rl.default)),
        ("POST", "/users") => Some(("SubuserCreate", rl.subuser_create)),
        ("POST", "/files/pull") => Some(("FilePull", rl.file_pull)),
        _ => None,
    }
}

/// Steps 9–10 of the client pipeline: `AccountSubject` / `ServerSubject`,
/// `AuthenticateServerAccess`, then the per-route `ResourceLimit` throttle.
async fn client_route_checks(
    st: &AppState,
    ctx: &mut Ctx,
    method: &Method,
    template: &str,
    unix: i64,
) -> ApiResult<Option<Throttle>> {
    let user = ctx.user()?.clone();
    if template.starts_with("/api/client/account") {
        ctx.subject = Some((morph::USER, user.id()));
    }
    let Some(server) = ctx.bound.server.clone() else { return Ok(None) };
    ctx.subject = Some((morph::SERVER, server.id.0));
    // AuthenticateServerAccess
    let admin_update = user.gate_before("update server");
    if server.owner_id.0 != user.id()
        && !admin_update
        && pelican_core::permissions::subuser_permissions(&st.db, server.id.0, user.id()).await?.is_none()
    {
        return Err(ApiError::not_found("The requested resource does not exist on this server."));
    }
    if let Some(detail) = pelican_core::server_state::conflict(&st.db, &server).await? {
        let is_view = method == Method::GET && template == "/api/client/servers/{server}";
        let is_ws = template == "/api/client/servers/{server}/websocket";
        let is_files = template.starts_with("/api/client/servers/{server}/files/");
        if !(is_view || ((is_ws || is_files) && admin_update)) {
            return Err(ApiError::new(409, "ServerStateConflictException", detail, crate::error::Style::Pretty));
        }
    }
    if let Some((name, limit)) = resource_limit(st, method, template) {
        let key = format!("api.client:server-resource:{name}|{}", server.uuid);
        let t = st.limiter.attempt(&key, limit.max, limit.decay_seconds(), unix);
        if let Throttle::Exceeded { limit, retry_after, reset_at } = t {
            return Err(too_many(limit, retry_after, reset_at));
        }
        return Ok(Some(t));
    }
    Ok(None)
}

pub fn too_many(limit: u32, retry_after: i64, reset_at: i64) -> ApiError {
    ApiError::new(429, "ThrottleRequestsException", "Too Many Attempts.", crate::error::Style::Pretty)
        .with_header("x-ratelimit-limit", limit)
        .with_header("x-ratelimit-remaining", 0)
        .with_header("retry-after", retry_after)
        .with_header("x-ratelimit-reset", reset_at)
}

/// `ThrottleRequests::addHeaders`: an outer limiter does not overwrite a more
/// restrictive (or equal) remaining count already set by an inner one.
pub fn add_rate_limit_headers(h: &mut HeaderMap, limit: u32, remaining: i64) {
    if let Some(existing) =
        h.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok())
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
    let bytes: Bytes = body.collect().await.map_err(|_| ApiError::bad_request("Invalid request body."))?.to_bytes();
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
async fn daemon_authenticate(st: &AppState, headers: &HeaderMap) -> ApiResult<pelican_db::entities::nodes::Model> {
    use pelican_db::entities::nodes;
    use sea_orm::{ColumnTrait, QueryFilter};
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(auth::bearer_token);
    let Some(token) = token else {
        return Err(ApiError::http(401, "Access to this endpoint must include an Authorization header.")
            .with_header("www-authenticate", "Bearer"));
    };
    let parts: Vec<&str> = token.split('.').collect();
    // PHP empty(): "" and "0" are empty.
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty() || *p == "0") {
        return Err(ApiError::bad_request("The Authorization header provided was not in a valid format."));
    }
    let node = nodes::Entity::find()
        .filter(nodes::Column::DaemonTokenId.eq(parts[0]))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    let secret =
        st.enc.decrypt_string_lenient(&node.daemon_token).map_err(|_| ApiError::internal("DecryptException"))?;
    if !laravel_compat::hashing::constant_time_eq(secret.as_bytes(), parts[1].as_bytes()) {
        return Err(ApiError::access_denied("You are not authorized to access this resource."));
    }
    Ok(node)
}

/// Laravel only renders JSON errors when the client expects JSON; otherwise
/// guests are redirected to the login page (client-api.md §6.1).
pub async fn negotiate_errors(req: Request, next: Next) -> Response {
    let expects_json = expects_json(req.headers());
    let root = request_root(req.headers(), true);
    let referer = req.headers().get(header::REFERER).and_then(|v| v.to_str().ok()).map(str::to_string);
    let is_head = req.method() == Method::HEAD;
    let method = req.method().clone();
    let path = req.uri().path().trim_matches('/').to_string();
    // Symfony matches routes against the rawurldecoded path: an encoded `/`
    // adds a segment, so no route parameter can contain it.
    if path.to_ascii_lowercase().contains("%2f") {
        return ApiError::not_found(format!("The route {path} could not be found.")).into_response();
    }
    let mut resp = next.run(req).await;
    // axum's bare 405 → Laravel MethodNotAllowedHttpException (before any middleware).
    if resp.status() == axum::http::StatusCode::METHOD_NOT_ALLOWED && !resp.extensions().get::<Kind>().is_some() {
        let allow = resp.headers().get(header::ALLOW).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let mut methods: Vec<String> =
            allow.split(',').map(|m| m.trim().to_ascii_uppercase()).filter(|m| !m.is_empty()).collect();
        methods.sort_by_key(|m| {
            ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"].iter().position(|x| x == m).unwrap_or(99)
        });
        let detail = format!(
            "The {method} method is not supported for route {path}. Supported methods: {}.",
            methods.join(", ")
        );
        resp = ApiError::new(405, "MethodNotAllowedHttpException", detail, crate::error::Style::Pretty)
            .with_header("allow", methods.join(", "))
            .into_response();
    }
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
    let accept = h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let first = accept.split(',').next().unwrap_or("").split(';').next().unwrap_or("").trim();
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
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(url) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}
