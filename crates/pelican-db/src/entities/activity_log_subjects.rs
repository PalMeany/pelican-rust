//! `activity_log_subjects` table (app/Models/ActivityLogSubject.php).

use sea_orm::entity::prelude::*;

use crate::types::Int;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "activity_log_subjects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub activity_log_id: Int,
    pub subject_type: String,
    pub subject_id: Int,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
