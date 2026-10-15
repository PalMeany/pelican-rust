//! Eloquent model events (`eloquent.created|updated|deleted: App\Models\X`, ADR-0019):
//! `DispatchWebhooks::handleEloquentEvent()` for global webhooks and `on-event` for
//! WASM plugins. Call [`created`]/[`updated`]/[`deleted`] (or [`fire_id`]) after a
//! model-instance write the original performs with `create()/save()/update()/delete()`
//! (mass `query()->update()` fires nothing). Fire after the transaction commits: webhook
//! delivery is a synchronous HTTP call.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use pelican_core::state::AppState;
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
    fn to_array(&self) -> Value;
}

macro_rules! eloquent {
    ($($m:ident => $class:literal, $f:ident;)*) => {$(
        impl Eloquent for $m::Model {
            const CLASS: &'static str = $class;
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
    if pelican_plugins::runtime::global().is_some_and(|rt| rt.wants(&event)) {
        return Ok(true);
    }
    Ok(global_webhooks(&st.db).await?.iter().any(|h| crate::services::listens(h, &event)))
}

/// `eloquent.created` for a model instance (as stored).
pub async fn created<M: Eloquent>(st: &AppState, m: &M) -> ApiResult<()> {
    fire(st, &st.db, Action::Created, M::CLASS, m.to_array()).await
}

/// `eloquent.updated` for a model instance (as stored after the update).
pub async fn updated<M: Eloquent>(st: &AppState, m: &M) -> ApiResult<()> {
    fire(st, &st.db, Action::Updated, M::CLASS, m.to_array()).await
}

/// `eloquent.deleted` for a model instance (as it was before the delete; soft-deleted
/// models carry their `deleted_at`).
pub async fn deleted<M: Eloquent>(st: &AppState, m: &M) -> ApiResult<()> {
    fire(st, &st.db, Action::Deleted, M::CLASS, m.to_array()).await
}

/// Re-read the row `id` and fire `action` for it (nothing is read without listeners).
pub async fn fire_id<E>(st: &AppState, action: Action, id: i64) -> ApiResult<()>
where
    E: EntityTrait,
    E::Model: Eloquent,
    <E::PrimaryKey as PrimaryKeyTrait>::ValueType: From<Int>,
{
    if !listening(st, action, <E::Model as Eloquent>::CLASS).await? {
        return Ok(());
    }
    match E::find_by_id(Int(id)).one(&st.db).await? {
        Some(m) => fire(st, &st.db, action, <E::Model as Eloquent>::CLASS, m.to_array()).await,
        None => Ok(()),
    }
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
    let hooks: Vec<webhook_configurations::Model> =
        global_webhooks(db).await?.into_iter().filter(|h| crate::services::listens(h, &event)).collect();
    if hooks.is_empty() && plugins.is_none() {
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
    if !crate::services::has_content(&payload) {
        return Ok(());
    }
    for h in &hooks {
        crate::application::webhooks::process_webhook_in(st, db, h, &event, &payload).await?;
    }
    Ok(())
}
