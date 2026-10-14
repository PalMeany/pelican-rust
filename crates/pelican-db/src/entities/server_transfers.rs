//! `server_transfers` table (app/Models/ServerTransfer.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "server_transfers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub server_id: Int,
    pub successful: Option<Bool>,
    pub old_node: Int,
    pub new_node: Int,
    pub old_allocation: Option<Int>,
    pub new_allocation: Option<Int>,
    pub old_additional_allocations: Option<Json>,
    pub new_additional_allocations: Option<Json>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub archived: Bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
