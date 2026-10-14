//! `nodes` table (app/Models/Node.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub uuid: Option<String>,
    pub public: SmallBool,
    pub name: String,
    pub description: Option<String>,
    pub fqdn: String,
    pub scheme: String,
    pub behind_proxy: Bool,
    pub maintenance_mode: Bool,
    pub memory: Int,
    pub memory_overallocate: Int,
    pub disk: Int,
    pub disk_overallocate: Int,
    pub cpu: Int,
    pub cpu_overallocate: Int,
    pub upload_size: Int,
    pub daemon_token_id: Option<String>,
    /// Laravel-encrypted (see laravel-compat::Encrypter).
    pub daemon_token: String,
    pub daemon_listen: Int,
    pub daemon_sftp: Int,
    pub daemon_sftp_alias: Option<String>,
    pub daemon_base: String,
    pub daemon_connect: Int,
    pub tags: Option<Json>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
