//! `api_logs` table (legacy API request log; no dedicated Eloquent model in the
//! original panel, written through the query builder).

use sea_orm::entity::prelude::*;

use crate::types::{Bool, Int, Timestamp};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "api_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Int,
    pub authorized: Bool,
    pub error: Option<String>,
    pub key: Option<String>,
    pub method: String,
    pub route: String,
    pub content: Option<String>,
    pub user_agent: String,
    /// PostgreSQL column type is `inet`.
    pub request_ip: String,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
