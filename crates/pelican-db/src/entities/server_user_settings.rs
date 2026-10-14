//! `server_user_settings` table (app/Models/ServerUserSettings.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "server_user_settings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub user_id: Int,
    pub server_id: Int,
    pub settings: Option<Json>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
