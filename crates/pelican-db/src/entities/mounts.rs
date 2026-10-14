//! `mounts` table (app/Models/Mount.php, `$timestamps = false`).

use sea_orm::entity::prelude::*;

use crate::types::{Int, SmallBool};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mounts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub uuid: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub target: String,
    pub read_only: SmallBool,
    pub user_mountable: SmallBool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
