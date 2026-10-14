//! `/servers/{server}/startup` — `StartupController`.

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::server_config::{ServerBundle, php_assoc};
use pelican_core::state::AppState;
use pelican_db::entities::{egg_variables, server_variables};
use pelican_db::{Int, Timestamp, raw};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter};
use serde_json::{Map, Value, json};

use super::data::{egg_variable_data, startup_command};
use crate::envelope::{list, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::PhpJson;
use crate::pipeline::Ctx;
use crate::validation::Validator;

async fn bundle(st: &AppState, ctx: &Ctx) -> ApiResult<ServerBundle> {
    let server = ctx.bound.server()?.clone();
    ServerBundle::load(&st.db, server).await.map_err(|e| match e {
        pelican_core::server_config::ConfigError::Db(d) => d.into(),
        pelican_core::server_config::ConfigError::Php(c) => ApiError::internal(c),
    })
}

const VARIABLES_SQL: &str = "select {egg_variables}.*, {server_variables.variable_value} as {server_value} from {egg_variables} \
     left join {server_variables} on {server_variables.variable_id} = {egg_variables.id} and {server_variables.server_id} = ? \
     where {egg_variables.egg_id} = ? and {egg_variables.egg_id} is not null";

/// `GET /startup`.
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "startup.read").await?;
    let b = bundle(&st, &ctx).await?;
    let db = &st.db;
    let backend = db.get_database_backend();
    // `$server->variables()->where('user_viewable', true)->orderBy('sort')`.
    let rows = db
        .query_all_raw(raw::stmt(
            backend,
            &format!("{VARIABLES_SQL} and {{user_viewable}} = ? order by {{sort}} asc"),
            vec![
                b.server.id.into(),
                b.server.egg_id.into(),
                if backend == sea_orm::DbBackend::Postgres { 1i64.into() } else { 1.into() },
            ],
        ))
        .await?;
    let mut data = Vec::new();
    for r in rows {
        let v = egg_variables::Model::from_query_result(&r, "")?;
        let sv: Option<String> = r.try_get("", "server_value")?;
        data.push(egg_variable_data(&v, sv.as_deref()));
    }
    let mut meta = Map::new();
    meta.insert("startup_command".into(), json!(startup_command(&b, false)));
    meta.insert(
        "docker_images".into(),
        b.egg.docker_images.as_ref().map(|j| php_assoc(j.0.clone())).unwrap_or(Value::Null),
    );
    meta.insert("raw_startup_command".into(), json!(b.server.startup));
    Ok(PhpJson::ok(with_meta(list(data), meta)).into_response())
}

/// `PUT /startup/variable`.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "startup.update").await?;
    let mut v = Validator::new(&input);
    v.rules("key", &["required", "string"]);
    v.rules("value", &["present"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let db = &st.db;
    let backend = db.get_database_backend();
    let key = input.get("key").map(pelican_core::php::string).unwrap_or_default();
    let row = db
        .query_one_raw(raw::stmt(
            backend,
            &format!("{VARIABLES_SQL} and {{env_variable}} = ? limit 1"),
            vec![server.id.into(), server.egg_id.into(), key.into()],
        ))
        .await?;
    let (var, original) = match row {
        Some(r) => (egg_variables::Model::from_query_result(&r, "")?, r.try_get::<Option<String>>("", "server_value")?),
        None => return Err(ApiError::bad_request("The environment variable you are trying to edit does not exist.")),
    };
    if !var.user_viewable.0 {
        return Err(ApiError::bad_request("The environment variable you are trying to edit does not exist."));
    }
    if !var.user_editable.0 {
        return Err(ApiError::bad_request("The environment variable you are trying to edit is read-only."));
    }
    // $request->validate(['value' => $variable->rules])
    let rules: Vec<String> = match &var.rules.0 {
        Value::Array(a) => a.iter().map(pelican_core::php::string).collect(),
        Value::String(s) => s.split('|').map(str::to_string).collect(),
        _ => Vec::new(),
    };
    let rule_refs: Vec<&str> = rules.iter().map(String::as_str).collect();
    let mut v = Validator::new(&input);
    v.rules("value", &rule_refs);
    v.validate()?;
    let raw_value = input.get("value").cloned().unwrap_or(Value::Null);
    if raw_value.is_array() || raw_value.is_object() {
        // `?string` typed property in EggVariableData.
        return Err(ApiError::internal("TypeError"));
    }
    let stored = if raw_value.is_null() { String::new() } else { pelican_core::php::string(&raw_value) };
    let now = Timestamp(pelican_core::time::now());
    let existing = server_variables::Entity::find()
        .filter(server_variables::Column::ServerId.eq(server.id))
        .filter(server_variables::Column::VariableId.eq(var.id))
        .one(db)
        .await?;
    match existing {
        Some(e) => {
            if e.variable_value != stored {
                server_variables::Entity::update_many()
                    .col_expr(server_variables::Column::VariableValue, Expr::value(stored.clone()))
                    .col_expr(server_variables::Column::UpdatedAt, Expr::value(now))
                    .filter(server_variables::Column::Id.eq(e.id))
                    .exec(db)
                    .await?;
            }
        }
        None => {
            let am = server_variables::ActiveModel {
                server_id: Set(server.id),
                variable_id: Set(var.id),
                variable_value: Set(stored.clone()),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            pelican_db::write::insert_id(db, &am).await?;
        }
    }
    let b = bundle(&st, &ctx).await?;
    let startup = startup_command(&b, false);
    // `$original !== $request->input('value')` (strict).
    let changed = match (&original, &raw_value) {
        (Some(o), Value::String(s)) => o != s,
        (None, Value::Null) => false,
        _ => true,
    };
    if changed {
        ctx.activity("server:startup.edit")
            .subject(morph::EGG_VARIABLE, var.id.0)
            .property("variable", var.env_variable.clone())
            .property("old", original.clone().map(Value::String).unwrap_or(Value::Null))
            .property("new", if raw_value.is_null() { json!("") } else { raw_value.clone() })
            .save(db)
            .await?;
    }
    // After refresh() the joined server_value is gone and replaced by the raw input.
    let server_value = if raw_value.is_null() { None } else { Some(pelican_core::php::string(&raw_value)) };
    let mut meta = Map::new();
    meta.insert("startup_command".into(), json!(startup));
    meta.insert("raw_startup_command".into(), json!(server.startup));
    let _ = Int(0);
    Ok(PhpJson::ok(with_meta(egg_variable_data(&var, server_value.as_deref()), meta)).into_response())
}
