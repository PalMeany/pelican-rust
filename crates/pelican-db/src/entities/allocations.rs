//! `allocations` table (app/Models/Allocation.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "allocations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub node_id: Int,
    pub ip: String,
    pub port: Int,
    pub server_id: Option<Int>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub ip_alias: Option<String>,
    pub notes: Option<String>,
    pub is_locked: Bool,
    pub show_port: Bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
