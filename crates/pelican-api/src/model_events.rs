//! Eloquent model events (`eloquent.created|updated|deleted: App\Models\X`, ADR-0019):
//! `DispatchWebhooks::handleEloquentEvent()` for global webhooks and `on-event` for
//! WASM plugins. Call [`created`]/[`updated`]/[`deleted`] (or [`fire_id`]) after a
//! model-instance write the original performs with `create()/save()/update()/delete()`
//! (mass `query()->update()` fires nothing). Fire after the transaction commits: webhook
//! delivery is a synchronous HTTP call.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use pelican_core::state::AppState;
use pelican_db::Driver;
use pelican_db::entities::webhook_configurations;
use pelican_db::entities::*;
use pelican_db::types::Int;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PrimaryKeyTrait, QueryFilter};
use serde_json::{Map, Value, json};

use crate::error::ApiResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Deleted,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Created => "created",
            Action::Updated => "updated",
            Action::Deleted => "deleted",
        }
    }
}

/// `eloquent.<action>: App\Models\<Model>`.
pub fn event_name(action: Action, model: &str) -> String {
    format!("eloquent.{}: App\\Models\\{model}", action.as_str())
}

/// An `App\Models\*` model with its `toArray()` ([`crate::model_array`]).
pub trait Eloquent {
    const CLASS: &'static str;
    const TABLE: &'static str;
    /// Attributes in the entity's field order.
    fn to_array(&self) -> Value;
    /// `toArray()` as the original emits it: the backend's physical column order
    /// (`SELECT *`; migrations append columns differently per driver).
    fn array(&self, driver: Driver) -> Value {
        physical_order(driver, Self::TABLE, self.to_array())
    }
}

fn physical_order(driver: Driver, table: &str, data: Value) -> Value {
    let Some(cols) = pelican_db::column_order::columns(driver, table) else {
        return data;
    };
    match data {
        Value::Object(mut attrs) => {
            let mut out = Map::new();
            for c in cols {
                if let Some(v) = attrs.shift_remove(*c) {
                    out.insert((*c).to_string(), v);
                }
            }
            out.extend(attrs);
            Value::Object(out)
        }
        other => other,
    }
}

macro_rules! eloquent {
    ($($m:ident => $class:literal, $f:ident;)*) => {$(
        impl Eloquent for $m::Model {
            const CLASS: &'static str = $class;
            const TABLE: &'static str = stringify!($m);
            fn to_array(&self) -> Value {
                crate::model_array::$f(self)
            }
        }
    )*};
}

eloquent! {
    activity_logs => "ActivityLog", activity_log;
    activity_log_subjects => "ActivityLogSubject", activity_log_subject;
    allocations => "Allocation", allocation;
    api_keys => "ApiKey", api_key;
    backups => "Backup", backup;
    backup_hosts => "BackupHost", backup_host;
    databases => "Database", database;
    database_hosts => "DatabaseHost", database_host;
    eggs => "Egg", egg;
    egg_variables => "EggVariable", egg_variable;
    mounts => "Mount", mount;
    nodes => "Node", node;
    roles => "Role", role;
    schedules => "Schedule", schedule;
    servers => "Server", server;
    server_transfers => "ServerTransfer", server_transfer;
    server_user_settings => "ServerUserSettings", server_user_settings;
    server_variables => "ServerVariable", server_variable;
    subusers => "Subuser", subuser;
    tasks => "Task", task;
    users => "User", user;
    user_ssh_keys => "UserSSHKey", user_ssh_key;
    webhooks => "Webhook", webhook;
    webhook_configurations => "WebhookConfiguration", webhook_configuration;
}

/// `WebhookConfiguration::$eventBlacklist`.
const BLACKLIST: &[&str] = &["eloquent.created: App\\Models\\Webhook"];

/// Global webhooks by event, refreshed every few seconds (the original caches
/// `watchedWebhooks` until a webhook changes).
static WATCHED: Mutex<Option<(Instant, Vec<webhook_configurations::Model>)>> = Mutex::new(None);
const WATCHED_TTL: Duration = Duration::from_secs(5);

async fn global_webhooks<C: ConnectionTrait>(db: &C) -> ApiResult<Vec<webhook_configurations::Model>> {
    if let Some((at, hooks)) = WATCHED.lock().ok().and_then(|g| g.clone()) {
        if at.elapsed() < WATCHED_TTL {
            return Ok(hooks);
        }
    }
    let hooks = webhook_configurations::Entity::find()
        .filter(webhook_configurations::Column::DeletedAt.is_null())
        .filter(webhook_configurations::Column::Scope.eq("global"))
        .all(db)
        .await?;
    if let Ok(mut g) = WATCHED.lock() {
        *g = Some((Instant::now(), hooks.clone()));
    }
    Ok(hooks)
}

/// Drop the cached webhook list (after a webhook was saved or deleted).
pub fn forget_watched() {
    if let Ok(mut g) = WATCHED.lock() {
        *g = None;
    }
}

/// Whether a global webhook or a plugin listens to the event.
pub async fn listening(st: &AppState, action: Action, model: &str) -> ApiResult<bool> {
    let event = event_name(action, model);
    if BLACKLIST.contains(&event.as_str()) {
        return Ok(false);
    }
    if pelican_plugins::runtime::global().is_some_and(|rt| rt.wants(&event)) || crate::sidecar::listens(&event) {
        return Ok(true);
    }
    Ok(global_webhooks(&st.db).await?.iter().any(|h| crate::services::listens(h, &event)))
}

/// `eloquent.created` for a model instance (as stored; a fresh instance has no relations).
pub async fn created<M: Eloquent>(st: &AppState, m: &M) {
    emit(st, &st.db, Action::Created, M::CLASS, m.array(st.driver)).await
}

/// `eloquent.updated` for a model instance (as stored after the update).
pub async fn updated<M: Eloquent>(st: &AppState, m: &M) {
    emit(st, &st.db, Action::Updated, M::CLASS, m.array(st.driver)).await
}

/// `eloquent.deleted` for a model instance (as it was before the delete; soft-deleted
/// models carry their `deleted_at`). A Schedule's tasks go with it: use [`pending`]
/// before deleting one.
pub async fn deleted<M: Eloquent>(st: &AppState, m: &M) {
    emit(st, &st.db, Action::Deleted, M::CLASS, m.array(st.driver)).await
}

/// An event whose payload was built before the write (see [`pending`]).
pub struct Pending {
    action: Action,
    class: &'static str,
    data: Value,
}

/// Build the payload now (`$with` relations included) and send it after the write with
/// [`Pending::send`]; `None` without listeners.
pub async fn pending<M: Eloquent>(st: &AppState, action: Action, m: &M) -> Option<Pending> {
    let res: ApiResult<Option<Pending>> = async {
        if !listening(st, action, M::CLASS).await? {
            return Ok(None);
        }
        let data = with_relations(&st.db, st.driver, action, M::CLASS, m.array(st.driver)).await?;
        Ok(Some(Pending { action, class: M::CLASS, data }))
    }
    .await;
    res.unwrap_or_else(|e| {
        tracing::warn!(error = ?e, model = M::CLASS, "model event failed");
        None
    })
}

impl Pending {
    pub async fn send(self, st: &AppState) {
        if let Err(e) = fire(st, &st.db, self.action, self.class, self.data).await {
            tracing::warn!(error = ?e, model = self.class, "model event failed");
        }
    }
}

/// Re-read the row `id` and fire `action` for it (nothing is read without listeners).
pub async fn fire_id<E>(st: &AppState, action: Action, id: i64)
where
    E: EntityTrait,
    E::Model: Eloquent,
    <E::PrimaryKey as PrimaryKeyTrait>::ValueType: From<Int>,
{
    fire_id_in::<E, _>(st, &st.db, action, id).await
}

/// [`fire_id`] reading through `db` (inside a transaction that must see the row).
pub async fn fire_id_in<E, C>(st: &AppState, db: &C, action: Action, id: i64)
where
    E: EntityTrait,
    E::Model: Eloquent,
    <E::PrimaryKey as PrimaryKeyTrait>::ValueType: From<Int>,
    C: ConnectionTrait,
{
    fire_id_with::<E, C>(st, db, action, id, Vec::new()).await
}

/// [`fire_id_in`] plus relations the original's code path lazy-loaded on the instance
/// before the write (`$oldOwner = $server->user` → `("user", old owner toArray())`),
/// appended after the `$with` ones in load order.
pub async fn fire_id_with<E, C>(st: &AppState, db: &C, action: Action, id: i64, loaded: Vec<(&str, Value)>)
where
    E: EntityTrait,
    E::Model: Eloquent,
    <E::PrimaryKey as PrimaryKeyTrait>::ValueType: From<Int>,
    C: ConnectionTrait,
{
    let class = <E::Model as Eloquent>::CLASS;
    let res = async {
        if !listening(st, action, class).await? {
            return Ok(());
        }
        match E::find_by_id(Int(id)).one(db).await? {
            Some(m) => {
                let mut data = with_relations(db, st.driver, action, class, m.array(st.driver)).await?;
                if let Value::Object(o) = &mut data {
                    for (k, v) in loaded {
                        o.insert(k.to_string(), v);
                    }
                }
                fire(st, db, action, class, data).await
            }
            None => Ok(()),
        }
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(error = ?e, model = class, "model event failed");
    }
}

/// The write already happened: a failed event is logged, not returned.
async fn emit<C: ConnectionTrait>(st: &AppState, db: &C, action: Action, model: &str, data: Value) {
    let res = async {
        if !listening(st, action, model).await? {
            return Ok(());
        }
        let data = with_relations(db, st.driver, action, model, data).await?;
        fire(st, db, action, model, data).await
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(error = ?e, model, "model event failed");
    }
}

/// `toArray()` includes loaded relations after the attributes: the models' `$with`
/// (`Server::$with = ['allocation']`, `Schedule::$with = ['tasks']`,
/// `ActivityLog::$with = ['subjects']`) are loaded by every query, so instances that
/// were read before an update/delete carry them; a just-created instance does not.
async fn with_relations<C: ConnectionTrait>(
    db: &C,
    driver: Driver,
    action: Action,
    model: &str,
    mut data: Value,
) -> ApiResult<Value> {
    use sea_orm::QueryOrder;
    if action == Action::Created {
        return Ok(data);
    }
    let id = data.get("id").and_then(Value::as_i64).unwrap_or_default();
    let relation = match model {
        "Server" => {
            let alloc = match data.get("allocation_id").and_then(Value::as_i64) {
                Some(a) => allocations::Entity::find_by_id(Int(a)).one(db).await?,
                None => None,
            };
            Some(("allocation", alloc.map(|a| a.array(driver)).unwrap_or(Value::Null)))
        }
        "Schedule" => {
            let tasks = tasks::Entity::find()
                .filter(tasks::Column::ScheduleId.eq(id))
                .order_by_asc(tasks::Column::Id)
                .all(db)
                .await?;
            Some(("tasks", Value::Array(tasks.iter().map(|t| t.array(driver)).collect())))
        }
        "ActivityLog" => {
            let subjects = activity_log_subjects::Entity::find()
                .filter(activity_log_subjects::Column::ActivityLogId.eq(id))
                .order_by_asc(activity_log_subjects::Column::Id)
                .all(db)
                .await?;
            Some(("subjects", Value::Array(subjects.iter().map(|s| s.array(driver)).collect())))
        }
        _ => None,
    };
    if let (Some((key, value)), Value::Object(o)) = (relation, &mut data) {
        o.insert(key.into(), value);
    }
    Ok(data)
}

/// Fire `eloquent.<action>: App\Models\<model>` with `data` = the model's `toArray()`
/// (see `crate::model_array`).
pub async fn fire<C: ConnectionTrait>(
    st: &AppState,
    db: &C,
    action: Action,
    model: &str,
    data: Value,
) -> ApiResult<()> {
    if model == "WebhookConfiguration" {
        forget_watched();
    }
    let event = event_name(action, model);
    if BLACKLIST.contains(&event.as_str()) {
        return Ok(());
    }
    let plugins = pelican_plugins::runtime::global().filter(|rt| rt.wants(&event));
    let sidecar = crate::sidecar::listens(&event);
    let hooks: Vec<webhook_configurations::Model> =
        global_webhooks(db).await?.into_iter().filter(|h| crate::services::listens(h, &event)).collect();
    if hooks.is_empty() && plugins.is_none() && !sidecar {
        return Ok(());
    }
    let mut payload = Map::new();
    payload.insert("event".into(), json!(event));
    payload.insert("data".into(), data);
    payload.insert("timestamp".into(), json!(pelican_core::time::atom(&pelican_core::time::now())));
    let payload = Value::Object(payload);
    if let Some(rt) = plugins {
        rt.dispatch(&event, &payload);
    }
    // DispatchWebhooks runs before the PHP plugins' listeners (registered later).
    if crate::services::has_content(&payload) {
        for h in &hooks {
            crate::application::webhooks::process_webhook_in(st, db, h, &event, &payload).await?;
        }
    }
    if sidecar {
        crate::sidecar::model_event(st, &event, &payload["data"]).await;
    }
    Ok(())
}

/// Relations the Client API's `AuthenticateServerAccess` leaves loaded on the bound
/// server before the controller saves it: `subusers` (requester is neither the owner
/// nor allowed `update server`), then `node` and `transfer` from
/// `validateCurrentState()`, short-circuiting like `Server::isInConflictState()`.
pub async fn client_server_loaded(
    st: &AppState,
    ctx: &crate::pipeline::Ctx,
    server: &servers::Model,
) -> Vec<(&'static str, Value)> {
    use pelican_core::server_state::{active_transfer, is_installed};
    use sea_orm::QueryOrder;
    let mut out = Vec::new();
    let res: ApiResult<()> = async {
        let user = ctx.user()?;
        if server.owner_id.0 != user.id() && !user.gate_before("update server") {
            let subs = subusers::Entity::find()
                .filter(subusers::Column::ServerId.eq(server.id))
                .order_by_asc(subusers::Column::Id)
                .all(&st.db)
                .await?;
            out.push(("subusers", Value::Array(subs.iter().map(|s| s.array(st.driver)).collect())));
        }
        if server.status.as_deref() == Some("suspended") {
            return Ok(());
        }
        let node = nodes::Entity::find_by_id(server.node_id).one(&st.db).await?;
        let maintenance = node.as_ref().is_some_and(|n| n.maintenance_mode.0);
        out.push(("node", node.map(|n| n.array(st.driver)).unwrap_or(Value::Null)));
        if maintenance || !is_installed(server) || server.status.as_deref() == Some("restoring_backup") {
            return Ok(());
        }
        let transfer = active_transfer(&st.db, server.id.0).await?;
        out.push(("transfer", transfer.map(|t| t.array(st.driver)).unwrap_or(Value::Null)));
        Ok(())
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(error = ?e, "model event relations failed");
    }
    out
}
