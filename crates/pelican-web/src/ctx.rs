//! Request context for page handlers: session, signed-in user, rendering.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequestParts, OriginalUri};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use minijinja::{Value, context};
use pelican_core::auth::{AuthUser, Credential};
use pelican_core::state::AppState;
use pelican_db::entities::users;
use sea_orm::EntityTrait;
use serde::Serialize;

use crate::error::{WebError, WebResult};
use crate::session::Session;
use crate::views;

/// Every page request: state, session and (maybe) the signed-in user.
pub struct Web {
    pub st: AppState,
    pub session: Session,
    pub user: Option<AuthUser>,
    /// `HX-Request`: answer with fragments / `HX-Redirect`.
    pub hx: bool,
    pub path: String,
    pub query: Option<String>,
    /// `$request->ip()` (trusted proxies applied).
    pub ip: String,
    pub user_agent: Option<String>,
}

/// A page that requires a signed-in user (Filament `Authenticate`).
pub struct Authed {
    pub web: Web,
    pub user: AuthUser,
}

impl std::ops::Deref for Authed {
    type Target = Web;
    fn deref(&self) -> &Web {
        &self.web
    }
}

#[derive(Serialize)]
struct UserView<'a> {
    id: i64,
    uuid: &'a str,
    username: &'a str,
    email: &'a str,
    language: &'a str,
    timezone: &'a str,
    root_admin: bool,
    admin: bool,
    avatar: String,
}

/// Gravatar-like initials avatar (Filament's default `ui-avatars` provider, inlined).
fn avatar(name: &str) -> String {
    let initials: String = name
        .split([' ', '_', '-', '.'])
        .filter_map(|p| p.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    initials
}

impl Web {
    pub fn locale(&self) -> String {
        self.user
            .as_ref()
            .map(|u| u.user.language.clone())
            .unwrap_or_else(|| self.st.config.env.string_or("APP_LOCALE", "en"))
    }

    pub fn timezone(&self) -> String {
        self.user.as_ref().map(|u| u.user.timezone.clone()).unwrap_or_else(|| "UTC".into())
    }

    pub fn t(&self, key: &str) -> String {
        crate::i18n::trans(&self.locale(), key, &[])
    }

    pub fn t_with(&self, key: &str, params: &[(&str, &str)]) -> String {
        let p: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        crate::i18n::trans(&self.locale(), key, &p)
    }

    /// Variables every template sees.
    pub fn base(&self) -> Value {
        let user = self.user.as_ref().map(|u| {
            Value::from_serialize(UserView {
                id: u.id(),
                uuid: &u.user.uuid,
                username: &u.user.username,
                email: &u.user.email,
                language: &u.user.language,
                timezone: &u.user.timezone,
                root_admin: u.is_root_admin(),
                admin: u.is_admin(),
                avatar: avatar(&u.user.username),
            })
        });
        let flash = self.session.take_flash();
        context! {
            app_name => self.st.config.app_name.clone(),
            app_url => self.st.config.app_url.clone(),
            locale => self.locale(),
            timezone => self.timezone(),
            csrf => self.session.csrf(),
            user => user,
            flash => Value::from_serialize(&flash),
            binary_prefix => self.st.config.use_binary_prefix,
            path => self.path.clone(),
            hx => self.hx,
        }
    }

    /// Render a full page (or the `content` block for boosted htmx navigation).
    pub fn render(&self, name: &str, ctx: Value) -> WebResult {
        let merged = context! { ..ctx, ..self.base() };
        let html = views::render(name, merged)?;
        Ok(page(html))
    }

    /// Render a single block of a template (htmx partial swaps).
    pub fn render_block(&self, name: &str, block: &str, ctx: Value) -> WebResult {
        let merged = context! { ..ctx, ..self.base() };
        Ok(page(views::render_block(name, block, merged)?))
    }

    /// Redirect that also works for htmx requests (`HX-Redirect`).
    pub fn redirect(&self, to: &str) -> Response {
        if self.hx {
            let mut r = StatusCode::NO_CONTENT.into_response();
            if let Ok(v) = HeaderValue::from_str(to) {
                r.headers_mut().insert("hx-redirect", v);
            }
            return r;
        }
        Redirect::to(to).into_response()
    }

    /// `Activity::event(...)->withRequestMetadata()` with the signed-in user as actor.
    pub fn activity(&self, event: &str) -> pelican_core::activity::Activity {
        let mut a = pelican_core::activity::Activity::new(event, self.ip.clone())
            .property("ip", self.ip.clone())
            .property("useragent", self.user_agent.clone());
        if let Some(u) = &self.user {
            a = a.actor(pelican_core::activity::morph::USER, u.id());
        }
        a
    }

    pub fn flash_success(&self, title: impl Into<String>) {
        self.session.flash("success", title, None);
    }

    pub fn flash_danger(&self, title: impl Into<String>, body: Option<String>) {
        self.session.flash("danger", title, body);
    }
}

fn page(html: String) -> Response {
    let mut resp = Html(html).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, private"));
    resp
}

impl FromRequestParts<AppState> for Web {
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let session = parts.extensions.get::<Session>().cloned().ok_or_else(|| WebError::internal("no session"))?;
        let user = match session.user_id() {
            Some(id) => match users::Entity::find_by_id(id).one(&st.db).await? {
                Some(u) => Some(AuthUser::load(&st.db, u, Credential::Session).await?),
                None => {
                    session.logout();
                    None
                }
            },
            None => None,
        };
        let uri = parts.extensions.get::<OriginalUri>().map(|u| u.0.clone()).unwrap_or_else(|| parts.uri.clone());
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| {
                pelican_core::net::client_ip(
                    c.0.ip(),
                    parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()),
                    &st.config.trusted_proxies,
                )
                .to_string()
            })
            .unwrap_or_else(|| "127.0.0.1".into());
        Ok(Web {
            st: st.clone(),
            session,
            user,
            hx: parts.headers.contains_key("hx-request"),
            path: uri.path().to_string(),
            query: uri.query().map(str::to_string),
            ip,
            user_agent: parts.headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string),
        })
    }
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let web = Web::from_request_parts(parts, st).await.map_err(IntoResponse::into_response)?;
        let Some(user) = web.user.clone() else {
            if parts.method == axum::http::Method::GET && !web.hx {
                let intended = match &web.query {
                    Some(q) => format!("{}?{q}", web.path),
                    None => web.path.clone(),
                };
                web.session.update(|d| d.intended = Some(intended));
            }
            return Err(web.redirect("/login"));
        };
        Ok(Authed { web, user })
    }
}
