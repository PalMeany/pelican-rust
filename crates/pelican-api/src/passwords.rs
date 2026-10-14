//! Laravel's password broker (`Illuminate\Auth\Passwords\{PasswordBroker,
//! DatabaseTokenRepository}`) with Pelican's settings (`auth.passwords.users`:
//! table `password_resets`, expire 60 minutes, throttle 60 seconds) and the
//! Filament `ResetPassword` notification.

use chrono::Duration;
use hmac::Mac;
use pelican_core::mail::MailMessage;
use pelican_core::state::AppState;
use pelican_db::entities::{password_resets, users};
use pelican_db::raw;
use pelican_db::{Int, Timestamp};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::error::{ApiError, ApiResult};

const EXPIRE_MINUTES: i64 = 60;
const THROTTLE_SECONDS: i64 = 60;

/// Broker statuses; `key()` is the translation line (`passwords.*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Sent,
    Reset,
    InvalidUser,
    InvalidToken,
    Throttled,
}

impl Status {
    pub fn key(self) -> &'static str {
        match self {
            Status::Sent => "passwords.sent",
            Status::Reset => "passwords.reset",
            Status::InvalidUser => "passwords.user",
            Status::InvalidToken => "passwords.token",
            Status::Throttled => "passwords.throttled",
        }
    }
}

/// `retrieveByCredentials(['email' => …])`.
async fn user_by_email(st: &AppState, email: &str) -> ApiResult<Option<users::Model>> {
    Ok(users::Entity::find().filter(users::Column::Email.eq(email)).one(&st.db).await?)
}

/// `createNewToken()`: `hash_hmac('sha256', Str::random(40), $hashKey)` with the decoded app key.
fn new_token(st: &AppState) -> String {
    let key = st.config.env.string_or("APP_KEY", "");
    let key = match key.strip_prefix("base64:") {
        Some(b) => base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b)
            .unwrap_or_else(|_| key.clone().into_bytes()),
        None => key.into_bytes(),
    };
    hmac::Hmac::<sha2::Sha256>::new_from_slice(&key)
        .map(|mut m| {
            m.update(pelican_wings::jwt::str_random(40).as_bytes());
            hex::encode(m.finalize().into_bytes())
        })
        .unwrap_or_else(|_| pelican_wings::jwt::str_random(64))
}

/// `DatabaseTokenRepository::create()`: drop old tokens, store the bcrypt hash.
pub async fn create_token<C: ConnectionTrait>(st: &AppState, db: &C, email: &str) -> ApiResult<String> {
    password_resets::Entity::delete_many().filter(password_resets::Column::Email.eq(email)).exec(db).await?;
    let token = new_token(st);
    let hash = laravel_compat::hashing::make(&token, st.config.bcrypt_rounds)
        .map_err(|_| ApiError::internal("RuntimeException"))?;
    db.execute_raw(raw::stmt(
        db.get_database_backend(),
        "insert into {password_resets} ({email}, {token}, {created_at}) values (?, ?, ?)",
        vec![email.into(), hash.into(), Timestamp(pelican_core::time::now()).into()],
    ))
    .await?;
    Ok(token)
}

async fn record(st: &AppState, email: &str) -> ApiResult<Option<password_resets::Model>> {
    Ok(password_resets::Entity::find().filter(password_resets::Column::Email.eq(email)).one(&st.db).await?)
}

/// `PasswordBroker::sendResetLink()` + the Filament `ResetPassword` notification.
pub async fn send_reset_link(st: &AppState, email: &str) -> ApiResult<Status> {
    let Some(user) = user_by_email(st, email).await? else { return Ok(Status::InvalidUser) };
    if let Some(r) = record(st, &user.email).await? {
        // recentlyCreatedToken()
        if r.created_at.0 + Duration::seconds(THROTTLE_SECONDS) >= pelican_core::time::now() {
            return Ok(Status::Throttled);
        }
    }
    let token = create_token(st, &st.db, &user.email).await?;
    let msg = MailMessage {
        outro: vec![
            format!("This password reset link will expire in {EXPIRE_MINUTES} minutes."),
            "If you did not request a password reset, no further action is required.".into(),
        ],
        subject: "Reset Password Notification".into(),
        error: false,
        greeting: None,
        lines: vec![
            "You are receiving this email because we received a password reset request for your account.".into(),
        ],
        action: Some(("Reset Password".into(), crate::services::reset_url(st, &user.email, &token))),
    };
    crate::services::notify(st, &user, msg).await?;
    Ok(Status::Sent)
}

/// `PasswordBroker::reset()`: on success the password is replaced, the remember
/// token rotated and the token deleted; returns the user for the caller's events.
pub async fn reset(
    st: &AppState,
    email: &str,
    token: &str,
    password: &str,
) -> ApiResult<(Status, Option<users::Model>)> {
    let Some(user) = user_by_email(st, email).await? else { return Ok((Status::InvalidUser, None)) };
    let valid = match record(st, &user.email).await? {
        Some(r) => {
            r.created_at.0 + Duration::minutes(EXPIRE_MINUTES) >= pelican_core::time::now()
                && laravel_compat::hashing::check(token, &r.token).unwrap_or(false)
        }
        None => false,
    };
    if !valid {
        return Ok((Status::InvalidToken, None));
    }
    let hash = laravel_compat::hashing::make(password, st.config.bcrypt_rounds)
        .map_err(|_| ApiError::internal("RuntimeException"))?;
    st.db
        .execute_raw(raw::stmt(
            st.db.get_database_backend(),
            "update {users} set {password} = ?, {remember_token} = ?, {updated_at} = ? where {id} = ?",
            vec![
                hash.into(),
                pelican_wings::jwt::str_random(60).into(),
                Timestamp(pelican_core::time::now()).into(),
                Int(user.id.0).into(),
            ],
        ))
        .await?;
    password_resets::Entity::delete_many()
        .filter(password_resets::Column::Email.eq(user.email.clone()))
        .exec(&st.db)
        .await?;
    Ok((Status::Reset, Some(user)))
}
