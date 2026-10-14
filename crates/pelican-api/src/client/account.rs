//! `/api/client/account` (`app/Http/Controllers/Api/Client/AccountController.php`).

use axum::Extension;
use axum::response::IntoResponse;
use axum::response::Response;

use crate::error::ApiResult;
use crate::json::{Item, PhpJson};
use crate::pipeline::Ctx;
use crate::resources::user_data;

/// `GET /account` → item `user`.
pub async fn index(Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let u = ctx.user()?;
    Ok(PhpJson::ok(Item {
        object: "user",
        attributes: user_data(u),
    })
    .into_response())
}
