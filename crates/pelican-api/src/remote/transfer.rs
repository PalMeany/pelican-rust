//! R8/R9 transfer callbacks (`ServerTransferController`).

use axum::Extension;
use axum::extract::State;
use axum::response::Response;
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, nodes, server_transfers, servers};
use pelican_db::{Bool, Int, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};

use super::{node, node_forbidden};
use crate::error::{ApiError, ApiResult};
use crate::json::no_content;
use crate::model_events::{self, Action};
use crate::pipeline::Ctx;

fn not_transferring() -> ApiError {
    ApiError::new(409, "ConflictHttpException", "Server is not being transferred.", crate::error::Style::Pretty)
}

fn ids(list: &Option<pelican_db::Json>) -> Vec<i64> {
    match list.as_ref().map(|j| &j.0) {
        Some(serde_json::Value::Array(a)) => {
            a.iter().filter_map(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).collect()
        }
        _ => Vec::new(),
    }
}

/// `Allocation::RELEASE_ATTRIBUTES` via a builder update (sets updated_at).
async fn release<C: sea_orm::ConnectionTrait>(db: &C, ids: Vec<i64>) -> Result<(), sea_orm::DbErr> {
    if ids.is_empty() {
        return Ok(());
    }
    let now = Timestamp(pelican_core::time::now());
    allocations::Entity::update_many()
        .col_expr(allocations::Column::ServerId, Expr::value(Option::<Int>::None))
        .col_expr(allocations::Column::Notes, Expr::value(Option::<String>::None))
        .col_expr(allocations::Column::IsLocked, Expr::value(Bool(false)))
        .col_expr(allocations::Column::ShowPort, Expr::value(Bool(true)))
        .col_expr(allocations::Column::UpdatedAt, Expr::value(now))
        .filter(allocations::Column::Id.is_in(ids))
        .exec(db)
        .await?;
    Ok(())
}

fn non_empty(v: &Option<pelican_db::Json>) -> bool {
    v.as_ref().is_some_and(|j| !pelican_core::php::empty(&j.0))
}

/// R8 `POST /servers/{server}/transfer/failure`.
pub async fn failure(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let server = ctx.bound.server()?;
    let transfer =
        pelican_core::server_state::active_transfer(&st.db, server.id.0).await?.ok_or_else(not_transferring)?;
    let node = node(&ctx)?;
    if node.id != transfer.new_node && node.id != transfer.old_node {
        return Err(node_forbidden());
    }
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    // $transfer->forceFill(['successful' => false])->saveOrFail() — no write (and
    // no event) when it was already false.
    let transfer_dirty = transfer.successful != Some(Bool(false));
    if transfer_dirty {
        server_transfers::Entity::update_many()
            .col_expr(server_transfers::Column::Successful, Expr::value(Some(Bool(false))))
            .col_expr(server_transfers::Column::UpdatedAt, Expr::value(now))
            .filter(server_transfers::Column::Id.eq(transfer.id))
            .exec(&txn)
            .await?;
    }
    if transfer.new_allocation.is_some_and(|a| a.0 != 0) || non_empty(&transfer.new_additional_allocations) {
        let mut list: Vec<i64> = transfer.new_allocation.map(|a| a.0).into_iter().collect();
        list.extend(ids(&transfer.new_additional_allocations));
        release(&txn, list).await?;
    }
    txn.commit().await?;
    // ServerTransferController::failure: instance saveOrFail fires eloquent.updated
    // when dirty; the RELEASE_ATTRIBUTES write is a mass update and fires nothing.
    if transfer_dirty {
        model_events::fire_id::<server_transfers::Entity>(&st, Action::Updated, transfer.id.0).await;
    }
    Ok(no_content())
}

/// R9 `POST /servers/{server}/transfer/success`.
pub async fn success(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let server = ctx.bound.server()?.clone();
    let transfer =
        pelican_core::server_state::active_transfer(&st.db, server.id.0).await?.ok_or_else(not_transferring)?;
    let node = node(&ctx)?;
    if node.id != transfer.new_node {
        return Err(node_forbidden());
    }
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    let mut upd = servers::Entity::update_many().filter(servers::Column::Id.eq(server.id));
    let mut dirty = false;
    if transfer.old_allocation.is_some_and(|a| a.0 != 0) || non_empty(&transfer.old_additional_allocations) {
        let mut list: Vec<i64> = transfer.old_allocation.map(|a| a.0).into_iter().collect();
        list.extend(ids(&transfer.old_additional_allocations));
        release(&txn, list).await?;
        if server.allocation_id != transfer.new_allocation {
            upd = upd.col_expr(servers::Column::AllocationId, Expr::value(transfer.new_allocation));
            dirty = true;
        }
    }
    if server.node_id != transfer.new_node {
        upd = upd.col_expr(servers::Column::NodeId, Expr::value(transfer.new_node));
        dirty = true;
    }
    if dirty {
        upd.col_expr(servers::Column::UpdatedAt, Expr::value(now)).exec(&txn).await?;
    }
    // $server->transfer->update(['successful' => true]) — no write when already true.
    let transfer_dirty = transfer.successful != Some(Bool(true));
    if transfer_dirty {
        server_transfers::Entity::update_many()
            .col_expr(server_transfers::Column::Successful, Expr::value(Some(Bool(true))))
            .col_expr(server_transfers::Column::UpdatedAt, Expr::value(now))
            .filter(server_transfers::Column::Id.eq(transfer.id))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    // ServerTransferController::success: $server->update($data) and
    // $server->transfer->update(['successful' => true]) — both instance saves.
    if dirty {
        model_events::fire_id::<servers::Entity>(&st, Action::Updated, server.id.0).await;
    }
    if transfer_dirty {
        model_events::fire_id::<server_transfers::Entity>(&st, Action::Updated, transfer.id.0).await;
    }
    // Delete the server on the old node; only connection errors are tolerated.
    if let Some(old) = nodes::Entity::find_by_id(transfer.old_node).one(&st.db).await? {
        let conn = st.node_conn(&old).map_err(|_| ApiError::internal("DecryptException"))?;
        match st.wings.delete_server(&conn, &server.uuid).await {
            Ok(()) => {}
            Err(pelican_wings::WingsError::Connection(e)) => tracing::warn!("transfer cleanup on old node failed: {e}"),
            Err(e) => return Err(ApiError::internal(e.code())),
        }
    }
    Ok(no_content())
}
