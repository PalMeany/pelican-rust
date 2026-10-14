//! `mountables` polymorphic pivot table
//! (app/Models/Egg.php `mounts()` / app/Models/Mount.php `eggs()|nodes()|servers()`).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mountables")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub mount_id: Int,
    #[sea_orm(primary_key, auto_increment = false)]
    pub mountable_type: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub mountable_id: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
