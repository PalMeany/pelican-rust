//! `/servers/{server}/users` — `SubuserController` + Subuser services.

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::events::{FilamentNotification, filament_button};
use pelican_core::i18n::trans;
use pelican_core::state::AppState;
use pelican_db::entities::{nodes, password_resets, servers, subusers, users};
use pelican_db::{Json, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{Value, json};

use super::data::{server_subusers, subuser_item};
use super::servers::PERMISSIONS;
use crate::envelope::list;
use crate::error::{ApiError, ApiResult, Style};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::validation::Validator;

/// `SubuserRequest::authorize` (after the permission check).
async fn authorize(st: &AppState, ctx: &Ctx, permission: &str, input: Option<&Input>) -> ApiResult<()> {
    ctx.authorize(st, permission).await?;
    let user = ctx.user()?;
    if ctx.bound.user.as_ref().is_some_and(|u| u.uuid == user.user.uuid) {
        return Err(ApiError::unauthorized_action());
    }
    if let Some(input) = input.filter(|i| i.has("permissions")) {
        // validatePermissionsCanBeAssigned(array $permissions) — a non-array is a TypeError.
        if !matches!(input.get("permissions"), Some(Value::Array(_) | Value::Object(_) | Value::Null)) {
            return Err(ApiError::internal("TypeError"));
        }
        let server = ctx.bound.server()?;
        if !(user.can_update_server(server.node_id.0) || user.id() == server.owner_id.0) {
            let requested: Vec<String> = match input.get("permissions") {
                Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
                Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
                _ => Vec::new(),
            };
            let have = pelican_core::permissions::user_permissions(&st.db, server, user).await?;
            let have: Vec<String> = match have {
                Value::Array(a) => a.iter().map(pelican_core::php::string).collect(),
                Value::Object(m) => m.values().map(pelican_core::php::string).collect(),
                _ => Vec::new(),
            };
            if requested.iter().any(|p| !have.contains(p)) {
                return Err(ApiError::new(
                    403,
                    "HttpForbiddenException",
                    "Cannot assign permissions to a subuser that your account does not actively possess.",
                    Style::Pretty,
                ));
            }
        }
    }
    Ok(())
}

/// `getCleanedPermissions`: input ∩ all keys, + websocket.connect, unique.
fn cleaned_permissions(input: &Input) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let items: Vec<String> = match input.get("permissions") {
        Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
        Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
        _ => Vec::new(),
    };
    for p in items {
        if PERMISSIONS.contains(&p.as_str()) && !out.contains(&p) {
            out.push(p);
        }
    }
    if !out.iter().any(|p| p == "websocket.connect") {
        out.push("websocket.connect".into());
    }
    out
}

/// Service filter: keep `websocket.connect` and what the requester HAS, sorted.
async fn service_permissions(
    st: &AppState,
    ctx: &Ctx,
    server: &servers::Model,
    perms: Vec<String>,
) -> ApiResult<Vec<String>> {
    let user = ctx.user()?;
    let mut out: Vec<String> = Vec::new();
    for p in perms {
        if out.contains(&p) {
            continue;
        }
        if p == "websocket.connect"
            || pelican_core::permissions::has_server_permission(&st.db, user, server, &p).await?
        {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

pub async fn revoke<C: ConnectionTrait>(
    st: &AppState,
    db: &C,
    server: &servers::Model,
    user_uuid: &str,
) -> ApiResult<()> {
    let node =
        nodes::Entity::find_by_id(server.node_id).one(db).await?.ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    match st.wings.deauthorize_user(&conn, user_uuid, std::slice::from_ref(&server.uuid)).await {
        Ok(()) | Err(pelican_wings::WingsError::Connection(_)) => Ok(()),
        Err(e) => Err(ApiError::internal(e.code())),
    }
}

/// `GET /users`.
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "user.read").await?;
    let server = ctx.bound.server()?;
    let mut out = Vec::new();
    for s in server_subusers(&st.db, server.id.0).await? {
        if let Some(u) = users::Entity::find_by_id(s.user_id).one(&st.db).await? {
            out.push(subuser_item(&st.db, &s, &u).await?);
        }
    }
    Ok(PhpJson::ok(list(out)).into_response())
}

/// `GET /users/{user}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    authorize(&st, &ctx, "user.read", None).await?;
    let (sub, user) = bound(&ctx)?;
    Ok(PhpJson::ok(subuser_item(&st.db, sub, user).await?).into_response())
}

fn bound(ctx: &Ctx) -> ApiResult<(&subusers::Model, &users::Model)> {
    match (&ctx.bound.subuser, &ctx.bound.user) {
        (Some(s), Some(u)) => Ok((s, u)),
        _ => Err(ApiError::internal("InvalidArgumentException")),
    }
}

/// `UserCreationService::handle(['email' => ..., 'root_admin' => false])`.
async fn create_user<C: ConnectionTrait>(st: &AppState, db: &C, email: &str) -> ApiResult<(users::Model, String)> {
    let random_password = pelican_wings::jwt::str_random(30);
    let hash = laravel_compat::hashing::make(&random_password, st.config.bcrypt_rounds)
        .map_err(|_| ApiError::internal("RuntimeException"))?;
    let local = email.split('@').next().unwrap_or("");
    let username = format!("{local}{}", pelican_wings::jwt::str_random(3));
    let now = Timestamp(pelican_core::time::now());
    let am = users::ActiveModel {
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        email: Set(email.trim().to_lowercase()),
        password: Set(hash),
        language: Set("en".into()),
        username: Set(username.trim().to_lowercase()),
        external_id: Set(None),
        timezone: Set("UTC".into()),
        oauth: Set(Some(Json(json!([])))),
        customization: Set(None),
        mfa_app_secret: Set(None),
        mfa_app_recovery_codes: Set(None),
        mfa_email_enabled: Set(pelican_db::Bool(false)),
        is_managed_externally: Set(pelican_db::Bool(false)),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let user = pelican_db::write::insert(db, am).await?;
    // Password broker createToken(): delete existing rows, insert a hashed token.
    password_resets::Entity::delete_many()
        .filter(password_resets::Column::Email.eq(user.email.clone()))
        .exec(db)
        .await?;
    let token = pelican_wings::jwt::str_random(40);
    let reset = password_resets::ActiveModel {
        email: Set(user.email.clone()),
        token: Set(laravel_compat::hashing::make(&token, st.config.bcrypt_rounds)
            .map_err(|_| ApiError::internal("RuntimeException"))?),
        created_at: Set(now),
    };
    pelican_db::write::insert_id(db, &reset).await?;
    Ok((user, token))
}

/// `POST /users`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    authorize(&st, &ctx, "user.create", Some(&input)).await?;
    let mut v = Validator::new(&input);
    v.rules("email", &["required", "email", "between:1,255"]);
    v.rules("permissions", &["required", "array"]);
    v.rules("permissions.*", &["string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let email = input.get("email").map(pelican_core::php::string).unwrap_or_default();
    let perms = cleaned_permissions(&input);
    let txn = st.db.begin().await?;
    let existing = users::Entity::find().filter(users::Column::Email.eq(email.clone())).one(&txn).await?;
    let mut created_token = None;
    let user = match existing {
        None => {
            let (u, token) = create_user(&st, &txn, &email).await?;
            created_token = Some(token);
            u
        }
        Some(u) => {
            if server.owner_id == u.id {
                return Err(ApiError::display(
                    "UserIsServerOwnerException",
                    400,
                    "You cannot add the server owner as a subuser for this server.",
                ));
            }
            let exists = subusers::Entity::find()
                .filter(subusers::Column::ServerId.eq(server.id))
                .filter(subusers::Column::UserId.eq(u.id))
                .one(&txn)
                .await?
                .is_some();
            if exists {
                return Err(ApiError::display(
                    "ServerSubuserExistsException",
                    400,
                    "A user with that email address is already assigned as a subuser for this server.",
                ));
            }
            u
        }
    };
    let stored = service_permissions(&st, &ctx, &server, perms).await?;
    let now = Timestamp(pelican_core::time::now());
    let am = subusers::ActiveModel {
        user_id: Set(user.id),
        server_id: Set(server.id),
        permissions: Set(Some(Json(json!(stored)))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let sub = pelican_db::write::insert(&txn, am).await?;
    FilamentNotification {
        title: trans(&user.language, "notifications.user_added.title", &[]),
        body: Some(trans(&user.language, "notifications.user_added.body", &[("server", &server.name)])),
        status: None,
        actions: vec![filament_button(
            "exclude_view",
            &trans(&user.language, "notifications.open_server", &[]),
            &format!("{}/server/{}/console", ctx.root, server.uuid_short),
        )],
    }
    .send(&txn, user.id.0)
    .await?;
    if let Some(token) = &created_token {
        crate::services::account_created(&st, &user, Some(token)).await?;
    }
    crate::services::subuser_mail(&st, &user, &server, true).await?;
    txn.commit().await?;
    ctx.activity("server:subuser.create")
        .subject(morph::USER, user.id.0)
        .property("email", email)
        .property("permissions", json!(stored))
        .save(&st.db)
        .await?;
    Ok(PhpJson::ok(subuser_item(&st.db, &sub, &user).await?).into_response())
}

/// `POST /users/{user}`.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    authorize(&st, &ctx, "user.update", Some(&input)).await?;
    let mut v = Validator::new(&input);
    v.rules("permissions", &["required", "array"]);
    v.rules("permissions.*", &["string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let (sub, user) = bound(&ctx)?;
    let cleaned = service_permissions(&st, &ctx, &server, cleaned_permissions(&input)).await?;
    let mut current: Vec<String> = match sub.permissions.as_ref().map(|j| &j.0) {
        Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
        Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
        _ => Vec::new(),
    };
    current.sort();
    if cleaned != current {
        let txn = st.db.begin().await?;
        let now = Timestamp(pelican_core::time::now());
        subusers::Entity::update_many()
            .col_expr(
                subusers::Column::Permissions,
                pelican_db::write::json_expr(txn.get_database_backend(), Some(&json!(cleaned))),
            )
            .col_expr(subusers::Column::UpdatedAt, Expr::value(now))
            .filter(subusers::Column::Id.eq(sub.id))
            .exec(&txn)
            .await?;
        revoke(&st, &txn, &server, &user.uuid).await?;
        ctx.activity("server:subuser.update")
            .subject(morph::USER, user.id.0)
            .property("email", user.email.clone())
            .property("old", json!(current))
            .property("new", json!(cleaned))
            .property("revoked", true)
            .save_in(&txn)
            .await?;
        txn.commit().await?;
    }
    let fresh = subusers::Entity::find_by_id(sub.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(subuser_item(&st.db, &fresh, user).await?).into_response())
}

/// `DELETE /users/{user}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    authorize(&st, &ctx, "user.delete", None).await?;
    let server = ctx.bound.server()?.clone();
    let (sub, user) = bound(&ctx)?;
    delete_subuser(&st, &ctx, &server, sub, user).await?;
    Ok(no_content())
}

/// `SubuserDeletionService::handle`.
pub async fn delete_subuser(
    st: &AppState,
    ctx: &Ctx,
    server: &servers::Model,
    sub: &subusers::Model,
    user: &users::Model,
) -> ApiResult<()> {
    let txn = st.db.begin().await?;
    delete_subuser_in(st, ctx, &txn, server, sub, user).await?;
    txn.commit().await?;
    Ok(())
}

/// `SubuserDeletionService::handle()` on the caller's transaction.
pub async fn delete_subuser_in<C: ConnectionTrait>(
    st: &AppState,
    ctx: &Ctx,
    txn: &C,
    server: &servers::Model,
    sub: &subusers::Model,
    user: &users::Model,
) -> ApiResult<()> {
    subusers::Entity::delete_by_id(sub.id).exec(txn).await?;
    FilamentNotification {
        title: trans(&user.language, "notifications.user_removed.title", &[]),
        body: Some(trans(&user.language, "notifications.user_removed.body", &[("server", &server.name)])),
        status: None,
        actions: vec![],
    }
    .send(txn, user.id.0)
    .await?;
    crate::services::subuser_mail(st, user, server, false).await?;
    revoke(st, txn, server, &user.uuid).await?;
    ctx.activity("server:subuser.delete")
        .subject(morph::USER, user.id.0)
        .property("email", user.email.clone())
        .property("revoked", true)
        .save_in(txn)
        .await?;
    Ok(())
}
