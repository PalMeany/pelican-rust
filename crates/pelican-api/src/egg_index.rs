//! The public egg index and egg update checks (`p:egg:update-index`,
//! `p:egg:check-updates`, `InstallEgg`). Results are kept in memory like the
//! original's cache entries (`eggs.index` forever, `eggs.<uuid>.update` one hour).

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use pelican_core::state::AppState;
use pelican_db::entities::eggs;
use sea_orm::EntityTrait;
use serde_json::Value;

/// `nest name => [download_url => egg name]` in index order.
pub type EggIndex = IndexMap<String, IndexMap<String, String>>;

static INDEX: RwLock<Option<EggIndex>> = RwLock::new(None);
static UPDATES: RwLock<Option<HashMap<String, (Instant, bool)>>> = RwLock::new(None);

pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/pelican-eggs/pelican-eggs.github.io/refs/heads/main/content/pelican.json";

fn client(timeout: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .connect_timeout(Duration::from_secs(1))
        .user_agent("GuzzleHttp/7")
        .build()
        .map_err(|e| e.to_string())
}

/// `cache()->get('eggs.index', [])`.
pub fn index() -> Option<EggIndex> {
    INDEX.read().ok().and_then(|g| g.clone())
}

/// `UpdateEggIndexCommand`: fetch `panel.cdn.egg_index_url` and store the index.
pub async fn update_index(st: &AppState) -> Result<EggIndex, String> {
    let url = st.config.env.string_or("PANEL_EGG_INDEX_URL", DEFAULT_INDEX_URL);
    let index = fetch_index(&url).await?;
    if let Ok(mut g) = INDEX.write() {
        *g = Some(index.clone());
    }
    Ok(index)
}

/// Download and group the index (`nests[].Eggs[]` → nest name → download URL → egg name).
pub async fn fetch_index(url: &str) -> Result<EggIndex, String> {
    let resp = client(5)?.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP request returned status code {}", resp.status().as_u16()));
    }
    let data: Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut index = EggIndex::new();
    for nest in data.get("nests").and_then(Value::as_array).into_iter().flatten() {
        let name = pelican_core::php::string(nest.get("nest_type").unwrap_or(&Value::Null));
        let mut eggs = IndexMap::new();
        for egg in nest.get("Eggs").and_then(Value::as_array).into_iter().flatten() {
            let url = pelican_core::php::string(egg.get("download_url").unwrap_or(&Value::Null));
            let egg_name = pelican_core::php::string(egg.get("egg").and_then(|e| e.get("name")).unwrap_or(&Value::Null));
            eggs.insert(url, egg_name);
        }
        index.insert(name, eggs);
    }
    Ok(index)
}

/// The index, fetching it first when it was never loaded (the import modal and installer do that).
pub async fn index_or_fetch(st: &AppState) -> EggIndex {
    match index() {
        Some(i) => i,
        None => update_index(st).await.unwrap_or_default(),
    }
}

/// `cache()->get("eggs.$uuid.update", false)`: an update was found for this egg.
pub fn has_update(uuid: &str) -> bool {
    UPDATES
        .read()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(uuid).copied()))
        .is_some_and(|(at, found)| found && at.elapsed() < Duration::from_secs(3600))
}

pub fn forget_update(uuid: &str) {
    if let Ok(mut g) = UPDATES.write() {
        if let Some(m) = g.as_mut() {
            m.remove(uuid);
        }
    }
}

/// `CheckEggUpdatesCommand`: compare each egg's export with its `update_url` content.
pub async fn check_updates(st: &AppState) {
    let Ok(all) = eggs::Entity::find().all(&st.db).await else { return };
    for egg in all {
        if let Err(e) = check(st, &egg).await {
            tracing::warn!(egg = %egg.name, error = %e, "egg update check failed");
        }
    }
}

async fn check(st: &AppState, egg: &eggs::Model) -> Result<(), String> {
    let Some(url) = egg.update_url.as_deref() else { return Ok(()) };
    let ext = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.path().rsplit_once('.').map(|(_, e)| e.to_lowercase()))
        .unwrap_or_default();
    if ext.is_empty() {
        return Err("Unsupported file format.".into());
    }
    let yaml = matches!(ext.as_str(), "yaml" | "yml");
    let doc =
        crate::application::eggs::export_document(st, egg).await.map_err(|e| format!("{:?}", e.errors.first()))?;
    let local: Value = if yaml {
        parse_yaml(&egg_format::export::to_yaml(&doc))?
    } else {
        serde_json::from_str(&egg_format::export::to_json(&doc).ok_or("JsonException")?).map_err(|e| e.to_string())?
    };
    let resp = client(5)?.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP request returned status code {}", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let remote: Value =
        if yaml { parse_yaml(&body)? } else { serde_json::from_str(&body).map_err(|e| e.to_string())? };
    // md5(json_encode(...)) of both without `exported_at`: same serialization → same hash.
    let strip = |mut v: Value| {
        if let Value::Object(m) = &mut v {
            m.shift_remove("exported_at");
        }
        serde_json::to_string(&v).unwrap_or_default()
    };
    let found = strip(local) != strip(remote);
    if let (Ok(mut g), Some(uuid)) = (UPDATES.write(), egg.uuid.clone()) {
        g.get_or_insert_with(HashMap::new).insert(uuid, (Instant::now(), found));
    }
    Ok(())
}

fn parse_yaml(s: &str) -> Result<Value, String> {
    egg_format::import::parse_yaml(s).map_err(|e| format!("{e:?}"))
}

/// `InstallEgg` job: import an egg from its download URL (failures are logged).
pub async fn install_from_url(st: &AppState, url: &str) {
    let result = async {
        let ext = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.path().rsplit_once('.').map(|(_, e)| e.to_lowercase()))
            .unwrap_or_default();
        if !matches!(ext.as_str(), "json" | "yaml" | "yml") {
            return Err("Unsupported file format.".to_string());
        }
        let resp = client(5)?.get(url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP request returned status code {}", resp.status().as_u16()));
        }
        let body = resp.text().await.map_err(|e| e.to_string())?;
        crate::application::eggs::import_egg(st, &body).await.map(|_| ()).map_err(|e| format!("{:?}", e.errors.first()))
    }
    .await;
    if let Err(e) = result {
        tracing::error!(url, error = %e, "Failed to install egg from URL");
    }
}
