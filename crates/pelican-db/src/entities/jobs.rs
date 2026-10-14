//! `jobs` table (Laravel database queue driver, Illuminate\Queue\DatabaseQueue;
//! timestamps are unix epochs written via the query builder).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub queue: String,
    pub payload: String,
    pub attempts: Int,
    pub reserved_at: Option<Int>,
    pub available_at: Int,
    pub created_at: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
