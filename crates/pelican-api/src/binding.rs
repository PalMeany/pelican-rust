//! Route-model binding (`SubstituteBindings`), which runs BEFORE authentication:
//! unknown models give 404 even to unauthenticated requests.

use axum::extract::RawPathParams;
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, backups, databases, schedules, servers, subusers, tasks, users};
use pelican_db::{Driver, Int};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};

use crate::error::{ApiError, ApiResult};
use crate::pipeline::Group;

/// Models resolved from route parameters.
#[derive(Debug, Default)]
pub struct Bound {
    pub server: Option<servers::Model>,
    pub user: Option<users::Model>,
    pub database: Option<databases::Model>,
    pub schedule: Option<schedules::Model>,
    pub task: Option<tasks::Model>,
    pub allocation: Option<allocations::Model>,
    pub backup: Option<backups::Model>,
    /// Client `{user:uuid}`: the subuser row of the bound user.
    pub subuser: Option<subusers::Model>,
    /// Raw parameters by name (for string parameters and late lookups).
    pub raw: Vec<(String, String)>,
}

impl Bound {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.raw.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn server(&self) -> ApiResult<&servers::Model> {
        self.server.as_ref().ok_or_else(|| ApiError::internal("InvalidArgumentException"))
    }
}

/// `where('id', $value)` with the database's own string → integer coercion:
/// MySQL compares numerically using the leading digits, SQLite never matches a
/// non-integer string, PostgreSQL raises a query error (HTTP 500).
pub fn bind_id(raw: &str, driver: Driver) -> ApiResult<Option<i64>> {
    if let Ok(v) = raw.parse::<i64>() {
        return Ok(Some(v));
    }
    match driver {
        Driver::Mysql | Driver::Mariadb => Ok(Some(pelican_core::env::php_intval(raw))),
        Driver::Sqlite => Ok(None),
        Driver::Pgsql => Err(ApiError::internal("QueryException")),
    }
}

pub async fn resolve(st: &AppState, group: Group, params: &RawPathParams) -> ApiResult<Bound> {
    let mut b =
        Bound { raw: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), ..Default::default() };
    for (name, value) in params.iter() {
        match (group, name) {
            // `Server::resolveRouteBinding`: uuid_short OR uuid (client and remote).
            (Group::Client | Group::Remote, "server") => {
                let s = servers::Entity::find()
                    .filter(
                        Condition::any().add(servers::Column::UuidShort.eq(value)).add(servers::Column::Uuid.eq(value)),
                    )
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?;
                b.server = Some(s);
            }
            (Group::Application, "server") => {
                let id = bind_id(value, st.driver)?.ok_or_else(ApiError::model_not_found)?;
                b.server = Some(
                    servers::Entity::find_by_id(Int(id)).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?,
                );
            }
            (Group::Application, "user") => {
                let id = bind_id(value, st.driver)?.ok_or_else(ApiError::model_not_found)?;
                b.user =
                    Some(users::Entity::find_by_id(Int(id)).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?);
            }
            (Group::Client, child) => bind_client_child(st, &mut b, child, value).await?,
            _ => {}
        }
    }
    Ok(b)
}

/// Scoped child bindings of `/api/client/servers/{server}/...` (`scopeBindings()`).
async fn bind_client_child(st: &AppState, b: &mut Bound, name: &str, value: &str) -> ApiResult<()> {
    let Some(server_id) = b.server.as_ref().map(|s| s.id) else { return Ok(()) };
    let id = || -> ApiResult<i64> { bind_id(value, st.driver)?.ok_or_else(ApiError::model_not_found) };
    match name {
        "database" => {
            b.database = Some(
                databases::Entity::find()
                    .filter(databases::Column::ServerId.eq(server_id))
                    .filter(databases::Column::Id.eq(id()?))
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?,
            );
        }
        "schedule" => {
            b.schedule = Some(
                schedules::Entity::find()
                    .filter(schedules::Column::ServerId.eq(server_id))
                    .filter(schedules::Column::Id.eq(id()?))
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?,
            );
        }
        "task" => {
            let Some(schedule_id) = b.schedule.as_ref().map(|s| s.id) else { return Ok(()) };
            b.task = Some(
                tasks::Entity::find()
                    .filter(tasks::Column::ScheduleId.eq(schedule_id))
                    .filter(tasks::Column::Id.eq(id()?))
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?,
            );
        }
        "allocation" => {
            b.allocation = Some(
                allocations::Entity::find()
                    .filter(allocations::Column::ServerId.eq(server_id))
                    .filter(allocations::Column::Id.eq(id()?))
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?,
            );
        }
        "backup" => {
            b.backup = Some(
                backups::Entity::find()
                    .filter(backups::Column::ServerId.eq(server_id))
                    .filter(backups::Column::Uuid.eq(value))
                    .one(&st.db)
                    .await?
                    .ok_or_else(ApiError::model_not_found)?,
            );
        }
        "user" => {
            // Server::resolveChildRouteBinding: subusers()->whereRelation('user','uuid',$v)->firstOrFail()->user
            let user = users::Entity::find()
                .filter(users::Column::Uuid.eq(value))
                .one(&st.db)
                .await?
                .ok_or_else(ApiError::model_not_found)?;
            let sub = subusers::Entity::find()
                .filter(subusers::Column::ServerId.eq(server_id))
                .filter(subusers::Column::UserId.eq(user.id))
                .one(&st.db)
                .await?
                .ok_or_else(ApiError::model_not_found)?;
            b.subuser = Some(sub);
            b.user = Some(user);
        }
        _ => {}
    }
    Ok(())
}
