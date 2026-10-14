//! `Server::validateCurrentState()` and `ServerStateConflictException` messages.

use pelican_db::entities::{nodes, server_transfers, servers};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

pub const SUSPENDED: &str = "This server is currently suspended and the functionality requested is unavailable.";
pub const MAINTENANCE: &str =
    "The node of this server is currently under maintenance and the functionality requested is unavailable.";
pub const NOT_INSTALLED: &str = "This server has not yet completed its installation process, please try again later.";
pub const RESTORING: &str = "This server is currently restoring from a backup, please try again later.";
pub const TRANSFERRING: &str = "This server is currently being transferred to a new machine, please try again later.";

/// `server.transfer`: latest unfinished transfer.
pub async fn active_transfer<C: ConnectionTrait>(
    db: &C,
    server_id: i64,
) -> Result<Option<server_transfers::Model>, DbErr> {
    server_transfers::Entity::find()
        .filter(server_transfers::Column::ServerId.eq(server_id))
        .filter(server_transfers::Column::Successful.is_null())
        .order_by_desc(server_transfers::Column::Id)
        .one(db)
        .await
}

pub fn is_installed(server: &servers::Model) -> bool {
    !matches!(server.status.as_deref(), Some("installing" | "install_failed" | "reinstall_failed"))
}

/// Returns the 409 detail if the server is in a conflicting state.
pub async fn conflict<C: ConnectionTrait>(db: &C, server: &servers::Model) -> Result<Option<&'static str>, DbErr> {
    if server.status.as_deref() == Some("suspended") {
        return Ok(Some(SUSPENDED));
    }
    let node = nodes::Entity::find_by_id(server.node_id).one(db).await?;
    if node.is_some_and(|n| n.maintenance_mode.0) {
        return Ok(Some(MAINTENANCE));
    }
    if !is_installed(server) {
        return Ok(Some(NOT_INSTALLED));
    }
    if server.status.as_deref() == Some("restoring_backup") {
        return Ok(Some(RESTORING));
    }
    if active_transfer(db, server.id.0).await?.is_some() {
        return Ok(Some(TRANSFERRING));
    }
    Ok(None)
}
