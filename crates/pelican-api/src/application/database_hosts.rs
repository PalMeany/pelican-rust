//! `/api/application/database-hosts` — `DatabaseHostController`,
//! `HostCreationService` / `HostUpdateService` (spec §3.5).

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::databases::{HostConnection, HostError};
use pelican_core::state::AppState;
use pelican_db::entities::{database_hosts, nodes};
use pelican_db::{Int, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, FromQueryResult, TransactionTrait,
};
use serde_json::{Value, json};

use super::data::database_host_item;
use super::{Page, READ, WRITE, resource_meta};
use crate::envelope::{Includes, with_meta};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query::{filters, partial, sorts};
use crate::validation::{CustomRule, Validator};

/// A failed connection test rethrows the connector's raw PDOException
/// unwrapped (`Connector::createConnection`,
/// `ConnectionFactory::createPdoResolverWithHosts`); only statements run over
/// an open connection get wrapped into `QueryException`.
fn conn_err(e: HostError) -> ApiError {
    match e {
        HostError::Decrypt => ApiError::internal("DecryptException"),
        HostError::Query(_) => ApiError::internal("PDOException"),
    }
}

fn bound_host(ctx: &Ctx) -> ApiResult<database_hosts::Model> {
    ctx.bound.database_host.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /database-hosts`.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("database_host", READ)?;
    let b = st.db.get_database_backend();
    let fs = filters(&input, &["name", "host"])?;
    let ss = sorts(&input, &["id", "name", "host"])?;
    let page = Page::new(&input, 10)?;
    let includes = Includes::parse(&input);
    let mut wh = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    for name in ["name", "host"] {
        if let Some((_, v)) = fs.iter().find(|(n, _)| n == name) {
            if let Some((sql, mut values)) = partial(b, &format!("database_hosts.{name}"), v) {
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
            &format!("select count(*) as {{aggregate}} from {{database_hosts}}{where_sql}"),
            vals.clone(),
        ))
        .await?;
    let total = total_row.and_then(|r| r.try_get::<Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0);
    let mut data = Vec::new();
    if total > 0 {
        let rows = st
            .db
            .query_all_raw(raw::stmt(
                b,
                &format!("select * from {{database_hosts}}{where_sql}{order}{}", page.sql()),
                vals,
            ))
            .await?;
        for r in rows {
            let h = database_hosts::Model::from_query_result(&r, "")?;
            data.push(database_host_item(&st, &ctx, &h, &includes).await?);
        }
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// `GET /database-hosts/{database_host}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("database_host", READ)?;
    let h = bound_host(&ctx)?;
    Ok(PhpJson::ok(database_host_item(&st, &ctx, &h, &Includes::parse(&input)).await?).into_response())
}

/// `DatabaseHost::$validationRules`, field order of `$validationRules`.
async fn validate_host(st: &AppState, input: &Input) -> ApiResult<()> {
    let mut v = Validator::new(input);
    v.rules("name", &["required", "string", "max:255"]);
    v.rules("host", &["required", "string"]);
    v.rules("port", &["required", "numeric", "between:1,65535"]);
    v.rules("username", &["required", "string", "max:32"]);
    v.rules("password", &["nullable", "string"]);
    v.rules("node_ids", &["nullable", "array"]);
    v.rules("node_ids.*", &["required", "integer"]);
    // exists:nodes,id per element (after the string rules; skipped when a
    // presence rule already failed — hasNotFailedPreviousRuleIfPresenceRule).
    for path in crate::validation::expand_wildcard(&input.data, "node_ids.*") {
        let val = input.get(&path).cloned().unwrap_or(Value::Null);
        let exists = match &val {
            Value::Null => true,
            other => match el_to_id(other) {
                Some(id) => nodes::Entity::find_by_id(Int(id)).one(&st.db).await?.is_some(),
                None => false,
            },
        };
        v.custom(
            &path,
            CustomRule::precomputed("exists", (!exists).then(|| "The selected :attribute is invalid.".into())),
        );
    }
    v.validate()
}

/// An element as PHP sees it when it becomes an attach id (array-key casting:
/// bools become ints, numbers truncate toward zero).
fn el_to_id(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_f64().map(|f| f.trunc() as i64),
        Value::Object(_) | Value::Array(_) => None,
        other => Some(pelican_core::php::intval(&pelican_core::php::string(other))),
    }
}

const FIELDS: [&str; 6] = ["name", "host", "port", "username", "password", "node_ids"];

/// `$request->validated()`: present keys that have rules.
fn validated(input: &Input) -> Vec<(&'static str, Value)> {
    FIELDS.iter().filter_map(|k| input.get(k).map(|v| (*k, v.clone()))).collect()
}

fn field<'a>(data: &'a [(&'static str, Value)], k: &str) -> Option<&'a Value> {
    data.iter().find(|(n, _)| *n == k).map(|(_, v)| v)
}

fn str_field(data: &[(&'static str, Value)], k: &str) -> String {
    field(data, k).map(pelican_core::php::string).unwrap_or_default()
}

/// PHP `(int)` cast of the validated numeric port.
fn int_field(data: &[(&'static str, Value)], k: &str) -> i64 {
    match field(data, k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0).trunc() as i64,
        _ => pelican_core::php::intval(&str_field(data, k)),
    }
}

/// `BelongsToMany::sync`: object keys are dropped, duplicates collapse onto the
/// first occurrence, bools cast like PHP array keys.
fn requested_node_ids(input: &Input) -> Vec<i64> {
    let vals: Vec<Value> = match input.get("node_ids") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<i64> = Vec::new();
    for v in vals {
        if let Some(id) = el_to_id(&v) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// INSERT the row exactly as Eloquent would: password bound as given (NULL
/// included — the column is NOT NULL, so an absent password fails the insert
/// under strict mode like the original's statement does).
async fn insert_host(
    txn: &DatabaseTransaction,
    b: DbBackend,
    pw_encrypted: Option<String>,
    data: &[(&'static str, Value)],
    ts: Timestamp,
) -> ApiResult<Int> {
    let sql = "insert into {database_hosts} ({password}, {name}, {host}, {port}, {username}, \
               {max_databases}, {created_at}, {updated_at}) values (?, ?, ?, ?, ?, ?, ?, ?)";
    let pw_v: sea_orm::Value = pw_encrypted.into();
    let vals: Vec<sea_orm::Value> = vec![
        pw_v,
        str_field(data, "name").into(),
        str_field(data, "host").into(),
        int_field(data, "port").into(),
        str_field(data, "username").into(),
        None::<Int>.into(),
        ts.into(),
        ts.into(),
    ];
    match b {
        DbBackend::Postgres => {
            let row = txn.query_one_raw(raw::stmt(b, &format!("{sql} returning {{id}}"), vals)).await?;
            row.and_then(|r| r.try_get::<Int>("", "id").ok()).ok_or_else(|| ApiError::internal("QueryException"))
        }
        _ => {
            let res = txn.execute_raw(raw::stmt(b, sql, vals)).await?;
            Ok(Int(res.last_insert_id() as i64))
        }
    }
}

/// `$host->nodes()->sync($ids)` for a freshly created host: attach every id.
/// The relation declares no pivot columns, so no timestamps are written.
pub async fn sync_nodes(db: &impl ConnectionTrait, b: DbBackend, host_id: Int, ids: &[i64]) -> ApiResult<()> {
    let rows = db
        .query_all_raw(raw::stmt(
            b,
            "select {node_id} from {database_host_node} where {database_host_id} = ?",
            vec![host_id.into()],
        ))
        .await?;
    let current: Vec<i64> = rows.iter().filter_map(|r| r.try_get::<Int>("", "node_id").ok().map(|v| v.0)).collect();
    for c in &current {
        if !ids.contains(c) {
            db.execute_raw(raw::stmt(
                b,
                "delete from {database_host_node} where {database_host_id} = ? and {node_id} = ?",
                vec![host_id.into(), Int(*c).into()],
            ))
            .await?;
        }
    }
    for id in ids {
        if !current.contains(id) {
            db.execute_raw(raw::stmt(
                b,
                "insert into {database_host_node} ({node_id}, {database_host_id}) values (?, ?)",
                vec![Int(*id).into(), host_id.into()],
            ))
            .await?;
        }
    }
    Ok(())
}

/// The credentials the connection test must use: Laravel encrypts on set and
/// decrypts through the cast on get, so re-encrypting the effective plaintext
/// round-trips to exactly what `buildConnection()` would use.
fn probe_model(host: &database_hosts::Model, st: &AppState, pw_plain: Option<&str>) -> database_hosts::Model {
    let mut m = host.clone();
    if let Some(p) = pw_plain {
        m.password = st.enc.encrypt_string(p);
    }
    m
}

/// `buildConnection()->getPdo()` against database `mysql`.
async fn test_connection(st: &AppState, probe: &database_hosts::Model) -> ApiResult<()> {
    let conn = HostConnection::open(probe, &st.enc).await.map_err(conn_err)?;
    conn.close().await;
    Ok(())
}

/// `POST /database-hosts` (`HostCreationService`).
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("database_host", WRITE)?;
    validate_host(&st, &input).await?;
    let data = validated(&input);
    // Any present non-null string goes through the encrypted cast ("0"/" "
    // included); an absent or null password reaches the DB as NULL.
    let pw_plain: Option<String> = field(&data, "password").and_then(|v| match v {
        Value::Null => None,
        other => Some(pelican_core::php::string(other)),
    });
    let now = Timestamp(pelican_core::time::now());
    let b = st.db.get_database_backend();
    let txn = st.db.begin().await?;
    let id = insert_host(&txn, b, pw_plain.as_ref().map(|p| st.enc.encrypt_string(p)), &data, now).await?;
    sync_nodes(&txn, b, id, &requested_node_ids(&input)).await?;
    // Confirm access using the provided credentials before saving; rollback on
    // failure happens because the transaction is left uncommitted.
    let candidate = database_hosts::Model {
        id,
        name: str_field(&data, "name"),
        host: str_field(&data, "host"),
        port: Int(int_field(&data, "port")),
        username: str_field(&data, "username"),
        password: String::new(),
        max_databases: None,
        created_at: Some(now),
        updated_at: Some(now),
    };
    test_connection(&st, &probe_model(&candidate, &st, pw_plain.as_deref())).await?;
    // The response reflects the stored row (the encrypted password is never exposed).
    let host = database_hosts::Entity::find_by_id(id).one(&txn).await?.ok_or_else(ApiError::model_not_found)?;
    txn.commit().await?;
    // HostCreationService: DatabaseHost::create() (eloquent.created); pivot
    // database_host_node attach/detach fires nothing.
    crate::model_events::created(&st, &host).await;
    let body = with_meta(
        database_host_item(&st, &ctx, &host, &Includes::parse(&input)).await?,
        resource_meta(&st, &format!("database-hosts/{}", id.0)),
    );
    Ok(PhpJson::with_status(StatusCode::CREATED, body).into_response())
}

/// `PATCH /database-hosts/{database_host}` (`HostUpdateService`): an empty
/// password is ignored, `node_ids` is not fillable, and the connection test
/// always runs — even when nothing was dirty.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("database_host", WRITE)?;
    let h = bound_host(&ctx)?;
    validate_host(&st, &input).await?;
    let data = validated(&input);
    // An `empty()` provided password keeps the stored one.
    let pw_new: Option<String> = field(&data, "password")
        .map(pelican_core::php::string)
        .filter(|p| !pelican_core::php::empty(&json!(p.clone())));
    let mut m = h.clone();
    let mut am = database_hosts::ActiveModel { id: sea_orm::ActiveValue::Unchanged(h.id), ..Default::default() };
    let mut dirty = false;
    if let Some(n) = field(&data, "name").map(pelican_core::php::string) {
        if n != h.name {
            am.name = Set(n.clone());
            m.name = n;
            dirty = true;
        }
    }
    if let Some(hs) = field(&data, "host").map(pelican_core::php::string) {
        if hs != h.host {
            am.host = Set(hs.clone());
            m.host = hs;
            dirty = true;
        }
    }
    let port = int_field(&data, "port");
    if port != h.port.0 {
        am.port = Set(Int(port));
        m.port = Int(port);
        dirty = true;
    }
    if let Some(u) = field(&data, "username").map(pelican_core::php::string) {
        if u != h.username {
            am.username = Set(u.clone());
            m.username = u;
            dirty = true;
        }
    }
    // A freshly encrypted ciphertext never equals the stored one, so a
    // non-empty provided password always counts as dirty.
    if let Some(p) = pw_new.clone() {
        let cipher = st.enc.encrypt_string(&p);
        am.password = Set(cipher.clone());
        m.password = cipher;
        dirty = true;
    }
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    if dirty {
        am.updated_at = Set(Some(now));
        m.updated_at = Some(now);
        pelican_db::write::update(&txn, &am).await?;
    }
    test_connection(&st, &probe_model(&m, &st, pw_new.as_deref())).await?;
    txn.commit().await?;
    if dirty {
        // HostUpdateService: $host->save() only when isDirty (eloquent.updated)
        crate::model_events::updated(&st, &m).await;
    }
    Ok(PhpJson::ok(database_host_item(&st, &ctx, &m, &Includes::parse(&input)).await?).into_response())
}

/// `DELETE /database-hosts/{database_host}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("database_host", WRITE)?;
    let h = bound_host(&ctx)?;
    database_hosts::Entity::delete_by_id(h.id).exec(&st.db).await?;
    // $host->delete() (eloquent.deleted, attributes as before the delete)
    crate::model_events::deleted(&st, &h).await;
    Ok(no_content())
}
