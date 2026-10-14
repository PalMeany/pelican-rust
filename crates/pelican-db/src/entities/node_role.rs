//! `node_role` pivot table (node access restrictions per role; see
//! User::accessibleNodes() in app/Models/User.php).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "node_role")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: Int,
    #[sea_orm(primary_key, auto_increment = false)]
    pub role_id: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
