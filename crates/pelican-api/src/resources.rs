//! API resources (`app/Data/Api/*Data.php`). Field order = declaration order.

use md5::{Digest, Md5};
use pelican_core::auth::AuthUser;
use pelican_db::Timestamp;
use serde::Serialize;

/// `ApiResource::formatTimestamp`: `Y-m-d H:i:s` re-parsed as UTC → ATOM.
pub fn format_timestamp(t: Option<&Timestamp>) -> String {
    t.map(|t| pelican_core::time::atom(&t.0)).unwrap_or_default()
}

/// Carbon `toAtomString()` of a nullable model date.
pub fn atom(t: Option<&Timestamp>) -> Option<String> {
    t.map(|t| pelican_core::time::atom(&t.0))
}

/// `UserData` (resource `user`).
#[derive(Serialize)]
pub struct UserData {
    pub uuid: String,
    pub username: String,
    pub email: String,
    pub language: Option<String>,
    pub image: String,
    pub admin: bool,
    pub root_admin: bool,
    #[serde(rename = "2fa_enabled")]
    pub two_factor_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub fn gravatar(email: &str) -> String {
    format!("https://gravatar.com/avatar/{}", hex::encode(Md5::digest(email.to_lowercase().as_bytes())))
}

pub fn user_data(u: &AuthUser) -> UserData {
    let m = &u.user;
    UserData {
        uuid: m.uuid.clone(),
        username: m.username.clone(),
        email: m.email.clone(),
        language: Some(m.language.clone()),
        image: gravatar(&m.email),
        admin: u.is_root_admin(),
        root_admin: u.is_root_admin(),
        // filled($model->mfa_app_secret): column non-empty (decrypted value non-blank).
        two_factor_enabled: m.mfa_app_secret.as_deref().is_some_and(|s| !s.trim().is_empty()),
        created_at: format_timestamp(m.created_at.as_ref()),
        updated_at: format_timestamp(m.updated_at.as_ref()),
    }
}
