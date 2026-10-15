//! `SettingsController` (`app/Http/Controllers/Api/Client/Servers/SettingsController.php`):
//! rename / description / reinstall / docker-image.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::Timestamp;
use pelican_db::entities::{eggs, nodes, servers};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::model_events::{self, Action};
use crate::pipeline::Ctx;
use crate::services::LogActivity;
use crate::validation::{CustomRule, Validator, php_loose_eq};

/// Eloquent `save()` semantics: a dirty update writes only the changed column
/// plus `updated_at`; an unchanged value writes nothing.
async fn touch_column(st: &AppState, server_id: i64, col: servers::Column, value: Expr) -> ApiResult<()> {
    servers::Entity::update_many()
        .col_expr(col, value)
        .col_expr(servers::Column::UpdatedAt, Expr::value(Timestamp(pelican_core::time::now())))
        .filter(servers::Column::Id.eq(server_id))
        .exec(&st.db)
        .await?;
    Ok(())
}

/// `POST /settings/rename` (`RenameServerRequest`, `settings.rename`).
pub async fn rename(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "settings.rename").await?;
    let mut v = Validator::new(&input);
    v.rules("name", &["required", "string", "min:1", "max:255"]); // Server::getRules()['name']
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let name = input.str("name").unwrap_or_default().to_string();
    if server.name != name {
        // `$server->wasChanged('name')`.
        touch_column(&st, server.id.0, servers::Column::Name, Expr::value(name.clone())).await?;
        // SettingsController: $server->update(['name' => $name]).
        {
            // Relations loaded by AuthenticateServerAccess travel with the model.
            let loaded = model_events::client_server_loaded(&st, &ctx, &server).await;
            model_events::fire_id_with::<servers::Entity, _>(&st, &st.db, Action::Updated, server.id.0, loaded).await;
        }
        // Server::saved hook (owner must not be a subuser) arrives with the subusers work.
        ctx.activity("server:settings.rename")
            .property("old", server.name.clone())
            .property("new", name.clone())
            .log(&st)
            .await?;
    }
    Ok(no_content())
}

/// `POST /settings/description` (`DescriptionServerRequest`, `settings.description`).
pub async fn description(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "settings.description").await?;
    let mut v = Validator::new(&input);
    v.rules("description", &["string", "nullable"]);
    v.validate()?;
    if !st.config.env.bool_or("PANEL_EDITABLE_SERVER_DESCRIPTIONS", true) {
        return Ok(PhpJson::with_status(StatusCode::FORBIDDEN, json!([])).into_response());
    }
    let server = ctx.bound.server()?.clone();
    // `$description ?? ''`; `wasChanged` compares the raw stored values.
    let stored = input.str("description").unwrap_or("").to_string();
    if server.description.as_deref() != Some(stored.as_str()) {
        touch_column(&st, server.id.0, servers::Column::Description, Expr::value(Some(stored.clone()))).await?;
        // SettingsController: $server->update(['description' => $description ?? '']).
        {
            // Relations loaded by AuthenticateServerAccess travel with the model.
            let loaded = model_events::client_server_loaded(&st, &ctx, &server).await;
            model_events::fire_id_with::<servers::Entity, _>(&st, &st.db, Action::Updated, server.id.0, loaded).await;
        }
        // The log keeps the RAW input (null when omitted), unlike the stored column.
        let new_raw = match input.get("description") {
            Some(Value::String(s)) => Value::String(s.clone()),
            _ => Value::Null,
        };
        ctx.activity("server:settings.description")
            .property("old", server.description.clone().map(Value::String).unwrap_or(Value::Null))
            .property("new", new_raw)
            .log(&st)
            .await?;
    }
    Ok(no_content())
}

/// `POST /settings/reinstall` (`ReinstallServerRequest`, `settings.reinstall`).
pub async fn reinstall(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "settings.reinstall").await?;
    let server = ctx.bound.server()?.clone();
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    // ReinstallServerService::handle(): status change and the daemon call share
    // one transaction (rolled back when Wings fails).
    let txn = st.db.begin().await?;
    if server.status.as_deref() != Some("installing") {
        servers::Entity::update_many()
            .col_expr(servers::Column::Status, Expr::value(Some("installing".to_string())))
            .col_expr(servers::Column::UpdatedAt, Expr::value(Timestamp(pelican_core::time::now())))
            .filter(servers::Column::Id.eq(server.id))
            .exec(&txn)
            .await?;
    }
    st.wings.reinstall_server(&conn, &server.uuid).await.map_err(|e| ApiError::internal(e.code()))?;
    txn.commit().await?;
    // ReinstallServerService: $server->fill(['status' => Installing])->save().
    if server.status.as_deref() != Some("installing") {
        {
            // Relations loaded by AuthenticateServerAccess travel with the model.
            let loaded = model_events::client_server_loaded(&st, &ctx, &server).await;
            model_events::fire_id_with::<servers::Entity, _>(&st, &st.db, Action::Updated, server.id.0, loaded).await;
        }
    }
    ctx.activity("server:reinstall").log(&st).await?;
    Ok(PhpJson::with_status(StatusCode::ACCEPTED, json!([])).into_response())
}

/// Allowed docker images (`Rule::in(array_values($server->egg->docker_images))`).
fn docker_images_of(egg: &eggs::Model) -> Option<Vec<Value>> {
    match egg.docker_images.as_ref().map(|j| &j.0) {
        Some(Value::Object(m)) => Some(m.values().cloned().collect()),
        Some(Value::Array(a)) => Some(a.clone()),
        _ => None,
    }
}

const IN_MSG: &str = "The selected :attribute is invalid.";

/// `PUT /settings/docker-image` (`SetDockerImageRequest`, `startup.docker-image`).
pub async fn docker_image(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "startup.docker-image").await?;
    let server = ctx.bound.server()?.clone();
    // SetDockerImageRequest::rules() resolves egg.docker_images during validation.
    let egg = eggs::Entity::find_by_id(server.egg_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let images = docker_images_of(&egg).ok_or_else(|| ApiError::internal("TypeError"))?;
    let in_list = images.clone();
    let mut v = Validator::new(&input);
    v.rules("docker_image", &["required", "string"]);
    v.custom(
        "docker_image",
        CustomRule {
            name: "in".into(),
            check: Box::new(move |_attr, val| match val {
                Value::Object(_) | Value::Array(_) => Some(IN_MSG.into()),
                // `validateIn`: loose == against each allowed value.
                _ => {
                    if in_list.iter().any(|p| php_loose_eq(val, p)) {
                        None
                    } else {
                        Some(IN_MSG.into())
                    }
                }
            }),
        },
    );
    v.validate()?;
    if !images.iter().any(|p| php_loose_eq(&Value::String(server.image.clone()), p)) {
        return Err(ApiError::bad_request(
            "This server's Docker image has been manually set by an administrator and cannot be updated.",
        ));
    }
    let image = input.str("docker_image").unwrap_or_default().to_string();
    if server.image != image {
        touch_column(&st, server.id.0, servers::Column::Image, Expr::value(image.clone())).await?;
        // SettingsController::dockerImage: $server->forceFill(['image' => ...])->saveOrFail().
        {
            // Relations loaded by AuthenticateServerAccess travel with the model.
            let loaded = model_events::client_server_loaded(&st, &ctx, &server).await;
            model_events::fire_id_with::<servers::Entity, _>(&st, &st.db, Action::Updated, server.id.0, loaded).await;
        }
        ctx.activity("server:startup.image")
            .property("old", server.image.clone())
            .property("new", image.clone())
            .log(&st)
            .await?;
    }
    Ok(no_content())
}
