//! `role_has_permissions` pivot table (spatie/laravel-permission).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "role_has_permissions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub permission_id: Int,
    #[sea_orm(primary_key, auto_increment = false)]
    pub role_id: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
