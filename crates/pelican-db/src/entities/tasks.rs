//! `tasks` table (app/Models/Task.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub schedule_id: Int,
    pub sequence_id: Int,
    pub action: String,
    pub payload: String,
    pub time_offset: Int,
    pub is_queued: Bool,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub continue_on_failure: SmallBool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
