//! `subusers` table (app/Models/Subuser.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "subusers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub user_id: Int,
    pub server_id: Int,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub permissions: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
