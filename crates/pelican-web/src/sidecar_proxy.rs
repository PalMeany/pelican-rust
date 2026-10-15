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

/// The web router's fallback: proxy to the sidecar or answer 404. A plugin's Filament page is
/// shown inside pelican-rs's own panel layout ([`transplant`]).
pub async fn fallback(web: Web, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let Some(sc) = sidecar::get(&web.st).filter(|_| sidecar::owns(&path)) else {
        return WebError::NotFound.into_response();
    };
    let h = req.headers();
    let page_request = req.method() == axum::http::Method::GET
        && !h.contains_key("x-livewire")
        && !h.contains_key("hx-request")
        && h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
    let resp = match forward(&sc, &web, req).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, path, "sidecar proxy failed");
            return (StatusCode::SERVICE_UNAVAILABLE, "The plugin service is unavailable.").into_response();
        }
    };
    let html = resp.status() == StatusCode::OK
        && resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/html"));
    if !(page_request && html && web.user.is_some()) {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 32 * 1024 * 1024).await else {
        return (StatusCode::BAD_GATEWAY, "The plugin service sent an unreadable page.").into_response();
    };
    let page = String::from_utf8_lossy(&bytes);
    let Some(t) = transplant(&page) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let mut out = match in_panel(web, &path, t).await {
        Some(r) => r,
        None => return Response::from_parts(parts, Body::from(bytes)),
    };
    // The sidecar's session and XSRF cookies (Livewire needs them on the next request).
    for c in parts.headers.get_all(header::SET_COOKIE) {
        out.headers_mut().append(header::SET_COOKIE, c.clone());
    }
    out
}

/// A sidecar page fetched for a pelican-rs page that embeds it (the plugins' dashboard widgets):
/// the user's own request headers (sidecar cookies), the page cut out ([`transplant`]) and the
/// cookies the sidecar sets, which the embedding page passes on.
pub async fn embedded(
    web: &Web,
    headers: &axum::http::HeaderMap,
    path: &str,
) -> Option<(Transplant, Vec<HeaderValue>)> {
    let sc = sidecar::get(&web.st)?;
    let mut req = Request::builder().method(axum::http::Method::GET).uri(path);
    for (name, value) in headers {
        if matches!(name.as_str(), "cookie" | "user-agent" | "accept-language") {
            req = req.header(name, value);
        }
    }
    let req = req.header(header::ACCEPT, "text/html").body(Body::empty()).ok()?;
    let resp = forward(&sc, web, req).await.ok()?;
    if resp.status() != StatusCode::OK {
        return None;
    }
    let cookies: Vec<HeaderValue> = resp.headers().get_all(header::SET_COOKIE).iter().cloned().collect();
    let bytes = axum::body::to_bytes(resp.into_body(), 16 * 1024 * 1024).await.ok()?;
    let page = String::from_utf8_lossy(&bytes);
    Some((transplant(&page)?, cookies))
}

/// What [`embedded_detached`] needs of a request to stand in for it later (background refreshes).
#[derive(Clone)]
pub struct Origin {
    st: pelican_core::state::AppState,
    secure: bool,
    ip: String,
    host: String,
    user: Option<i64>,
    cookie: Option<HeaderValue>,
    user_agent: Option<HeaderValue>,
    accept_language: Option<HeaderValue>,
}

impl Origin {
    pub fn of(web: &Web, headers: &axum::http::HeaderMap) -> Origin {
        Origin {
            st: web.st.clone(),
            secure: web.secure,
            ip: web.ip.clone(),
            host: web.host.clone(),
            user: web.user.as_ref().map(|u| u.id()),
            cookie: headers.get(header::COOKIE).cloned(),
            user_agent: headers.get(header::USER_AGENT).cloned(),
            accept_language: headers.get(header::ACCEPT_LANGUAGE).cloned(),
        }
    }
}

/// [`embedded`] outside a request: the same signed GET with the request's cookies, its
/// Set-Cookie and sign-in handoffs ignored (they belong to the request that is long answered).
pub async fn embedded_detached(o: Origin, path: String) -> Option<Transplant> {
    let sc = sidecar::get(&o.st)?;
    let mut rb = client().get(format!("{}{path}", sc.url)).header(header::ACCEPT, "text/html");
    if let Some(c) = o.cookie.as_ref().and_then(|c| c.to_str().ok()) {
        let cookies = forwarded_cookies(c, &o.st.config.session_cookie);
        if !cookies.is_empty() {
            rb = rb.header(header::COOKIE, cookies);
        }
    }
    for (name, value) in [(header::USER_AGENT, &o.user_agent), (header::ACCEPT_LANGUAGE, &o.accept_language)] {
        if let Some(v) = value {
            rb = rb.header(name, v.clone());
        }
    }
    rb = rb
        .header("x-forwarded-for", o.ip.as_str())
        .header("x-forwarded-proto", if o.secure { "https" } else { "http" })
        .header("x-forwarded-host", o.host.as_str());
    for (k, v) in sc.signature("PROXY", "/", o.user, b"") {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.ok()?;
    if resp.status() != reqwest::StatusCode::OK {
        return None;
    }
    transplant(&resp.text().await.ok()?)
}

/// A plugin's Filament page cut out of the sidecar's layout: its head (styles, scripts), the
/// page (`main.fi-main`, as a `div`) and what follows the layout in `body` (Livewire, Filament's
/// scripts and notifications). Cut along marker comments so the markup stays as sent.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transplant {
    pub title: Option<String>,
    pub head: String,
    pub main: String,
    pub tail: String,
}

const HEAD_START: &str = "<!--pelican-rs:head-->";
const HEAD_END: &str = "<!--/pelican-rs:head-->";
const MAIN_START: &str = "<!--pelican-rs:main-->";
const MAIN_END: &str = "<!--/pelican-rs:main-->";
const TAIL_START: &str = "<!--pelican-rs:tail-->";

pub fn transplant(html: &str) -> Option<Transplant> {
    use lol_html::html_content::ContentType;
    use lol_html::{RewriteStrSettings, element, rewrite_str, text};
    if !html.contains("fi-layout") {
        return None;
    }
    let title = std::cell::RefCell::new(String::new());
    let out = rewrite_str(
        html,
        RewriteStrSettings {
            element_content_handlers: vec![
                element!("head", |el| {
                    el.prepend(HEAD_START, ContentType::Html);
                    el.append(HEAD_END, ContentType::Html);
                    Ok(())
                }),
                text!("head > title", |t| {
                    title.borrow_mut().push_str(t.as_str());
                    Ok(())
                }),
                // pelican-rs's page has its own; the sidecar's CSRF token is on Livewire's script tag.
                element!(
                    "head > title, head > meta[charset], head > meta[name=viewport], head > meta[name=csrf-token], head > link[rel=icon], head > link[rel=\"shortcut icon\"]",
                    |el| {
                        el.remove();
                        Ok(())
                    }
                ),
                element!("main.fi-main", |el| {
                    el.before(MAIN_START, ContentType::Html);
                    el.after(MAIN_END, ContentType::Html);
                    el.set_tag_name("div")?;
                    Ok(())
                }),
                element!("body > .fi-layout", |el| {
                    el.after(TAIL_START, ContentType::Html);
                    Ok(())
                }),
                // The sidebar's notification slide-over (pelican-rs has its own bell).
                element!("#database-notifications", |el| {
                    el.remove();
                    Ok(())
                }),
            ],
            ..RewriteStrSettings::new()
        },
    )
    .ok()?;
    let between = |a: &str, b: &str| -> Option<String> {
        let start = out.find(a)? + a.len();
        let end = start + out[start..].find(b)?;
        Some(out[start..end].to_string())
    };
    let tail_start = out.find(TAIL_START)? + TAIL_START.len();
    let tail_end = out.rfind("</body>").filter(|e| *e >= tail_start).unwrap_or(out.len());
    // Blade indents the title over several lines.
    let title = title.into_inner().split_whitespace().collect::<Vec<_>>().join(" ");
    Some(Transplant {
        title: (!title.is_empty()).then(|| html_unescape(&title)),
        head: between(HEAD_START, HEAD_END)?,
        main: between(MAIN_START, MAIN_END)?,
        tail: out[tail_start..tail_end].to_string(),
    })
}

/// The entities Blade's `{{ }}` produces (`htmlspecialchars`).
fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#039;", "'").replace("&amp;", "&")
}

/// Render a transplanted page in the layout of its panel: the server panel (`/server/{id}/…`),
/// the admin panel (`/admin/…`), else the app panel. None when the user may not see that panel.
async fn in_panel(web: Web, path: &str, t: Transplant) -> Option<Response> {
    const TPL: &str = "plugins/sidecar_page.html";
    let user = web.user.clone()?;
    let authed = crate::ctx::Authed { web, user };
    let app_name = authed.st.config.app_name.clone();
    let title = t.title.map(|s| s.strip_suffix(&format!(" - {app_name}")).map(str::to_string).unwrap_or(s));
    let ctx = minijinja::context! {
        title => title,
        sidecar_head => t.head,
        sidecar_main => t.main,
        sidecar_tail => t.tail,
        // The page shows the plugins' alert banners itself (PanelsRenderHook::PAGE_START).
        plugin_banners => Vec::<String>::new(),
    };
    let mut segs = path.trim_start_matches('/').split('/');
    let rendered = match segs.next() {
        Some("server") => {
            let panel = crate::server::load(authed, segs.next()?).await.ok()?;
            panel.page(path, TPL, ctx)
        }
        Some("admin") => {
            if !authed.user.is_admin() {
                return None;
            }
            crate::admin::Admin(authed).page(path, TPL, ctx).await
        }
        _ => authed.render(TPL, ctx),
    };
    Some(rendered.unwrap_or_else(IntoResponse::into_response))
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
    fn transplant_cuts_the_page_out_of_the_layout() {
        let html = r#"<!DOCTYPE html><html class="fi"><head><meta charset="utf-8"><meta name="csrf-token" content="x"><title>Players - Nuvira panel</title><link rel="stylesheet" href="/css/filament/filament/app.css"><style>.a{}</style></head><body class="fi-body"><a class="fi-skip-link">skip</a><div class="fi-layout"><aside class="fi-sidebar">nav</aside><div class="fi-main-ctn"><main id="fi-main-content" class="fi-main"><div wire:id="1" wire:snapshot="{&quot;a&quot;:1}"><template x-if="open"><b>x</b></template></div></main><footer>f</footer></div></div><div wire:id="2">toasts</div><div id="database-notifications">bell</div><script src="/livewire/livewire.min.js" data-csrf="t"></script></body></html>"#;
        let t = super::transplant(html).expect("a Filament page");
        assert_eq!(t.title.as_deref(), Some("Players - Nuvira panel"));
        assert_eq!(t.head, r#"<link rel="stylesheet" href="/css/filament/filament/app.css"><style>.a{}</style>"#);
        assert_eq!(
            t.main,
            r#"<div id="fi-main-content" class="fi-main"><div wire:id="1" wire:snapshot="{&quot;a&quot;:1}"><template x-if="open"><b>x</b></template></div></div>"#
        );
        assert_eq!(
            t.tail,
            r#"<div wire:id="2">toasts</div><script src="/livewire/livewire.min.js" data-csrf="t"></script>"#
        );
        assert!(super::transplant("<html><body>plain plugin route</body></html>").is_none());
    }

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
