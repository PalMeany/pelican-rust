//! Typed configuration, read from the same `.env` as the original panel.
//! Keys and defaults mirror `config/*.php` of the original (cited per field).

use std::path::{Path, PathBuf};

use laravel_compat::Encrypter;
use pelican_db::DbConfig;

use crate::env::Env;

/// `Limit::perMinutes(period, max)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    pub max: u32,
    pub period_minutes: u32,
}

impl RateLimit {
    fn from_env(env: &Env, key: &str, max: i64, period: i64) -> Self {
        RateLimit {
            max: env.int_or(key, max).max(0) as u32,
            period_minutes: env.int_or(&format!("{key}_PERIOD"), period).max(0) as u32,
        }
    }

    pub fn decay_seconds(&self) -> i64 {
        i64::from(self.period_minutes) * 60
    }
}

/// `config/http.php` rate limits.
#[derive(Clone, Debug)]
pub struct RateLimits {
    pub client: RateLimit,
    pub application: RateLimit,
    pub password_reset: RateLimit,
    pub websocket: RateLimit,
    pub backup_restore: RateLimit,
    pub database_create: RateLimit,
    pub subuser_create: RateLimit,
    pub file_pull: RateLimit,
    pub default: RateLimit,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub base_path: PathBuf,
    pub env: Env,
    /// `APP_NAME` (config/app.php), default `Pelican`.
    pub app_name: String,
    /// `APP_URL` without trailing slash.
    pub app_url: String,
    /// `APP_ENV`, default `production`.
    pub app_env: String,
    pub app_debug: bool,
    pub app_installed: bool,
    pub db: DbConfig,
    pub rate_limits: RateLimits,
    /// `TRUSTED_PROXIES` (config/trustedproxy.php): `*`/`**` or a comma list.
    pub trusted_proxies: Vec<String>,
    /// `API_KEYS_LIMIT` (config/panel.php `api.key_limit`).
    pub api_key_limit: i64,
    /// `BCRYPT_ROUNDS` (framework config/hashing.php).
    pub bcrypt_rounds: u32,
    /// `GUZZLE_TIMEOUT` / `GUZZLE_CONNECT_TIMEOUT` seconds (config/panel.php).
    pub wings_timeout: u64,
    pub wings_connect_timeout: u64,
    /// `SESSION_COOKIE` (config/session.php), default `pelican_session`.
    pub session_cookie: String,
    /// `PANEL_USE_BINARY_PREFIX` (config/panel.php), default true.
    pub use_binary_prefix: bool,
    /// `PANEL_FILES_MAX_EDIT_SIZE` (config/panel.php), default 4 MiB; 0 disables the check.
    pub files_max_edit_size: i64,
    /// `PANEL_CLIENT_DATABASES_ENABLED` / `_ALLOW_RANDOM` (config/panel.php), default true.
    pub client_databases_enabled: bool,
    pub client_databases_allow_random: bool,
    /// `CACHE_STORE` (config/cache.php), default `file`; `array` disables throttle persistence.
    pub cache_store: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("APP_KEY is missing; copy the .env of the original panel")]
    MissingKey,
    #[error("APP_KEY: {0}")]
    Key(#[from] laravel_compat::encrypter::KeyError),
    #[error("database: {0}")]
    Db(String),
    #[error("DB_PREFIX is not supported (QUESTIONS.md Q4)")]
    DbPrefix,
}

impl Config {
    pub fn from_env(env: Env, base_path: &Path) -> Result<(Config, Encrypter), ConfigError> {
        let key = env.string("APP_KEY").filter(|k| !k.is_empty()).ok_or(ConfigError::MissingKey)?;
        let enc = Encrypter::from_env_values(&key, env.string("APP_PREVIOUS_KEYS").as_deref())?;
        if env.string("DB_PREFIX").is_some_and(|p| !p.is_empty()) {
            return Err(ConfigError::DbPrefix);
        }
        let db = DbConfig::from_lookup(env.lookup(), base_path).map_err(ConfigError::Db)?;
        let rl = |k: &str, max, period| RateLimit::from_env(&env, k, max, period);
        let rate_limits = RateLimits {
            client: rl("APP_API_CLIENT_RATELIMIT", 256, 1),
            application: rl("APP_API_APPLICATION_RATELIMIT", 256, 1),
            password_reset: rl("APP_API_PASSWORD_RESET_RATELIMIT", 2, 1),
            websocket: rl("APP_API_WEBSOCKET_RATELIMIT", 5, 1),
            backup_restore: rl("APP_API_BACKUP_RESTORE_RATELIMIT", 3, 15),
            database_create: rl("APP_API_DATABASE_CREATE_RATELIMIT", 2, 1),
            subuser_create: rl("APP_API_SUBUSER_CREATE_RATELIMIT", 10, 15),
            file_pull: rl("APP_API_FILE_PULL_RATELIMIT", 5, 10),
            default: rl("APP_API_DEFAULT_RATELIMIT", 2, 1),
        };
        let trusted_proxies = match env.string("TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(s) if s == "*" || s == "**" => vec![s],
            Some(s) => s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
        };
        let cfg = Config {
            base_path: base_path.to_path_buf(),
            app_name: env.string_or("APP_NAME", "Pelican"),
            app_url: env.string_or("APP_URL", "http://localhost").trim_end_matches('/').to_string(),
            app_env: env.string_or("APP_ENV", "production"),
            app_debug: env.bool_or("APP_DEBUG", false),
            app_installed: env.bool_or("APP_INSTALLED", true),
            db,
            rate_limits,
            trusted_proxies,
            api_key_limit: env.int_or("API_KEYS_LIMIT", 25),
            bcrypt_rounds: env.int_or("BCRYPT_ROUNDS", 12).clamp(4, 31) as u32,
            wings_timeout: env.int_or("GUZZLE_TIMEOUT", 15).max(0) as u64,
            wings_connect_timeout: env.int_or("GUZZLE_CONNECT_TIMEOUT", 5).max(0) as u64,
            session_cookie: env.string_or("SESSION_COOKIE", "pelican_session"),
            use_binary_prefix: env.bool_or("PANEL_USE_BINARY_PREFIX", true),
            files_max_edit_size: env.int_or("PANEL_FILES_MAX_EDIT_SIZE", 4 * 1024 * 1024),
            client_databases_enabled: env.bool_or("PANEL_CLIENT_DATABASES_ENABLED", true),
            client_databases_allow_random: env.bool_or("PANEL_CLIENT_DATABASES_ALLOW_RANDOM", true),
            cache_store: env.string_or("CACHE_STORE", "file"),
            env,
        };
        Ok((cfg, enc))
    }

    pub fn is_production(&self) -> bool {
        self.app_env == "production"
    }
}
