//! `activity_logs` table (app/Models/ActivityLog.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "activity_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub event: String,
    pub ip: String,
    pub description: Option<String>,
    pub actor_type: Option<String>,
    pub actor_id: Option<Int>,
    /// Casted to `collection` in the original model.
    pub properties: Json,
    pub timestamp: Timestamp,
    pub api_key_id: Option<Int>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
