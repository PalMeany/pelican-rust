//! Model hooks shared by the API groups.

use pelican_core::state::AppState;
use pelican_db::entities::{servers, subusers, users};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::error::{ApiError, ApiResult};
use crate::pipeline::Ctx;

/// `Server::saved` hook: a subuser entry of the (new) owner is removed
/// through `SubuserDeletionService`.
pub async fn server_saved<C: ConnectionTrait>(
    st: &AppState,
    ctx: &Ctx,
    db: &C,
    server: &servers::Model,
) -> ApiResult<()> {
    let sub = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(server.id))
        .filter(subusers::Column::UserId.eq(server.owner_id))
        .one(db)
        .await?;
    if let Some(sub) = sub {
        let user = users::Entity::find_by_id(sub.user_id).one(db).await?.ok_or_else(ApiError::model_not_found)?;
        crate::client::subusers::delete_subuser_in(st, ctx, db, server, &sub, &user).await?;
    }
    Ok(())
}
