//! Model hooks shared by the API groups.

use pelican_core::state::AppState;
use pelican_db::entities::{servers, subusers, users};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::error::{ApiError, ApiResult};
use crate::pipeline::Ctx;

/// `Server::saved` hook: a subuser entry of the (new) owner is removed
/// through `SubuserDeletionService`.
pub async fn server_saved<C: ConnectionTrait>(
    st: &AppState,
    ctx: &Ctx,
    db: &C,
    server: &servers::Model,
) -> ApiResult<()> {
    let sub = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(server.id))
        .filter(subusers::Column::UserId.eq(server.owner_id))
        .one(db)
        .await?;
    if let Some(sub) = sub {
        let user = users::Entity::find_by_id(sub.user_id).one(db).await?.ok_or_else(ApiError::model_not_found)?;
        crate::client::subusers::delete_subuser_in(st, ctx, db, server, &sub, &user).await?;
    }
    Ok(())
}

// --- mail notifications (app/Notifications/*, lang/*/mail.php) ---------------------

use pelican_core::i18n::trans;
use pelican_core::mail::{MailConfig, MailMessage};

fn mail_config(st: &AppState) -> MailConfig {
    MailConfig::from_env(&st.config.env, &st.config.app_name)
}

fn console_url(st: &AppState, server: &servers::Model) -> String {
    format!("{}/server/{}/console", st.config.app_url.trim_end_matches('/'), server.uuid_short)
}

/// `$user->notify(...)`: queued mail — inline (errors propagate) with
/// `QUEUE_CONNECTION=sync`, otherwise sent in the background.
pub async fn notify(st: &AppState, user: &users::Model, msg: MailMessage) -> ApiResult<()> {
    let cfg = mail_config(st);
    let (name, email) = (user.username.clone(), user.email.clone());
    if st.config.env.string_or("QUEUE_CONNECTION", "database") == "sync" {
        return pelican_core::mail::send(&cfg, &name, &email, &msg)
            .await
            .map_err(|_| ApiError::internal("TransportException"));
    }
    tokio::spawn(async move {
        if let Err(e) = pelican_core::mail::send(&cfg, &name, &email, &msg).await {
            tracing::warn!(error = %e, "sending mail failed");
        }
    });
    Ok(())
}

fn enabled(st: &AppState, key: &str) -> bool {
    st.config.env.bool_or(key, true)
}

/// Filament app panel reset URL: `URL::signedRoute('filament.app.auth.password-reset.reset', [email, token])`.
pub fn reset_url(st: &AppState, email: &str, token: &str) -> String {
    let q = |s: &str| crate::envelope::rfc3986(s);
    let url = format!(
        "{}/password-reset/reset?email={}&token={}",
        st.config.app_url.trim_end_matches('/'),
        q(email),
        q(token)
    );
    use hmac::Mac;
    let key = st.config.env.string_or("APP_KEY", "");
    // hash_hmac('sha256', $url, config('app.key')) — the key string as configured.
    let sig = hmac::Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
        .map(|mut m| {
            m.update(url.as_bytes());
            hex::encode(m.finalize().into_bytes())
        })
        .unwrap_or_default();
    format!("{url}&signature={sig}")
}

/// `AccountCreated` (if `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION`).
pub async fn account_created(st: &AppState, user: &users::Model, token: Option<&str>) -> ApiResult<()> {
    if !enabled(st, "PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION") {
        return Ok(());
    }
    let l = user.language.as_str();
    let msg = MailMessage {
        outro: Vec::new(),
        subject: "Account Created".into(),
        error: false,
        greeting: Some(trans(l, "mail.greeting", &[("name", &user.username)])),
        lines: vec![
            trans(l, "mail.account_created.body", &[("app", &st.config.app_name)]),
            trans(l, "mail.account_created.username", &[("username", &user.username)]),
            trans(l, "mail.account_created.email", &[("email", &user.email)]),
        ],
        action: token.map(|t| (trans(l, "mail.account_created.action", &[]), reset_url(st, &user.email, t))),
    };
    notify(st, user, msg).await
}

/// `AddedToServer` / `RemovedFromServer` (subuser listeners).
pub async fn subuser_mail(st: &AppState, user: &users::Model, server: &servers::Model, added: bool) -> ApiResult<()> {
    let key =
        if added { "PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION" } else { "PANEL_SEND_REMOVED_FROM_SERVER_NOTIFICATION" };
    if !enabled(st, key) {
        return Ok(());
    }
    let l = user.language.as_str();
    let group = if added { "added_to_server" } else { "removed_from_server" };
    let url = if added { console_url(st, server) } else { st.config.app_url.clone() };
    let msg = MailMessage {
        outro: Vec::new(),
        subject: if added { "Added To Server".into() } else { "Removed From Server".into() },
        error: !added,
        greeting: Some(trans(l, "mail.greeting", &[("name", &user.username)])),
        lines: vec![
            trans(l, &format!("mail.{group}.body"), &[]),
            trans(l, &format!("mail.{group}.server_name"), &[("name", &server.name)]),
        ],
        action: Some((trans(l, &format!("mail.{group}.action"), &[]), url)),
    };
    notify(st, user, msg).await
}

/// `ServerInstalled` (install / reinstall switches).
pub async fn server_installed_mail(st: &AppState, server: &servers::Model, initial: bool) -> ApiResult<()> {
    let key = if initial { "PANEL_SEND_INSTALL_NOTIFICATION" } else { "PANEL_SEND_REINSTALL_NOTIFICATION" };
    if !enabled(st, key) {
        return Ok(());
    }
    let Some(owner) = users::Entity::find_by_id(server.owner_id).one(&st.db).await? else { return Ok(()) };
    let l = owner.language.as_str();
    let msg = MailMessage {
        outro: Vec::new(),
        subject: "Server Installed".into(),
        error: false,
        greeting: Some(trans(l, "mail.greeting", &[("name", &owner.username)])),
        lines: vec![
            trans(l, "mail.server_installed.body", &[]),
            trans(l, "mail.server_installed.server_name", &[("name", &server.name)]),
        ],
        action: Some((trans(l, "mail.server_installed.action", &[]), console_url(st, server))),
    };
    notify(st, &owner, msg).await
}

// --- ActivityLogged → DispatchWebhooks (app/Listeners/DispatchWebhooks.php) ---------

use std::future::Future;

use pelican_core::activity::Activity;
use pelican_db::entities::webhook_configurations;
use serde_json::{Map, Value, json};

/// Saving an activity also fires `ActivityLogged`, which delivers webhooks
/// (inline under `QUEUE_CONNECTION=sync`, like the original's sync queue).
pub trait LogActivity {
    fn log<'a>(self, st: &'a AppState) -> impl Future<Output = ApiResult<i64>> + Send + 'a;
    fn log_in<'a, C: ConnectionTrait + Sync>(
        self,
        st: &'a AppState,
        db: &'a C,
    ) -> impl Future<Output = ApiResult<i64>> + Send + 'a;
}

impl LogActivity for Activity {
    fn log<'a>(self, st: &'a AppState) -> impl Future<Output = ApiResult<i64>> + Send + 'a {
        async move {
            let id = self.save_in(&st.db).await?;
            activity_created(st, &st.db, id).await;
            activity_logged(st, &st.db, &self, id).await?;
            Ok(id)
        }
    }

    fn log_in<'a, C: ConnectionTrait + Sync>(
        self,
        st: &'a AppState,
        db: &'a C,
    ) -> impl Future<Output = ApiResult<i64>> + Send + 'a {
        async move {
            let id = self.save_in(db).await?;
            activity_created(st, db, id).await;
            activity_logged(st, db, &self, id).await?;
            Ok(id)
        }
    }
}

/// `eloquent.created` of the log row, then of each subject (`ActivityLogService::save()`).
async fn activity_created<C: ConnectionTrait>(st: &AppState, db: &C, id: i64) {
    use crate::model_events::{Action, Eloquent, fire, fire_id_in, listening};
    use pelican_db::entities::{activity_log_subjects, activity_logs};
    use sea_orm::{EntityTrait, QueryFilter, QueryOrder};

    fire_id_in::<activity_logs::Entity, _>(st, db, Action::Created, id).await;
    if !listening(st, Action::Created, "ActivityLogSubject").await.unwrap_or(false) {
        return;
    }
    let subjects = activity_log_subjects::Entity::find()
        .filter(activity_log_subjects::Column::ActivityLogId.eq(id))
        .order_by_asc(activity_log_subjects::Column::Id)
        .all(db)
        .await
        .unwrap_or_default();
    for s in subjects {
        if let Err(e) = fire(st, db, Action::Created, "ActivityLogSubject", s.array(st.driver)).await {
            tracing::warn!(error = ?e, "model event failed");
        }
    }
}

pub(crate) fn listens(c: &webhook_configurations::Model, event: &str) -> bool {
    match &c.events.0 {
        Value::Array(a) => a.iter().any(|e| e.as_str() == Some(event)),
        Value::Object(m) => m.values().any(|e| e.as_str() == Some(event)),
        _ => false,
    }
}

/// `buildActivityPayload()`.
async fn activity_payload<C: ConnectionTrait>(db: &C, a: &Activity) -> ApiResult<Value> {
    let mut m = Map::new();
    m.insert("event".into(), json!(a.event));
    m.insert("description".into(), json!(a.description));
    m.insert("ip".into(), json!(a.ip));
    m.insert("timestamp".into(), json!(pelican_core::time::atom(&pelican_core::time::now())));
    if let Some((kind, id)) = &a.actor {
        let username = if kind == "user" {
            users::Entity::find_by_id(pelican_db::Int(*id)).one(db).await?.map(|u| u.username)
        } else {
            None
        };
        m.insert("actor".into(), json!({"id": id, "type": kind, "username": username}));
    }
    let props_empty = match &a.properties {
        Value::Array(x) => x.is_empty(),
        Value::Object(x) => x.is_empty(),
        Value::Null => true,
        _ => false,
    };
    if !props_empty {
        m.insert("properties".into(), pelican_core::server_config::php_assoc(a.properties.clone()));
    }
    if !a.subjects.is_empty() {
        let subjects: Vec<Value> = a.subjects.iter().map(|(t, id)| json!({"id": id, "type": t})).collect();
        m.insert("subjects".into(), Value::Array(subjects));
    }
    Ok(Value::Object(m))
}

/// `hasPayloadContent()`: anything besides `event` that is not null/""/empty.
pub(crate) fn has_content(data: &Value) -> bool {
    data.as_object().is_some_and(|m| {
        m.iter().filter(|(k, _)| k.as_str() != "event").any(|(_, v)| match v {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        })
    })
}

/// `DispatchWebhooks::handleActivityLogged()` + `handleGlobalWebhooks()`.
pub async fn activity_logged<C: ConnectionTrait>(st: &AppState, db: &C, a: &Activity, id: i64) -> ApiResult<()> {
    activity_logged_core(st, db, a).await?;
    // The PHP plugins' listeners run after the core's DispatchWebhooks (ADR-0020).
    crate::sidecar::activity_logged(st, id, a).await;
    Ok(())
}

async fn activity_logged_core<C: ConnectionTrait>(st: &AppState, db: &C, a: &Activity) -> ApiResult<()> {
    // Plugins (ADR-0018): the event itself and `App\Events\ActivityLogged` listeners.
    if let Some(rt) = pelican_plugins::runtime::global() {
        let (own, all) = (rt.wants(&a.event), rt.wants("App\\Events\\ActivityLogged"));
        if own || all {
            let data = activity_payload(db, a).await?;
            if own {
                rt.dispatch(&a.event, &data);
            }
            if all {
                rt.dispatch("App\\Events\\ActivityLogged", &data);
            }
        }
    }
    let configs: Vec<webhook_configurations::Model> = webhook_configurations::Entity::find()
        .filter(webhook_configurations::Column::DeletedAt.is_null())
        .all(db)
        .await?;
    if configs.is_empty() {
        return Ok(());
    }
    let event = a.event.as_str();
    let mut payload: Option<Value> = None;
    // Server scope: the first `server` subject's webhooks listening to the event.
    if event.starts_with("server:") {
        let server_id = a
            .subjects
            .iter()
            .find(|(t, _)| t == "server")
            .map(|(_, id)| *id)
            .or_else(|| a.properties.get("server").and_then(|s| s.get("id")).and_then(Value::as_i64));
        if let Some(sid) = server_id {
            let hooks: Vec<&webhook_configurations::Model> =
                configs.iter().filter(|c| c.server_id.map(|s| s.0) == Some(sid) && listens(c, event)).collect();
            if !hooks.is_empty() {
                let data = activity_payload(db, a).await?;
                if has_content(&data) {
                    for c in hooks {
                        crate::application::webhooks::process_webhook_in(st, db, c, event, &data).await?;
                    }
                }
                payload = Some(data);
            }
        }
    }
    // Global: listeners of the event name, then of `App\Events\ActivityLogged` (unique by id).
    let mut global: Vec<&webhook_configurations::Model> =
        configs.iter().filter(|c| c.scope == "global" && listens(c, event)).collect();
    for c in configs.iter().filter(|c| c.scope == "global" && listens(c, "App\\Events\\ActivityLogged")) {
        if !global.iter().any(|g| g.id == c.id) {
            global.push(c);
        }
    }
    if global.is_empty() {
        return Ok(());
    }
    let data = match payload {
        Some(p) => p,
        None => activity_payload(db, a).await?,
    };
    if !has_content(&data) {
        return Ok(());
    }
    for c in global {
        crate::application::webhooks::process_webhook_in(st, db, c, event, &data).await?;
    }
    Ok(())
}
