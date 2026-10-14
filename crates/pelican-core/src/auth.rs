//! Authentication and admin authorization (docs/spec/auth-crypto.md §3, §7).

use std::collections::HashSet;

use laravel_compat::Encrypter;
use pelican_db::entities::{
    api_keys, model_has_permissions, model_has_roles, node_role, permissions, role_has_permissions,
    roles, users,
};
use pelican_db::{Int, Timestamp};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};

pub const ROOT_ADMIN_ROLE: &str = "Root Admin";
/// `ApiKey::TYPE_*` (app/Models/ApiKey.php:63-67).
pub const KEY_TYPE_ACCOUNT: i64 = 1;
pub const KEY_TYPE_APPLICATION: i64 = 2;

/// `Request::bearerToken()`: last case-insensitive `Bearer ` occurrence, cut at
/// `,`. `"Bearer "` alone yields `Some("")` (not null), like PHP.
pub fn bearer_token(authorization: &str) -> Option<String> {
    let lower = authorization.to_ascii_lowercase();
    let pos = lower.rfind("bearer ")?;
    let rest = &authorization[pos + 7..];
    Some(rest.split(',').next().unwrap_or("").to_string())
}

/// Sanctum `Guard::isValidBearerToken`.
pub fn is_valid_bearer_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    match token.split_once('|') {
        Some((id, rest)) => {
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty()
        }
        None => true,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Db(#[from] DbErr),
    /// `decrypt()` of a stored token failed — PHP throws (HTTP 500).
    #[error("could not decrypt api key: {0}")]
    Decrypt(#[from] laravel_compat::DecryptError),
}

/// `ApiKey::findToken` + Sanctum expiry check. Returns the key row if valid.
pub async fn find_api_key<C: ConnectionTrait>(
    db: &C,
    enc: &Encrypter,
    token: &str,
    now: chrono::NaiveDateTime,
) -> Result<Option<api_keys::Model>, AuthError> {
    if !is_valid_bearer_token(token) {
        return Ok(None);
    }
    // PHP substr is byte based.
    let bytes = token.as_bytes();
    let split = bytes.len().min(16);
    let identifier = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let secret = String::from_utf8_lossy(&bytes[split..]).into_owned();
    let Some(key) = api_keys::Entity::find()
        .filter(api_keys::Column::Identifier.eq(identifier))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let stored = enc.decrypt_string_lenient(&key.token)?;
    if stored != secret {
        return Ok(None);
    }
    if key.expires_at.is_some_and(|e| e.0 <= now) {
        return Ok(None);
    }
    Ok(Some(key))
}

/// Sanctum `updateLastUsedAt`: `forceFill(['last_used_at' => now()])->save()`.
/// Eloquent only writes when the value actually changes (dirty check).
pub async fn touch_last_used<C: ConnectionTrait>(
    db: &C,
    key: &api_keys::Model,
    now: chrono::NaiveDateTime,
) -> Result<bool, DbErr> {
    if key.last_used_at.is_some_and(|t| t.0 == now) {
        return Ok(false);
    }
    api_keys::Entity::update_many()
        .col_expr(
            api_keys::Column::LastUsedAt,
            sea_orm::sea_query::Expr::value(Timestamp(now)),
        )
        .col_expr(
            api_keys::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Timestamp(now)),
        )
        .filter(api_keys::Column::Id.eq(key.id))
        .exec(db)
        .await?;
    Ok(true)
}

/// How the request authenticated.
#[derive(Clone, Debug)]
pub enum Credential {
    ApiKey(api_keys::Model),
    /// Sanctum `TransientToken` (browser session).
    Session,
}

/// The authenticated user with roles and permissions resolved (spatie).
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user: users::Model,
    pub credential: Credential,
    pub role_names: Vec<String>,
    /// All permission names via roles and direct grants (`getAllPermissions`).
    pub permissions: HashSet<String>,
    /// Permission names with guard `web` (what `checkPermissionTo` sees).
    pub web_permissions: HashSet<String>,
    /// `accessibleNodes()`: None = every node.
    pub node_ids: Option<HashSet<i64>>,
}

impl AuthUser {
    pub async fn load<C: ConnectionTrait>(
        db: &C,
        user: users::Model,
        credential: Credential,
    ) -> Result<AuthUser, DbErr> {
        let role_ids: Vec<Int> = model_has_roles::Entity::find()
            .select_only()
            .column(model_has_roles::Column::RoleId)
            .filter(model_has_roles::Column::ModelType.eq("user"))
            .filter(model_has_roles::Column::ModelId.eq(user.id))
            .into_tuple()
            .all(db)
            .await?;
        let role_rows = if role_ids.is_empty() {
            Vec::new()
        } else {
            roles::Entity::find()
                .filter(roles::Column::Id.is_in(role_ids.clone()))
                .all(db)
                .await?
        };
        let mut perm_ids: Vec<Int> = if role_ids.is_empty() {
            Vec::new()
        } else {
            role_has_permissions::Entity::find()
                .select_only()
                .column(role_has_permissions::Column::PermissionId)
                .filter(role_has_permissions::Column::RoleId.is_in(role_ids.clone()))
                .into_tuple()
                .all(db)
                .await?
        };
        let direct: Vec<Int> = model_has_permissions::Entity::find()
            .select_only()
            .column(model_has_permissions::Column::PermissionId)
            .filter(model_has_permissions::Column::ModelType.eq("user"))
            .filter(model_has_permissions::Column::ModelId.eq(user.id))
            .into_tuple()
            .all(db)
            .await?;
        perm_ids.extend(direct);
        let perms = if perm_ids.is_empty() {
            Vec::new()
        } else {
            permissions::Entity::find()
                .filter(permissions::Column::Id.is_in(perm_ids))
                .all(db)
                .await?
        };
        let role_names: Vec<String> = role_rows.iter().map(|r| r.name.clone()).collect();
        let is_root = role_names.iter().any(|n| n == ROOT_ADMIN_ROLE);
        let node_ids = if is_root || role_ids.is_empty() {
            None
        } else {
            let rows: Vec<Int> = node_role::Entity::find()
                .select_only()
                .column(node_role::Column::NodeId)
                .filter(node_role::Column::RoleId.is_in(role_ids))
                .into_tuple()
                .all(db)
                .await?;
            (!rows.is_empty()).then(|| rows.into_iter().map(|i| i.0).collect())
        };
        Ok(AuthUser {
            user,
            credential,
            role_names,
            permissions: perms.iter().map(|p| p.name.clone()).collect(),
            web_permissions: perms
                .iter()
                .filter(|p| p.guard_name == "web")
                .map(|p| p.name.clone())
                .collect(),
            node_ids,
        })
    }

    pub fn id(&self) -> i64 {
        self.user.id.0
    }

    pub fn api_key(&self) -> Option<&api_keys::Model> {
        match &self.credential {
            Credential::ApiKey(k) => Some(k),
            Credential::Session => None,
        }
    }

    /// `User::isRootAdmin()` = `hasRole('Root Admin')`.
    pub fn is_root_admin(&self) -> bool {
        self.role_names.iter().any(|n| n == ROOT_ADMIN_ROLE)
    }

    /// `User::isAdmin()`.
    pub fn is_admin(&self) -> bool {
        self.is_root_admin() || (!self.role_names.is_empty() && !self.permissions.is_empty())
    }

    /// Gate global before-callbacks: root admin or spatie `checkPermissionTo`.
    pub fn gate_before(&self, ability: &str) -> bool {
        self.is_root_admin() || self.web_permissions.contains(ability)
    }

    /// `canTarget(Node)`.
    pub fn can_target_node(&self, node_id: i64) -> bool {
        self.is_root_admin()
            || self
                .node_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&node_id))
    }
}
