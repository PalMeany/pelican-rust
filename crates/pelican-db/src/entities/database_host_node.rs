//! `database_host_node` pivot table
//! (app/Models/DatabaseHost.php `nodes()` / app/Models/Node.php relationship);
//! unlike other pivots it carries its own `id` primary key.

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "database_host_node")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub node_id: Int,
    pub database_host_id: Int,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
