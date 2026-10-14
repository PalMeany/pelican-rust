//! `/api/application/users` — `UserController`, `ExternalUserController` (spec §3.1).

use std::sync::LazyLock;

use axum::Extension;
use axum::extract::{OriginalUri, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{password_resets, roles, users};
use pelican_db::{Bool, Int, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Value, json};

use super::data::{user_item, user_roles};
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::{CustomRule, Validator};

static PHP_LISTS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../../data/php-lists.json")).unwrap_or(Value::Null));

/// `in:` parameter list of `ResourceBundle::getLocales('')` / `DateTimeZone::listIdentifiers()`.
fn in_rule(key: &str) -> String {
    let items: Vec<&str> =
        PHP_LISTS[key].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    format!("in:{}", items.join(","))
}

fn bound_user(ctx: &Ctx) -> ApiResult<users::Model> {
    ctx.bound.user.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /users`.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("user", READ)?;
    let b = st.db.get_database_backend();
    let fs = filters(&input, &["email", "uuid", "username", "external_id"])?;
    let ss = sorts(&input, &["id", "uuid"])?;
    let page = Page::new(&input, 50)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in ["email", "uuid", "username", "external_id"] {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("users.{name}"), v) {
                wh.push(sql);
                vals.append(&mut values);
            }
        }
    }
    let where_sql = if wh.is_empty() { String::new() } else { format!(" where {}", wh.join(" and ")) };
    let order = if ss.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> =
            ss.iter().map(|(n, d)| format!("{} {}", raw::q(b, n), if *d { "desc" } else { "asc" })).collect();
        format!(" order by {}", parts.join(", "))
    };
    let total_row = st
        .db
        .query_one_raw(raw::stmt(
            b,
            &format!("select count(*) as {{aggregate}} from {{users}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = total_row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = st
            .db
            .query_all_raw(raw::stmt(b, &format!("select * from {{users}}{where_sql}{order}{}", page.sql()), vals))
            .await?;
        for r in rows {
            let u = users::Model::from_query_result(&r, "")?;
            data.push(user_item(&st, &ctx, &u, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// `GET /users/{user}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("user", READ)?;
    let u = bound_user(&ctx)?;
    Ok(PhpJson::ok(user_item(&st, &ctx, &u, &Includes::parse(&input)).await?).into_response())
}

/// `GET /users/external/{external_id}`.
pub async fn view_external(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    Path(external_id): Path<String>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("user", READ)?;
    let u = users::Entity::find()
        .filter(users::Column::ExternalId.eq(external_id))
        .one(&st.db)
        .await?
        .ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(user_item(&st, &ctx, &u, &Includes::parse(&input)).await?).into_response())
}

/// `unique:users,<column>[,<ignore id>]` (database collation decides case).
async fn unique(st: &AppState, column: &str, value: Option<&Value>, ignore: Option<i64>) -> ApiResult<CustomRule> {
    let taken = match value {
        None | Some(Value::Null) => false,
        Some(v) => {
            let b = st.db.get_database_backend();
            let mut sql = format!("select count(*) as {{aggregate}} from {{users}} where {{{column}}} = ?");
            let mut vals: Vec<sea_orm::Value> = vec![pelican_core::php::string(v).into()];
            if let Some(id) = ignore {
                sql.push_str(" and {id} <> ?");
                vals.push(id.into());
            }
            let row = st.db.query_one_raw(raw::stmt(b, &sql, vals)).await?;
            row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).is_some_and(|c| c.0 > 0)
        }
    };
    Ok(CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())))
}

/// `StoreUserRequest::rules()` (field order of `User::$validationRules`).
async fn validate_user(st: &AppState, input: &Input, ignore: Option<i64>) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("email", &["required", "email", "between:1,255"]);
    v.custom("email", unique(st, "email", input.get("email"), ignore).await?);
    v.rules("external_id", &["sometimes", "nullable", "string", "max:255"]);
    v.custom("external_id", unique(st, "external_id", input.get("external_id"), ignore).await?);
    v.rules("is_managed_externally", &["boolean"]);
    v.rules("username", &["required", "between:1,255"]);
    v.custom("username", unique(st, "username", input.get("username"), ignore).await?);
    v.rules("password", &["sometimes", "nullable", "string"]);
    let locales = in_rule("locales");
    let timezones = in_rule("timezones");
    v.rules("language", &["string", &locales]);
    v.rules("timezone", &["string", &timezones]);
    v.attribute_name("external_id", "Third Party Identifier");
    v.attribute_name("is_managed_externally", "Is managed by Third Party?");
    v.validate()
}

const FIELDS: [&str; 7] =
    ["email", "external_id", "is_managed_externally", "username", "password", "language", "timezone"];

/// `$request->validated()`: present keys that have rules.
fn validated(input: &Input) -> Vec<(&'static str, Value)> {
    FIELDS.iter().filter_map(|k| input.get(k).map(|v| (*k, v.clone()))).collect()
}

fn hash(st: &AppState, plain: &str) -> ApiResult<String> {
    laravel_compat::hashing::make(plain, st.config.bcrypt_rounds).map_err(|_| ApiError::internal("RuntimeException"))
}

fn str_or_null(v: &Value) -> Option<String> {
    if v.is_null() { None } else { Some(pelican_core::php::string(v)) }
}

/// `POST /users` (`UserCreationService`).
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("user", WRITE)?;
    validate_user(&st, &input, None).await?;
    let data = validated(&input);
    let get = |k: &str| data.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let password = get("password").filter(|p| !pelican_core::php::empty(p)).map(|p| pelican_core::php::string(&p));
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let reset = password.is_none();
    let hashed = match &password {
        Some(p) => hash(&st, p)?,
        None => hash(&st, &pelican_wings::jwt::str_random(30))?,
    };
    // The saving hook lowercases username and email.
    let email = get("email").map(|v| pelican_core::php::string(&v).to_lowercase()).unwrap_or_default();
    let username = get("username").map(|v| pelican_core::php::string(&v).to_lowercase()).unwrap_or_default();
    let am = users::ActiveModel {
        external_id: Set(get("external_id").as_ref().and_then(str_or_null)),
        is_managed_externally: Set(Bool(get("is_managed_externally").is_some_and(|v| !pelican_core::php::empty(&v)))),
        email: Set(email.clone()),
        username: Set(username),
        password: Set(hashed),
        language: Set(get("language").as_ref().and_then(str_or_null).unwrap_or_else(|| "en".into())),
        timezone: Set(get("timezone").as_ref().and_then(str_or_null).unwrap_or_else(|| "UTC".into())),
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        mfa_app_secret: Set(None),
        mfa_app_recovery_codes: Set(None),
        mfa_email_enabled: Set(Bool(false)),
        oauth: Set(Some(pelican_db::Json(json!([])))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let user = pelican_db::write::insert(&txn, am).await?;
    let mut reset_token = None;
    if reset {
        // Password broker: delete existing tokens for the email, insert a hashed random token.
        password_resets::Entity::delete_many()
            .filter(password_resets::Column::Email.eq(email.clone()))
            .exec(&txn)
            .await?;
        let plain = pelican_wings::jwt::str_random(64);
        let token = hash(&st, &plain)?;
        reset_token = Some(plain);
        txn.execute_raw(raw::stmt(
            st.db.get_database_backend(),
            "insert into {password_resets} ({email}, {token}, {created_at}) values (?, ?, ?)",
            vec![email.into(), token.into(), now.into()],
        ))
        .await?;
    }
    txn.commit().await?;
    crate::services::account_created(&st, &user, reset_token.as_deref()).await?;
    let body = with_meta(
        user_item(&st, &ctx, &user, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("users/{}", user.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// `UserCreationService::handle(['username', 'email', 'oauth'])` without a password
/// (OAuth sign-up): random password, reset token and the AccountCreated mail.
pub async fn create_user(st: &AppState, username: &str, email: &str, oauth: Value) -> ApiResult<users::Model> {
    let username = if username.is_empty() {
        format!("{}{}", email.split('@').next().unwrap_or_default(), pelican_wings::jwt::str_random(3))
    } else {
        username.to_string()
    };
    // The saving hook lowercases both; User::$validationRules require them unique.
    let (username, email) = (username.to_lowercase(), email.to_lowercase());
    let taken = users::Entity::find()
        .filter(
            sea_orm::Condition::any()
                .add(users::Column::Username.eq(username.clone()))
                .add(users::Column::Email.eq(email.clone())),
        )
        .one(&st.db)
        .await?
        .is_some();
    if taken || username.chars().count() > 255 || email.chars().count() > 255 {
        return Err(ApiError::internal("ValidationException"));
    }
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let am = users::ActiveModel {
        external_id: Set(None),
        is_managed_externally: Set(Bool(false)),
        email: Set(email.clone()),
        username: Set(username),
        password: Set(hash(st, &pelican_wings::jwt::str_random(30))?),
        language: Set("en".into()),
        timezone: Set(st.config.env.string_or("APP_TIMEZONE", "UTC")),
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        mfa_app_secret: Set(None),
        mfa_app_recovery_codes: Set(None),
        mfa_email_enabled: Set(Bool(false)),
        oauth: Set(Some(pelican_db::Json(oauth))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let user = pelican_db::write::insert(&txn, am).await?;
    let token = crate::passwords::create_token(st, &txn, &email).await?;
    txn.commit().await?;
    crate::services::account_created(st, &user, Some(&token)).await?;
    Ok(user)
}

/// `DatabaseSeeder`: `Role::firstOrCreate(['name' => Role::ROOT_ADMIN])` (guard `web`).
pub async fn ensure_root_admin_role(st: &AppState) -> ApiResult<i64> {
    if let Some(r) = roles::Entity::find().filter(roles::Column::Name.eq("Root Admin")).one(&st.db).await? {
        return Ok(r.id.0);
    }
    let now = Timestamp(pelican_core::time::now());
    let id = pelican_db::write::insert_id(
        &st.db,
        &roles::ActiveModel {
            name: Set("Root Admin".into()),
            guard_name: Set("web".into()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?
    .ok_or_else(|| ApiError::internal("QueryException"))?;
    Ok(id)
}

/// `UserCreationService::handle([... 'root_admin' => true])` for the installer: the
/// given password, the Root Admin role and the AccountCreated mail (no reset token).
pub async fn create_admin(st: &AppState, username: &str, email: &str, password: &str) -> ApiResult<users::Model> {
    let (username, email) = (username.to_lowercase(), email.to_lowercase());
    let taken = users::Entity::find()
        .filter(sea_orm::Condition::any().add(users::Column::Username.eq(username.clone())).add(users::Column::Email.eq(email.clone())))
        .one(&st.db)
        .await?
        .is_some();
    if taken {
        return Err(ApiError::display("ValidationException", 422, "The username or email has already been taken."));
    }
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let am = users::ActiveModel {
        external_id: Set(None),
        is_managed_externally: Set(Bool(false)),
        email: Set(email),
        username: Set(username),
        password: Set(hash(st, password)?),
        language: Set("en".into()),
        timezone: Set(st.config.env.string_or("APP_TIMEZONE", "UTC")),
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        mfa_app_secret: Set(None),
        mfa_app_recovery_codes: Set(None),
        mfa_email_enabled: Set(Bool(false)),
        oauth: Set(Some(pelican_db::Json(json!([])))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let user = pelican_db::write::insert(&txn, am).await?;
    let role = roles::Entity::find()
        .filter(roles::Column::Name.eq("Root Admin"))
        .one(&txn)
        .await?
        .ok_or_else(|| ApiError::internal("RoleDoesNotExist"))?;
    txn.execute_raw(raw::stmt(
        txn.get_database_backend(),
        "insert into {model_has_roles} ({role_id}, {model_type}, {model_id}) values (?, ?, ?)",
        vec![role.id.into(), "user".into(), user.id.into()],
    ))
    .await?;
    txn.commit().await?;
    crate::services::account_created(st, &user, None).await?;
    Ok(user)
}

/// `PATCH /users/{user}` (`UserUpdateService`).
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("user", WRITE)?;
    let u = bound_user(&ctx)?;
    validate_user(&st, &input, Some(u.id.0)).await?;
    let data = validated(&input);
    let get = |k: &str| data.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let mut am = users::ActiveModel { id: sea_orm::ActiveValue::Unchanged(u.id), ..Default::default() };
    let mut dirty = false;
    let mut password_changed = false;
    if let Some(v) = get("email") {
        let e = pelican_core::php::string(&v).to_lowercase();
        if e != u.email {
            am.email = Set(e);
            dirty = true;
        }
    }
    if let Some(v) = get("username") {
        let n = pelican_core::php::string(&v).to_lowercase();
        if n != u.username {
            am.username = Set(n);
            dirty = true;
        }
    }
    if let Some(v) = get("external_id") {
        let e = str_or_null(&v);
        if e != u.external_id {
            am.external_id = Set(e);
            dirty = true;
        }
    }
    if let Some(v) = get("is_managed_externally") {
        let b = !pelican_core::php::empty(&v);
        if b != u.is_managed_externally.0 {
            am.is_managed_externally = Set(Bool(b));
            dirty = true;
        }
    }
    if let Some(p) = get("password").filter(|p| !pelican_core::php::empty(p)) {
        am.password = Set(hash(&st, &pelican_core::php::string(&p))?);
        dirty = true;
        password_changed = true;
    }
    for (k, col) in [("language", 0), ("timezone", 1)] {
        if let Some(v) = get(k) {
            let s = str_or_null(&v).unwrap_or_default();
            let cur = if col == 0 { &u.language } else { &u.timezone };
            if &s != cur {
                if col == 0 {
                    am.language = Set(s);
                } else {
                    am.timezone = Set(s);
                }
                dirty = true;
            }
        }
    }
    if dirty {
        am.updated_at = Set(Some(Timestamp(pelican_core::time::now())));
        pelican_db::write::update(&st.db, &am).await?;
    }
    if password_changed {
        // PasswordChanged → RevocationListener: deauthorize SFTP on every node with the user's servers.
        crate::jobs::revoke_sftp_access(&st, &u).await?;
    }
    let fresh = users::Entity::find_by_id(u.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(user_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?).into_response())
}

/// `Role::getRootAdmin()` = `findOrCreate('Root Admin', 'web')`.
async fn root_admin_role(st: &AppState) -> ApiResult<roles::Model> {
    if let Some(r) = roles::Entity::find()
        .filter(roles::Column::Name.eq(pelican_core::auth::ROOT_ADMIN_ROLE))
        .filter(roles::Column::GuardName.eq("web"))
        .one(&st.db)
        .await?
    {
        return Ok(r);
    }
    let now = Timestamp(pelican_core::time::now());
    Ok(pelican_db::write::insert(
        &st.db,
        roles::ActiveModel {
            name: Set(pelican_core::auth::ROOT_ADMIN_ROLE.into()),
            guard_name: Set("web".into()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?)
}

/// `AssignUserRolesRequest::rules()`.
async fn validate_roles(st: &AppState, input: &Input) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("roles", &["required", "array"]);
    v.rules("roles.*", &["integer"]);
    // exists:roles,id per element.
    for path in crate::validation::expand_wildcard(&input.data, "roles.*") {
        let val = input.get(&path).cloned().unwrap_or(Value::Null);
        let exists = match &val {
            Value::Null => true,
            other => {
                let id = pelican_core::php::string(other);
                match id.trim().parse::<i64>() {
                    Ok(i) => roles::Entity::find_by_id(Int(i)).one(&st.db).await?.is_some(),
                    Err(_) => false,
                }
            }
        };
        v.custom(
            &path,
            CustomRule::precomputed("exists", (!exists).then(|| "The selected :attribute is invalid.".into())),
        );
    }
    v.validate()
}

/// The `roles` input as PHP sees it: ints stay ints, strings are role *names*
/// for spatie (`getStoredRole`: `is_int` → findById, `is_string` → findByName).
enum RoleRef {
    Id(i64),
    Name(String),
}

fn role_inputs(input: &Input) -> Vec<RoleRef> {
    let vals: Vec<Value> = match input.get("roles") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        _ => Vec::new(),
    };
    vals.iter()
        .filter_map(|v| match v {
            Value::Number(n) => n.as_i64().map(RoleRef::Id),
            Value::String(s) => Some(RoleRef::Name(s.clone())),
            _ => None,
        })
        .collect()
}

/// `Role::findById/findByName($x, 'web')` → `RoleDoesNotExist` (500) when missing.
async fn stored_role(st: &AppState, r: &RoleRef) -> ApiResult<roles::Model> {
    let q = roles::Entity::find().filter(roles::Column::GuardName.eq("web"));
    let found = match r {
        RoleRef::Id(id) => q.filter(roles::Column::Id.eq(Int(*id))).one(&st.db).await?,
        RoleRef::Name(n) => q.filter(roles::Column::Name.eq(n.clone())).one(&st.db).await?,
    };
    found.ok_or_else(|| ApiError::internal("RoleDoesNotExist"))
}

async fn change_roles(st: &AppState, ctx: &Ctx, input: &Input, assign: bool) -> ApiResult<Response> {
    ctx.acl("user", WRITE)?;
    validate_roles(st, input).await?;
    let u = bound_user(ctx)?;
    if !pelican_core::auth::is_root_admin(&st.db, u.id.0).await? {
        let root = root_admin_role(st).await?;
        let b = st.db.get_database_backend();
        for r in role_inputs(input) {
            if matches!(r, RoleRef::Id(id) if id == root.id.0) {
                continue;
            }
            let role = stored_role(st, &r).await?.id.0;
            let held = user_roles(&st.db, u.id.0).await?.iter().any(|r| r.id.0 == role);
            if assign && !held {
                st.db
                    .execute_raw(raw::stmt(
                        b,
                        "insert into {model_has_roles} ({role_id}, {model_type}, {model_id}) values (?, ?, ?)",
                        vec![role.into(), "user".into(), u.id.into()],
                    ))
                    .await?;
            } else if !assign && held {
                st.db
                    .execute_raw(raw::stmt(
                        b,
                        "delete from {model_has_roles} where {model_id} = ? and {model_type} = ? and {role_id} = ?",
                        vec![u.id.into(), "user".into(), role.into()],
                    ))
                    .await?;
            }
        }
    }
    Ok(PhpJson::ok(user_item(st, ctx, &u, &Includes::parse(input)).await?).into_response())
}

/// `PATCH /users/{user}/roles/assign`.
pub async fn assign_roles(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    change_roles(&st, &ctx, &input, true).await
}

/// `PATCH /users/{user}/roles/remove`.
pub async fn remove_roles(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    change_roles(&st, &ctx, &input, false).await
}

/// `DELETE /users/{user}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("user", WRITE)?;
    let u = bound_user(&ctx)?;
    if pelican_core::auth::is_root_admin(&st.db, u.id.0).await? {
        return Ok(PhpJson::with_status(StatusCode::FORBIDDEN, json!([])).into_response());
    }
    // deleting hook
    let owned = pelican_db::entities::servers::Entity::find()
        .filter(pelican_db::entities::servers::Column::OwnerId.eq(u.id))
        .one(&st.db)
        .await?;
    if owned.is_some() {
        return Err(ApiError::display(
            "DisplayException",
            400,
            "Cannot delete a user with active servers attached to their account. Please delete their servers before continuing.",
        ));
    }
    if ctx.user()?.id() == u.id.0 {
        return Err(ApiError::display("DisplayException", 400, "Cannot delete your own user account."));
    }
    // User\Deleting → RevocationListener.
    crate::jobs::revoke_sftp_access(&st, &u).await?;
    users::Entity::delete_by_id(u.id).exec(&st.db).await?;
    Ok(no_content())
}
