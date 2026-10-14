//! `eggs` table (app/Models/Egg.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "eggs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub name: String,
    pub description: Option<String>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub config_from: Option<Int>,
    pub config_stop: Option<String>,
    /// JSON-encoded string (validation `'json'`, no Eloquent cast).
    pub config_logs: Option<String>,
    /// JSON-encoded string (validation `'json'`, no Eloquent cast).
    pub config_startup: Option<String>,
    /// JSON-encoded string (validation `'json'`, no Eloquent cast).
    pub config_files: Option<String>,
    pub script_install: Option<String>,
    pub script_is_privileged: Bool,
    pub script_entry: String,
    pub script_container: String,
    pub copy_script_from: Option<Int>,
    pub uuid: Option<String>,
    pub author: String,
    pub features: Option<Json>,
    pub docker_images: Option<Json>,
    pub update_url: Option<String>,
    pub file_denylist: Option<Json>,
    pub force_outgoing_ip: Bool,
    /// Cast to `array` but stored as `text` on PostgreSQL.
    pub tags: Option<Json>,
    pub startup_commands: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
