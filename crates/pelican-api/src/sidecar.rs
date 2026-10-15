//! PHP sidecar for the original's plugins (ADR-0020): the original panel of the same
//! version runs next to pelican-rs with the `pelican-rs-bridge` plugin. This module signs
//! requests to it, keeps its manifest (what the PHP plugins registered), forwards events
//! and schedule tasks, and runs plugin lifecycle steps there. Request proxying lives in
//! `pelican-web` (`sidecar_proxy`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hmac::{Hmac, Mac};
use pelican_core::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{ApiError, ApiResult};

/// The bridge plugin shipped into the sidecar's `plugins/` (hidden from pelican-rs).
pub const BRIDGE_ID: &str = "pelican-rs-bridge";

const BRIDGE_FILES: &[(&str, &str)] = &[
    ("plugin.json", include_str!("../../../sidecar/pelican-rs-bridge/plugin.json")),
    ("src/BridgePlugin.php", include_str!("../../../sidecar/pelican-rs-bridge/src/BridgePlugin.php")),
    ("src/Origin.php", include_str!("../../../sidecar/pelican-rs-bridge/src/Origin.php")),
    ("src/Signature.php", include_str!("../../../sidecar/pelican-rs-bridge/src/Signature.php")),
    (
        "src/Providers/BridgeProvider.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Providers/BridgeProvider.php"),
    ),
    (
        "src/Http/Controllers/BridgeController.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Http/Controllers/BridgeController.php"),
    ),
    (
        "src/Http/Middleware/AuthenticateFromPelicanRs.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Http/Middleware/AuthenticateFromPelicanRs.php"),
    ),
    (
        "src/Filament/ConsoleActionHost.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Filament/ConsoleActionHost.php"),
    ),
    (
        "src/Filament/PluginSettingsHost.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Filament/PluginSettingsHost.php"),
    ),
    (
        "resources/views/plugin-settings.blade.php",
        include_str!("../../../sidecar/pelican-rs-bridge/resources/views/plugin-settings.blade.php"),
    ),
    (
        "resources/views/console-action.blade.php",
        include_str!("../../../sidecar/pelican-rs-bridge/resources/views/console-action.blade.php"),
    ),
    (
        "src/Http/Middleware/LoginHandoff.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Http/Middleware/LoginHandoff.php"),
    ),
    (
        "src/Http/Middleware/VerifyPelicanRs.php",
        include_str!("../../../sidecar/pelican-rs-bridge/src/Http/Middleware/VerifyPelicanRs.php"),
    ),
];

/// Paths of the Filament/Livewire front end the sidecar's pages load.
const ASSET_PREFIXES: &[&str] = &["/build/", "/css/", "/js/", "/fonts/", "/vendor/"];

/// After a failed call events are not forwarded for a while: an unreachable sidecar must
/// not hold up every write by a connect timeout. User actions still try.
static DOWN_UNTIL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
const DOWN_FOR: i64 = 30;

fn unix_now() -> i64 {
    pelican_core::time::now().and_utc().timestamp()
}

fn mark_down() {
    DOWN_UNTIL.store(unix_now() + DOWN_FOR, std::sync::atomic::Ordering::Relaxed);
}

fn is_down() -> bool {
    DOWN_UNTIL.load(std::sync::atomic::Ordering::Relaxed) > unix_now()
}

/// Sidecar connection settings (`PANEL_PHP_SIDECAR=<base url>`).
#[derive(Clone)]
pub struct Sidecar {
    pub url: String,
    secret: [u8; 32],
}

/// `PANEL_PHP_SIDECAR`; `None` when not configured.
pub fn get(st: &AppState) -> Option<Sidecar> {
    let url = st.config.env.string("PANEL_PHP_SIDECAR").filter(|u| !u.is_empty())?;
    let key = st.config.env.string("APP_KEY").unwrap_or_default();
    Some(Sidecar { url: url.trim_end_matches('/').to_string(), secret: secret(&key) })
}

/// `hash_hmac('sha256', 'pelican-rs-sidecar', APP_KEY, true)`.
fn secret(app_key: &str) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(app_key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"pelican-rs-sidecar");
    mac.finalize().into_bytes().into()
}

/// Sidecar failures surface as `SidecarException`; the detail goes to the log.
fn fail(detail: String) -> ApiError {
    tracing::warn!(%detail, "sidecar");
    ApiError::internal("SidecarException")
}

/// Laravel's `$request->path()` with a leading slash: no surrounding slashes.
pub fn canonical_path(path: &str) -> String {
    format!("/{}", path.trim_matches('/'))
}

impl Sidecar {
    /// `X-Pelican-Rs-*` headers for a request (see the bridge's `Signature`).
    pub fn signature(&self, method: &str, path: &str, user: Option<i64>, body: &[u8]) -> Vec<(&'static str, String)> {
        let ts = pelican_core::time::now().and_utc().timestamp().to_string();
        let user = user.map(|u| u.to_string()).unwrap_or_default();
        let message = [
            method.to_ascii_uppercase(),
            canonical_path(path),
            ts.clone(),
            user.clone(),
            hex::encode(Sha256::digest(body)),
        ]
        .join("\n");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(message.as_bytes());
        let mut out =
            vec![("x-pelican-rs-timestamp", ts), ("x-pelican-rs-signature", hex::encode(mac.finalize().into_bytes()))];
        if !user.is_empty() {
            out.push(("x-pelican-rs-user", user));
        }
        out
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default()
    }

    /// A signed JSON call to a bridge endpoint.
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> ApiResult<(u16, Value)> {
        self.call_as(method, path, None, body).await
    }

    /// [`Sidecar::call`] on behalf of a user (`X-Pelican-Rs-User`).
    async fn call_as(
        &self,
        method: reqwest::Method,
        path: &str,
        user: Option<i64>,
        body: Option<&Value>,
    ) -> ApiResult<(u16, Value)> {
        let bytes = body.map(|b| serde_json::to_vec(b).unwrap_or_default()).unwrap_or_default();
        let mut req = Self::client()
            .request(method.clone(), format!("{}{path}", self.url))
            .header("accept", "application/json")
            .header("content-type", "application/json");
        for (k, v) in self.signature(method.as_str(), path, user, &bytes) {
            req = req.header(k, v);
        }
        let resp = match req.body(bytes).send().await {
            Ok(r) => r,
            Err(e) => {
                mark_down();
                return Err(fail(format!("sidecar: {e}")));
            }
        };
        let status = resp.status().as_u16();
        let value = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, value))
    }
}

// --- manifest ------------------------------------------------------------------

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
    /// Livewire endpoints and Filament action downloads the plugins' pages use.
    #[serde(default)]
    pub framework_routes: Vec<String>,
    #[serde(default)]
    pub navigation: Vec<NavEntry>,
    #[serde(default)]
    pub listeners: Vec<String>,
    /// Filament render hooks registered by plugins (`panels::body.end`, `pelican::footer.end`, …).
    #[serde(default)]
    pub render_hooks: Vec<String>,
    /// A plugin acts when a panel boots (`boot()` sending alert banners, panel render hooks).
    #[serde(default)]
    pub boots: bool,
    /// Names of the console header actions plugins added (`Console::registerCustomHeaderActions()`).
    #[serde(default)]
    pub console_actions: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<TaskEntry>,
    #[serde(default)]
    pub features: Vec<FeatureEntry>,
    #[serde(default)]
    pub avatars: Vec<AvatarEntry>,
    /// OAuth providers registered by plugins (`OAuthService::register()`).
    #[serde(default)]
    pub oauth_providers: Vec<OAuthEntry>,
    /// Enabled PHP plugins with settings (`HasPluginSettings`), edited on the bridge's
    /// `/admin/rs-plugin-settings?plugin=<id>` page.
    #[serde(default)]
    pub plugin_settings: Vec<String>,
    /// `Role::$customPermissions` (`[]` when empty, an object otherwise).
    #[serde(default)]
    pub admin_permissions: Value,
    #[serde(default)]
    pub subuser_permissions: Vec<SubuserGroup>,
    #[serde(default)]
    pub api_resources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteEntry {
    pub plugin: String,
    pub methods: Vec<String>,
    pub uri: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NavEntry {
    pub plugin: String,
    pub panel: String,
    pub path: String,
    #[serde(default)]
    pub tenant: bool,
    pub label: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sort: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskEntry {
    pub plugin: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub payload_label: Option<String>,
    #[serde(default)]
    pub default_payload: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeatureEntry {
    pub plugin: String,
    pub id: String,
    pub listeners: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AvatarEntry {
    pub plugin: String,
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthEntry {
    pub plugin: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubuserGroup {
    pub name: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub translation_prefix: Option<String>,
}

static MANIFEST: RwLock<Option<Arc<Manifest>>> = RwLock::new(None);
static TRANSLATIONS: RwLock<Option<Arc<Value>>> = RwLock::new(None);

/// Translations of the PHP language plugins: locale → group → lines (`*` = JSON lines).
pub fn translations() -> Option<Arc<Value>> {
    TRANSLATIONS.read().ok().and_then(|t| t.clone())
}

/// The last manifest fetched from the sidecar.
pub fn manifest() -> Option<Arc<Manifest>> {
    MANIFEST.read().ok().and_then(|m| m.clone())
}

/// Re-read the manifest (after start-up and plugin changes). A failed fetch keeps the
/// previous one.
pub async fn refresh(st: &AppState) -> ApiResult<()> {
    let Some(sc) = get(st) else {
        if let Ok(mut m) = MANIFEST.write() {
            *m = None;
        }
        return Ok(());
    };
    let (status, body) = sc.call(reqwest::Method::GET, "/_bridge/manifest", None).await?;
    if status != 200 {
        return Err(fail(format!("sidecar manifest: HTTP {status}")));
    }
    // Cached per-user answers are dropped when the plugins changed (not on every minute's refresh).
    static LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let raw = body.to_string();
    let changed = LAST.lock().map(|mut l| l.replace(raw.clone()).as_deref() != Some(raw.as_str())).unwrap_or(true);
    let manifest: Manifest = serde_json::from_value(body).map_err(|e| fail(format!("sidecar manifest: {e}")))?;
    // The sidecar must be the original of the same version as the schema (ADR-0020).
    let ours = crate::application::plugins::config(st).panel_version_display;
    let theirs = manifest.version.clone().unwrap_or_default();
    let dev = |v: &str| v.is_empty() || v.contains("canary") || v.contains("dev");
    let accepted = dev(&ours) || dev(&theirs) || theirs.trim_start_matches('v') == ours.trim_start_matches('v');
    if !accepted {
        tracing::error!(sidecar = %theirs, panel = %ours, "the PHP sidecar runs another panel version; PHP plugins are off");
    }
    if let Ok(mut m) = MANIFEST.write() {
        *m = accepted.then(|| Arc::new(manifest));
    }
    if changed {
        clear_user_caches();
    }
    let lang = if accepted {
        match sc.call(reqwest::Method::GET, "/_bridge/lang", None).await? {
            (200, v @ Value::Object(_)) => Some(Arc::new(v)),
            _ => None,
        }
    } else {
        None
    };
    if let Ok(mut t) = TRANSLATIONS.write() {
        *t = lang;
    }
    Ok(())
}

/// Keep the manifest fresh: now and every minute (plugins may change in the sidecar too).
pub fn spawn_refresher(st: AppState) {
    if get(&st).is_none() {
        return;
    }
    tokio::spawn(async move {
        loop {
            if let Err(e) = refresh(&st).await {
                tracing::warn!(error = ?e, "sidecar manifest refresh failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

/// Laravel route URI pattern (`/admin/x/{record}/edit`, `{param?}`) against a path.
pub fn uri_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let got: Vec<&str> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let required = pat.iter().filter(|p| !(p.starts_with('{') && p.ends_with("?}"))).count();
    if got.len() < required || got.len() > pat.len() {
        return false;
    }
    pat.iter().zip(got.iter()).all(|(p, g)| (p.starts_with('{') && p.ends_with('}')) || p == g)
}

/// Whether a web path belongs to the sidecar: a PHP plugin's route or the front-end
/// assets its pages load.
pub fn owns(path: &str) -> bool {
    let Some(m) = manifest() else { return false };
    ASSET_PREFIXES.iter().any(|p| path.starts_with(p))
        || m.routes.iter().any(|r| uri_matches(&r.uri, path))
        || m.framework_routes.iter().any(|r| uri_matches(r, path))
}

/// `Str::is()` for listener names (`*` wildcards).
fn name_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == name;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Whether a PHP plugin listens to the event.
pub fn listens(event: &str) -> bool {
    manifest().is_some_and(|m| m.listeners.iter().any(|l| name_matches(l, event)))
}

/// Deliver an event to the PHP plugins' listeners (`body` = the bridge's event request).
pub async fn event(st: &AppState, event: &str, body: Value) {
    let Some(sc) = get(st) else { return };
    if !listens(event) {
        return;
    }
    if is_down() {
        tracing::debug!(event, "sidecar unavailable: event not forwarded");
        return;
    }
    match sc.call(reqwest::Method::POST, "/_bridge/event", Some(&body)).await {
        Ok((200, _)) => {}
        Ok((status, body)) => tracing::warn!(status, %body, event, "sidecar event failed"),
        Err(e) => tracing::warn!(error = ?e, event, "sidecar event failed"),
    }
}

/// `eloquent.*` for the PHP plugins: re-read by id, or rebuilt from `attributes` (deleted).
pub async fn model_event(st: &AppState, event: &str, data: &Value) {
    if !listens(event) {
        return;
    }
    let body = json!({ "event": event, "model": { "id": data.get("id"), "attributes": data } });
    self::event(st, event, body).await
}

/// `App\Events\ActivityLogged` for the PHP plugins. The log row is sent as raw
/// attributes: it may not be committed yet (`Activity::transaction`).
pub async fn activity_logged(st: &AppState, id: i64, a: &pelican_core::activity::Activity) {
    const EVENT: &str = "App\\Events\\ActivityLogged";
    if !listens(EVENT) {
        return;
    }
    let attributes = json!({
        "id": id,
        "event": a.event,
        "ip": a.ip,
        "description": a.description,
        "actor_type": a.actor.as_ref().map(|(t, _)| t),
        "actor_id": a.actor.as_ref().map(|(_, i)| i),
        "api_key_id": a.api_key_id,
        "properties": pelican_db::write::php_json(&a.properties),
        "timestamp": pelican_core::time::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    });
    let body = json!({ "event": EVENT, "args": [{ "model": "App\\Models\\ActivityLog", "attributes": attributes }] });
    self::event(st, EVENT, body).await
}

/// A schedule task of a type registered by a PHP plugin.
pub fn task_type(action: &str) -> Option<TaskEntry> {
    manifest().and_then(|m| m.tasks.iter().find(|t| t.id == action).cloned())
}

pub async fn run_task(st: &AppState, task_id: i64) -> ApiResult<()> {
    let sc = get(st).ok_or_else(|| fail("sidecar not configured".into()))?;
    match sc.call(reqwest::Method::POST, "/_bridge/task", Some(&json!({ "task": task_id }))).await? {
        (200, _) => Ok(()),
        (status, body) => Err(fail(format!(
            "sidecar task: HTTP {status}: {}",
            body.get("error").and_then(Value::as_str).unwrap_or_default()
        ))),
    }
}

/// `PluginService::{install,update,uninstall,enable,disable}Plugin` in the sidecar.
pub async fn lifecycle(st: &AppState, plugin: &str, action: &str, extra: Value) -> Result<(), String> {
    let sc = get(st).ok_or_else(|| "sidecar not configured".to_string())?;
    let mut body = json!({ "plugin": plugin, "action": action });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        b.extend(e);
    }
    let result = match sc.call(reqwest::Method::POST, "/_bridge/artisan", Some(&body)).await {
        Ok((200, _)) => Ok(()),
        Ok((status, body)) => Err(body
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("sidecar: HTTP {status}"))),
        Err(e) => Err(format!("{e:?}")),
    };
    if let Err(e) = refresh(st).await {
        tracing::warn!(error = ?e, "sidecar manifest refresh failed");
    }
    result
}

// --- the sidecar's plugins directory --------------------------------------------

/// `storage/sidecar/plugins`: the bridge plus links to the PHP plugins (WASM plugins
/// stay out of the original's sight: it would mark them errored).
pub fn plugins_dir(base: &Path) -> PathBuf {
    base.join("storage").join("sidecar").join("plugins")
}

/// Write the bridge and link every PHP plugin; links point at `PANEL_PHP_SIDECAR_DATA`
/// (this data directory as the sidecar sees it; defaults to the same path).
pub fn sync_plugins_dir(st: &AppState, plugins: &[pelican_plugins::Plugin]) -> std::io::Result<()> {
    let base = st.config.base_path.clone();
    let dir = plugins_dir(&base);
    std::fs::create_dir_all(&dir)?;
    for (rel, content) in BRIDGE_FILES {
        let path = dir.join(BRIDGE_ID).join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*content) {
            std::fs::write(&path, content)?;
        }
    }
    let seen_as = st
        .config
        .env
        .string("PANEL_PHP_SIDECAR_DATA")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| base.clone());
    let wanted: Vec<&pelican_plugins::Plugin> = plugins.iter().filter(|p| p.wasm.is_none()).collect();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name != BRIDGE_ID && !wanted.iter().any(|p| p.id == name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    #[cfg(unix)]
    for p in wanted {
        let link = dir.join(&p.id);
        let target = seen_as.join("plugins").join(&p.id);
        if std::fs::read_link(&link).ok().as_deref() != Some(target.as_path()) {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        assert!(uri_matches("/admin/greeter", "/admin/greeter"));
        assert!(uri_matches("/admin/greeter", "/admin/greeter/"));
        assert!(uri_matches("/admin/things/{record}/edit", "/admin/things/5/edit"));
        assert!(uri_matches("/plugin/{id?}", "/plugin"));
        assert!(uri_matches("/plugin/{id?}", "/plugin/3"));
        assert!(!uri_matches("/admin/greeter", "/admin/greeter/x"));
        assert!(!uri_matches("/admin/things/{record}/edit", "/admin/things/5"));
    }

    #[test]
    fn wildcards() {
        assert!(name_matches("eloquent.*", "eloquent.updated: App\\Models\\Server"));
        assert!(name_matches("eloquent.updated: *", "eloquent.updated: App\\Models\\Server"));
        assert!(!name_matches("eloquent.created: *", "eloquent.updated: App\\Models\\Server"));
        assert!(name_matches("App\\Events\\ActivityLogged", "App\\Events\\ActivityLogged"));
    }

    /// Header set and canonical paths (the HMAC itself is checked live against the bridge).
    #[test]
    fn signature_shape() {
        let sc = Sidecar { url: String::new(), secret: secret("base64:AAAA") };
        let h = sc.signature("get", "/_bridge/manifest/", Some(1), b"");
        assert_eq!(h.len(), 3);
        assert_eq!(h[1].1.len(), 64);
        assert_eq!(canonical_path("/_bridge/manifest/"), "/_bridge/manifest");
        assert_eq!(canonical_path("/"), "/");
    }
}

/// Schedule task types of WASM plugins and of PHP plugins (`(plugin, type)`); PHP
/// plugins' names arrive translated.
pub fn all_task_types() -> Vec<(String, pelican_plugins::runtime::TaskType)> {
    let mut out = pelican_plugins::runtime::task_types();
    if let Some(m) = manifest() {
        // `TaskService::register()` keeps the first schema of an id.
        let taken: Vec<String> = out.iter().map(|(_, t)| t.id.clone()).collect();
        out.extend(m.tasks.iter().filter(|t| !taken.contains(&t.id)).map(|t| {
            (
                t.plugin.clone(),
                pelican_plugins::runtime::TaskType {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    payload_label: t.payload_label.clone(),
                    default_payload: t.default_payload.clone(),
                },
            )
        }));
    }
    out
}

/// A lifecycle step of a PHP plugin, run in the sidecar after linking the plugin into its
/// `plugins/` (`None` for WASM plugins or without a sidecar: the caller does it itself).
pub async fn delegate(
    st: &AppState,
    p: &pelican_plugins::Plugin,
    action: &str,
    extra: Value,
) -> Option<Result<(), String>> {
    if p.wasm.is_some() || get(st).is_none() {
        return None;
    }
    let cfg = crate::application::plugins::config(st);
    if let Err(e) = sync_plugins_dir(st, &cfg.plugins()) {
        return Some(Err(e.to_string()));
    }
    Some(lifecycle(st, &p.id, action, extra).await)
}

/// `PluginService::manageComposerPackages()` in the sidecar (`p:plugin:composer`; `None`
/// without a sidecar: only PHP plugins have composer packages).
pub async fn composer(st: &AppState) -> Option<Result<(), String>> {
    get(st)?;
    let cfg = crate::application::plugins::config(st);
    if let Err(e) = sync_plugins_dir(st, &cfg.plugins()) {
        return Some(Err(e.to_string()));
    }
    Some(lifecycle(st, "", "composer", json!({})).await)
}

/// `Role::registerCustomPermissions()` of WASM and PHP plugins: `(model, prefixes)`.
pub fn all_admin_permissions() -> Vec<(String, Vec<String>)> {
    let mut out = pelican_plugins::runtime::admin_permissions();
    if let Some(Value::Object(models)) = manifest().map(|m| m.admin_permissions.clone()) {
        for (model, prefixes) in models {
            let prefixes =
                prefixes.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
            out.push((model, prefixes.unwrap_or_default()));
        }
    }
    out
}

/// `Subuser::registerCustomPermissions()` of WASM and PHP plugins (hidden groups left out).
pub fn all_subuser_permissions() -> Vec<pelican_plugins::runtime::PermissionGroup> {
    let mut out = pelican_plugins::runtime::subuser_permissions();
    if let Some(m) = manifest() {
        out.extend(m.subuser_permissions.iter().filter(|g| !g.hidden).map(|g| {
            pelican_plugins::runtime::PermissionGroup {
                name: g.name.clone(),
                permissions: g.permissions.clone(),
                icon: g.icon.clone(),
            }
        }));
    }
    out
}

/// `Subuser::allPermissionKeys()`: the core keys, then the plugins' groups (hidden ones too) merged
/// by group name, as `group.permission`.
pub fn all_subuser_permission_keys() -> Vec<String> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |group: &str, perms: &[String]| {
        let i = match groups.iter().position(|(g, _)| g == group) {
            Some(i) => i,
            None => {
                groups.push((group.to_string(), Vec::new()));
                groups.len() - 1
            }
        };
        for p in perms {
            if !groups[i].1.contains(p) {
                groups[i].1.push(p.clone());
            }
        }
    };
    for key in crate::client::servers::PERMISSIONS {
        if let Some((group, perm)) = key.split_once('.') {
            add(group, &[perm.to_string()]);
        }
    }
    for g in pelican_plugins::runtime::subuser_permissions() {
        add(&g.name, &g.permissions);
    }
    if let Some(m) = manifest() {
        for g in &m.subuser_permissions {
            add(&g.name, &g.permissions);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (group, perms) in groups {
        for p in perms {
            let key = format!("{group}.{p}");
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

/// `ApiKey::registerCustomResourceName()` of WASM and PHP plugins.
pub fn all_api_resources() -> Vec<String> {
    let mut out = pelican_plugins::runtime::api_resources();
    if let Some(m) = manifest() {
        out.extend(m.api_resources.iter().cloned());
    }
    // `array_unique()` in `ApiKey::getPermissionList()`.
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.clone()));
    out
}

/// Avatar providers of WASM and PHP plugins: `(plugin, provider)`.
pub fn all_avatar_providers() -> Vec<(String, pelican_plugins::runtime::AvatarProvider)> {
    let mut out = pelican_plugins::runtime::avatar_providers();
    if let Some(m) = manifest() {
        // `AvatarService::register()` keeps the first schema of an id.
        let taken: Vec<String> = out.iter().map(|(_, a)| a.id.clone()).collect();
        out.extend(m.avatars.iter().filter(|a| !taken.contains(&a.id)).map(|a| {
            (a.plugin.clone(), pelican_plugins::runtime::AvatarProvider { id: a.id.clone(), name: a.name.clone() })
        }));
    }
    out
}

type AvatarCache = std::collections::HashMap<(String, i64), (std::time::Instant, Option<String>)>;
static AVATARS: std::sync::Mutex<Option<AvatarCache>> = std::sync::Mutex::new(None);

/// A PHP plugin's avatar URL for a user (`AvatarSchemaInterface::get()`), cached for ten
/// minutes and fetched in the background: `None` until known.
pub fn cached_avatar(st: &AppState, provider: &str, user_id: i64) -> Option<String> {
    if !manifest().is_some_and(|m| m.avatars.iter().any(|a| a.id == provider)) {
        return None;
    }
    let key = (provider.to_string(), user_id);
    let hit = AVATARS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&key).cloned()));
    if let Some((at, url)) = &hit {
        if at.elapsed() < Duration::from_secs(600) {
            return url.clone();
        }
    }
    let sc = get(st)?;
    if let Ok(mut g) = AVATARS.lock() {
        g.get_or_insert_with(Default::default).insert(key.clone(), (std::time::Instant::now(), hit.and_then(|h| h.1)));
    }
    let body = json!({ "provider": provider, "user": user_id });
    tokio::spawn(async move {
        let url = match sc.call(reqwest::Method::POST, "/_bridge/avatar", Some(&body)).await {
            Ok((200, v)) => v.get("url").and_then(Value::as_str).map(str::to_string),
            _ => None,
        };
        if let Ok(mut g) = AVATARS.lock() {
            g.get_or_insert_with(Default::default).insert(key, (std::time::Instant::now(), url));
        }
    });
    None
}

/// A cache of per-user sidecar answers (menus, render hooks, console actions) that never makes a
/// page wait twice: a cached answer is returned at once and, once older than [`FRESH`],
/// refreshed in the background; only the first request for a key waits for PHP.
type SwrMap<K, V> = std::collections::HashMap<K, (std::time::Instant, V, bool)>;
const FRESH: Duration = Duration::from_secs(30);

async fn swr<K, V, Fut>(
    cache: &'static std::sync::Mutex<Option<SwrMap<K, V>>>,
    key: K,
    fetch: impl FnOnce() -> Fut,
) -> V
where
    K: std::hash::Hash + Eq + Clone + Send + 'static,
    V: Clone + Default + Send + 'static,
    Fut: std::future::Future<Output = Option<V>> + Send + 'static,
{
    let hit = cache.lock().ok().and_then(|g| g.as_ref().and_then(|c| c.get(&key).cloned()));
    if let Some((at, value, refreshing)) = hit {
        if at.elapsed() >= FRESH && !refreshing {
            if let Ok(mut g) = cache.lock() {
                if let Some(e) = g.get_or_insert_with(Default::default).get_mut(&key) {
                    e.2 = true;
                }
            }
            let fut = fetch();
            tokio::spawn(async move {
                let fresh = fut.await;
                if let Ok(mut g) = cache.lock() {
                    let map = g.get_or_insert_with(Default::default);
                    match fresh {
                        Some(v) => {
                            map.insert(key, (std::time::Instant::now(), v, false));
                        }
                        // Keep what we had; the next page view tries again.
                        None => {
                            if let Some(e) = map.get_mut(&key) {
                                e.2 = false;
                            }
                        }
                    }
                }
            });
        }
        return value;
    }
    // First request: wait. A failed call is remembered as empty for a while too.
    let value = fetch().await.unwrap_or_default();
    if let Ok(mut g) = cache.lock() {
        g.get_or_insert_with(Default::default).insert(key, (std::time::Instant::now(), value.clone(), false));
    }
    value
}

/// Forget cached per-user answers (the plugins changed: a new manifest).
fn clear_user_caches() {
    if let Ok(mut g) = PAGE.lock() {
        *g = None;
    }
}

static PAGE: std::sync::Mutex<Option<SwrMap<(String, i64, String), Arc<PageExtras>>>> = std::sync::Mutex::new(None);

/// What the PHP plugins add to a page of `panel` (`admin`, `app`, `server` with its `tenant`) as
/// `user` (or a guest) sees it, from one call to the bridge (`/_bridge/page`): menu entries,
/// render hooks, alert banners, console actions. Per user, panel and server ([`swr`]); no call
/// when no plugin adds any of these.
pub async fn page_extras(st: &AppState, panel: &str, user: Option<i64>, tenant: Option<&str>) -> Arc<PageExtras> {
    let Some(m) = manifest() else { return Arc::default() };
    let wanted = m.boots
        || !m.render_hooks.is_empty()
        || (user.is_some()
            && (m.navigation.iter().any(|n| n.panel == panel) || (panel == "server" && !m.console_actions.is_empty())));
    if !wanted || is_down() {
        return Arc::default();
    }
    let Some(sc) = get(st) else { return Arc::default() };
    let key = (panel.to_string(), user.unwrap_or(0), tenant.unwrap_or_default().to_string());
    let body = json!({ "panel": panel, "tenant": tenant });
    let panel = panel.to_string();
    swr(&PAGE, key, move || async move {
        match sc.call_as(reqwest::Method::POST, "/_bridge/page", user, Some(&body)).await {
            Ok((200, v)) => Some(Arc::new(serde_json::from_value(v).unwrap_or_default())),
            Ok((status, _)) => {
                tracing::warn!(status, panel, "sidecar page data failed");
                None
            }
            Err(_) => None,
        }
    })
    .await
}

/// The PHP plugins' navigation of a panel as `user` sees it (on server `tenant`): Filament
/// hides pages whose `canAccess()` denies.
pub async fn user_navigation(st: &AppState, panel: &str, user: i64, tenant: Option<&str>) -> Vec<NavEntry> {
    page_extras(st, panel, Some(user), tenant).await.navigation.clone()
}

/// An alert banner a plugin sent (`AlertBanner::make()->send()`), title and body sanitized HTML.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Banner {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub closeable: bool,
}

/// What PHP plugins add to a page: menu entries, their render hooks' HTML by hook name, the
/// alert banners their `boot()` sends (shown at the top of the page,
/// `PanelsRenderHook::PAGE_START`) and, on server pages, console actions.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PageExtras {
    #[serde(default)]
    pub navigation: Vec<NavEntry>,
    #[serde(default)]
    pub hooks: serde_json::Map<String, Value>,
    #[serde(default)]
    pub banners: Vec<Banner>,
    #[serde(default)]
    pub console_actions: Vec<ConsoleAction>,
}

/// The PHP plugins' additions to a page of `panel` as `user` (or a guest) sees it ([`page_extras`]).
pub async fn render_hooks(st: &AppState, panel: &str, user: Option<i64>, tenant: Option<&str>) -> Arc<PageExtras> {
    page_extras(st, panel, user, tenant).await
}

/// An OAuth provider registered by a PHP plugin (its redirect and callback run in the sidecar).
pub fn oauth_provider(id: &str) -> Option<OAuthEntry> {
    manifest().and_then(|m| m.oauth_providers.iter().find(|p| p.id == id).cloned())
}

/// The bridge's `X-Pelican-Rs-Login: <id>.<ts>.<hmac>` (a sign-in inside the sidecar):
/// the user id when the signature holds and the timestamp is fresh.
pub fn verify_login_handoff(st: &AppState, value: &str) -> Option<i64> {
    let sc = get(st)?;
    let mut parts = value.splitn(3, '.');
    let (id, ts, sig) = (parts.next()?, parts.next()?, parts.next()?);
    let (id, ts_num) = (id.parse::<i64>().ok()?, ts.parse::<i64>().ok()?);
    if (unix_now() - ts_num).abs() > 60 {
        return None;
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&sc.secret).ok()?;
    mac.update(format!("login\n{id}\n{ts}").as_bytes());
    mac.verify_slice(&hex::decode(sig).ok()?).ok()?;
    Some(id)
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ConsoleAction {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// The PHP plugins' console actions `user` can use on server `tenant` (run on the bridge's
/// host page `/server/<tenant>/rs-console-action?action=<name>`).
pub async fn console_actions(st: &AppState, user: i64, tenant: &str) -> Vec<ConsoleAction> {
    page_extras(st, "server", Some(user), Some(tenant)).await.console_actions.clone()
}

/// Server features (Wings console listeners) of WASM and PHP plugins: `(id, listeners)`.
pub fn all_feature_listeners() -> Vec<(String, Vec<String>)> {
    let mut out = pelican_plugins::runtime::feature_listeners();
    if let Some(m) = manifest() {
        for f in &m.features {
            if !out.iter().any(|(id, _)| *id == f.id) {
                out.push((f.id.clone(), f.listeners.clone()));
            }
        }
    }
    out
}

/// A server feature registered by a PHP plugin (its modal is the feature's Filament action,
/// run on the bridge's host page).
pub fn php_feature(id: &str) -> Option<FeatureEntry> {
    manifest().and_then(|m| m.features.iter().find(|f| f.id == id).cloned())
}
