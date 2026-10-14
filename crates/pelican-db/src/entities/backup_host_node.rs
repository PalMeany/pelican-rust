//! `backup_host_node` pivot table
//! (app/Models/BackupHost.php `nodes()` / app/Models/Node.php relationship).
//! The schema defines no primary key; `node_id` is UNIQUE and identifies the row.

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "backup_host_node")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: Int,
    pub backup_host_id: Int,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
