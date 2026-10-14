//! Bootstrapping shared by the binary and the difftest harness.

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

/// Serve the panel on an already bound listener.
pub async fn serve(state: AppState, listener: tokio::net::TcpListener) -> Result<()> {
    let live = Live::new(state.clone());
    if state.config.env.bool_or("PELICAN_SCHEDULER", true) {
        pelican_api::jobs::spawn_scheduler(live.clone());
    }
    spawn_reloader(live.clone());
    axum::serve(listener, app(live).into_make_service_with_connect_info::<SocketAddr>()).await?;
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
