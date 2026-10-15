//! `/api/client/account` — `AccountController`, `ApiKeyController`, `SSHKeyController`.

use axum::Extension;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{api_keys, nodes, user_ssh_keys, users};
use pelican_db::raw;
use pelican_db::{Int, Json, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use crate::envelope::{item, list, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::model_events::{self, Action};
use crate::pipeline::Ctx;
use crate::resources::{atom, user_data};
use crate::services::LogActivity;
use crate::validation::{CustomRule, Validator};

/// `GET /account` → item `user`.
pub async fn index(Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let u = ctx.user()?;
    Ok(PhpJson::ok(item("user", user_data(u), vec![])).into_response())
}

/// `InvalidPasswordProvidedException` (DisplayException, 400 compact).
fn invalid_password() -> ApiError {
    ApiError::display("InvalidPasswordProvidedException", 400, "The password provided was invalid for this account.")
}

/// FormRequest authorize(): password check, then not managed externally.
fn authorize_with_password(ctx: &Ctx, input: &crate::input::Input, field: &str) -> ApiResult<()> {
    let u = ctx.user()?;
    let given = input.get(field).and_then(Value::as_str).unwrap_or("");
    let ok = input.get(field).is_some_and(|v| !v.is_null())
        && laravel_compat::hashing::check(given, &u.user.password).unwrap_or(false);
    if !ok {
        return Err(invalid_password());
    }
    if u.user.is_managed_externally.0 {
        return Err(ApiError::unauthorized_action());
    }
    Ok(())
}

/// `unique:users,<col>` ignoring the current user (DB collation decides case).
async fn unique_rule(st: &AppState, column: &str, value: Option<&Value>, user_id: i64) -> ApiResult<CustomRule> {
    let v = value.map(pelican_core::php::string).unwrap_or_default();
    let b = st.db.get_database_backend();
    let sql = format!("select count(*) as {{aggregate}} from {{users}} where {{{column}}} = ? and {{id}} <> ?");
    let row = st.db.query_one_raw(raw::stmt(b, &sql, vec![v.into(), user_id.into()])).await?;
    let taken = row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).is_some_and(|c| c.0 > 0);
    Ok(CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())))
}

/// `PUT /account/username`.
pub async fn update_username(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    authorize_with_password(&ctx, &input, "password")?;
    let u = ctx.user()?;
    let mut v = Validator::new(&input);
    v.rules("username", &["required", "between:1,255"]);
    v.custom("username", unique_rule(&st, "username", input.get("username"), u.id()).await?);
    v.validate()?;
    let raw_input = input.get("username").map(pelican_core::php::string).unwrap_or_default();
    let new_value = raw_input.trim().to_lowercase();
    let original = u.user.username.clone();
    if new_value != original {
        let now = Timestamp(pelican_core::time::now());
        users::Entity::update_many()
            .col_expr(users::Column::Username, Expr::value(new_value))
            .col_expr(users::Column::UpdatedAt, Expr::value(now))
            .filter(users::Column::Id.eq(u.user.id))
            .exec(&st.db)
            .await?;
        // UserUpdateService: $user->forceFill($data)->saveOrFail().
        model_events::fire_id::<users::Entity>(&st, Action::Updated, u.user.id.0).await;
    }
    if original != raw_input {
        ctx.activity("user:account.username-changed")
            .property("old", original)
            .property("new", raw_input)
            .log(&st)
            .await?;
    }
    Ok(no_content())
}

/// `PUT /account/email`.
pub async fn update_email(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    authorize_with_password(&ctx, &input, "password")?;
    let u = ctx.user()?;
    let mut v = Validator::new(&input);
    v.rules("email", &["required", "email", "between:1,255"]);
    v.custom("email", unique_rule(&st, "email", input.get("email"), u.id()).await?);
    v.validate()?;
    let now_unix = chrono::Utc::now().timestamp();
    let key = format!("user:update-email:{}", u.user.uuid);
    if st.limiter.too_many_attempts(&key, 3, now_unix).is_some() {
        return Err(ApiError::new(
            429,
            "TooManyRequestsHttpException",
            "Your email address has been changed too many times today. Please try again later.",
            crate::error::Style::Pretty,
        ));
    }
    let raw_input = input.get("email").map(pelican_core::php::string).unwrap_or_default();
    let original = u.user.email.clone();
    if original.to_lowercase() != raw_input.to_lowercase() {
        st.limiter.hit(&key, 86400, now_unix);
        let new_value = raw_input.trim().to_lowercase();
        if new_value != original {
            let now = Timestamp(pelican_core::time::now());
            users::Entity::update_many()
                .col_expr(users::Column::Email, Expr::value(new_value))
                .col_expr(users::Column::UpdatedAt, Expr::value(now))
                .filter(users::Column::Id.eq(u.user.id))
                .exec(&st.db)
                .await?;
            // UserUpdateService: $user->forceFill($data)->saveOrFail().
            model_events::fire_id::<users::Entity>(&st, Action::Updated, u.user.id.0).await;
        }
        ctx.activity("user:account.email-changed")
            .property("old", original)
            .property("new", raw_input)
            .log(&st)
            .await?;
    }
    Ok(no_content())
}

/// `PUT /account/password`.
pub async fn update_password(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    authorize_with_password(&ctx, &input, "current_password")?;
    let u = ctx.user()?;
    let mut v = Validator::new(&input);
    v.rules("password", &["required", "string", "confirmed", "min:8"]);
    v.validate()?;
    let password = input.str("password").unwrap_or_default();
    let hash = laravel_compat::hashing::make(password, st.config.bcrypt_rounds)
        .map_err(|_| ApiError::internal("RuntimeException"))?;
    let txn = st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    users::Entity::update_many()
        .col_expr(users::Column::Password, Expr::value(hash))
        .col_expr(users::Column::UpdatedAt, Expr::value(now))
        .filter(users::Column::Id.eq(u.user.id))
        .exec(&txn)
        .await?;
    // PasswordChanged → RevocationListener: one deauthorize call per node (sync queue).
    revoke_sftp_access(&st, &txn, &u.user).await?;
    ctx.activity("user:account.password-changed").log_in(&st, &txn).await?;
    txn.commit().await?;
    // UserUpdateService: $user->forceFill($data)->saveOrFail().
    model_events::fire_id::<users::Entity>(&st, Action::Updated, u.user.id.0).await;
    Ok(no_content())
}

/// `RevocationListener`: `RevokeSftpAccessJob(user, node)` for every node hosting a
/// server the user owns or is a subuser of (`chunk(50)` → ordered by node id).
pub async fn revoke_sftp_access<C: ConnectionTrait>(st: &AppState, db: &C, user: &users::Model) -> ApiResult<()> {
    let b = db.get_database_backend();
    let rows = nodes::Model::find_by_statement(raw::stmt(
        b,
        "select * from {nodes} where {nodes.id} in (select distinct {servers.node_id} from {servers} left join {subusers} \
         on {subusers.server_id} = {servers.id} where ({servers.owner_id} = ? or {subusers.user_id} = ?)) order by {nodes.id} asc",
        vec![user.id.into(), user.id.into()],
    ))
    .all(db)
    .await?;
    for node in rows {
        let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
        match st.wings.deauthorize_user(&conn, &user.uuid, &[]).await {
            Ok(()) | Err(pelican_wings::WingsError::Connection(_)) => {}
            Err(e) => return Err(ApiError::internal(e.code())),
        }
    }
    Ok(())
}

fn api_key_item(k: &api_keys::Model) -> Value {
    let ips = match &k.allowed_ips.0 {
        Value::Array(a) => Value::Array(a.clone()),
        other => pelican_core::server_config::php_assoc(other.clone()),
    };
    let mut m = Map::new();
    m.insert("identifier".into(), json!(k.identifier));
    m.insert("description".into(), json!(k.memo));
    m.insert("allowed_ips".into(), ips);
    m.insert("last_used_at".into(), json!(atom(k.last_used_at.as_ref())));
    m.insert("created_at".into(), json!(atom(k.created_at.as_ref())));
    item("api_key", Value::Object(m), vec![])
}

/// `$user->apiKeys` (key_type = 1), natural order.
async fn account_keys<C: ConnectionTrait>(db: &C, user_id: i64) -> ApiResult<Vec<api_keys::Model>> {
    let b = db.get_database_backend();
    Ok(api_keys::Model::find_by_statement(raw::stmt(
        b,
        "select * from {api_keys} where {api_keys.user_id} = ? and {api_keys.user_id} is not null and {key_type} = ?",
        vec![user_id.into(), 1.into()],
    ))
    .all(db)
    .await?)
}

/// `GET /account/api-keys`.
pub async fn api_keys_index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let keys = account_keys(&st.db, ctx.user()?.id()).await?;
    Ok(PhpJson::ok(list(keys.iter().map(api_key_item).collect())).into_response())
}

/// `POST /account/api-keys`.
pub async fn api_keys_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    let mut v = Validator::new(&input);
    v.rules("description", &["required", "nullable", "string", "max:500"]);
    v.rules("allowed_ips", &["array", "max:50"]);
    v.rules("allowed_ips.*", &["string"]);
    v.run();
    // after(): every entry must be a valid IPTools range.
    if let Some(Value::Array(ips)) = input.get("allowed_ips") {
        for (i, ip) in ips.iter().enumerate() {
            let s = pelican_core::php::string(ip);
            match pelican_core::net::IpRange::try_parse(&s) {
                Ok(_) => {}
                Err(pelican_core::net::IpParseError::InvalidFormat) => v.add_error(
                    &format!("allowed_ips.{i}"),
                    &format!("\"{s}\" is not a valid IP address or CIDR range."),
                ),
                Err(pelican_core::net::IpParseError::Other(_)) => return Err(ApiError::internal("NetworkException")),
            }
        }
    }
    v.validate()?;
    let u = ctx.user()?;
    if account_keys(&st.db, u.id()).await?.len() as i64 >= st.config.api_key_limit {
        return Err(ApiError::display(
            "DisplayException",
            400,
            "You have reached the account limit for number of API keys.",
        ));
    }
    let identifier = format!("pacc_{}", pelican_wings::jwt::str_random(11));
    let secret = pelican_wings::jwt::str_random(32);
    let now = Timestamp(pelican_core::time::now());
    let ips = match input.get("allowed_ips") {
        Some(Value::Array(a)) => Value::Array(a.clone()),
        _ => json!([]),
    };
    let am = api_keys::ActiveModel {
        user_id: Set(Some(u.user.id)),
        key_type: Set(Int(1)),
        identifier: Set(Some(identifier.clone())),
        token: Set(st.enc.encrypt_string(&secret)),
        memo: Set(Some(input.get("description").map(pelican_core::php::string).unwrap_or_default())),
        allowed_ips: Set(Json(ips)),
        permissions: Set(Some(Json(json!([])))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let key = pelican_db::write::insert(&st.db, am).await?;
    // HasAccessTokens::createToken: $this->tokens()->forceCreate([...]).
    model_events::created(&st, &key).await;
    ctx.activity("user:api-key.create")
        .subject(morph::API_KEY, key.id.0)
        .property("identifier", identifier)
        .log(&st)
        .await?;
    let mut meta = Map::new();
    meta.insert("secret_token".into(), json!(secret));
    Ok(PhpJson::ok(with_meta(api_key_item(&key), meta)).into_response())
}

/// `DELETE /account/api-keys/{identifier}`.
pub async fn api_keys_delete(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    Path(identifier): Path<String>,
) -> ApiResult<Response> {
    let u = ctx.user()?;
    let key = api_keys::Entity::find()
        .filter(api_keys::Column::UserId.eq(u.user.id))
        .filter(api_keys::Column::KeyType.eq(1))
        .filter(api_keys::Column::Identifier.eq(identifier))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    ctx.activity("user:api-key.delete").property("identifier", key.identifier.clone()).log(&st).await?;
    api_keys::Entity::delete_by_id(key.id).exec(&st.db).await?;
    // ApiKeyController::delete: $key->delete().
    model_events::deleted(&st, &key).await;
    Ok(no_content())
}

fn ssh_key_item(k: &user_ssh_keys::Model) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(k.name));
    m.insert("fingerprint".into(), json!(k.fingerprint));
    m.insert("public_key".into(), json!(k.public_key));
    m.insert("created_at".into(), json!(atom(k.created_at.as_ref())));
    item("ssh_key", Value::Object(m), vec![])
}

async fn account_ssh_keys<C: ConnectionTrait>(db: &C, user_id: i64) -> ApiResult<Vec<user_ssh_keys::Model>> {
    let b = db.get_database_backend();
    Ok(user_ssh_keys::Model::find_by_statement(raw::stmt(
        b,
        "select * from {user_ssh_keys} where {user_ssh_keys.user_id} = ? and {user_ssh_keys.user_id} is not null \
         and {user_ssh_keys.deleted_at} is null",
        vec![user_id.into()],
    ))
    .all(db)
    .await?)
}

/// `GET /account/ssh-keys`.
pub async fn ssh_keys_index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let keys = account_ssh_keys(&st.db, ctx.user()?.id()).await?;
    Ok(PhpJson::ok(list(keys.iter().map(ssh_key_item).collect())).into_response())
}

/// `POST /account/ssh-keys`.
pub async fn ssh_keys_store(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    use pelican_core::sshkey::{KeyKind, PublicKey};
    let u = ctx.user()?;
    let mut v = Validator::new(&input);
    v.rules("name", &["required", "string"]);
    v.rules("public_key", &["required", "string"]);
    v.run();
    // loadPublicKey(null) is a TypeError before any error is added.
    let Some(raw_key) = input.get("public_key").filter(|v| !v.is_null()) else {
        return Err(ApiError::internal("TypeError"));
    };
    let existing = account_ssh_keys(&st.db, u.id()).await?;
    let key = match PublicKey::parse(&pelican_core::php::string(raw_key)) {
        Err(_) => {
            v.add_error("public_key", "The public key provided is not valid.");
            None
        }
        Ok(k) => {
            if matches!(k.kind, KeyKind::Dsa) {
                v.add_error("public_key", "DSA keys are not supported.");
            }
            if k.rsa_bits().is_some_and(|b| b < 2048) {
                v.add_error("public_key", "RSA keys must be at least 2048 bytes in length.");
            }
            let fp = k.fingerprint_sha256();
            if existing.iter().any(|e| e.fingerprint == fp) {
                v.add_error("public_key", "The public key provided already exists on your account.");
            }
            Some(k)
        }
    };
    v.validate()?;
    let key = key.ok_or_else(|| ApiError::internal("TypeError"))?;
    let fingerprint = key.fingerprint_sha256();
    let now = Timestamp(pelican_core::time::now());
    let am = user_ssh_keys::ActiveModel {
        user_id: Set(u.user.id),
        name: Set(input.get("name").map(pelican_core::php::string).unwrap_or_default()),
        fingerprint: Set(fingerprint.clone()),
        public_key: Set(key.to_pkcs8_pem()),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let model = pelican_db::write::insert(&st.db, am).await?;
    // SSHKeyController::store: $request->user()->sshKeys()->create([...]).
    model_events::created(&st, &model).await;
    ctx.activity("user:ssh-key.create")
        .subject(morph::SSH_KEY, model.id.0)
        .property("fingerprint", fingerprint)
        .log(&st)
        .await?;
    Ok(PhpJson::ok(ssh_key_item(&model)).into_response())
}

/// `DELETE /account/ssh-keys/{fingerprint}` (soft delete).
pub async fn ssh_keys_delete(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    Path(fingerprint): Path<String>,
) -> ApiResult<Response> {
    let u = ctx.user()?;
    let key = user_ssh_keys::Entity::find()
        .filter(user_ssh_keys::Column::UserId.eq(u.user.id))
        .filter(user_ssh_keys::Column::DeletedAt.is_null())
        .filter(user_ssh_keys::Column::Fingerprint.eq(fingerprint))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    ctx.activity("user:ssh-key.delete")
        .subject(morph::SSH_KEY, key.id.0)
        .property("fingerprint", key.fingerprint.clone())
        .log(&st)
        .await?;
    let now = Timestamp(pelican_core::time::now());
    user_ssh_keys::Entity::update_many()
        .col_expr(user_ssh_keys::Column::DeletedAt, Expr::value(Some(now)))
        .col_expr(user_ssh_keys::Column::UpdatedAt, Expr::value(now))
        .filter(user_ssh_keys::Column::Id.eq(key.id))
        .exec(&st.db)
        .await?;
    // SSHKeyController::delete: $key->delete() (SoftDeletes — row already carries deleted_at).
    model_events::fire_id::<user_ssh_keys::Entity>(&st, Action::Deleted, key.id.0).await;
    Ok(no_content())
}
