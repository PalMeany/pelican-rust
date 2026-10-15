//! Requests for paths the PHP sidecar owns (ADR-0020): its plugins' routes and Filament
//! pages, Livewire and the front-end assets those pages load. pelican-rs adds who is
//! signed in (signed `X-Pelican-Rs-*` headers) and streams the request and the answer.

use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pelican_api::sidecar;

use crate::ctx::Web;
use crate::error::WebError;

/// Hop-by-hop and pelican-rs-owned request headers that are not forwarded.
const DROPPED: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "content-length",
    "upgrade",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-forwarded-port",
];

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default()
    })
}

/// The web router's fallback: proxy to the sidecar or answer 404.
pub async fn fallback(web: Web, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(sc) = sidecar::get(&web.st).filter(|_| sidecar::owns(&path)) else {
        return WebError::NotFound.into_response();
    };
    match forward(&sc, &web, req).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, path, "sidecar proxy failed");
            (StatusCode::SERVICE_UNAVAILABLE, "The plugin service is unavailable.").into_response()
        }
    }
}

/// Proxy a request a pelican-rs route handler gives up to the sidecar (plugin OAuth
/// providers), optionally under another path.
pub async fn proxy(web: &Web, mut req: Request, path: Option<&str>) -> Response {
    let Some(sc) = sidecar::get(&web.st) else {
        return WebError::NotFound.into_response();
    };
    if let Some(p) = path {
        let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        if let Ok(uri) = format!("{p}{query}").parse() {
            *req.uri_mut() = uri;
        }
    }
    match forward(&sc, web, req).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "sidecar proxy failed");
            (StatusCode::SERVICE_UNAVAILABLE, "The plugin service is unavailable.").into_response()
        }
    }
}

/// Our session cookie stays here; everything else (the sidecar's session, XSRF) goes along.
fn forwarded_cookies(raw: &str, own: &str) -> String {
    raw.split(';')
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.split('=').next() != Some(own))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn forward(sc: &sidecar::Sidecar, web: &Web, req: Request) -> Result<Response, reqwest::Error> {
    let (parts, body) = req.into_parts();
    let path_query = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut rb = client().request(parts.method.clone(), format!("{}{path_query}", sc.url));
    for (name, value) in &parts.headers {
        let n = name.as_str();
        if DROPPED.contains(&n) || n.starts_with("x-pelican-rs-") {
            continue;
        }
        if name == header::COOKIE {
            let cookies = forwarded_cookies(value.to_str().unwrap_or(""), &web.st.config.session_cookie);
            if !cookies.is_empty() {
                rb = rb.header(header::COOKIE, cookies);
            }
            continue;
        }
        rb = rb.header(name, value);
    }
    let scheme = if web.secure { "https" } else { "http" };
    rb = rb
        .header("x-forwarded-for", web.ip.as_str())
        .header("x-forwarded-proto", scheme)
        .header("x-forwarded-host", web.host.as_str());
    // Method and path are not signed: Livewire re-runs page middleware on a rebuilt request.
    for (k, v) in sc.signature("PROXY", "/", web.user.as_ref().map(|u| u.id()), b"") {
        rb = rb.header(k, v);
    }
    let upstream = rb.body(reqwest::Body::wrap_stream(body.into_data_stream())).send().await?;
    // A sign-in inside the sidecar (plugin OAuth provider, plugin registration page): the
    // session is pelican-rs's (`auth()->login($user, true)`).
    if let Some(id) = upstream
        .headers()
        .get("x-pelican-rs-login")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| sidecar::verify_login_handoff(&web.st, v))
    {
        web.session.login(id, true);
    }

    let mut out = Response::builder().status(upstream.status().as_u16());
    if let Some(headers) = out.headers_mut() {
        for (name, value) in upstream.headers() {
            if matches!(name.as_str(), "connection" | "transfer-encoding" | "keep-alive" | "x-pelican-rs-login") {
                continue;
            }
            if let (Ok(n), Ok(v)) =
                (HeaderName::from_bytes(name.as_str().as_bytes()), HeaderValue::from_bytes(value.as_bytes()))
            {
                headers.append(n, v);
            }
        }
    }
    Ok(out.body(Body::from_stream(upstream.bytes_stream())).unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_session_cookie_stays() {
        assert_eq!(
            super::forwarded_cookies("pelican_session=abc; pelican_sidecar_session=x; XSRF-TOKEN=t", "pelican_session"),
            "pelican_sidecar_session=x; XSRF-TOKEN=t"
        );
        assert_eq!(super::forwarded_cookies("pelican_session=abc", "pelican_session"), "");
    }
}

/// Navigation entries of PHP plugins' pages for one Filament panel (`admin`, `server`,
/// `app`) that `user` may open (on server `tenant`), by sort.
pub async fn nav(
    st: &pelican_core::state::AppState,
    panel: &str,
    user: i64,
    tenant: Option<&str>,
) -> Vec<sidecar::NavEntry> {
    let mut out = sidecar::user_navigation(st, panel, user, tenant).await;
    out.sort_by_key(|n| n.sort.unwrap_or(0));
    out
}

/// Filament icons are Heroicons (`o-hand-raised`); use the Tabler icon of the same name
/// when there is one.
pub fn icon(name: Option<&str>) -> &'static str {
    let name = name.map(|n| n.trim_start_matches("heroicon-").trim_start_matches("o-").trim_start_matches("s-"));
    crate::plugin_pages::icon(name)
}
