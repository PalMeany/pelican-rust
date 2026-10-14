//! `backup_hosts` table (app/Models/BackupHost.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "backup_hosts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub name: String,
    pub schema: String,
    pub configuration: Option<Json>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
