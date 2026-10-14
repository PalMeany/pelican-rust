//! R1 `POST /api/remote/sftp/auth` (`SftpAuthenticationController`).

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use pelican_core::activity::{Activity, morph};
use pelican_core::auth::{AuthUser, Credential};
use pelican_core::state::AppState;
use pelican_db::entities::{servers, user_ssh_keys, users};
use sea_orm::{ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::node;
use crate::error::{ApiError, ApiResult, Style};
use crate::input::In;
use crate::json::PhpJson;
use crate::pipeline::Ctx;
use crate::services::LogActivity;
use crate::validation::Validator;

const MAX_ATTEMPTS: u32 = 5;
const DECAY: i64 = 60;

fn forbidden(detail: &str) -> ApiError {
    ApiError::new(403, "HttpForbiddenException", detail, Style::Pretty)
}

fn reject() -> ApiError {
    forbidden("Authorization credentials were not correct, please try again.")
}

/// phpseclib `PublicKeyLoader::loadPublicKey` (OpenSSH format) + `getFingerprint('sha256')`:
/// base64(sha256(blob)) without the trailing `=`.
fn openssh_fingerprint(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let kind = parts.next()?;
    let b64 = parts.next()?;
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    // The blob starts with the key type as an SSH string.
    let len = u32::from_be_bytes(blob.get(0..4)?.try_into().ok()?) as usize;
    let inner_kind = std::str::from_utf8(blob.get(4..4 + len)?).ok()?;
    if inner_kind != kind || !kind.starts_with("ssh-") && !kind.starts_with("ecdsa-") {
        return None;
    }
    let fp = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&blob));
    Some(fp.trim_end_matches('=').to_string())
}

pub async fn auth(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let mut v = Validator::new(&input);
    v.rules("type", &["nullable", "in:password,public_key"]);
    v.rules("username", &["required", "string"]);
    v.rules("password", &["required", "string"]);
    v.validate()?;
    let node = node(&ctx)?;
    let username_raw = input.str("username").unwrap_or("");
    // explode('.', strrev($value), 2) — split at the LAST dot.
    let (user_part, server_part) = match username_raw.rfind('.') {
        Some(i) => (&username_raw[..i], &username_raw[i + 1..]),
        None => ("", username_raw),
    };
    if server_part.is_empty() || server_part == "0" {
        return Err(ApiError::bad_request("No valid server identifier was included in the request."));
    }
    let throttle_key = format!("sftp|{}|{}", server_part.to_lowercase(), ctx.ip).to_lowercase();
    let now = chrono::Utc::now().timestamp();
    if let Some(retry_after) = st.limiter.too_many_attempts(&throttle_key, MAX_ATTEMPTS, now) {
        return Err(ApiError::new(
            429,
            "TooManyRequestsHttpException",
            format!("Too many login attempts for this account, please try again in {retry_after} seconds."),
            Style::Pretty,
        )
        .with_header("retry-after", retry_after));
    }
    let increment = |st: &AppState| {
        st.limiter.hit(&throttle_key, DECAY, now);
    };

    let user =
        users::Entity::find().filter(users::Column::Username.eq(user_part.trim().to_lowercase())).one(&st.db).await?;
    let Some(user) = user else {
        increment(&st);
        return Err(reject());
    };
    let server = servers::Entity::find()
        .filter(
            Condition::any().add(servers::Column::Uuid.eq(server_part)).add(servers::Column::UuidShort.eq(server_part)),
        )
        .filter(servers::Column::NodeId.eq(node.id))
        .one(&st.db)
        .await?;
    let Some(server) = server else {
        increment(&st);
        return Err(reject());
    };
    let password = input.str("password").unwrap_or("");
    if input.str("type") != Some("public_key") {
        let ok = laravel_compat::hashing::check(password, &user.password).unwrap_or(false);
        if !ok {
            Activity::new("auth:sftp.fail", ctx.ip.to_string())
                .property("method", "password")
                .subject(morph::USER, user.id.0)
                .log(&st)
                .await?;
            increment(&st);
            return Err(reject());
        }
    } else {
        let fp = openssh_fingerprint(password.trim());
        let found = match &fp {
            None => false,
            Some(f) => {
                user_ssh_keys::Entity::find()
                    .filter(user_ssh_keys::Column::UserId.eq(user.id))
                    .filter(user_ssh_keys::Column::DeletedAt.is_null())
                    .filter(user_ssh_keys::Column::Fingerprint.eq(f.as_str()))
                    .count(&st.db)
                    .await?
                    > 0
            }
        };
        if !found {
            if fp.is_none() {
                increment(&st);
            }
            return Err(reject());
        }
    }

    let auth = AuthUser::load(&st.db, user.clone(), Credential::Session).await?;
    let perms = pelican_core::permissions::user_permissions(&st.db, &server, &auth).await?;
    if !auth.gate_before("update server") && server.owner_id != user.id {
        let has_sftp = match &perms {
            Value::Array(a) => a.iter().any(|p| p.as_str() == Some("file.sftp")),
            Value::Object(m) => m.values().any(|p| p.as_str() == Some("file.sftp")),
            _ => false,
        };
        if !has_sftp {
            Activity::new("server:sftp.denied", ctx.ip.to_string())
                .actor(morph::USER, user.id.0)
                .subject(morph::SERVER, server.id.0)
                .log(&st)
                .await?;
            return Err(forbidden("You do not have permission to access SFTP for this server."));
        }
    }
    if let Some(detail) = pelican_core::server_state::conflict(&st.db, &server).await? {
        return Err(ApiError::new(409, "ServerStateConflictException", detail, Style::Pretty));
    }
    let mut body = Map::new();
    body.insert("user".into(), json!(user.uuid));
    body.insert("server".into(), json!(server.uuid));
    body.insert("permissions".into(), perms);
    Ok(PhpJson::ok(Value::Object(body)).into_response())
}
