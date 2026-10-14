//! `databases` table (app/Models/Database.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "databases")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub server_id: Int,
    pub database_host_id: Int,
    pub database: String,
    pub username: String,
    pub remote: String,
    /// Laravel-encrypted (see laravel-compat::Encrypter).
    pub password: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub max_connections: Option<Int>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
