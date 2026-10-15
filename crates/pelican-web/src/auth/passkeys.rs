//! Passkeys: the `laravel/passkeys` routes (login, registration, deletion) with the
//! panel's `AllowPasskeyOrigin` / `AddPasskeyErrorMessage` middleware and the
//! `Handler::invalidPasskey` error rendering. Ceremonies: `pelican_core::webauthn`.

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use pelican_api::error::ApiError;
use pelican_api::services::LogActivity;
use pelican_api::validation::Validator;
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_core::webauthn::{self, CreationOptions, CredentialRecord, Party, RequestOptions};
use pelican_db::entities::{passkeys, users};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    TransactionTrait,
};
use serde_json::{Value, json};

use crate::ctx::{Authed, Web};

const LOGIN_OPTIONS: &str = "passkey.verification_options";
const REGISTRATION_OPTIONS: &str = "passkey.registration_options";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/passkeys/login/options", get(login_options))
        .route("/passkeys/login", post(login))
        .route("/user/passkeys/options", get(registration_options))
        .route("/user/passkeys", post(store))
        .route("/user/passkeys/{passkey}", delete(destroy))
}

/// `config/passkeys.php` plus the request's own origin (`AllowPasskeyOrigin`).
pub fn party(web: &Web) -> Party {
    Party::from_env(&web.st.config.env).for_request(web.origin.as_deref(), &web.scheme_host, &web.host)
}

/// JSON error with `AddPasskeyErrorMessage`'s `message` (the first error's detail).
fn json_error(e: ApiError) -> Response {
    let message = e.errors.first().map(|o| o.detail.clone()).unwrap_or_default();
    (e.status, axum::Json(json!({ "errors": e.errors, "message": message }))).into_response()
}

/// `InvalidPasskeyException` / FormRequest messages: a validation error on `credential`.
fn invalid(message: &str) -> Response {
    json_error(ApiError::validation(vec![("credential".into(), String::new(), message.into())]))
}

/// `Handler::invalidPasskey`: log the failure, answer with a translated detail.
fn verification_failed(web: &Web, party: &Party, cred: &Value, e: &webauthn::Error) -> Response {
    let ceremony_origin = cred
        .pointer("/response/clientDataJSON")
        .and_then(Value::as_str)
        .and_then(webauthn::decode_b64)
        .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        .and_then(|v| v.get("origin").and_then(Value::as_str).map(str::to_string));
    tracing::warn!(
        error = %e,
        ceremony_origin = ?ceremony_origin,
        request_origin = ?web.origin,
        relying_party_id = %party.rp_id,
        enforced_origins = ?party.allowed_origins,
        "passkey verification failed"
    );
    let detail = if e.is_origin_failure() {
        web.t_with("passkeys.invalid_origin", &[("domain", &party.rp_id)])
    } else {
        web.t("passkeys.invalid")
    };
    json_error(ApiError::new(
        422,
        "AuthenticatorResponseVerificationException",
        detail,
        pelican_api::error::Style::Compact,
    ))
}

/// `throttle:6,1` keyed like `ThrottleRequests::resolveRequestSignature`.
fn throttled(web: &Web) -> Option<Response> {
    let who = match &web.user {
        Some(u) => u.id().to_string(),
        None => format!("|{}", web.ip),
    };
    let key = super::sha1_hex(&who);
    let now = chrono::Utc::now().timestamp();
    if let Some(retry) = web.st.limiter.too_many_attempts(&key, 6, now) {
        let mut r = json_error(ApiError::new(
            429,
            "TooManyRequestsHttpException",
            "Too Many Attempts.",
            pelican_api::error::Style::Compact,
        ));
        if let Ok(v) = retry.to_string().parse() {
            r.headers_mut().insert("retry-after", v);
        }
        return Some(r);
    }
    web.st.limiter.hit(&key, 60, now);
    None
}

fn body_json(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

/// The FormRequest rules on `credential` (+ `name` for registration).
fn validate_request(body: &Value, with_name: bool) -> Result<(), Response> {
    let mut v = Validator::for_value(body);
    if with_name {
        v.rules("name", &["required", "string", "max:255"]);
    }
    v.rules("credential", &["required", "array"])
        .rules("credential.id", &["required", "string"])
        .rules("credential.rawId", &["required", "string"])
        .rules("credential.type", &["required", "string", "in:public-key"])
        .rules("credential.response", &["required", "array"]);
    if !with_name {
        v.rules("remember", &["boolean"]);
    }
    v.validate().map_err(json_error)
}

fn take_options<T: serde::de::DeserializeOwned>(web: &Web, key: &str) -> Option<T> {
    let mut out = None;
    web.session.update(|d| out = d.extra.remove(key));
    out.and_then(|v| serde_json::from_value(v).ok())
}

fn put_options<T: serde::Serialize>(web: &Web, key: &str, options: &T) {
    if let Ok(v) = serde_json::to_value(options) {
        web.session.update(|d| {
            d.extra.insert(key.to_string(), v);
        });
    }
}

async fn user_credential_ids(web: &Web, user_id: i64) -> Result<Vec<Vec<u8>>, Response> {
    let rows = passkeys::Entity::find()
        .filter(passkeys::Column::UserId.eq(user_id))
        .order_by_asc(passkeys::Column::Id)
        .all(&web.st.db)
        .await
        .map_err(|e| crate::error::WebError::from(e).into_response())?;
    Ok(rows.iter().filter_map(|p| webauthn::decode_b64(&p.credential_id)).collect())
}

/// `GET /passkeys/login/options` (guest).
async fn login_options(web: Web) -> Response {
    if web.user.is_some() {
        return web.redirect("/");
    }
    if let Some(r) = throttled(&web) {
        return r;
    }
    let options = RequestOptions::new(&party(&web), Vec::new());
    put_options(&web, LOGIN_OPTIONS, &options);
    axum::Json(json!({ "options": options.to_browser() })).into_response()
}

/// `POST /passkeys/login` (guest): verify, sign in (Login event → `auth:success`).
async fn login(web: Web, body: Bytes) -> Response {
    if web.user.is_some() {
        return web.redirect("/");
    }
    if let Some(r) = throttled(&web) {
        return r;
    }
    let body = body_json(&body);
    if let Err(r) = validate_request(&body, false) {
        return r;
    }
    let cred = &body["credential"];
    let Ok(raw_id) = webauthn::assertion_credential_id(cred) else {
        return invalid("Invalid credential format.");
    };
    let Some(options) = take_options::<RequestOptions>(&web, LOGIN_OPTIONS) else {
        return invalid("Passkey verification session expired. Please try again.");
    };
    let party = party(&web);
    let user_id = match verify(&web, &party, cred, &raw_id, &options).await {
        Ok(id) => id,
        Err(r) => return r,
    };
    let remember = matches!(body.get("remember"), Some(Value::Bool(true))) || body.get("remember") == Some(&json!(1));
    web.session.login(user_id, remember);
    let _ = web.activity("auth:success").actor(morph::USER, user_id).subject(morph::USER, user_id).log(&web.st).await;
    let intended = web.session.data().intended.filter(|p| p.starts_with('/') && !p.starts_with("//"));
    web.session.update(|d| d.intended = None);
    axum::Json(json!({ "redirect": intended.unwrap_or_else(|| "/".into()) })).into_response()
}

/// `VerifyPasskey`: look the passkey up (locked), check it, store the updated record.
async fn verify(
    web: &Web,
    party: &Party,
    cred: &Value,
    raw_id: &[u8],
    options: &RequestOptions,
) -> Result<i64, Response> {
    let db_err = |e: sea_orm::DbErr| crate::error::WebError::from(e).into_response();
    if cred.pointer("/response/attestationObject").is_some() || cred.pointer("/response/signature").is_none() {
        return Err(invalid("Unable to verify passkey. Please try again."));
    }
    let txn = web.st.db.begin().await.map_err(db_err)?;
    let mut q = passkeys::Entity::find().filter(passkeys::Column::CredentialId.eq(webauthn::b64url(raw_id)));
    if txn.get_database_backend() != DbBackend::Sqlite {
        q = q.lock_exclusive();
    }
    let Some(passkey) = q.one(&txn).await.map_err(db_err)? else {
        return Err(invalid("Passkey not recognized. It may have been removed from your account."));
    };
    let mut record = CredentialRecord::from_json(&passkey.credential.0).map_err(|e| invalid(&e.to_string()))?;
    match webauthn::verify_assertion(cred, options, &mut record, &party.allowed_origins) {
        Ok(()) => {}
        Err(webauthn::Error::Format(_)) => return Err(invalid("Invalid credential format.")),
        Err(e) => return Err(verification_failed(web, party, cred, &e)),
    }
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::update(
        &txn,
        &passkeys::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(passkey.id),
            credential: Set(Json(record.to_json())),
            last_used_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await
    .map_err(db_err)?;
    txn.commit().await.map_err(db_err)?;
    Ok(passkey.user_id.0)
}

/// `GET /user/passkeys/options`.
async fn registration_options(web: Authed) -> Response {
    if let Some(r) = throttled(&web) {
        return r;
    }
    let u: &users::Model = &web.user.user;
    let exclude = match user_credential_ids(&web, u.id.0).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let options = CreationOptions::new(&party(&web), u.id.0, &u.email, &u.username, exclude);
    put_options(&web, REGISTRATION_OPTIONS, &options);
    axum::Json(json!({ "options": options.to_browser() })).into_response()
}

/// `POST /user/passkeys`: `StorePasskey`.
async fn store(web: Authed, body: Bytes) -> Response {
    if let Some(r) = throttled(&web) {
        return r;
    }
    let body = body_json(&body);
    if let Err(r) = validate_request(&body, true) {
        return r;
    }
    let cred = &body["credential"];
    if webauthn::assertion_credential_id(cred).is_err() {
        return invalid("Invalid credential format.");
    }
    let Some(options) = take_options::<CreationOptions>(&web, REGISTRATION_OPTIONS) else {
        return invalid("Passkey registration session expired. Please try again.");
    };
    if cred.pointer("/response/attestationObject").is_none() {
        return invalid("Unable to register passkey. Please try again.");
    }
    let party = party(&web);
    let record = match webauthn::verify_registration(cred, &options, &party.allowed_origins) {
        Ok(r) => r,
        Err(webauthn::Error::Format(_)) => return invalid("Invalid credential format."),
        Err(e) => return verification_failed(&web, &party, cred, &e),
    };
    let credential_id = webauthn::b64url(&record.public_key_credential_id);
    let db_err = |e: sea_orm::DbErr| crate::error::WebError::from(e).into_response();
    let exists = passkeys::Entity::find()
        .filter(passkeys::Column::CredentialId.eq(&credential_id))
        .one(&web.st.db)
        .await
        .map_err(db_err);
    match exists {
        Ok(Some(_)) => return invalid("Unable to register this passkey."),
        Ok(None) => {}
        Err(r) => return r,
    }
    let name = pelican_api::validation::php_string(&body["name"]);
    let now = Timestamp(pelican_core::time::now());
    let created = pelican_db::write::insert(
        &web.st.db,
        passkeys::ActiveModel {
            user_id: Set(Int(web.user.id())),
            name: Set(name.clone()),
            credential_id: Set(credential_id),
            credential: Set(Json(record.to_json())),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await;
    match created {
        Ok(p) => {
            // Passkeys::onPasskeyRegistered().
            web.flash_success(web.t("passkeys.created_notification_title"));
            axum::Json(json!({ "status": "passkey-registered", "id": p.id.0.to_string(), "name": name }))
                .into_response()
        }
        Err(e) => db_err(e),
    }
}

/// `DELETE /user/passkeys/{passkey}`.
async fn destroy(web: Authed, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<i64>() else { return StatusCode::NOT_FOUND.into_response() };
    match delete_for(&web, id).await {
        Ok(()) => axum::Json(json!({ "status": "passkey-deleted" })).into_response(),
        Err(r) => r,
    }
}

/// Delete one of the signed-in user's passkeys (404 when missing, 403 when not theirs).
pub async fn delete_for(web: &Authed, id: i64) -> Result<(), Response> {
    let db_err = |e: sea_orm::DbErr| crate::error::WebError::from(e).into_response();
    let Some(p) = passkeys::Entity::find_by_id(Int(id)).one(&web.st.db).await.map_err(db_err)? else {
        return Err(crate::error::WebError::NotFound.into_response());
    };
    if p.user_id.0 != web.user.id() {
        return Err(crate::error::WebError::Forbidden.into_response());
    }
    passkeys::Entity::delete_by_id(p.id).exec(&web.st.db).await.map_err(db_err)?;
    Ok(())
}
