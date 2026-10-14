//! `backups` table (app/Models/Backup.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "backups")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub server_id: Int,
    pub uuid: String,
    pub name: String,
    /// Array cast in the original model; stored as text on non-PostgreSQL backends.
    pub ignored_files: Json,
    pub checksum: Option<String>,
    pub bytes: Int,
    pub completed_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub is_successful: Bool,
    pub upload_id: Option<String>,
    pub is_locked: SmallBool,
    pub backup_host_id: Int,
    pub is_scheduled: Bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
