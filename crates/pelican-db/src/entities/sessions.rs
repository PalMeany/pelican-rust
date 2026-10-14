//! `sessions` table (Laravel's database session driver).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub user_id: Option<Int>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    /// Serialized PHP session payload (see laravel-compat phpserialize).
    pub payload: String,
    /// Unix timestamp of last activity.
    pub last_activity: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
