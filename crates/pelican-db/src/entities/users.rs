//! `users` table (app/Models/User.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub uuid: String,
    pub email: String,
    /// bcrypt hash (auth), never decrypted in application code.
    pub password: String,
    pub remember_token: Option<String>,
    pub language: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub username: String,
    pub external_id: Option<String>,
    pub timezone: String,
    /// Cast to `array` in app/Models/User.php.
    pub oauth: Option<Json>,
    /// Cast to `array` in app/Models/User.php (legacy rows may store raw JSON-as-string).
    pub customization: Option<Json>,
    /// Laravel-encrypted (see laravel-compat::Encrypter).
    pub mfa_app_secret: Option<String>,
    /// Laravel-encrypted (`encrypted:array` in app/Models/User.php).
    pub mfa_app_recovery_codes: Option<String>,
    pub mfa_email_enabled: Bool,
    pub is_managed_externally: Bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
