//! `App\Services\Helpers\SoftwareVersionService`: current build version and the
//! latest Panel/Wings releases from GitHub (cached `panel.cdn.cache_time` minutes;
//! failures are not cached).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pelican_core::state::AppState;

/// `config('app.version')`: this build is a development build of the panel.
pub const APP_VERSION: &str = "canary";

static LATEST: Mutex<Option<HashMap<&'static str, (Instant, String)>>> = Mutex::new(None);

/// `currentPanelVersion()`: display form ("canary (…)"); never compare it.
pub fn current_panel_version() -> String {
    format!("{APP_VERSION} (pelican-rs {})", env!("CARGO_PKG_VERSION"))
}

/// `currentComparableVersion()`: null for canary builds.
pub fn current_comparable_version() -> Option<&'static str> {
    (APP_VERSION != "canary").then_some(APP_VERSION)
}

pub async fn latest_panel_version(st: &AppState) -> Option<String> {
    latest(st, "panel").await
}

pub async fn latest_wings_version(st: &AppState) -> Option<String> {
    latest(st, "wings").await
}

/// `isLatestPanel()`: unknown versions count as latest.
pub async fn is_latest_panel(st: &AppState) -> bool {
    match current_comparable_version() {
        None => true,
        Some(current) => match latest_panel_version(st).await {
            None => true,
            Some(latest) => pelican_core::php::version_compare(current, &latest).is_ge(),
        },
    }
}

/// `isLatestWings($version)`.
pub async fn is_latest_wings(st: &AppState, version: &str) -> bool {
    if version == "develop" {
        return true;
    }
    match latest_wings_version(st).await {
        None => true,
        Some(latest) => pelican_core::php::version_compare(version, &latest).is_ge(),
    }
}

async fn latest(st: &AppState, repo: &'static str) -> Option<String> {
    let ttl = Duration::from_secs(60 * st.config.env.int_or("PANEL_CDN_CACHE_TIME", 60).max(0) as u64);
    if let Ok(guard) = LATEST.lock() {
        if let Some((at, v)) = guard.as_ref().and_then(|m| m.get(repo)) {
            if at.elapsed() < ttl {
                return Some(v.clone());
            }
        }
    }
    let version = fetch(repo).await?;
    if let Ok(mut guard) = LATEST.lock() {
        guard.get_or_insert_with(HashMap::new).insert(repo, (Instant::now(), version.clone()));
    }
    Some(version)
}

/// `Http::timeout(5)->connectTimeout(1)->get(…/releases/latest)->throw()->json()['tag_name']`.
async fn fetch(repo: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(1))
        .user_agent("GuzzleHttp/7")
        .build()
        .ok()?;
    let resp = client.get(format!("https://api.github.com/repos/pelican/{repo}/releases/latest")).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    Some(json.get("tag_name")?.as_str()?.trim_matches('v').to_string())
}
