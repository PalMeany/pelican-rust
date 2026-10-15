//! Plugins (ADR-0018): the original's `plugins/<id>/plugin.json` registry, status machine
//! and lifecycle (`App\Models\Plugin`, `App\Services\Helpers\PluginService`).

pub mod compat;
pub mod lifecycle;
pub mod manifest;
pub mod runtime;
pub mod updates;

use std::path::PathBuf;

pub use manifest::{Category, Plugin, Status};

/// `Plugin::SUPPORTED_API_VERSION`.
pub const SUPPORTED_API_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A plain `Exception`/`PanelException` (500 in the API without debug).
    #[error("{0}")]
    Message(String),
    /// `InvalidFileUploadException` (a DisplayException: 400 with its message).
    #[error("{0}")]
    InvalidUpload(String),
    #[error("No query results for model [App\\Models\\Plugin] {0}")]
    NotFound(String),
}

impl Error {
    fn io(e: std::io::Error) -> Error {
        Error::Message(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `trans($key, $replace)` for the few translated service messages.
pub type Translate = std::sync::Arc<dyn Fn(&str, &[(&str, &str)]) -> String + Send + Sync>;

/// English lines of `lang/en/admin/plugin.php` used by the service.
pub fn english(key: &str, replace: &[(&str, &str)]) -> String {
    let line = match key {
        "admin/plugin.notifications.import_no_manifest" => "The zip does not contain a valid plugin.json",
        "admin/plugin.notifications.import_invalid_id" => "The plugin.json contains an invalid id",
        "admin/plugin.notifications.import_id_mismatch" => "The zip is for plugin \":actual\", not \":expected\"",
        "admin/plugin.notifications.import_checksum_mismatch" => {
            "The downloaded zip does not match the checksum from the update feed"
        }
        other => other,
    };
    replace.iter().fold(line.to_string(), |s, (k, v)| s.replace(&format!(":{k}"), v))
}

/// `config('panel.plugin')` plus the paths and version the service needs.
#[derive(Clone)]
pub struct Config {
    /// `base_path('plugins')`.
    pub root: PathBuf,
    pub dev_mode: bool,
    pub safe_mode: bool,
    pub max_import_size: u64,
    pub hub_url: String,
    pub hub_api_key: Option<String>,
    /// `currentComparableVersion()`: `None` for canary builds.
    pub panel_version: Option<String>,
    /// `currentPanelVersion()` for messages.
    pub panel_version_display: String,
    pub production: bool,
    pub translate: Translate,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("root", &self.root).field("safe_mode", &self.safe_mode).finish()
    }
}

impl Config {
    pub fn from_env(
        env: &pelican_core::env::Env,
        base: &std::path::Path,
        version: Option<&str>,
        display: String,
    ) -> Config {
        Config {
            root: base.join("plugins"),
            dev_mode: env.bool_or("PANEL_PLUGIN_DEV_MODE", false),
            safe_mode: env.bool_or("PANEL_PLUGIN_SAFE_MODE", false),
            max_import_size: env.int_or("PANEL_PLUGIN_MAX_IMPORT_SIZE", 104_857_600).max(0) as u64,
            hub_url: env.string_or("PANEL_PLUGIN_HUB_URL", "https://hub.pelican.dev"),
            hub_api_key: env.string("PANEL_PLUGIN_HUB_API_KEY").filter(|k| !k.is_empty()),
            panel_version: version.map(str::to_string),
            panel_version_display: display,
            production: env.string_or("APP_ENV", "production") == "production",
            translate: std::sync::Arc::new(english),
        }
    }

    pub fn plugins(&self) -> Vec<Plugin> {
        manifest::scan(&self.root)
    }

    /// `Plugin::findOrFail($id)` (ids are lower-cased).
    pub fn find(&self, id: &str) -> Result<Plugin> {
        let id = id.to_lowercase();
        self.plugins().into_iter().find(|p| p.id == id).ok_or(Error::NotFound(id))
    }

    /// `setStatus()`.
    pub fn set_status(&self, p: &Plugin, status: Status, message: Option<String>) -> Result<()> {
        manifest::set_meta(
            &self.root,
            &p.dir,
            &[
                ("status", status.as_str().into()),
                ("status_message", message.map_or(serde_json::Value::Null, Into::into)),
            ],
        )
    }

    /// The status part of `PluginService::loadPlugins()` (run on every boot / request in the
    /// original): API-version and panel-version checks write `incompatible`, a plugin that
    /// became compatible again is disabled.
    pub fn refresh_statuses(&self) {
        if self.safe_mode {
            return;
        }
        let mut plugins = self.plugins();
        plugins.sort_by_key(|p| p.load_order);
        for p in plugins {
            if p.status == Status::Crashed {
                continue;
            }
            let result = if self.production && p.effective_api_version() > SUPPORTED_API_VERSION {
                self.set_status(
                    &p,
                    Status::Incompatible,
                    Some(format!(
                        "This Plugin requires plugin api version {} but this Panel only supports up to version {SUPPORTED_API_VERSION}!",
                        p.effective_api_version()
                    )),
                )
            } else if !compat::is_compatible(&p, self.panel_version.as_deref()) {
                let newer = if p.is_panel_version_strict() { "" } else { " or a compatible newer version" };
                self.set_status(
                    &p,
                    Status::Incompatible,
                    Some(format!(
                        "This Plugin is only compatible with Panel version {}{newer} but you are using version {}!",
                        p.panel_version.as_deref().unwrap_or(""),
                        self.panel_version_display
                    )),
                )
            } else if p.status == Status::Incompatible {
                self.set_status(&p, Status::Disabled, None)
            } else {
                Ok(())
            };
            if let Err(e) = result {
                tracing::warn!(plugin = %p.id, error = %e, "updating plugin status failed");
            }
        }
    }
}
