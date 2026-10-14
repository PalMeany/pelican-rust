//! `egg_variables` table (app/Models/EggVariable.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "egg_variables")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub egg_id: Int,
    pub name: String,
    pub description: String,
    pub env_variable: String,
    pub default_value: String,
    pub user_viewable: SmallBool,
    pub user_editable: SmallBool,
    pub rules: Json,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub sort: Option<Int>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
