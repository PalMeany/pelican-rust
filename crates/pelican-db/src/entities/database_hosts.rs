//! `database_hosts` table (app/Models/DatabaseHost.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "database_hosts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub name: String,
    pub host: String,
    pub port: Int,
    pub username: String,
    /// Laravel-encrypted (see laravel-compat::Encrypter).
    pub password: String,
    pub max_databases: Option<Int>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
