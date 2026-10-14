//! `/servers/{server}/databases` — `DatabaseController` (client-api.md §8.4),
//! `DeployServerDatabaseService` + `DatabaseManagementService`.

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::databases::{HostConnection, HostError, random_password, str_random, unique_name};
use pelican_core::state::AppState;
use pelican_db::entities::{database_hosts, databases, servers};
use pelican_db::{Int, Timestamp, raw};
use rand::seq::IndexedRandom;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use crate::envelope::{Includes, item, list, null_resource};
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::services::LogActivity;
use crate::validation::{CustomRule, Validator};

fn host_err(e: HostError) -> ApiError {
    match e {
        HostError::Query(_) => ApiError::internal("QueryException"),
        HostError::Decrypt => ApiError::internal("DecryptException"),
    }
}

/// `DatabaseData` (§9.6) with the optional `password` include.
async fn database_item(
    st: &AppState,
    ctx: &Ctx,
    db: &databases::Model,
    host: &database_hosts::Model,
    includes: &Includes,
) -> ApiResult<Value> {
    let mut a = Map::new();
    a.insert("id".into(), json!(db.id.0));
    a.insert("host".into(), json!({"address": host.host, "port": host.port.0}));
    a.insert("name".into(), json!(db.database));
    a.insert("username".into(), json!(db.username));
    a.insert("connections_from".into(), json!(db.remote));
    a.insert("max_connections".into(), json!(db.max_connections.map(|m| m.0)));
    let mut rel = Vec::new();
    if includes.is_requested("password") {
        let v = if ctx.gate(st, "database.view-password").await? {
            let pw = st.enc.decrypt_string_lenient(&db.password).map_err(|_| ApiError::internal("DecryptException"))?;
            json!({"object": "database_password", "attributes": {"password": pw}})
        } else {
            null_resource()
        };
        rel.push(("password", v));
    }
    Ok(item("server_database", Value::Object(a), rel))
}

async fn host_of<C: ConnectionTrait>(db: &C, id: Int) -> ApiResult<database_hosts::Model> {
    database_hosts::Entity::find_by_id(id).one(db).await?.ok_or_else(|| ApiError::internal("ErrorException"))
}

/// `GET /databases`.
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "database.read").await?;
    let server = ctx.bound.server()?.clone();
    let includes = Includes::parse(&input);
    let backend = st.db.get_database_backend();
    // `$server->databases` (no explicit order, ADR-0011).
    let rows = st
        .db
        .query_all_raw(raw::stmt(
            backend,
            "select * from {databases} where {databases.server_id} = ? and {databases.server_id} is not null",
            vec![server.id.into()],
        ))
        .await?;
    let mut data = Vec::new();
    for r in rows {
        let d = databases::Model::from_query_result(&r, "")?;
        let host = host_of(&st.db, d.database_host_id).await?;
        data.push(database_item(&st, &ctx, &d, &host, &includes).await?);
    }
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `SELECT id FROM servers WHERE id = ? FOR UPDATE` (no lock clause on SQLite).
async fn lock_server(txn: &DatabaseTransaction, backend: DbBackend, id: Int) -> ApiResult<()> {
    let lock = if backend == DbBackend::Sqlite { "" } else { " for update" };
    let sql = format!("select {{id}} from {{servers}} where {{servers.id}} = ? limit 1{lock}");
    txn.query_one_raw(raw::stmt(backend, &sql, vec![id.into()])).await?;
    Ok(())
}

/// `POST /databases`.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "database.create").await?;
    let server = ctx.bound.server()?.clone();
    let backend = st.db.get_database_backend();

    let mut v = Validator::new(&input);
    v.rules("database", &["required", "alpha_dash", "min:3", "max:48"]);
    // Rule::unique('databases')->where(server_id, database = prefixed(input)) on top of
    // `database = input`: both can only hold when the input already equals its prefixed form.
    let unique_failed = match input.get("database") {
        Some(Value::Array(_)) | Some(Value::Object(_)) => return Err(ApiError::internal("TypeError")),
        None | Some(Value::Null) => false,
        Some(val) => {
            let raw_name = pelican_core::php::string(val);
            let prefixed = unique_name(&raw_name, server.id.0);
            prefixed == raw_name
                && databases::Entity::find()
                    .filter(databases::Column::ServerId.eq(server.id))
                    .filter(databases::Column::Database.eq(prefixed))
                    .one(&st.db)
                    .await?
                    .is_some()
        }
    };
    v.custom(
        "database",
        CustomRule::precomputed(
            "unique",
            unique_failed.then(|| "The database name you have selected is already in use by this server.".into()),
        ),
    );
    v.rules("remote", &["required", "string", r"regex:/^[\w\-\/.%:]+$/"]);
    v.validate()?;
    let name = input.get("database").map(pelican_core::php::string).unwrap_or_default();
    let remote = input.get("remote").map(pelican_core::php::string).unwrap_or_default();

    let txn = st.db.begin().await?;
    lock_server(&txn, backend, server.id).await?;

    // DeployServerDatabaseService::handle()
    let no_host = || {
        ApiError::display(
            "NoSuitableDatabaseHostException",
            400,
            "No database host was found that meets the requirements for this server.",
        )
    };
    let hosts = database_hosts::Entity::find().all(&txn).await?;
    if hosts.is_empty() {
        return Err(no_host());
    }
    let node_hosts: Vec<database_hosts::Model> = {
        let rows = txn
            .query_all_raw(raw::stmt(
                backend,
                "select {database_hosts}.* from {database_hosts} inner join {database_host_node} on {database_hosts.id} = {database_host_node.database_host_id} where {database_host_node.node_id} = ?",
                vec![server.node_id.into()],
            ))
            .await?;
        rows.iter().map(|r| database_hosts::Model::from_query_result(r, "")).collect::<Result<_, _>>()?
    };
    if node_hosts.is_empty() && !st.config.client_databases_allow_random {
        return Err(no_host());
    }
    let pool = if node_hosts.is_empty() { &hosts } else { &node_hosts };
    let host = pool.choose(&mut rand::rng()).cloned().ok_or_else(no_host)?;

    let mut created =
        create_database_in(&st, &ctx, &txn, &server, &host, &unique_name(&name, server.id.0), &remote).await?;
    txn.commit().await?;

    // The response uses the in-memory model: `max_connections` was never set (the column default is 0).
    created.max_connections = None;
    let includes = Includes::default().with(&["password"]);
    Ok(PhpJson::ok(database_item(&st, &ctx, &created, &host, &includes).await?).into_response())
}

/// `POST /databases/{database}/rotate-password`.
pub async fn rotate_password(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "database.update").await?;
    let db = ctx.bound.database.clone().ok_or_else(ApiError::model_not_found)?;
    let host = host_of(&st.db, db.database_host_id).await?;
    let txn = st.db.begin().await?;
    rotate_password_in(&st, &txn, &db).await?;
    ctx.activity("server:database.rotate-password")
        .subject(morph::DATABASE, db.id.0)
        .property("name", db.database.clone())
        .log_in(&st, &txn)
        .await?;
    txn.commit().await?;

    let fresh = databases::Entity::find_by_id(db.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    let includes = Includes::default().with(&["password"]);
    Ok(PhpJson::ok(database_item(&st, &ctx, &fresh, &host, &includes).await?).into_response())
}

/// `DELETE /databases/{database}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "database.delete").await?;
    let db = ctx.bound.database.clone().ok_or_else(ApiError::model_not_found)?;
    let txn = st.db.begin().await?;
    delete_database_in(&st, &ctx, &txn, &db).await?;
    txn.commit().await?;
    Ok(no_content())
}

/// `DatabaseManagementService::create()` on the caller's transaction: feature
/// flag, limit, duplicate name, insert, DDL on the host, activity.
pub async fn create_database_in<C: ConnectionTrait>(
    st: &AppState,
    ctx: &Ctx,
    txn: &C,
    server: &servers::Model,
    host: &database_hosts::Model,
    database: &str,
    remote: &str,
) -> ApiResult<databases::Model> {
    if !st.config.client_databases_enabled {
        return Err(ApiError::internal("DatabaseClientFeatureNotEnabledException"));
    }
    if let Some(limit) = server.database_limit {
        let count = databases::Entity::find().filter(databases::Column::ServerId.eq(server.id)).all(txn).await?.len();
        if count as i64 >= limit.0 {
            return Err(ApiError::display(
                "TooManyDatabasesException",
                400,
                "Operation aborted: creating a new database would put this server over the defined limit.",
            ));
        }
    }
    let exists = databases::Entity::find()
        .filter(databases::Column::ServerId.eq(server.id))
        .filter(databases::Column::Database.eq(database.to_string()))
        .one(txn)
        .await?
        .is_some();
    if exists {
        return Err(ApiError::display(
            "DuplicateDatabaseNameException",
            400,
            "A database with that name already exists for this server.",
        ));
    }
    let password = random_password(24);
    let now = Timestamp(pelican_core::time::now());
    let am = databases::ActiveModel {
        database_host_id: Set(host.id),
        database: Set(database.to_string()),
        remote: Set(remote.to_string()),
        server_id: Set(server.id),
        username: Set(format!("u{}_{}", server.id.0, str_random(10))),
        password: Set(st.enc.encrypt_string(&password)),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let created = pelican_db::write::insert(txn, am).await?;
    let conn = HostConnection::open(host, &st.enc).await.map_err(host_err)?;
    let ddl = async {
        conn.create_database(&created).await?;
        conn.create_user(&created, &password).await?;
        conn.assign_user(&created).await?;
        conn.flush_privileges().await
    }
    .await;
    conn.close().await;
    ddl.map_err(host_err)?;
    ctx.activity("server:database.create")
        .subject(morph::DATABASE, created.id.0)
        .property("name", database.to_string())
        .log_in(st, txn)
        .await?;
    Ok(created)
}

/// `DatabaseManagementService::create()` in its own transaction.
pub async fn create_database(
    st: &AppState,
    ctx: &Ctx,
    server: &servers::Model,
    host: &database_hosts::Model,
    database: &str,
    remote: &str,
    _client: bool,
) -> ApiResult<databases::Model> {
    let txn = st.db.begin().await?;
    let created = create_database_in(st, ctx, &txn, server, host, database, remote).await?;
    txn.commit().await?;
    Ok(created)
}

/// `DatabaseManagementService::rotatePassword()` (no activity).
pub async fn rotate_password_in<C: ConnectionTrait>(st: &AppState, txn: &C, db: &databases::Model) -> ApiResult<()> {
    let host = host_of(txn, db.database_host_id).await?;
    let password = random_password(24);
    let am = databases::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(db.id),
        password: Set(st.enc.encrypt_string(&password)),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    pelican_db::write::update(txn, &am).await?;
    let conn = HostConnection::open(&host, &st.enc).await.map_err(host_err)?;
    let ddl = async {
        conn.drop_user(db).await?;
        conn.create_user(db, &password).await?;
        conn.assign_user(db).await?;
        conn.flush_privileges().await
    }
    .await;
    conn.close().await;
    ddl.map_err(host_err)
}

/// `DatabaseManagementService::delete()`: DDL, activity, row.
pub async fn delete_database_in<C: ConnectionTrait>(
    st: &AppState,
    ctx: &Ctx,
    txn: &C,
    db: &databases::Model,
) -> ApiResult<()> {
    let host = host_of(txn, db.database_host_id).await?;
    let conn = HostConnection::open(&host, &st.enc).await.map_err(host_err)?;
    let ddl = async {
        conn.drop_database(db).await?;
        conn.drop_user(db).await?;
        conn.flush_privileges().await
    }
    .await;
    conn.close().await;
    ddl.map_err(host_err)?;
    ctx.activity("server:database.delete")
        .subject(morph::DATABASE, db.id.0)
        .property("name", db.database.clone())
        .log_in(st, txn)
        .await?;
    databases::Entity::delete_by_id(db.id).exec(txn).await?;
    Ok(())
}
