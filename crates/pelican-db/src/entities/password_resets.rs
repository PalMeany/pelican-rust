//! `password_resets` table (Laravel's default password reset table).

use sea_orm::entity::prelude::*;

use crate::types::Timestamp;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "password_resets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub email: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub token: String,
    pub created_at: Timestamp,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
