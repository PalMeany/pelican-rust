//! `tasks_log` table (no model in the original panel;
//! see database/migrations/2016_02_27_163447_add_tasks_log_table.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "tasks_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub task_id: Int,
    pub run_time: Timestamp,
    pub run_status: Int,
    pub response: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
