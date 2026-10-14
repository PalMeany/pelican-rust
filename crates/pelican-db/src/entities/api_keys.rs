//! `api_keys` table (app/Models/ApiKey.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "api_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    // `public` (legacy varchar(16)) exists on MySQL/MariaDB/PostgreSQL but not on
    // SQLite and is unused by the panel, so it is not mapped.
    /// Laravel-encrypted (`token` => 'encrypted' in app/Models/ApiKey.php).
    pub token: String,
    /// Cast to `array` in app/Models/ApiKey.php.
    pub allowed_ips: Json,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub user_id: Option<Int>,
    pub memo: Option<String>,
    pub identifier: Option<String>,
    /// Key type constant (TYPE_NONE/TYPE_ACCOUNT/TYPE_APPLICATION).
    pub key_type: Int,
    pub last_used_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    /// Cast to `array` in app/Models/ApiKey.php.
    pub permissions: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
