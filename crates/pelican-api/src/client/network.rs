//! `NetworkAllocationController` — client API `/servers/{server}/network/allocations*`
//! (routes/api-client.php; app/Http/Controllers/Api/Client/Servers/NetworkAllocationController.php).

use std::net::Ipv6Addr;

use axum::Extension;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_core::time;
use pelican_db::Timestamp;
use pelican_db::entities::{allocations, servers};
use pelican_db::raw;
use sea_orm::{ActiveValue::Set, ConnectionTrait, EntityTrait, FromQueryResult, TransactionTrait, Value as DbValue};
use serde_json::Value;

use super::data::allocation_data;
use crate::envelope::list;
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::services::LogActivity;
use crate::validation::Validator;

/// `Allocation::address`: alias (ip_alias else ip), bracketed when IPv6, + `:port`.
fn alloc_address(a: &allocations::Model) -> String {
    let alias = a.ip_alias.clone().unwrap_or_else(|| a.ip.clone());
    let addr = if alias.parse::<Ipv6Addr>().is_ok() { format!("[{alias}]") } else { alias };
    format!("{addr}:{}", a.port.0)
}

fn ts_now() -> Timestamp {
    Timestamp(time::now())
}

fn display_ex(code: &str, msg: &str) -> ApiError {
    // DisplayException subclasses report their own class basename as the
    // error `code` (Handler::exceptionToArray class_basename).
    ApiError::display(code, 400, msg)
}

fn no_space() -> ApiError {
    display_ex(
        "NoAutoAllocationSpaceAvailableException",
        "Cannot assign additional allocation: no more space available on node.",
    )
}

/// The plain `hasMany` query of `Server::allocations()`, no ORDER BY (ADR-0011).
async fn server_allocations<C: ConnectionTrait>(db: &C, server_id: i64) -> ApiResult<Vec<allocations::Model>> {
    Ok(allocations::Model::find_by_statement(raw::stmt(
        db.get_database_backend(),
        "select * from {allocations} where {allocations.server_id} = ?",
        vec![DbValue::from(server_id)],
    ))
    .all(db)
    .await?)
}

/// Server's allocations on one node excluding one id (`keepOne` check).
async fn other_node_allocations<C: ConnectionTrait>(
    db: &C,
    server_id: i64,
    node_id: i64,
    exclude_id: i64,
) -> ApiResult<Vec<allocations::Model>> {
    Ok(allocations::Model::find_by_statement(raw::stmt(
        db.get_database_backend(),
        "select * from {allocations} where {allocations.server_id} = ? and {allocations.node_id} = ? and {allocations.id} <> ?",
        vec![DbValue::from(server_id), DbValue::from(node_id), DbValue::from(exclude_id)],
    ))
    .all(db)
    .await?)
}

/// Stand-in for SQL `inRandomOrder()` / PHP `array_rand`: uniform pick.
fn rand_pick<T>(v: &[T]) -> Option<&T> {
    use rand::seq::IndexedRandom;
    v.choose(&mut rand::rng())
}

/// Webmozart `Assert::integerish` over the env string (is_numeric + integral);
/// failure bubbles up as an uncaught `InvalidArgumentException` → generic 500.
fn integerish(v: Option<&str>) -> ApiResult<i64> {
    let bad = || ApiError::internal("InvalidArgumentException");
    let Some(s) = v else { return Err(bad()) };
    if !pelican_core::php::is_numeric_str(s) {
        return Err(bad());
    }
    match s.trim().parse::<i64>() {
        Ok(n) => Ok(n),
        Err(_) => Err(bad()),
    }
}

/// GET /servers/{server}/network/allocations (`index`, `GetNetworkRequest`).
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "allocation.read").await?;
    let server = ctx.bound.server()?.clone();
    let rows = server_allocations(&st.db, server.id.0).await?;
    let data: Vec<Value> = rows.iter().map(|a| allocation_data(a, server.allocation_id.map(|x| x.0), false)).collect();
    Ok(PhpJson::ok(list(data)).into_response())
}

/// POST .../network/allocations (`store`, `NewAllocationRequest`, no body):
/// allocation-limit guard, then FindAssignableAllocationService inside the
/// `server:allocation.create` activity transaction.
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "allocation.create").await?;
    let server = ctx.bound.server()?.clone();

    let txn = st.db.begin().await?;
    let mine = server_allocations(&txn, server.id.0).await?;
    // PHP `count >= allocation_limit`: with no limit set `n >= null` is always
    // true, so every create on an unlimited server fails here.
    let reached = match server.allocation_limit {
        None => true,
        Some(l) => mine.len() as i64 >= l.0,
    };
    if reached {
        return Err(display_ex(
            "DisplayException",
            "Cannot assign additional allocations to this server: limit has been reached.",
        ));
    }

    // FindAssignableAllocationService::handle
    let enabled = st.config.env.bool_or("PANEL_CLIENT_ALLOCATIONS_ENABLED", false);
    if !enabled {
        return Err(display_ex(
            "AutoAllocationNotEnabledException",
            "Server auto-allocation is not enabled for this instance.",
        ));
    }
    let Some(pid) = server.allocation_id else {
        return Err(display_ex("DisplayException", "A primary allocation is required before adding more"));
    };
    let Some(primary) = allocations::Entity::find_by_id(pid).one(&txn).await? else {
        return Err(display_ex("DisplayException", "A primary allocation is required before adding more"));
    };

    let create_new = st.config.env.bool_or("PANEL_CLIENT_ALLOCATIONS_CREATE_NEW", true);
    // Assert::integerish runs before any allocation search.
    let start = integerish(st.config.env.string("PANEL_CLIENT_ALLOCATIONS_RANGE_START").as_deref())?;
    let end = integerish(st.config.env.string("PANEL_CLIENT_ALLOCATIONS_RANGE_END").as_deref())?;

    let backend = txn.get_database_backend();
    let cands = allocations::Model::find_by_statement(raw::stmt(
        backend,
        "select * from {allocations} where {allocations.node_id} = ? and {allocations.ip} = ? \
         and {allocations.port} between ? and ? and {allocations.server_id} is null",
        vec![
            DbValue::from(server.node_id.0),
            DbValue::from(primary.ip.clone()),
            DbValue::from(start),
            DbValue::from(end),
        ],
    ))
    .all(&txn)
    .await?;

    let chosen = match rand_pick(&cands) {
        Some(a) => a.clone(),
        None => {
            if !create_new {
                return Err(no_space());
            }
            create_new_allocation(&txn, &server, &primary, start, end).await?
        }
    };

    // `$allocation->update(['server_id' => $server->id])` — dirty columns plus
    // the automatic updated_at bump.
    pelican_db::write::update(
        &txn,
        &allocations::ActiveModel {
            id: Set(chosen.id),
            server_id: Set(Some(server.id)),
            updated_at: Set(Some(ts_now())),
            ..Default::default()
        },
    )
    .await?;
    let fresh = allocations::Entity::find_by_id(chosen.id)
        .one(&txn)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;

    ctx.activity("server:allocation.create")
        .subject(morph::ALLOCATION, fresh.id.0)
        .property("allocation", alloc_address(&fresh))
        .log_in(&st, &txn)
        .await?;
    txn.commit().await?;

    Ok(PhpJson::ok(allocation_data(&fresh, server.allocation_id.map(|x| x.0), false)).into_response())
}

/// `FindAssignableAllocationService::createNewAllocation` for one numeric port:
/// pick a free port in the range and insert an unassigned allocation through
/// AssignmentService semantics, then hand the row back for assignment.
async fn create_new_allocation<C: ConnectionTrait>(
    txn: &C,
    server: &servers::Model,
    primary: &allocations::Model,
    start: i64,
    end: i64,
) -> ApiResult<allocations::Model> {
    if start == 0 || end == 0 {
        return Err(no_space());
    }
    let backend = txn.get_database_backend();
    let taken = allocations::Model::find_by_statement(raw::stmt(
        backend,
        "select * from {allocations} where {allocations.node_id} = ? and {allocations.ip} = ? \
         and {allocations.port} between ? and ?",
        vec![
            DbValue::from(server.node_id.0),
            DbValue::from(primary.ip.clone()),
            DbValue::from(start),
            DbValue::from(end),
        ],
    ))
    .all(txn)
    .await?;
    let ports: Vec<i64> = taken.iter().map(|a| a.port.0).collect();
    let available: Vec<i64> = (start..=end).filter(|p| !ports.contains(p)).collect();
    let port = match rand_pick(&available) {
        Some(p) => *p,
        None => return Err(no_space()),
    };
    if !(1024..=65535).contains(&port) {
        return Err(display_ex(
            "PortOutOfRangeException",
            "Ports in an allocation must be greater than or equal to 1024 and less than or equal to 65535.",
        ));
    }
    let now = ts_now();
    pelican_db::write::insert_id(
        txn,
        &allocations::ActiveModel {
            node_id: Set(server.node_id),
            ip: Set(primary.ip.clone()),
            port: Set(pelican_db::Int(port)),
            server_id: Set(None),
            ip_alias: Set(None),
            is_locked: Set(pelican_db::Bool(false)),
            show_port: Set(pelican_db::Bool(true)),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    let found = allocations::Model::find_by_statement(raw::stmt(
        backend,
        "select * from {allocations} where {allocations.node_id} = ? and {allocations.ip} = ? and {allocations.port} = ?",
        vec![DbValue::from(server.node_id.0), DbValue::from(primary.ip.clone()), DbValue::from(port)],
    ))
    .all(txn)
    .await?;
    found.into_iter().next().ok_or_else(ApiError::model_not_found)
}

/// POST .../network/allocations/{allocation} (`update`, `UpdateAllocationRequest`):
/// notes rules are Allocation's own plus `present`; saving a clean model writes
/// nothing, and the activity event fires only when the value changed.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "allocation.update").await?;
    let mut v = Validator::new(&input);
    v.rules("notes", &["nullable", "string", "max:256", "present"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let alloc = ctx.bound.allocation.as_ref().ok_or_else(ApiError::model_not_found)?.clone();

    let original = alloc.notes.clone();
    // ConvertEmptyStringsToNull already turned "" into null before validation;
    // anything non-string was rejected by the `string` rule above.
    let new_notes = match input.get("notes") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    // The controller responds with the in-memory model after forceFill+save,
    // i.e. the fresh value even when the DB row was not touched (clean model).
    let mut alloc = alloc;
    if new_notes != original {
        alloc.notes = new_notes.clone();
        pelican_db::write::update(
            &st.db,
            &allocations::ActiveModel {
                id: Set(alloc.id),
                notes: Set(new_notes.clone()),
                updated_at: Set(Some(ts_now())),
                ..Default::default()
            },
        )
        .await?;
        ctx.activity("server:allocation.notes")
            .subject(morph::ALLOCATION, alloc.id.0)
            .property("allocation", alloc_address(&alloc))
            .property("old", original.map(Value::String).unwrap_or(Value::Null))
            .property("new", new_notes.map(Value::String).unwrap_or(Value::Null))
            .log(&st)
            .await?;
    }
    // AllocationData re-loads the `server` relation lazily, so is_default is
    // computed against current DB state.
    let fs = servers::Entity::find_by_id(server.id).one(&st.db).await?;
    let sid = fs.as_ref().and_then(|s| s.allocation_id).map(|x| x.0);
    Ok(PhpJson::ok(allocation_data(&alloc, sid, false)).into_response())
}

/// POST .../network/allocations/{allocation}/primary (`setPrimary`,
/// SetPrimaryAllocationRequest — extends UpdateAllocationRequest without rules).
pub async fn set_primary(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "allocation.update").await?;
    let server = ctx.bound.server()?.clone();
    let alloc = ctx.bound.allocation.as_ref().ok_or_else(ApiError::model_not_found)?.clone();

    // associate()+save(): clean models perform no write at all.
    if server.allocation_id != Some(alloc.id) {
        pelican_db::write::update(
            &st.db,
            &servers::ActiveModel {
                id: Set(server.id),
                allocation_id: Set(Some(alloc.id)),
                updated_at: Set(Some(ts_now())),
                ..Default::default()
            },
        )
        .await?;
        crate::services::server_saved(&st, &ctx, &st.db, &server).await?;
    }
    ctx.activity("server:allocation.primary")
        .subject(morph::ALLOCATION, alloc.id.0)
        .property("allocation", alloc_address(&alloc))
        .log(&st)
        .await?;
    Ok(PhpJson::ok(allocation_data(&alloc, Some(alloc.id.0), false)).into_response())
}

/// DELETE .../network/allocations/{allocation} (`delete`, DeleteAllocationRequest)
/// through Server::releaseAllocations([id], keepOne: true).
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "allocation.delete").await?;
    let server = ctx.bound.server()?.clone();
    let alloc = ctx.bound.allocation.as_ref().ok_or_else(ApiError::model_not_found)?.clone();

    // PHP empty(): unset (null) and zero limits both block deletion.
    if !matches!(server.allocation_limit, Some(l) if l.0 != 0) {
        return Err(display_ex(
            "DisplayException",
            "You cannot delete allocations for this server: no allocation limit is set.",
        ));
    }

    let txn = st.db.begin().await?;
    // releaseAllocations re-reads the locked primary pointer before releasing.
    let current = servers::Entity::find_by_id(server.id)
        .one(&txn)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?
        .allocation_id
        .map(|x| x.0);

    if other_node_allocations(&txn, server.id.0, server.node_id.0, alloc.id.0).await?.is_empty() {
        return Err(display_ex("DisplayException", "You cannot delete the last allocation for this server."));
    }

    // RELEASE_ATTRIBUTES via query-builder update (Eloquent Builder adds
    // updated_at even there).
    txn.execute_raw(raw::stmt(
        txn.get_database_backend(),
        // Postgres forbids table-qualified column names in the SET list.
        "update {allocations} set server_id = null, notes = null, is_locked = ?, show_port = ?, updated_at = ? \
         where {allocations.server_id} = ? and {allocations.id} = ?",
        vec![
            DbValue::Bool(Some(false)),
            DbValue::Bool(Some(true)),
            DbValue::ChronoDateTime(Some(time::now())),
            DbValue::from(server.id.0),
            DbValue::from(alloc.id.0),
        ],
    ))
    .await?;

    if current.is_none() || current == Some(alloc.id.0) {
        let rows = allocations::Model::find_by_statement(raw::stmt(
            txn.get_database_backend(),
            "select * from {allocations} where {allocations.server_id} = ? and {allocations.node_id} = ? \
             order by {allocations.id} asc limit 1",
            vec![DbValue::from(server.id.0), DbValue::from(server.node_id.0)],
        ))
        .all(&txn)
        .await?;
        let newp = rows.into_iter().next();
        let new_id = newp.as_ref().map(|a| a.id.0);
        // associate()+save() dirtiness against the just-locked pointer.
        if new_id != current {
            pelican_db::write::update(
                &txn,
                &servers::ActiveModel {
                    id: Set(server.id),
                    allocation_id: Set(new_id.map(pelican_db::Int)),
                    updated_at: Set(Some(ts_now())),
                    ..Default::default()
                },
            )
            .await?;
            crate::services::server_saved(&st, &ctx, &txn, &server).await?;
        }
        if let Some(p) = newp {
            ctx.activity("server:allocation.primary")
                .subject(morph::ALLOCATION, p.id.0)
                .property("allocation", alloc_address(&p))
                .log_in(&st, &txn)
                .await?;
        }
    }
    txn.commit().await?;

    ctx.activity("server:allocation.delete")
        .subject(morph::ALLOCATION, alloc.id.0)
        .property("allocation", alloc_address(&alloc))
        .log(&st)
        .await?;
    Ok(no_content())
}
