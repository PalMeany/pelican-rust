//! Web UI of pelican-rs (ADR-0005, ADR-0013): server-rendered minijinja pages
//! with htmx, sharing the domain services of the APIs.

pub mod activity;
pub mod assets;
mod auth;
pub mod cache;
pub mod csrf;
pub mod ctx;
pub mod error;
pub mod forms;
pub mod i18n;
pub mod installer;
pub mod models;
pub mod server_info;
pub mod session;
pub mod table;
pub mod variables;
pub mod views;
pub mod words;

mod admin;
mod app;
mod server;

use axum::Router;
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{get, post};
use pelican_core::state::AppState;

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// `SetSecurityHeaders` middleware of the original.
async fn security_headers(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let mut resp = next.run(req).await;
    for (k, v) in [
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("x-xss-protection", "1; mode=block"),
        ("referrer-policy", "no-referrer-when-downgrade"),
    ] {
        let name = HeaderName::from_static(k);
        if !resp.headers().contains_key(&name) {
            resp.headers_mut().insert(name, HeaderValue::from_static(v));
        }
    }
    resp
}

/// Everything outside `/api/*`.
pub fn router(state: AppState) -> Router {
    views::init(state.config.env.string("PELICAN_VIEWS_PATH").map(Into::into));
    let pages = Router::new()
        .route("/login", get(auth::show).post(auth::login))
        .route("/login/challenge", post(auth::challenge))
        .route("/login/challenge/resend", post(auth::resend))
        .route("/login/cancel", post(auth::cancel))
        .route("/logout", post(auth::logout))
        .route("/auth/oauth/redirect/{driver}", get(auth::oauth::redirect))
        .route("/auth/oauth/callback/{driver}", get(auth::oauth::callback))
        .route("/password-reset/request", get(auth::password_reset::request_form).post(auth::password_reset::request))
        .route("/password-reset/reset", get(auth::password_reset::reset_form).post(auth::password_reset::reset))
        .route("/installer/complete", get(installer_complete))
        .merge(app::routes())
        .merge(admin::routes())
        .merge(server::routes())
        .fallback(error::not_found)
        .layer(from_fn(csrf::layer))
        .layer(from_fn_with_state(state.clone(), session::layer));
    Router::new()
        .route("/assets/{*path}", get(assets::serve))
        .route(
            "/pelican.ico",
            get(|| assets::serve(axum::extract::Path("pelican.ico".into()), axum::extract::RawQuery(None))),
        )
        .route("/storage/{*path}", get(models::serve_storage))
        .merge(pages)
        .layer(from_fn(security_headers))
        .with_state(state)
}

/// The installer's last redirect: sign the new admin in (`auth()->login($user, true)`)
/// and open the admin panel. Without a valid one-time token this is the login page.
async fn installer_complete(web: ctx::Web) -> axum::response::Response {
    let token = web
        .query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "token").map(|(_, v)| v.into_owned()))
        .unwrap_or_default();
    match installer::take_login(&token) {
        Some(user_id) => {
            web.session.login(user_id, true);
            web.redirect("/admin")
        }
        None => web.redirect("/login"),
    }
}
