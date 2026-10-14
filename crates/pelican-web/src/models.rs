//! Model accessors the UI needs (Allocation display address, HasIcon,
//! User customization) and the public storage disk (`/storage/*`).

use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, users};
use serde_json::{Map, Value, json};

/// `Allocation::$alias`.
pub fn alias(a: &allocations::Model) -> &str {
    a.ip_alias.as_deref().unwrap_or(&a.ip)
}

/// `Allocation::$address`.
pub fn address(a: &allocations::Model) -> String {
    let alias = alias(a);
    if alias.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{alias}]:{}", a.port.0)
    } else {
        format!("{alias}:{}", a.port.0)
    }
}

/// `Allocation::$display_address`.
pub fn display_address(a: &allocations::Model) -> String {
    if a.show_port.0 { address(a) } else { alias(a).to_string() }
}

/// `storage/app/public` of the panel installation.
pub fn public_disk(st: &AppState) -> PathBuf {
    st.config.base_path.join("storage/app/public")
}

/// `HasIcon::getIconAttribute()`: URL of `icons/<kind>/<uuid>.<png|jpg|webp>`, if present.
pub fn icon_url(st: &AppState, kind: &str, uuid: &str) -> Option<String> {
    let dir = public_disk(st).join("icons").join(kind);
    ["png", "jpg", "webp"].iter().find_map(|ext| {
        dir.join(format!("{uuid}.{ext}"))
            .is_file()
            .then(|| format!("{}/storage/icons/{kind}/{uuid}.{ext}", st.config.app_url))
    })
}

/// `User::getCustomization()` merged over `CustomizationKey::getDefaultCustomization()`.
pub fn customization(st: &AppState, user: &users::Model) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("console_rows".into(), json!(30));
    out.insert("console_font".into(), json!("monospace"));
    out.insert("console_font_size".into(), json!(14));
    out.insert("console_graph_period".into(), json!(30));
    out.insert("top_navigation".into(), json!(st.config.env.string_or("FILAMENT_DEFAULT_NAVIGATION", "sidebar")));
    out.insert("dashboard_layout".into(), json!("grid"));
    out.insert("button_style".into(), json!(true));
    out.insert("redirect_to_admin".into(), json!(false));
    out.insert("servers_per_page".into(), json!(0));
    let stored = match user.customization.as_ref().map(|j| &j.0) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    if let Value::Object(m) = stored {
        for (k, v) in m {
            out.insert(k, v);
        }
    }
    out
}

fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(base.join(rel))
}

/// `GET /storage/{path}`: the `public/storage` symlink of a Laravel install.
pub async fn serve_storage(State(st): State<AppState>, UrlPath(path): UrlPath<String>) -> Response {
    let Some(file) = safe_join(&public_disk(&st), &path) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(bytes) = tokio::fs::read(&file).await else { return StatusCode::NOT_FOUND.into_response() };
    let ct = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };
    let mut resp = bytes.into_response();
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    resp
}
