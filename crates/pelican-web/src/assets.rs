//! Embedded static assets with content-hash cache busting (ADR-0013).

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::extract::{Path, RawQuery};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

struct Asset {
    body: &'static [u8],
    hash: String,
}

static INDEX: LazyLock<HashMap<&'static str, Asset>> = LazyLock::new(|| {
    crate::ASSETS
        .iter()
        .map(|(name, body)| {
            let hash = hex::encode(&Sha256::digest(body)[..6]);
            (*name, Asset { body, hash })
        })
        .collect()
});

/// Public URL of an asset, versioned by its content hash.
pub fn url(path: &str) -> String {
    let path = path.trim_start_matches('/');
    match INDEX.get(path) {
        Some(a) => format!("/assets/{path}?v={}", a.hash),
        None => format!("/assets/{path}"),
    }
}

/// Tabler icon SVG (`tabler-user` or `user`) from `assets/icons/`.
/// The `'static` name of an embedded icon (plugin navigation icons are dynamic strings).
pub fn icon_key(name: &str) -> Option<&'static str> {
    let name = name.strip_prefix("tabler-").unwrap_or(name);
    let (key, _) = INDEX.get_key_value(format!("icons/{name}.svg").as_str())?;
    key.strip_prefix("icons/")?.strip_suffix(".svg")
}

pub fn icon_svg(name: &str) -> Option<String> {
    let name = name.strip_prefix("tabler-").unwrap_or(name);
    INDEX.get(format!("icons/{name}.svg").as_str()).map(|a| String::from_utf8_lossy(a.body).into_owned())
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

pub async fn serve(Path(path): Path<String>, RawQuery(query): RawQuery) -> Response {
    let Some(asset) = INDEX.get(path.as_str()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let versioned = query.as_deref().and_then(|q| q.strip_prefix("v=")) == Some(asset.hash.as_str());
    let cache = if versioned { "public, max-age=31536000, immutable" } else { "public, max-age=300" };
    let mut resp = asset.body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&path)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", asset.hash)) {
        h.insert(header::ETAG, etag);
    }
    resp
}
