//! Server permission helpers (`GetUserPermissionsService`, `User::can($perm, $server)`).

use pelican_db::entities::{servers, subusers};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde_json::{Map, Value};

use crate::auth::AuthUser;

pub const ADMIN_WEBSOCKET: [&str; 3] =
    ["admin.websocket.errors", "admin.websocket.install", "admin.websocket.transfer"];

pub async fn subuser_permissions<C: ConnectionTrait>(
    db: &C,
    server_id: i64,
    user_id: i64,
) -> Result<Option<Vec<Value>>, DbErr> {
    let row = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(server_id))
        .filter(subusers::Column::UserId.eq(user_id))
        .one(db)
        .await?;
    Ok(row.map(|r| match r.permissions.map(|j| j.0) {
        Some(Value::Array(a)) => a,
        Some(Value::Object(m)) => m.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    }))
}

/// `GetUserPermissionsService::handle` — returns the JSON exactly as PHP
/// encodes it (an `array_unique` with holes becomes an object).
pub async fn user_permissions<C: ConnectionTrait>(
    db: &C,
    server: &servers::Model,
    user: &AuthUser,
) -> Result<Value, DbErr> {
    let node = server.node_id.0;
    let is_owner = user.id() == server.owner_id.0;
    let is_admin = user.is_admin() && (user.can_view_server(node) || user.can_update_server(node));
    if is_owner && !is_admin {
        return Ok(serde_json::json!(["*"]));
    }
    if is_admin && (is_owner || user.can_update_server(node)) {
        let mut v = vec![Value::String("*".into())];
        v.extend(ADMIN_WEBSOCKET.iter().map(|s| Value::String((*s).into())));
        return Ok(Value::Array(v));
    }
    let sub = subuser_permissions(db, server.id.0, user.id()).await?.unwrap_or_default();
    if is_admin {
        let mut all: Vec<Value> = vec![Value::String("websocket.connect".into())];
        all.extend(ADMIN_WEBSOCKET.iter().map(|s| Value::String((*s).into())));
        all.extend(sub);
        return Ok(array_unique(all));
    }
    Ok(Value::Array(sub))
}

/// PHP `array_unique` (string comparison, first occurrence kept, keys kept).
pub fn array_unique(items: Vec<Value>) -> Value {
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<(usize, Value)> = Vec::new();
    for (i, v) in items.into_iter().enumerate() {
        let key = crate::php::string(&v);
        if !seen.contains(&key) {
            seen.push(key);
            kept.push((i, v));
        }
    }
    if kept.iter().enumerate().all(|(pos, (i, _))| pos == *i) {
        Value::Array(kept.into_iter().map(|(_, v)| v).collect())
    } else {
        let mut m = Map::new();
        for (i, v) in kept {
            m.insert(i.to_string(), v);
        }
        Value::Object(m)
    }
}

/// `User::can(<subuser permission>, $server)` (`hasPermission`).
pub async fn has_server_permission<C: ConnectionTrait>(
    db: &C,
    user: &AuthUser,
    server: &servers::Model,
    permission: &str,
) -> Result<bool, DbErr> {
    let node = server.node_id.0;
    if user.can_update_server(node) || server.owner_id.0 == user.id() {
        return Ok(true);
    }
    if permission == "websocket.connect" && user.can_view_server(node) {
        return Ok(true);
    }
    let Some(perms) = subuser_permissions(db, server.id.0, user.id()).await? else { return Ok(false) };
    if permission.is_empty() {
        return Ok(false);
    }
    Ok(perms.iter().any(|p| p.as_str() == Some(permission)))
}

/// `$user->can($perm, [$server])` — Gate with array arguments (client-api.md §5.4):
/// root admin, a spatie permission literally named `$perm`, the owner, or a
/// subuser holding `$perm`.
pub async fn gate_server_permission<C: ConnectionTrait>(
    db: &C,
    user: &AuthUser,
    server: &servers::Model,
    permission: &str,
) -> Result<bool, DbErr> {
    if user.gate_before(permission) {
        return Ok(true);
    }
    if server.owner_id.0 == user.id() {
        return Ok(true);
    }
    let Some(perms) = subuser_permissions(db, server.id.0, user.id()).await? else { return Ok(false) };
    Ok(perms.iter().any(|p| p.as_str() == Some(permission)))
}
