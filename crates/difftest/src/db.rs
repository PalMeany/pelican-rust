//! Database reset / fixture loading / dumping for the shared difftest database.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use indexmap::IndexMap;
use laravel_compat::Encrypter;
use pelican_db::{DbConfig, Driver};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, JsonValue, Statement,
    TransactionTrait,
};

use crate::scenario::Rows;

pub const SQLITE_PATH: &str =
    "/Users/tazik/Desktop/Code/claude/pelican/.difftest/sqlite/database.sqlite";

/// Connection settings for the database panel-orig uses (see scripts/difftest).
pub fn config_for(backend: &str) -> Result<DbConfig> {
    let (driver, host, port, user, pass, db) = match backend {
        "mariadb" => (Driver::Mariadb, "mariadb", 3306, "root", "root", "panel"),
        "mysql" => (Driver::Mysql, "mysql", 3306, "root", "root", "panel"),
        "pgsql" => (
            Driver::Pgsql,
            "postgres",
            5432,
            "pelican",
            "pelican",
            "panel",
        ),
        "sqlite" => (Driver::Sqlite, "", 0, "", "", SQLITE_PATH),
        other => return Err(anyhow!("unknown backend {other}")),
    };
    DbConfig::from_lookup(
        |k| match k {
            "DB_CONNECTION" => Some(driver.as_str().into()),
            "DB_HOST" => Some(host.into()),
            "DB_PORT" => Some(port.to_string()),
            "DB_USERNAME" => Some(user.into()),
            "DB_PASSWORD" => Some(pass.into()),
            "DB_DATABASE" => Some(db.into()),
            _ => None,
        },
        Path::new("/nonexistent"),
    )
    .map_err(anyhow::Error::msg)
}

/// A table row as column → JSON value, in column order.
pub type Row = IndexMap<String, serde_json::Value>;
/// table → rows sorted by primary key.
pub type Dump = IndexMap<String, Vec<Row>>;

pub struct TestDb {
    pub db: DatabaseConnection,
    pub backend: DbBackend,
    pub tables: Vec<String>,
}

impl TestDb {
    pub async fn connect(cfg: &DbConfig) -> Result<Self> {
        let db = pelican_db::connect(cfg).await?;
        let backend = db.get_database_backend();
        let sql = match backend {
            DbBackend::Sqlite => {
                "SELECT name AS t FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            }
            DbBackend::MySql => {
                "SELECT table_name AS t FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name"
            }
            _ => {
                "SELECT table_name AS t FROM information_schema.tables WHERE table_schema = current_schema() ORDER BY table_name"
            }
        };
        let rows = db
            .query_all_raw(Statement::from_string(backend, sql))
            .await?;
        let tables = rows
            .iter()
            .filter_map(|r| r.try_get::<String>("", "t").ok())
            .filter(|t| t != "migrations")
            .collect();
        Ok(TestDb {
            db,
            backend,
            tables,
        })
    }

    fn q(&self, ident: &str) -> String {
        match self.backend {
            DbBackend::MySql => format!("`{ident}`"),
            _ => format!("\"{ident}\""),
        }
    }

    /// Empty every table (except `migrations`) and reset auto-increment counters.
    pub async fn reset(&self) -> Result<()> {
        let txn = self.db.begin().await?;
        match self.backend {
            DbBackend::MySql => {
                txn.execute_unprepared("SET FOREIGN_KEY_CHECKS=0").await?;
                for t in &self.tables {
                    txn.execute_unprepared(&format!("TRUNCATE TABLE {}", self.q(t)))
                        .await?;
                }
                txn.execute_unprepared("SET FOREIGN_KEY_CHECKS=1").await?;
            }
            DbBackend::Postgres => {
                let list = self
                    .tables
                    .iter()
                    .map(|t| self.q(t))
                    .collect::<Vec<_>>()
                    .join(", ");
                txn.execute_unprepared(&format!("TRUNCATE {list} RESTART IDENTITY CASCADE"))
                    .await?;
            }
            _ => {
                txn.execute_unprepared("PRAGMA defer_foreign_keys = ON")
                    .await?;
                for t in &self.tables {
                    txn.execute_unprepared(&format!("DELETE FROM {}", self.q(t)))
                        .await?;
                }
                txn.execute_unprepared("DELETE FROM sqlite_sequence")
                    .await?;
            }
        }
        txn.commit().await?;
        Ok(())
    }

    /// Insert fixture rows with literal SQL (untyped literals coerce on every backend).
    pub async fn load(
        &self,
        rows: &Rows,
        enc: &Encrypter,
        now: chrono::NaiveDateTime,
    ) -> Result<()> {
        let txn = self.db.begin().await?;
        if self.backend == DbBackend::MySql {
            txn.execute_unprepared("SET FOREIGN_KEY_CHECKS=0").await?;
        } else if self.backend == DbBackend::Sqlite {
            txn.execute_unprepared("PRAGMA defer_foreign_keys = ON")
                .await?;
        }
        for (table, list) in rows {
            for row in list {
                let cols = row.keys().map(|c| self.q(c)).collect::<Vec<_>>().join(", ");
                let mut vals = Vec::with_capacity(row.len());
                for (col, v) in row {
                    let lit =
                        fixture_value(v, enc, now).with_context(|| format!("{table}.{col}"))?;
                    vals.push(match lit {
                        None => "NULL".to_string(),
                        Some(s) => self.literal(&s),
                    });
                }
                let sql = format!(
                    "INSERT INTO {} ({cols}) VALUES ({})",
                    self.q(table),
                    vals.join(", ")
                );
                txn.execute_unprepared(&sql)
                    .await
                    .with_context(|| sql.clone())?;
            }
        }
        if self.backend == DbBackend::MySql {
            txn.execute_unprepared("SET FOREIGN_KEY_CHECKS=1").await?;
        }
        if self.backend == DbBackend::Postgres {
            // Advance sequences past explicitly inserted ids.
            for table in rows.keys() {
                let sql = format!(
                    "SELECT setval(pg_get_serial_sequence('{table}', 'id'), COALESCE((SELECT MAX(id) FROM {}), 0) + 1, false) \
                     WHERE pg_get_serial_sequence('{table}', 'id') IS NOT NULL",
                    self.q(table)
                );
                txn.execute_unprepared(&sql)
                    .await
                    .with_context(|| sql.clone())?;
            }
        }
        txn.commit().await?;
        Ok(())
    }

    fn literal(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for ch in s.chars() {
            match ch {
                '\'' => out.push_str("''"),
                '\\' if self.backend == DbBackend::MySql => out.push_str("\\\\"),
                _ => out.push(ch),
            }
        }
        out.push('\'');
        out
    }

    /// Dump every table, rows sorted by primary key (or all columns).
    pub async fn dump(&self) -> Result<Dump> {
        let mut out = Dump::new();
        for t in &self.tables {
            let sql = format!("SELECT * FROM {}", self.q(t));
            let rows = JsonValue::find_by_statement(Statement::from_string(self.backend, sql))
                .all(&self.db)
                .await
                .with_context(|| format!("dumping {t}"))?;
            let mut rows: Vec<Row> = rows
                .into_iter()
                .map(|v| match v {
                    serde_json::Value::Object(m) => m.into_iter().collect(),
                    _ => Row::new(),
                })
                .collect();
            rows.sort_by_key(row_key);
            out.insert(t.clone(), rows);
        }
        Ok(out)
    }
}

/// Primary-key-ish sort/identity key: `id` if present, otherwise all columns.
pub fn row_key(r: &Row) -> String {
    match r.get("id") {
        Some(serde_json::Value::Number(n)) => format!("{:>20}", n),
        Some(v) => v.to_string(),
        None => serde_json::to_string(r).unwrap_or_default(),
    }
}

/// Resolve a fixture value to the literal text to insert (None = NULL).
pub fn fixture_value(
    v: &serde_yaml::Value,
    enc: &Encrypter,
    now: chrono::NaiveDateTime,
) -> Result<Option<String>> {
    use serde_yaml::Value as Y;
    Ok(match v {
        Y::Null => None,
        Y::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
        Y::Number(n) => Some(n.to_string()),
        Y::String(s) => Some(directive(s, enc, now)?),
        Y::Sequence(_) | Y::Mapping(_) => {
            let json: serde_json::Value = serde_json::to_value(v)?;
            Some(laravel_compat::json::to_string(&json)?)
        }
        Y::Tagged(t) => return fixture_value(&t.value, enc, now),
    })
}

fn directive(s: &str, enc: &Encrypter, now: chrono::NaiveDateTime) -> Result<String> {
    let fmt = |t: chrono::NaiveDateTime| t.format("%Y-%m-%d %H:%M:%S").to_string();
    if let Some(p) = s.strip_prefix("@encrypt:") {
        return Ok(enc.encrypt_string(p));
    }
    if let Some(p) = s.strip_prefix("@bcrypt:") {
        return Ok(laravel_compat::hashing::make(
            p,
            laravel_compat::hashing::DEFAULT_ROUNDS,
        )?);
    }
    if s == "@now" {
        return Ok(fmt(now));
    }
    if let Some(p) = s.strip_prefix("@now+") {
        return Ok(fmt(now + chrono::Duration::seconds(p.parse()?)));
    }
    if let Some(p) = s.strip_prefix("@now-") {
        return Ok(fmt(now - chrono::Duration::seconds(p.parse()?)));
    }
    Ok(s.to_string())
}

/// A row-level change between two dumps.
#[derive(Debug, Clone)]
pub enum Change {
    Insert {
        table: String,
        row: Row,
    },
    Delete {
        table: String,
        row: Row,
    },
    Update {
        table: String,
        key: String,
        cols: IndexMap<String, (serde_json::Value, serde_json::Value)>,
    },
}

pub fn diff(before: &Dump, after: &Dump) -> Vec<Change> {
    let mut out = Vec::new();
    for (table, rows_after) in after {
        let empty = Vec::new();
        let rows_before = before.get(table).unwrap_or(&empty);
        let b: IndexMap<String, &Row> = rows_before.iter().map(|r| (row_key(r), r)).collect();
        let a: IndexMap<String, &Row> = rows_after.iter().map(|r| (row_key(r), r)).collect();
        for (k, ra) in &a {
            match b.get(k) {
                None => out.push(Change::Insert {
                    table: table.clone(),
                    row: (*ra).clone(),
                }),
                Some(rb) => {
                    let mut cols = IndexMap::new();
                    for (c, va) in ra.iter() {
                        let vb = rb.get(c).cloned().unwrap_or(serde_json::Value::Null);
                        if &vb != va {
                            cols.insert(c.clone(), (vb, va.clone()));
                        }
                    }
                    if !cols.is_empty() {
                        out.push(Change::Update {
                            table: table.clone(),
                            key: k.trim().to_string(),
                            cols,
                        });
                    }
                }
            }
        }
        for (k, rb) in &b {
            if !a.contains_key(k) {
                out.push(Change::Delete {
                    table: table.clone(),
                    row: (*rb).clone(),
                });
            }
        }
    }
    out
}
