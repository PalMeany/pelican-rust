//! `server_variables` table (app/Models/ServerVariable.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "server_variables")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub server_id: Int,
    pub variable_id: Int,
    pub variable_value: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
