//! Starting pelican-rs in-process as the second target.

use std::collections::HashMap;
use std::path::Path;

use pelican_core::env::Env;

/// Start pelican-rs against the difftest database and return its base URL.
pub async fn start(backend: &str, app_key: &str) -> anyhow::Result<String> {
    let cfg = crate::db::config_for(backend)?;
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut set = |k: &str, v: &str| {
        vars.insert(k.to_string(), v.to_string());
    };
    set("APP_KEY", app_key);
    set("APP_URL", "http://localhost:18000");
    set("APP_ENV", "production");
    set("TRUSTED_PROXIES", "0.0.0.0/0");
    // Same per-request cache semantics as panel-orig (difftest/compose.yml).
    set("CACHE_STORE", "array");
    set("PANEL_CLIENT_ALLOCATIONS_ENABLED", "true");
    set("PANEL_CLIENT_ALLOCATIONS_RANGE_START", "25570");
    set("PANEL_CLIENT_ALLOCATIONS_RANGE_END", "25571");
    set("DB_CONNECTION", cfg.driver.as_str());
    set("DB_HOST", &cfg.host);
    set("DB_PORT", &cfg.port.to_string());
    set("DB_DATABASE", &cfg.database);
    set("DB_USERNAME", &cfg.username);
    set("DB_PASSWORD", &cfg.password);
    let state = pelican::boot(Env::from_map(vars), Path::new("/nonexistent")).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = pelican::serve(state, listener).await {
            eprintln!("pelican-rs stopped: {e:#}");
        }
    });
    Ok(format!("http://{addr}"))
}
