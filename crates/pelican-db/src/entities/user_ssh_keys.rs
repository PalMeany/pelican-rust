//! `user_ssh_keys` table (app/Models/UserSSHKey.php).

use sea_orm::entity::prelude::*;

use crate::types::{Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "user_ssh_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub user_id: Int,
    pub name: String,
    pub fingerprint: String,
    pub public_key: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    /// SoftDeletes in app/Models/UserSSHKey.php.
    pub deleted_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
