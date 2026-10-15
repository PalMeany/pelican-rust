//! The WASM host (ADR-0018): one component instance per enabled plugin, calls serialized
//! per plugin, memory and time limits, WASI without any capability, and the `host`
//! interface backed by [`Services`].

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::{Map, Value};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::manifest::Plugin as Manifest;
use crate::{Config, Status};

wasmtime::component::bindgen!({
    path: "wit",
    world: "plugin",
    imports: { default: async },
    exports: { default: async },
});

pub use pelican::plugin::types::{Header, Level, NavItem, Page, Route, RouteScope, TaskType};
use pelican::plugin::{host, types};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the host functions need from the panel (implemented by the binary).
pub trait Services: Send + Sync + 'static {
    /// `trans()` in `locale`, with the plugin's own `lang/` for `<id>::` keys.
    fn trans(&self, plugin: &str, locale: &str, key: &str, replace: &[(String, String)]) -> String;
    /// An activity entry by the plugin.
    fn activity<'a>(&'a self, plugin: &'a str, event: &'a str, properties: Value) -> BoxFuture<'a, ()>;
    /// An Application API call with the plugin's grants.
    fn api<'a>(
        &'a self,
        plugin: &'a str,
        grants: &'a Map<String, Value>,
        method: &'a str,
        path: &'a str,
        body: Option<String>,
    ) -> BoxFuture<'a, Result<(u16, String), String>>;
}

/// `wasm.capabilities` of the manifest.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// Hosts the plugin may call (`example.com`, `*.example.com`).
    pub http: Vec<String>,
    /// Application API resource => 1 read / 2 write / 3 both.
    pub api: Map<String, Value>,
}

impl Capabilities {
    fn from_manifest(wasm: &Value) -> Capabilities {
        let caps = wasm.get("capabilities");
        Capabilities {
            http: caps
                .and_then(|c| c.get("http"))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|h| h.as_str().map(str::to_lowercase)).collect())
                .unwrap_or_default(),
            api: caps.and_then(|c| c.get("api")).and_then(Value::as_object).cloned().unwrap_or_default(),
        }
    }

    fn allows_host(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.http.iter().any(|h| match h.strip_prefix("*.") {
            Some(suffix) => host.ends_with(&format!(".{suffix}")),
            None => *h == host,
        })
    }
}

pub struct HostState {
    plugin: String,
    locale: String,
    kv: PathBuf,
    caps: Capabilities,
    services: Arc<dyn Services>,
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

fn read_kv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

impl types::Host for HostState {}

impl host::Host for HostState {
    async fn log(&mut self, level: Level, message: String) {
        let p = &self.plugin;
        match level {
            Level::Trace => tracing::trace!(plugin = %p, "{message}"),
            Level::Debug => tracing::debug!(plugin = %p, "{message}"),
            Level::Info => tracing::info!(plugin = %p, "{message}"),
            Level::Warn => tracing::warn!(plugin = %p, "{message}"),
            Level::Error => tracing::error!(plugin = %p, "{message}"),
        }
    }

    async fn kv_get(&mut self, key: String) -> Option<String> {
        read_kv(&self.kv).remove(&key)
    }

    async fn kv_set(&mut self, key: String, value: Option<String>) {
        let mut all = read_kv(&self.kv);
        match value {
            Some(v) => all.insert(key, v),
            None => all.remove(&key),
        };
        if let Some(parent) = self.kv.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec(&all) {
            if let Err(e) = std::fs::write(&self.kv, bytes) {
                tracing::warn!(plugin = %self.plugin, error = %e, "writing the plugin store failed");
            }
        }
    }

    async fn http(&mut self, req: host::HttpRequest) -> Result<host::HttpResponse, String> {
        let url = reqwest::Url::parse(&req.url).map_err(|e| e.to_string())?;
        if !matches!(url.scheme(), "http" | "https") || !url.host_str().is_some_and(|h| self.caps.allows_host(h)) {
            return Err(format!("host not allowed: {}", url.host_str().unwrap_or("")));
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| e.to_string())?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let mut r = client.request(method, url);
        for h in &req.headers {
            r = r.header(h.name.as_str(), h.value.as_str());
        }
        let resp = r.body(req.body).send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| Header { name: k.to_string(), value: v.to_str().unwrap_or("").to_string() })
            .collect();
        let body = resp.bytes().await.map_err(|e| e.to_string())?;
        if body.len() > 16 * 1024 * 1024 {
            return Err("response too large".into());
        }
        Ok(host::HttpResponse { status, headers, body: body.to_vec() })
    }

    async fn api(&mut self, method: String, path: String, body: Option<String>) -> Result<(u16, String), String> {
        if self.caps.api.is_empty() {
            return Err("no api capability".into());
        }
        let services = self.services.clone();
        services.api(&self.plugin, &self.caps.api, &method, &path, body).await
    }

    async fn activity(&mut self, event: String, properties: String) {
        let props = serde_json::from_str(&properties).unwrap_or(Value::Object(Map::new()));
        let services = self.services.clone();
        services.activity(&self.plugin, &event, props).await;
    }

    async fn trans(&mut self, key: String, replace: Vec<(String, String)>) -> String {
        self.services.trans(&self.plugin, &self.locale, &key, &replace)
    }
}

/// A loaded plugin: its registration and the instance (calls are serialized).
pub struct Loaded {
    pub id: String,
    pub dir: String,
    pub registration: Registration,
    inner: tokio::sync::Mutex<(Store<HostState>, Plugin)>,
}

static GLOBAL: std::sync::OnceLock<Arc<Runtime>> = std::sync::OnceLock::new();

/// The process-wide runtime (none in tests, difftests and safe mode).
pub fn global() -> Option<Arc<Runtime>> {
    GLOBAL.get().cloned()
}

pub fn set_global(rt: Arc<Runtime>) {
    let _ = GLOBAL.set(rt);
}

/// Reload the global runtime in the background (plugin enabled/disabled/installed/...).
pub fn reload_global(cfg: &Config) {
    if let Some(rt) = global() {
        let cfg = cfg.clone();
        tokio::spawn(async move { rt.load_all(&cfg).await });
    }
}

/// Schedule task types of the loaded plugins: `(plugin, type)`.
pub fn task_types() -> Vec<(String, pelican::plugin::types::TaskType)> {
    global()
        .map(|rt| {
            rt.loaded().iter().flat_map(|l| l.registration.tasks.iter().map(|t| (l.id.clone(), t.clone()))).collect()
        })
        .unwrap_or_default()
}

/// Per-call time budget (in epoch ticks of 10 ms).
const CALL_TICKS: u64 = 3000;

pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    services: Arc<dyn Services>,
    storage: PathBuf,
    loaded: RwLock<Vec<Arc<Loaded>>>,
    /// Compiled components by path, invalidated by mtime/size (reloads skip recompiling).
    compiled: std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::SystemTime, u64, Component)>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("plugin not loaded")]
    NotLoaded,
    #[error("{0}")]
    Trap(String),
}

impl Runtime {
    /// `storage` is where per-plugin stores live (`storage/plugins/<id>/`).
    pub fn new(services: Arc<dyn Services>, storage: PathBuf) -> wasmtime::Result<Arc<Runtime>> {
        let mut cfg = wasmtime::Config::new();
        cfg.wasm_component_model(true).epoch_interruption(true);
        let engine = Engine::new(&cfg)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Plugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(&mut linker, |s: &mut HostState| s)?;
        let ticker = engine.weak();
        std::thread::spawn(move || {
            while let Some(engine) = ticker.upgrade() {
                engine.increment_epoch();
                drop(engine);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(Arc::new(Runtime {
            engine,
            linker,
            services,
            storage,
            loaded: RwLock::new(Vec::new()),
            compiled: std::sync::Mutex::new(std::collections::HashMap::new()),
        }))
    }

    pub fn loaded(&self) -> Vec<Arc<Loaded>> {
        self.loaded.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Loaded>> {
        self.loaded().into_iter().find(|p| p.id == id)
    }

    fn store(&self, p: &Manifest, caps: Capabilities) -> Store<HostState> {
        let state = HostState {
            plugin: p.id.clone(),
            locale: "en".into(),
            kv: self.storage.join(&p.id).join("kv.json"),
            caps,
            services: self.services.clone(),
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new().memory_size(128 << 20).instances(64).build(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store
    }

    async fn instantiate(&self, cfg: &Config, p: &Manifest) -> Result<Loaded, String> {
        let wasm = p.wasm.as_ref().ok_or("not a WASM plugin")?;
        let file = wasm.get("component").and_then(Value::as_str).unwrap_or("plugin.wasm");
        if file.contains("..") || file.starts_with('/') {
            return Err("invalid component path".into());
        }
        let path = cfg.root.join(&p.dir).join(file);
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let stamp = (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len());
        let cached = self
            .compiled
            .lock()
            .ok()
            .and_then(|c| c.get(&path).filter(|(m, l, _)| (*m, *l) == stamp).map(|(_, _, comp)| comp.clone()));
        let component = match cached {
            Some(c) => c,
            None => {
                let (engine, p2) = (self.engine.clone(), path.clone());
                let c = tokio::task::spawn_blocking(move || Component::from_file(&engine, &p2))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("{e:#}"))?;
                if let Ok(mut g) = self.compiled.lock() {
                    g.insert(path, (stamp.0, stamp.1, c.clone()));
                }
                c
            }
        };
        let mut store = self.store(p, Capabilities::from_manifest(wasm));
        store.set_epoch_deadline(CALL_TICKS);
        let bindings =
            Plugin::instantiate_async(&mut store, &component, &self.linker).await.map_err(|e| format!("{e:#}"))?;
        store.set_epoch_deadline(CALL_TICKS);
        let registration = bindings.call_register(&mut store).await.map_err(|e| format!("{e:#}"))?;
        Ok(Loaded {
            id: p.id.clone(),
            dir: p.dir.clone(),
            registration,
            inner: tokio::sync::Mutex::new((store, bindings)),
        })
    }

    /// `loadPlugins()` + `loadPanelPlugins()`: instantiate every loadable WASM plugin by load
    /// order; failures mark it `errored`, a successful load turns `errored` back to `enabled`.
    pub async fn load_all(&self, cfg: &Config) {
        if cfg.safe_mode {
            if let Ok(mut g) = self.loaded.write() {
                g.clear();
            }
            return;
        }
        cfg.refresh_statuses();
        let mut plugins: Vec<Manifest> =
            cfg.plugins().into_iter().filter(|p| p.wasm.is_some() && p.should_load(None)).collect();
        plugins.sort_by_key(|p| p.load_order);
        let mut loaded = Vec::new();
        for p in plugins {
            match self.instantiate(cfg, &p).await {
                Ok(l) => {
                    if p.status == Status::Errored {
                        let _ = cfg.set_status(&p, Status::Enabled, None);
                    }
                    tracing::info!(plugin = %p.id, "plugin loaded");
                    loaded.push(Arc::new(l));
                }
                Err(e) => {
                    if cfg.dev_mode {
                        tracing::error!(plugin = %p.id, error = %e, "plugin failed to load");
                    }
                    tracing::warn!(plugin = %p.id, error = %e, "plugin failed to load");
                    let _ = cfg.set_status(&p, Status::Errored, Some(e));
                }
            }
        }
        if let Ok(mut g) = self.loaded.write() {
            *g = loaded;
        }
    }

    /// A plugin that is not loaded (disabled, not installed) instantiated just for an
    /// install/uninstall hook.
    pub async fn lifecycle_hook(&self, cfg: &Config, p: &Manifest, install: bool) -> Result<(), String> {
        if p.wasm.is_none() {
            return Ok(());
        }
        let l = match self.get(&p.id) {
            Some(l) => l,
            None => Arc::new(self.instantiate(cfg, p).await?),
        };
        let mut g = l.inner.lock().await;
        let (store, bindings) = &mut *g;
        store.set_epoch_deadline(CALL_TICKS);
        let r =
            if install { bindings.call_install(&mut *store).await } else { bindings.call_uninstall(&mut *store).await };
        r.map_err(|e| format!("{e:#}"))?
    }

    async fn with<T>(
        &self,
        id: &str,
        locale: Option<&str>,
        f: impl AsyncFnOnce(&Plugin, &mut Store<HostState>) -> wasmtime::Result<T>,
    ) -> Result<T, CallError> {
        let l = self.get(id).ok_or(CallError::NotLoaded)?;
        let mut g = l.inner.lock().await;
        let (store, bindings) = &mut *g;
        if let Some(locale) = locale {
            store.data_mut().locale = locale.to_string();
        }
        store.set_epoch_deadline(CALL_TICKS);
        f(bindings, store).await.map_err(|e| {
            tracing::error!(plugin = %id, error = %format!("{e:#}"), "plugin call trapped");
            CallError::Trap(format!("{e:#}"))
        })
    }

    /// Whether any loaded plugin subscribed to `event`.
    pub fn wants(&self, event: &str) -> bool {
        self.loaded().iter().any(|l| l.registration.events.iter().any(|e| e == event || e == "*"))
    }

    /// Deliver an event to every plugin subscribed to it (in the background).
    pub fn dispatch(self: &Arc<Self>, event: &str, payload: &Value) {
        for l in self.loaded() {
            let wants = l.registration.events.iter().any(|e| e == event || e == "*");
            if !wants {
                continue;
            }
            let rt = self.clone();
            let (id, event, payload) = (l.id.clone(), event.to_string(), payload.to_string());
            tokio::spawn(async move {
                let _ = rt.with(&id, None, async |b, s| b.call_on_event(s, &event, &payload).await).await;
            });
        }
    }

    pub async fn handle(&self, id: &str, req: Request) -> Result<Response, CallError> {
        let locale = req.locale.clone();
        self.with(id, Some(&locale), async |b, s| b.call_handle(s, &req).await).await
    }

    pub async fn render_page(
        &self,
        id: &str,
        panel: Panel,
        page: &str,
        req: Request,
    ) -> Result<Result<String, String>, CallError> {
        let locale = req.locale.clone();
        self.with(id, Some(&locale), async |b, s| b.call_render_page(s, panel, page, &req).await).await
    }

    pub async fn settings_form(&self, id: &str) -> Result<Vec<Field>, CallError> {
        self.with(id, None, async |b, s| b.call_settings_form(s).await).await
    }

    pub async fn settings_data(&self, id: &str) -> Result<String, CallError> {
        self.with(id, None, async |b, s| b.call_settings_data(s).await).await
    }

    pub async fn save_settings(&self, id: &str, data: &str) -> Result<Result<(), String>, CallError> {
        let data = data.to_string();
        self.with(id, None, async |b, s| b.call_save_settings(s, &data).await).await
    }

    pub async fn run_task(
        &self,
        id: &str,
        task: &str,
        payload: &str,
        server: &str,
    ) -> Result<Result<(), String>, CallError> {
        let server = server.to_string();
        self.with(id, None, async |b, s| b.call_run_task(s, task, payload, &server).await).await
    }

    pub async fn avatar_url(&self, id: &str, provider: &str, user: &str) -> Result<Option<String>, CallError> {
        let user = user.to_string();
        self.with(id, None, async |b, s| b.call_avatar_url(s, provider, &user).await).await
    }
}
