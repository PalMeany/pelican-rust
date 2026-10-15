//! Bootstrapping shared by the binary and the difftest harness.

pub mod cli;
pub mod console;
pub mod logfile;
pub mod plugins;

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use pelican_core::config::Config;
use pelican_core::env::Env;
use pelican_core::state::{AppState, Live};
use pelican_db::install::{SchemaState, schema_state};

/// Load config, connect and verify the database schema (ADR-0003).
pub async fn boot(env: Env, base: &Path) -> Result<AppState> {
    let (config, enc) = Config::from_env(env, base)?;
    let db = pelican_db::connect(&config.db).await.context("connecting to the database")?;
    match schema_state(&db).await? {
        SchemaState::Ready { unknown } => {
            // Plugins' own migrations (`plugins/<id>/database/migrations`) are not schema drift.
            let plugin_migrations = plugin_migration_names(base);
            let unknown: Vec<String> = unknown.into_iter().filter(|m| !plugin_migrations.contains(m)).collect();
            if !unknown.is_empty() {
                tracing::warn!("database has migrations unknown to this build: {}", unknown.join(", "));
            }
        }
        SchemaState::Empty => {
            bail!("the database is empty: run `pelican db:install` (fresh install) first")
        }
        SchemaState::Outdated { missing } => bail!(
            "the database schema is older than this build supports ({} migrations missing, first: {}). \
             Upgrade it with the original panel first.",
            missing.len(),
            missing[0]
        ),
    }
    Ok(AppState::new(config, enc, db))
}

/// Names of the migrations shipped by plugins (`plugins/*/database/migrations/*.php`).
fn plugin_migration_names(base: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for plugin in std::fs::read_dir(base.join("plugins")).into_iter().flatten().flatten() {
        let dir = plugin.path().join("database").join("migrations");
        for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if let Some(stem) = name.strip_suffix(".php") {
                out.push(stem.to_string());
            }
        }
    }
    out
}

/// Serve the panel on an already bound listener.
pub async fn serve(state: AppState, listener: tokio::net::TcpListener) -> Result<()> {
    axum::serve(listener, start_panel(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// Background tasks (scheduler, `.env` reloader) and the panel router for a booted state.
fn start_panel(state: AppState) -> axum::Router {
    let live = Live::new(state.clone());
    // PELICAN_SCHEDULER=false (the differential tests) also stops the node checks: background
    // Wings calls would land in the recorded steps.
    if state.config.env.bool_or("PELICAN_SCHEDULER", true) {
        pelican_api::jobs::spawn_scheduler(live.clone());
        pelican_web::server_info::spawn_node_warmer(state.clone());
    }
    spawn_reloader(live.clone());
    plugins::start(&live);
    app(live)
}

/// `pelican serve`: the panel, or the web installer while `APP_INSTALLED` is false
/// (ADR-0016). The installer hands over a booted state and the same listener then
/// serves the panel.
pub async fn serve_or_install(env: Env, base: &Path, listener: tokio::net::TcpListener) -> Result<()> {
    if env.bool_or("APP_INSTALLED", true) {
        let state = boot(env, base).await?;
        return serve(state, listener).await;
    }
    ensure_app_key(&env, base)?;
    tracing::info!("APP_INSTALLED is false: serving the web installer at /installer");
    let current = std::sync::Arc::new(std::sync::RwLock::new(axum::Router::new()));
    let switch = current.clone();
    let installer = pelican_web::installer::router(
        base.to_path_buf(),
        Box::new(move |state| {
            *switch.write().unwrap_or_else(|e| e.into_inner()) = start_panel(state);
            tracing::info!("installation finished: serving the panel");
        }),
    );
    *current.write().unwrap_or_else(|e| e.into_inner()) = installer;
    let svc = axum::Router::new().fallback(move |req: axum::extract::Request| {
        use tower::ServiceExt;
        let router = current.read().unwrap_or_else(|e| e.into_inner()).clone();
        async move { axum::response::IntoResponse::into_response(router.oneshot(req).await) }
    });
    axum::serve(listener, svc.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// `php artisan key:generate` when the installer starts without an `APP_KEY`.
pub(crate) fn ensure_app_key(env: &Env, base: &Path) -> Result<()> {
    if env.string("APP_KEY").is_some_and(|k| !k.is_empty()) {
        return Ok(());
    }
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    let value = format!("base64:{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key));
    let file = base.join(".env");
    if !file.exists() {
        std::fs::write(&file, "")?;
    }
    pelican_core::env::write_variables(&file, &[("APP_KEY".into(), pelican_core::env::EnvWrite::Str(value))], true)?;
    tracing::info!("generated a new APP_KEY");
    Ok(())
}

/// Re-read `.env` when a page asks for it (Settings save; ADR-0014). Requests
/// already running keep the state they started with.
fn spawn_reloader(live: Live) {
    tokio::spawn(async move {
        loop {
            let current = live.get();
            current.reload_requested().await;
            let base = current.config.base_path.clone();
            let next = Env::load(&base)
                .map_err(|e| e.to_string())
                .and_then(|env| Config::from_env(env, &base).map_err(|e| e.to_string()));
            match next {
                Ok((config, enc)) => {
                    live.set(current.rebuild(config, enc));
                    tracing::info!("configuration reloaded from .env");
                }
                Err(e) => tracing::error!(error = %e, "reloading .env failed; keeping the previous configuration"),
            }
        }
    });
}

/// `/api/*` goes to the API router (its own error negotiation), everything else to the web UI.
/// The routers are rebuilt when the live state changes.
pub fn app(live: Live) -> axum::Router {
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    type Routers = (AppState, axum::Router, axum::Router);
    let build = |st: AppState| -> Routers { (st.clone(), pelican_api::router(st.clone()), pelican_web::router(st)) };
    let cache: std::sync::Arc<std::sync::Mutex<Routers>> =
        std::sync::Arc::new(std::sync::Mutex::new(build(live.get())));
    axum::Router::new().fallback(move |req: axum::extract::Request| {
        let current = live.get();
        let (api, web) = {
            let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
            if !c.0.same(&current) {
                *c = build(current);
            }
            (c.1.clone(), c.2.clone())
        };
        async move {
            let path = req.uri().path();
            let target = if path == "/api" || path.starts_with("/api/") { api } else { web };
            target.oneshot(req).await.into_response()
        }
    })
}
