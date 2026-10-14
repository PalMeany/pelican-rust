//! `servers` table (app/Models/Server.php).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Json, SmallBool, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "servers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub uuid: String,
    pub uuid_short: String,
    pub node_id: Int,
    pub name: String,
    pub owner_id: Int,
    pub memory: Int,
    pub swap: Int,
    pub disk: Int,
    pub io: Int,
    pub cpu: Int,
    pub egg_id: Int,
    pub startup: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub allocation_id: Option<Int>,
    pub image: String,
    pub description: Option<String>,
    pub skip_scripts: Bool,
    pub external_id: Option<String>,
    pub database_limit: Option<Int>,
    pub allocation_limit: Option<Int>,
    pub threads: Option<String>,
    pub backup_limit: Int,
    /// Enum cast (`App\Enums\ServerState`).
    pub status: Option<String>,
    pub installed_at: Option<Timestamp>,
    pub oom_killer: SmallBool,
    pub docker_labels: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
