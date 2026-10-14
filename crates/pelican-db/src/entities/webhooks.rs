//! `webhooks` table (app/Models/Webhook.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "webhooks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub webhook_configuration_id: Int,
    pub event: String,
    pub endpoint: String,
    pub successful_at: Option<Timestamp>,
    /// Casted to `array` in the original model.
    pub payload: Json,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
