//! Shared application state.

use std::sync::Arc;

use laravel_compat::Encrypter;
use sea_orm::DatabaseConnection;

use crate::config::Config;
use crate::rate_limit::RateLimiter;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub wings: pelican_wings::WingsClient,
    pub config: Config,
    pub enc: Encrypter,
    pub db: DatabaseConnection,
    pub limiter: Arc<RateLimiter>,
    pub driver: pelican_db::Driver,
    reload: Arc<tokio::sync::Notify>,
}

impl AppState {
    pub fn new(config: Config, enc: Encrypter, db: DatabaseConnection) -> Self {
        let driver = config.db.driver;
        let wings =
            pelican_wings::WingsClient::new(config.is_production(), config.wings_timeout, config.wings_connect_timeout);
        let limiter = Arc::new(RateLimiter::new(&config.cache_store));
        let reload = Arc::new(tokio::sync::Notify::new());
        AppState(Arc::new(Inner { wings, config, enc, db, limiter, driver, reload }))
    }

    /// The state for a re-read `.env` (ADR-0014): same database pool, throttle
    /// counters and reload channel; everything derived from the config is rebuilt.
    pub fn rebuild(&self, config: Config, enc: Encrypter) -> AppState {
        let wings =
            pelican_wings::WingsClient::new(config.is_production(), config.wings_timeout, config.wings_connect_timeout);
        AppState(Arc::new(Inner {
            wings,
            driver: config.db.driver,
            config,
            enc,
            db: self.db.clone(),
            limiter: self.limiter.clone(),
            reload: self.reload.clone(),
        }))
    }

    /// Ask the running server to re-read `.env` (after `writeToEnvironment` + `config:clear`).
    pub fn request_reload(&self) {
        self.reload.notify_one();
    }

    /// Resolves when a reload was requested.
    pub async fn reload_requested(&self) {
        self.reload.notified().await;
    }

    pub fn same(&self, other: &AppState) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Connection data for a node (decrypts `daemon_token`).
    pub fn node_conn(
        &self,
        n: &pelican_db::entities::nodes::Model,
    ) -> Result<pelican_wings::Node, laravel_compat::DecryptError> {
        Ok(pelican_wings::Node {
            scheme: n.scheme.clone(),
            fqdn: n.fqdn.clone(),
            daemon_connect: n.daemon_connect.0,
            token_id: n.daemon_token_id.clone().unwrap_or_default(),
            token: self.enc.decrypt_string_lenient(&n.daemon_token)?,
        })
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// The current state of a running server; replaced when `.env` is re-read.
#[derive(Clone)]
pub struct Live(Arc<std::sync::RwLock<AppState>>);

impl Live {
    pub fn new(state: AppState) -> Live {
        Live(Arc::new(std::sync::RwLock::new(state)))
    }

    pub fn get(&self) -> AppState {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, state: AppState) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = state;
    }
}
