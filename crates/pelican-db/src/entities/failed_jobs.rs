//! `failed_jobs` table (Laravel database queue failures,
//! Illuminate\Queue\Failed\DatabaseUuidFailedJobProvider; `payload` holds a PHP-serialized exception dump).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "failed_jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub connection: String,
    pub queue: String,
    /// Contains the serialized job plus its exception (PHP-serialize format).
    pub payload: String,
    pub failed_at: Timestamp,
    pub exception: Option<String>,
    pub uuid: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
