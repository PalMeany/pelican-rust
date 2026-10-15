//! Eloquent model events (`eloquent.created|updated|deleted: App\Models\X`, ADR-0019):
//! `DispatchWebhooks::handleEloquentEvent()` for global webhooks and `on-event` for
//! WASM plugins. Call [`fire`] after a model-instance write the original performs with
//! `create()/save()/update()/delete()` (mass `query()->update()` fires nothing).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use pelican_core::state::AppState;
use pelican_db::entities::webhook_configurations;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
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

/// Fire `eloquent.<action>: App\Models\<model>` with `data` = the model's `toArray()`
/// (see `crate::model_array`).
pub async fn fire<C: ConnectionTrait>(st: &AppState, db: &C, action: Action, model: &str, data: Value) -> ApiResult<()> {
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
