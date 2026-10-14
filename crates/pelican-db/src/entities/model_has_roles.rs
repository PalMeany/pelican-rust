//! `model_has_roles` pivot table (spatie/laravel-permission).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "model_has_roles")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub role_id: Int,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model_type: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub model_id: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
