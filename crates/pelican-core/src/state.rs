//! Shared application state.

use std::sync::Arc;

use laravel_compat::Encrypter;
use sea_orm::DatabaseConnection;

use crate::config::Config;
use crate::rate_limit::RateLimiter;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub enc: Encrypter,
    pub db: DatabaseConnection,
    pub limiter: RateLimiter,
    pub driver: pelican_db::Driver,
}

impl AppState {
    pub fn new(config: Config, enc: Encrypter, db: DatabaseConnection) -> Self {
        let driver = config.db.driver;
        AppState(Arc::new(Inner {
            config,
            enc,
            db,
            limiter: RateLimiter::default(),
            driver,
        }))
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}
