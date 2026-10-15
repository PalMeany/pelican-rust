//! `/docs/api` (routes/docs.php, Dedoc Scramble): the index page, and for signed-in users
//! (`Gate::define('viewApiDocs', fn () => true)`) the Stoplight Elements UI and the OpenAPI
//! documents of the Application and Client APIs. The APIs are the original's, so the
//! documents are the original's own (`php artisan scramble:export`, `data/api-docs/`),
//! with this panel's `APP_URL` as the server.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use pelican_core::state::AppState;

use crate::ctx::Web;
use crate::error::WebError;

const APPLICATION: &str = include_str!("../data/api-docs/application.json");
const CLIENT: &str = include_str!("../data/api-docs/client.json");

pub fn routes() -> Router<AppState> {
    Router::new().route("/docs/api", get(index)).route("/docs/api/{page}", get(page))
}

/// `Route::view('', 'docs.api-index')`.
async fn index() -> Html<&'static str> {
    Html(INDEX)
}

fn spec(web: &Web, api: &str) -> Option<String> {
    let raw = match api {
        "application" => APPLICATION,
        "client" => CLIENT,
        _ => return None,
    };
    Some(raw.replace("__APP_URL__", web.st.config.app_url.trim_end_matches('/')))
}

/// `/docs/api/{api}` (UI) and `/docs/api/{api}.json` (document); 403 for guests
/// (`RestrictedDocsAccess`).
async fn page(web: Web, Path(page): Path<String>) -> Result<Response, WebError> {
    let (api, json) = match page.strip_suffix(".json") {
        Some(api) => (api, true),
        None => (page.as_str(), false),
    };
    let doc = spec(&web, api).ok_or(WebError::NotFound)?;
    if web.user.is_none() {
        return Err(WebError::Forbidden);
    }
    if json {
        let mut resp = doc.into_response();
        resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        return Ok(resp);
    }
    // `@json($spec)` escapes the characters that could end the script element.
    let embedded = doc.replace('<', "\\u003C").replace('>', "\\u003E").replace('&', "\\u0026").replace('\'', "\\u0027");
    let title = crate::views::escape(&format!("{} - API Docs", web.st.config.app_name));
    Ok(Html(UI.replace("{{title}}", &title).replace("{{spec}}", &embedded)).into_response())
}

const INDEX: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>API Documentation</title>
    <style>
        body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
               background: #f3f4f6; font-family: ui-sans-serif, system-ui, sans-serif; }
        .card { background: #fff; box-shadow: 0 10px 15px -3px rgb(0 0 0 / .1); border-radius: .5rem;
                padding: 2rem; max-width: 28rem; width: 100%; text-align: center; }
        h2 { font-size: 1.25rem; font-weight: 700; color: #1f2937; margin: 0 0 1rem; }
        a { text-decoration: none; }
        .mb-2 { margin-bottom: .5rem; } .mb-4 { margin-bottom: 1rem; }
        .note { font-size: .875rem; margin-top: 1rem; }
        @media (prefers-color-scheme: dark) { body { background: #030712; } .card { background: #111827; } h2 { color: #fff; } }
    </style>
</head>
<body>
    <div class="card">
        <h2>API Documentation</h2>

        <div class="mb-2">
            <a href="/docs/api/application"><span>📘</span> <span style="color: #2563EB;">Application API</span></a>
        </div>

        <div class="mb-4">
            <a href="/docs/api/client"><span>📗</span> <span style="color: #10B981;">Client API</span></a>
        </div>

        <div class="note">
            <span style="color: #EAB308;">⚠️</span>
            <span style="color: #EF4444;">Note: You need to be logged in to view the API docs!</span>
        </div>
    </div>
</body>
</html>
"#;

/// `scramble::docs` with the default Elements renderer options.
const UI: &str = r#"<!doctype html>
<html lang="en" data-theme="light">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1, shrink-to-fit=no">
    <meta name="color-scheme" content="light">
    <title>{{title}}</title>

    <script src="https://unpkg.com/@stoplight/elements@8.4.2/web-components.min.js"></script>
    <link rel="stylesheet" href="https://unpkg.com/@stoplight/elements@8.4.2/styles.min.css">

    <style>
        html, body { margin:0; height:100%; }
        body { background-color: var(--color-canvas); }
    </style>
</head>
<body style="height: 100vh; overflow-y: hidden">
<elements-api
    id="docs"
    tryItCredentialsPolicy="include"
    layout="responsive"
    router="hash"
/>
<script>
    (async () => {
        const docs = document.getElementById('docs');
        docs.apiDescriptionDocument = {{spec}};
    })();
</script>
</body>
</html>
"#;
