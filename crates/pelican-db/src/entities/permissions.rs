//! `permissions` table (Spatie\Permission\Models\Permission of the original panel).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "permissions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub name: String,
    pub guard_name: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
