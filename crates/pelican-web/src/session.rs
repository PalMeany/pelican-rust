//! Database sessions (`sessions` table, ADR-0013): our own JSON payload,
//! one row per browser session, `last_activity` refreshed on every request.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine;
use pelican_core::state::AppState;
use pelican_db::entities::sessions;
use pelican_db::{Int, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A one-shot message shown on the next page (Filament notification / toast).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Flash {
    pub status: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionData {
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub flash: Vec<Flash>,
    /// Where to go after logging in (`redirect()->intended()`).
    #[serde(default)]
    pub intended: Option<String>,
    #[serde(default)]
    pub remember: bool,
    /// User id waiting for its MFA challenge.
    #[serde(default)]
    pub mfa_pending: Option<i64>,
    /// Free-form per-page state (old input, validation errors, wizard steps).
    #[serde(default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Default)]
struct Inner {
    id: String,
    data: SessionData,
    regenerate: bool,
    destroy: bool,
}

/// Request-scoped handle (cloned into handlers through extensions).
#[derive(Clone, Debug)]
pub struct Session(Arc<Mutex<Inner>>);

fn random_id(n: usize) -> String {
    pelican_wings::jwt::str_random(n)
}

impl Session {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn data(&self) -> SessionData {
        self.lock().data.clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut SessionData)) {
        f(&mut self.lock().data)
    }

    pub fn user_id(&self) -> Option<i64> {
        self.lock().data.user_id
    }

    pub fn csrf(&self) -> String {
        self.lock().data.csrf.clone()
    }

    pub fn flash(&self, status: &str, title: impl Into<String>, body: Option<String>) {
        self.update(|d| d.flash.push(Flash { status: status.into(), title: title.into(), body }));
    }

    pub fn take_flash(&self) -> Vec<Flash> {
        std::mem::take(&mut self.lock().data.flash)
    }

    /// Log in: new session id (fixation protection), fresh CSRF token.
    pub fn login(&self, user_id: i64, remember: bool) {
        let mut g = self.lock();
        g.data.user_id = Some(user_id);
        g.data.remember = remember;
        g.data.mfa_pending = None;
        g.data.csrf = random_id(40);
        g.regenerate = true;
    }

    pub fn logout(&self) {
        let mut g = self.lock();
        g.data = SessionData { csrf: random_id(40), ..Default::default() };
        g.destroy = true;
    }
}

fn cookie_value(req: &Request, name: &str) -> Option<String> {
    let raw = req.headers().get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').map(str::trim).find_map(|c| c.strip_prefix(name)?.strip_prefix('=').map(str::to_string))
}

fn decode(payload: &str) -> Option<SessionData> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn encode(d: &SessionData) -> String {
    base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(d).unwrap_or_default())
}

/// "Remember me" cookie lifetime: 5 years, like Laravel's recaller.
const REMEMBER_SECONDS: i64 = 5 * 365 * 24 * 3600;

fn lifetime_minutes(st: &AppState) -> i64 {
    st.config.env.int_or("SESSION_LIFETIME", 120)
}

/// Middleware: load/create the session, run the request, persist and set the cookie.
pub async fn layer(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let now = chrono::Utc::now().timestamp();
    let name = st.config.session_cookie.clone();
    let mut inner = Inner::default();
    if let Some(id) =
        cookie_value(&req, &name).filter(|v| v.len() == 40 && v.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        if let Ok(Some(row)) = sessions::Entity::find_by_id(id.clone()).one(&st.db).await {
            let fresh = |d: &SessionData| d.remember || row.last_activity.0 >= now - lifetime_minutes(&st) * 60;
            if let Some(data) = decode(&row.payload).filter(fresh) {
                inner.id = id;
                inner.data = data;
            }
        }
    }
    if inner.id.is_empty() {
        inner.id = random_id(40);
        inner.data.csrf = random_id(40);
    }
    let old_id = inner.id.clone();
    let session = Session(Arc::new(Mutex::new(inner)));
    let ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()).map(|peer| {
        pelican_core::net::client_ip(
            peer,
            req.headers().get("x-forwarded-for").and_then(|v| v.to_str().ok()),
            &st.config.trusted_proxies,
        )
        .to_string()
    });
    let ua = req.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string);
    req.extensions_mut().insert(session.clone());
    let mut resp = next.run(req).await;

    let (id, data) = {
        let mut g = session.lock();
        if g.regenerate || g.destroy {
            g.id = random_id(40);
        }
        (g.id.clone(), g.data.clone())
    };
    if id != old_id {
        let _ = sessions::Entity::delete_by_id(old_id).exec(&st.db).await;
    }
    persist(&st, &id, &data, ip, ua, now).await;
    // Laravel's session GC lottery (config/session.php `lottery` [2, 100]).
    if rand::random_range(0..100) < 2 {
        tokio::spawn(gc(st.clone(), now - lifetime_minutes(&st) * 60));
    }
    let secure = st.config.app_url.starts_with("https://");
    let max_age = if data.remember { format!("; Max-Age={REMEMBER_SECONDS}") } else { String::new() };
    let cookie =
        format!("{name}={id}; Path=/; HttpOnly; SameSite=Lax{}{max_age}", if secure { "; Secure" } else { "" });
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// Drop expired sessions; "remember me" sessions only expire with their cookie.
async fn gc(st: AppState, cutoff: i64) {
    let b = st.db.get_database_backend();
    let rows = sessions::Entity::find()
        .filter(sessions::Column::LastActivity.lte(cutoff))
        .limit(500)
        .all(&st.db)
        .await
        .unwrap_or_default();
    let remember_cutoff = cutoff - REMEMBER_SECONDS;
    let ids: Vec<sea_orm::Value> = rows
        .into_iter()
        .filter(|r| r.last_activity.0 <= remember_cutoff || !decode(&r.payload).is_some_and(|d| d.remember))
        .map(|r| r.id.into())
        .collect();
    if ids.is_empty() {
        return;
    }
    let marks = vec!["?"; ids.len()].join(", ");
    let _ = st.db.execute_raw(raw::stmt(b, &format!("delete from {{sessions}} where {{id}} in ({marks})"), ids)).await;
}

async fn persist(st: &AppState, id: &str, data: &SessionData, ip: Option<String>, ua: Option<String>, now: i64) {
    let payload = encode(data);
    let b = st.db.get_database_backend();
    let updated = st
        .db
        .execute_raw(raw::stmt(
            b,
            "update {sessions} set {user_id} = ?, {ip_address} = ?, {user_agent} = ?, {payload} = ?, {last_activity} = ? where {id} = ?",
            vec![
                data.user_id.into(),
                ip.clone().into(),
                ua.clone().into(),
                payload.clone().into(),
                now.into(),
                id.into(),
            ],
        ))
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
    if updated == 0 {
        let am = sessions::ActiveModel {
            id: Set(id.to_string()),
            user_id: Set(data.user_id.map(Int)),
            ip_address: Set(ip),
            user_agent: Set(ua),
            payload: Set(payload),
            last_activity: Set(Int(now)),
        };
        if let Err(e) = sessions::Entity::insert(am).exec_without_returning(&st.db).await {
            tracing::warn!(error = %e, "storing session failed");
        }
    }
}
