//! `schedules` table (app/Models/Schedule.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "schedules")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub server_id: Int,
    pub name: String,
    pub cron_day_of_week: String,
    pub cron_day_of_month: String,
    pub cron_hour: String,
    pub cron_minute: String,
    pub is_active: Bool,
    pub is_processing: Bool,
    pub last_run_at: Option<Timestamp>,
    pub next_run_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub cron_month: String,
    pub only_when_online: SmallBool,
    pub timezone: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
