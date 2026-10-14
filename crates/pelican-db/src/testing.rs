//! Test support: throwaway databases on every backend (feature `testing`).
//!
//! Hosts default to the difftest compose services (`mariadb`, `mysql`,
//! `postgres`); `scripts/cargo` runs tests inside that network.
//! `TEST_DB_DRIVERS=sqlite,mariadb,mysql,pgsql` selects the backends.

use std::path::Path;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection};

use crate::connect::{DbConfig, Driver, connect};
use crate::install::install_fresh;

pub fn drivers_under_test() -> Vec<Driver> {
    std::env::var("TEST_DB_DRIVERS")
        .unwrap_or_else(|_| "sqlite,mariadb,mysql,pgsql".into())
        .split(',')
        .filter_map(|s| Driver::parse(s.trim()))
        .collect()
}

pub struct TestDb {
    pub cfg: DbConfig,
    pub db: DatabaseConnection,
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

/// Server-level settings for a backend: (host, port, admin user, admin password).
fn server(driver: Driver) -> (String, u16, String, String) {
    match driver {
        Driver::Mariadb => (env_or("TEST_MARIADB_HOST", "mariadb"), 3306, "root".into(), "root".into()),
        Driver::Mysql => (env_or("TEST_MYSQL_HOST", "mysql"), 3306, "root".into(), "root".into()),
        Driver::Pgsql => (env_or("TEST_PGSQL_HOST", "postgres"), 5432, "pelican".into(), "pelican".into()),
        Driver::Sqlite => (String::new(), 0, String::new(), String::new()),
    }
}

/// Create an empty database with a unique name and connect to it.
pub async fn empty(driver: Driver) -> anyhow::Result<TestDb> {
    let name = format!("t_{}", uuid_like());
    let (host, port, user, pass) = server(driver);
    let lookup = |k: &str| -> Option<String> {
        match k {
            "DB_CONNECTION" => Some(driver.as_str().into()),
            "DB_HOST" => Some(host.clone()),
            "DB_PORT" => Some(port.to_string()),
            "DB_USERNAME" => Some(user.clone()),
            "DB_PASSWORD" => Some(pass.clone()),
            "DB_DATABASE" => Some(match driver {
                Driver::Sqlite => format!("/tmp/{name}.sqlite"),
                _ => name.clone(),
            }),
            _ => None,
        }
    };
    let cfg = DbConfig::from_lookup(lookup, Path::new("/nonexistent")).map_err(anyhow::Error::msg)?;
    match driver {
        Driver::Mariadb | Driver::Mysql => {
            let admin = Database::connect(format!("mysql://{user}:{pass}@{host}:{port}")).await?;
            admin
                .execute_unprepared(&format!(
                    "CREATE DATABASE `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
                ))
                .await?;
            admin.close().await?;
        }
        Driver::Pgsql => {
            let admin = Database::connect(format!("postgres://{user}:{pass}@{host}:{port}/postgres")).await?;
            admin.execute_unprepared(&format!("CREATE DATABASE \"{name}\"")).await?;
            admin.close().await?;
        }
        Driver::Sqlite => {}
    }
    let db = connect(&cfg).await?;
    Ok(TestDb { cfg, db })
}

/// An empty database with the full original schema installed.
pub async fn fresh(driver: Driver) -> anyhow::Result<TestDb> {
    let t = empty(driver).await?;
    install_fresh(&t.db, driver).await?;
    Ok(t)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:x}{:04x}", nanos, std::process::id() & 0xffff)
}
