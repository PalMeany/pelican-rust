//! Data layer for pelican-rs: connection handling for MySQL/MariaDB, PostgreSQL
//! and SQLite, flexible column types and SeaORM entities mapped onto the schema
//! created by the original Laravel panel.

pub mod types;

pub use types::{Bool, Int, Json, SmallBool, Timestamp};
pub mod column_order;
pub mod connect;
pub mod install;

pub use connect::{DbConfig, Driver, connect};
pub mod entities;
#[cfg(feature = "testing")]
pub mod testing;
