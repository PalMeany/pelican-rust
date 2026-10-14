//! `webhook_configurations` table (app/Models/WebhookConfiguration.php, soft-deleting).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "webhook_configurations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub endpoint: String,
    pub name: String,
    /// Casted to `array` in the original model.
    pub events: Json,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    #[sea_orm(column_name = "type")]
    pub r#type: Option<String>,
    /// Casted to `array` in the original model.
    pub payload: Option<Json>,
    /// Casted to `array` in the original model.
    pub headers: Option<Json>,
    /// String-backed `WebhookScope` enum (`global` / `server`).
    pub scope: String,
    pub server_id: Option<Int>,
    pub description: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
