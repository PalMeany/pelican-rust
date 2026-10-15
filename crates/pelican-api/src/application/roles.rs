//! `/api/application/roles` — `RoleController` (spec §3.7).

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::roles;
use pelican_db::{Int, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter};

use super::data::role_item;
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::Validator;

const API_GUARD: &str = "sanctum";

fn bound(ctx: &Ctx) -> ApiResult<roles::Model> {
    ctx.bound.role.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /roles` (default per_page 10).
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("role", READ)?;
    let b = st.db.get_database_backend();
    let fs = filters(&input, &["id", "name"])?;
    let ss = sorts(&input, &["id", "name"])?;
    let page = Page::new(&input, 10)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in ["id", "name"] {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("roles.{name}"), v) {
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
            &format!("select count(*) as {{aggregate}} from {{roles}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = total_row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = st
            .db
            .query_all_raw(raw::stmt(b, &format!("select * from {{roles}}{where_sql}{order}{}", page.sql()), vals))
            .await?;
        for r in rows {
            data.push(role_item(&st, &roles::Model::from_query_result(&r, "")?, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// `GET /roles/{role}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("role", READ)?;
    Ok(PhpJson::ok(role_item(&st, &bound(&ctx)?, &Includes::parse(&input)).await?).into_response())
}

fn validate(input: &Input) -> ApiResult<String> {
    let mut v = Validator::new(input);
    v.rules("name", &["required", "string"]);
    v.validate()?;
    Ok(input.get("name").map(pelican_core::php::string).unwrap_or_default())
}

/// `POST /roles`: spatie `Role::create` with the request's default guard — `sanctum`
/// for API requests — so the duplicate check (`RoleAlreadyExists`) is per that guard.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("role", WRITE)?;
    let name = validate(&input)?;
    let exists = roles::Entity::find()
        .filter(roles::Column::Name.eq(name.clone()))
        .filter(roles::Column::GuardName.eq(API_GUARD))
        .one(&st.db)
        .await?;
    if exists.is_some() {
        return Err(ApiError::internal("RoleAlreadyExists"));
    }
    let now = Timestamp(pelican_core::time::now());
    let role = pelican_db::write::insert(
        &st.db,
        roles::ActiveModel {
            name: Set(name),
            guard_name: Set(API_GUARD.into()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // spatie Role::create (eloquent.created)
    crate::model_events::created(&st, &role).await;
    let body = with_meta(
        role_item(&st, &role, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("roles/{}", role.id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// `PATCH /roles/{role}`.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("role", WRITE)?;
    let name = validate(&input)?;
    let mut role = bound(&ctx)?;
    if role.name == pelican_core::auth::ROOT_ADMIN_ROLE {
        return Err(ApiError::internal("PanelException"));
    }
    if role.name != name {
        let now = Timestamp(pelican_core::time::now());
        pelican_db::write::update(
            &st.db,
            &roles::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(role.id),
                name: Set(name.clone()),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?;
        role.name = name;
        role.updated_at = Some(now);
        // $role->save() when isDirty (eloquent.updated)
        crate::model_events::updated(&st, &role).await;
    }
    Ok(PhpJson::ok(role_item(&st, &role, &Includes::parse(&input)).await?).into_response())
}

/// `DELETE /roles/{role}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("role", WRITE)?;
    let role = bound(&ctx)?;
    if role.name == pelican_core::auth::ROOT_ADMIN_ROLE {
        return Err(ApiError::internal("PanelException"));
    }
    roles::Entity::delete_by_id(role.id).exec(&st.db).await?;
    // $role->delete() (eloquent.deleted, attributes as before the delete)
    crate::model_events::deleted(&st, &role).await;
    Ok(no_content())
}
