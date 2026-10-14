//! `migrations` table (Laravel migration bookkeeping; accessed via the query
//! builder, no dedicated Eloquent model in the original panel).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "migrations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub migration: String,
    pub batch: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
