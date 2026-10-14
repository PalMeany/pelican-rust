//! Server databases on MySQL hosts: `DatabaseManagementService` naming/passwords
//! and the DDL of `app/Models/Database.php` run over `DatabaseHost::buildConnection()`.

use pelican_db::connect::{DbConfig, Driver};
use pelican_db::entities::{database_hosts, databases};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr};

/// `DatabaseManagementService::generateUniqueDatabaseName()`: `s{id}_` + name, 48 chars max.
pub fn unique_name(name: &str, server_id: i64) -> String {
    let prefix = format!("s{server_id}_");
    let keep = 48usize.saturating_sub(prefix.len());
    // substr() is byte based.
    let bytes = name.as_bytes();
    let cut = &bytes[..bytes.len().min(keep)];
    format!("{prefix}{}", String::from_utf8_lossy(cut))
}

/// `Utilities::randomStringWithSpecialCharacters($length)`.
pub fn random_password(length: usize) -> String {
    use rand::Rng;
    let mut s: Vec<u8> = str_random(length).into_bytes();
    let mut rng = rand::rng();
    // `for ($i = 0; $i < random_int(2, 6); $i++)` re-draws the bound every iteration.
    let mut i = 0;
    while i < rng.random_range(2..=6) {
        let c = [b'!', b'@', b'=', b'.', b'+', b'^'][rng.random_range(0..6)];
        let pos = rng.random_range(0..length.max(1));
        if let Some(slot) = s.get_mut(pos) {
            *slot = c;
        }
        i += 1;
    }
    String::from_utf8_lossy(&s).into_owned()
}

/// `Str::random($n)`: `[A-Za-z0-9]`.
pub fn str_random(n: usize) -> String {
    use rand::Rng;
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..n).map(|_| ALPHA[rng.random_range(0..ALPHA.len())] as char).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// `Illuminate\Database\QueryException` (connection or statement failure).
    #[error("query: {0}")]
    Query(#[from] DbErr),
    #[error("decrypt")]
    Decrypt,
}

/// `DatabaseHost::buildConnection()` (database `mysql`, utf8 / utf8_unicode_ci).
pub struct HostConnection {
    conn: DatabaseConnection,
}

impl HostConnection {
    pub async fn open(host: &database_hosts::Model, enc: &laravel_compat::Encrypter) -> Result<Self, HostError> {
        let password = enc.decrypt_string_lenient(&host.password).map_err(|_| HostError::Decrypt)?;
        let cfg = DbConfig {
            driver: Driver::Mysql,
            host: host.host.clone(),
            port: u16::try_from(host.port.0).unwrap_or(3306),
            database: "mysql".into(),
            username: host.username.clone(),
            password,
            socket: None,
            charset: "utf8".into(),
            collation: Some("utf8_unicode_ci".into()),
            strict: false,
            schema: "public".into(),
            foreign_keys: true,
            max_connections: 1,
        };
        Ok(HostConnection { conn: pelican_db::connect::connect(&cfg).await? })
    }

    async fn run(&self, sql: String) -> Result<(), HostError> {
        self.conn.execute_unprepared(&sql).await?;
        Ok(())
    }

    pub async fn create_database(&self, db: &databases::Model) -> Result<(), HostError> {
        self.run(format!("CREATE DATABASE IF NOT EXISTS `{}`", db.database)).await
    }

    pub async fn create_user(&self, db: &databases::Model, password: &str) -> Result<(), HostError> {
        let mut sql = format!("CREATE USER `{}`@`{}` IDENTIFIED BY '{}'", db.username, db.remote, password);
        // `!empty($this->max_connections)`.
        if let Some(n) = db.max_connections.filter(|n| n.0 != 0) {
            sql.push_str(&format!(" WITH MAX_USER_CONNECTIONS {}", n.0));
        }
        self.run(sql).await
    }

    pub async fn assign_user(&self, db: &databases::Model) -> Result<(), HostError> {
        self.run(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, REFERENCES, INDEX, LOCK TABLES, CREATE ROUTINE, ALTER ROUTINE, EXECUTE, CREATE TEMPORARY TABLES, CREATE VIEW, SHOW VIEW, EVENT, TRIGGER ON `{}`.* TO `{}`@`{}`",
            db.database, db.username, db.remote
        ))
        .await
    }

    pub async fn flush_privileges(&self) -> Result<(), HostError> {
        self.run("FLUSH PRIVILEGES".into()).await
    }

    pub async fn drop_database(&self, db: &databases::Model) -> Result<(), HostError> {
        self.run(format!("DROP DATABASE IF EXISTS `{}`", db.database)).await
    }

    pub async fn drop_user(&self, db: &databases::Model) -> Result<(), HostError> {
        self.run(format!("DROP USER IF EXISTS `{}`@`{}`", db.username, db.remote)).await
    }

    pub async fn close(self) {
        let _ = self.conn.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming() {
        assert_eq!(unique_name("abc", 12), "s12_abc");
        assert_eq!(unique_name(&"x".repeat(60), 1).len(), 48);
        let p = random_password(24);
        assert_eq!(p.len(), 24);
    }
}
