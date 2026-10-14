//! `notifications` table (Laravel's database notification channel,
//! Illuminate\Notifications\DatabaseNotification).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "notifications")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    #[sea_orm(column_name = "type")]
    pub r#type: String,
    pub notifiable_type: String,
    pub notifiable_id: Int,
    pub data: Json,
    pub read_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
