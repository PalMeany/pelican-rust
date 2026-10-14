//! Bootstrapping shared by the binary and the difftest harness.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use pelican_core::config::Config;
use pelican_core::env::Env;
use pelican_core::state::AppState;
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
    if state.config.env.bool_or("PELICAN_SCHEDULER", true) {
        pelican_api::jobs::spawn_scheduler(state.clone());
    }
    let app = pelican_api::router(state);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
