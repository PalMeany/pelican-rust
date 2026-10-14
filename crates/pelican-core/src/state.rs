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
    pub limiter: RateLimiter,
    pub driver: pelican_db::Driver,
}

impl AppState {
    pub fn new(config: Config, enc: Encrypter, db: DatabaseConnection) -> Self {
        let driver = config.db.driver;
        let wings =
            pelican_wings::WingsClient::new(config.is_production(), config.wings_timeout, config.wings_connect_timeout);
        let limiter = RateLimiter::new(&config.cache_store);
        AppState(Arc::new(Inner { wings, config, enc, db, limiter, driver }))
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
