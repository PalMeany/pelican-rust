//! Database configuration and connection, mirroring `config/database.php` of
//! the original panel and the session setup done by Laravel's connectors.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbBackend, DbErr};

/// Which Laravel driver the `.env` selects (`DB_CONNECTION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driver {
    Sqlite,
    Mysql,
    Mariadb,
    Pgsql,
}

impl Driver {
    pub fn parse(s: &str) -> Option<Driver> {
        match s {
            "sqlite" => Some(Driver::Sqlite),
            "mysql" => Some(Driver::Mysql),
            "mariadb" => Some(Driver::Mariadb),
            "pgsql" => Some(Driver::Pgsql),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Driver::Sqlite => "sqlite",
            Driver::Mysql => "mysql",
            Driver::Mariadb => "mariadb",
            Driver::Pgsql => "pgsql",
        }
    }

    pub fn backend(self) -> DbBackend {
        match self {
            Driver::Sqlite => DbBackend::Sqlite,
            Driver::Mysql | Driver::Mariadb => DbBackend::MySql,
            Driver::Pgsql => DbBackend::Postgres,
        }
    }
}

/// Connection settings. Field semantics and defaults follow `config/database.php`.
#[derive(Clone, Debug)]
pub struct DbConfig {
    pub driver: Driver,
    pub host: String,
    pub port: u16,
    /// Database name, or for SQLite the resolved file path.
    pub database: String,
    pub username: String,
    pub password: String,
    pub socket: Option<String>,
    pub charset: String,
    pub collation: Option<String>,
    /// `DB_STRICT_MODE` (MySQL/MariaDB only, default false).
    pub strict: bool,
    /// `DB_SCHEMA` (PostgreSQL search_path, default `public`).
    pub schema: String,
    /// `DB_FOREIGN_KEYS` (SQLite, default true).
    pub foreign_keys: bool,
    pub max_connections: u32,
}

impl DbConfig {
    /// Build from env-like lookups. `base_path` is the panel root; SQLite paths
    /// are resolved against `<base_path>/database` like Laravel's `database_path()`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, base_path: &Path) -> Result<Self, String> {
        let driver_s = get("DB_CONNECTION").unwrap_or_else(|| "sqlite".into());
        let driver = Driver::parse(&driver_s).ok_or_else(|| format!("unsupported DB_CONNECTION `{driver_s}`"))?;
        let database_raw = get("DB_DATABASE");
        let database = match driver {
            Driver::Sqlite => {
                let db = database_raw.unwrap_or_else(|| "database.sqlite".into());
                if db.starts_with('/') || db == ":memory:" {
                    db
                } else {
                    base_path.join("database").join(db).to_string_lossy().into_owned()
                }
            }
            _ => database_raw.unwrap_or_else(|| "panel".into()),
        };
        let default_port = match driver {
            Driver::Pgsql => 5432,
            _ => 3306,
        };
        let port = match get("DB_PORT") {
            Some(p) => p.parse().map_err(|_| format!("invalid DB_PORT `{p}`"))?,
            None => default_port,
        };
        let truthy = |k: &str, default: bool| match get(k).as_deref() {
            None => default,
            Some(v) => matches!(v.to_ascii_lowercase().as_str(), "true" | "(true)" | "1" | "on" | "yes"),
        };
        Ok(DbConfig {
            driver,
            host: get("DB_HOST").unwrap_or_else(|| "127.0.0.1".into()),
            port,
            database,
            username: get("DB_USERNAME").unwrap_or_else(|| "pelican".into()),
            password: get("DB_PASSWORD").unwrap_or_default(),
            socket: get("DB_SOCKET").filter(|s| !s.is_empty()),
            charset: get("DB_CHARSET").unwrap_or_else(|| match driver {
                Driver::Pgsql => "utf8".into(),
                _ => "utf8mb4".into(),
            }),
            collation: match driver {
                Driver::Mysql | Driver::Mariadb => {
                    Some(get("DB_COLLATION").unwrap_or_else(|| "utf8mb4_unicode_ci".into()))
                }
                _ => None,
            },
            strict: truthy("DB_STRICT_MODE", false),
            schema: get("DB_SCHEMA").unwrap_or_else(|| "public".into()),
            foreign_keys: truthy("DB_FOREIGN_KEYS", true),
            max_connections: get("PELICAN_DB_POOL").and_then(|v| v.parse().ok()).unwrap_or(16),
        })
    }

    pub fn sqlite_path(&self) -> PathBuf {
        PathBuf::from(&self.database)
    }

    fn url(&self) -> String {
        let enc = |s: &str| urlencode(s);
        match self.driver {
            Driver::Sqlite => format!("sqlite://{}?mode=rwc", self.database),
            Driver::Mysql | Driver::Mariadb => format!(
                "mysql://{}:{}@{}:{}/{}",
                enc(&self.username),
                enc(&self.password),
                self.host,
                self.port,
                enc(&self.database)
            ),
            Driver::Pgsql => format!(
                "postgres://{}:{}@{}:{}/{}",
                enc(&self.username),
                enc(&self.password),
                self.host,
                self.port,
                enc(&self.database)
            ),
        }
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `SET SESSION sql_mode` Laravel's MySqlConnector issues (`getSqlMode`).
/// Note: like Laravel, the version check does not distinguish MariaDB.
fn laravel_sql_mode_statement(strict: bool, server_version: &str) -> &'static str {
    if !strict {
        return "SET SESSION sql_mode='NO_ENGINE_SUBSTITUTION'";
    }
    if version_ge(server_version, (8, 0, 11)) {
        "SET SESSION sql_mode='ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION'"
    } else {
        "SET SESSION sql_mode='ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_AUTO_CREATE_USER,NO_ENGINE_SUBSTITUTION'"
    }
}

fn version_ge(v: &str, min: (u32, u32, u32)) -> bool {
    let mut parts =
        v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>().unwrap_or(0));
    let cur = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    cur >= min
}

/// Open a connection pool configured the way Laravel configures its sessions:
/// MySQL `SET NAMES <charset> COLLATE <collation>` and the non-strict
/// `sql_mode`, but NO forced `time_zone` (Laravel leaves the server default,
/// which determines how TIMESTAMP columns round-trip); PostgreSQL search_path;
/// SQLite foreign keys per `DB_FOREIGN_KEYS` and the file's own journal mode.
pub async fn connect(cfg: &DbConfig) -> Result<DatabaseConnection, DbErr> {
    let mut opts = ConnectOptions::new(cfg.url());
    opts.max_connections(cfg.max_connections)
        .min_connections(0)
        .connect_timeout(Duration::from_secs(10))
        .sqlx_logging(false);

    match cfg.driver {
        Driver::Mysql | Driver::Mariadb => {
            let charset = cfg.charset.clone();
            let collation = cfg.collation.clone();
            let socket = cfg.socket.clone();
            let strict = cfg.strict;
            opts.map_sqlx_mysql_pool_opts(move |p| {
                p.after_connect(move |conn, _meta| {
                    Box::pin(async move {
                        use sea_orm::sqlx::{Executor, Row};
                        let row = conn.fetch_one("SELECT VERSION()").await?;
                        let version: String = row.try_get(0)?;
                        conn.execute(laravel_sql_mode_statement(strict, &version)).await?;
                        Ok(())
                    })
                })
            });
            opts.map_sqlx_mysql_opts(move |o| {
                let mut o = o.pipes_as_concat(false).no_engine_substitution(false).timezone(None).charset(&charset);
                if let Some(c) = &collation {
                    o = o.collation(c);
                }
                if let Some(s) = &socket {
                    o = o.socket(s);
                }
                o
            });
        }
        Driver::Pgsql => {
            let schema = cfg.schema.clone();
            opts.map_sqlx_postgres_opts(move |o| o.options([("search_path", schema.as_str())]));
        }
        Driver::Sqlite => {
            let fk = cfg.foreign_keys;
            opts.map_sqlx_sqlite_opts(move |o| o.foreign_keys(fk).create_if_missing(true));
        }
    }

    Database::connect(opts).await
}
