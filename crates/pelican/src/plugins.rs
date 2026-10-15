//! The WASM plugin runtime of a running panel (ADR-0018): host services and startup.

use std::sync::Arc;

use pelican_api::services::LogActivity;
use pelican_core::state::Live;
use pelican_plugins::runtime::{BoxFuture, Runtime, Services};
use serde_json::{Map, Value};

struct PanelServices {
    live: Live,
}

impl Services for PanelServices {
    fn trans(&self, plugin: &str, locale: &str, key: &str, replace: &[(String, String)]) -> String {
        let cfg = pelican_api::application::plugins::config(&self.live.get());
        pelican_plugins::trans_line(&cfg, plugin, locale, key, replace)
            .unwrap_or_else(|| pelican_web::i18n::trans(locale, key, replace))
    }

    fn activity<'a>(&'a self, plugin: &'a str, event: &'a str, properties: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let st = self.live.get();
            let mut a =
                pelican_core::activity::Activity::new(event, "127.0.0.1".to_string()).property("plugin", plugin);
            if let Value::Object(m) = properties {
                for (k, v) in m {
                    a = a.property(&k, v);
                }
            }
            if let Err(e) = a.log(&st).await {
                tracing::warn!(plugin, error = ?e.errors.first().map(|o| &o.code), "plugin activity failed");
            }
        })
    }

    fn api<'a>(
        &'a self,
        _plugin: &'a str,
        _grants: &'a Map<String, Value>,
        _method: &'a str,
        _path: &'a str,
        _body: Option<String>,
    ) -> BoxFuture<'a, Result<(u16, String), String>> {
        Box::pin(async { Err("the api capability is not available in this panel version".into()) })
    }
}

/// Create the runtime and load the enabled plugins in the background.
pub fn start(live: &Live) {
    let st = live.get();
    let cfg = pelican_api::application::plugins::config(&st);
    if cfg.safe_mode {
        tracing::info!("PANEL_PLUGIN_SAFE_MODE: plugins are not loaded");
        return;
    }
    let storage = st.config.base_path.join("storage").join("plugins");
    match Runtime::new(Arc::new(PanelServices { live: live.clone() }), storage) {
        Ok(rt) => {
            pelican_plugins::runtime::set_global(rt.clone());
            tokio::spawn(async move { rt.load_all(&cfg).await });
        }
        Err(e) => tracing::error!(error = %e, "starting the plugin runtime failed"),
    }
}
