//! `passkeys` table (Laravel\Passkeys\Passkey model of the original panel).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Json, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "passkeys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub user_id: Int,
    pub name: String,
    pub credential_id: String,
    /// WebAuthn credential JSON blob.
    pub credential: Json,
    pub last_used_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
