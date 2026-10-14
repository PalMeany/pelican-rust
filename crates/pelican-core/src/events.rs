//! Side effects of model events / listeners that run inline under the sync
//! queue (models-services.md §0.9, §0.11).

use pelican_db::entities::{notifications, servers, subusers, users};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde_json::{Value, json};

/// `Filament\Actions\Action::make($name)->button()->label()->markAsRead()->url()`
/// as serialized into database notifications (`Action::toArray()`).
pub fn filament_button(name: &str, label: &str, url: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), json!(name));
    m.insert("alpineClickHandler".into(), Value::Null);
    m.insert("color".into(), Value::Null);
    m.insert("event".into(), Value::Null);
    m.insert("eventData".into(), json!([]));
    m.insert("dispatchDirection".into(), json!(false));
    m.insert("dispatchToComponent".into(), Value::Null);
    m.insert("extraAttributes".into(), json!([]));
    m.insert("icon".into(), Value::Null);
    m.insert("iconPosition".into(), json!("before"));
    m.insert("iconSize".into(), json!("lg"));
    m.insert("isOutlined".into(), json!(false));
    m.insert("isDisabled".into(), json!(false));
    m.insert("label".into(), json!(label));
    m.insert("shouldClose".into(), json!(false));
    m.insert("shouldMarkAsRead".into(), json!(true));
    m.insert("shouldMarkAsUnread".into(), json!(false));
    m.insert("shouldOpenUrlInNewTab".into(), json!(false));
    m.insert("shouldPostToUrl".into(), json!(false));
    m.insert("size".into(), json!("sm"));
    m.insert("tooltip".into(), Value::Null);
    m.insert("url".into(), json!(url));
    m.insert("view".into(), json!("filament::components.button.index"));
    Value::Object(m)
}

/// A Filament database notification (`Notification::make()->sendToDatabase()`).
pub struct FilamentNotification {
    pub title: String,
    pub body: Option<String>,
    pub status: Option<String>,
    pub actions: Vec<Value>,
}

impl FilamentNotification {
    /// `getDatabaseMessage()` key order; status icons are Pelican's Tabler overrides
    /// (FilamentServiceProvider NotificationsIconAlias).
    pub fn data(&self) -> Value {
        let icon = match self.status.as_deref() {
            Some("success") => json!("tabler-circle-check"),
            Some("danger") => json!("tabler-alert-circle"),
            Some("warning") => json!("tabler-alert-triangle"),
            Some("info") => json!("tabler-info-circle"),
            _ => Value::Null,
        };
        let mut m = serde_json::Map::new();
        m.insert("actions".into(), Value::Array(self.actions.clone()));
        m.insert("body".into(), self.body.clone().map(Value::String).unwrap_or(Value::Null));
        m.insert("color".into(), Value::Null);
        m.insert("duration".into(), json!("persistent"));
        m.insert("icon".into(), icon);
        m.insert("iconColor".into(), self.status.clone().map(Value::String).unwrap_or(Value::Null));
        m.insert("iconSize".into(), Value::Null);
        m.insert("status".into(), self.status.clone().map(Value::String).unwrap_or(Value::Null));
        m.insert("title".into(), json!(self.title));
        m.insert("view".into(), Value::Null);
        m.insert("viewData".into(), json!([]));
        m.insert("format".into(), json!("filament"));
        Value::Object(m)
    }

    pub async fn send<C: ConnectionTrait>(&self, db: &C, user_id: i64) -> Result<(), DbErr> {
        let now = Timestamp(crate::time::now());
        let row = notifications::ActiveModel {
            id: Set(uuid::Uuid::new_v4().to_string()),
            r#type: Set("Filament\\Notifications\\DatabaseNotification".into()),
            notifiable_type: Set("user".into()),
            notifiable_id: Set(Int(user_id)),
            data: Set(Json(self.data())),
            read_at: Set(None),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
        };
        pelican_db::write::insert_id(db, &row).await?;
        Ok(())
    }
}

/// `Server::saved` hook: an owner must not also be a subuser.
pub async fn server_saved<C: ConnectionTrait>(db: &C, server: &servers::Model, _ip: String) -> Result<(), DbErr> {
    let owner_sub = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(server.id))
        .filter(subusers::Column::UserId.eq(server.owner_id))
        .one(db)
        .await?;
    if owner_sub.is_some() {
        // SubuserDeletionService arrives with the client API (M4).
        return Err(DbErr::Custom("owner subuser deletion is not implemented yet".into()));
    }
    Ok(())
}

/// `ServerInstalledListener` (DB notification; mail is sent via MAIL_MAILER).
pub async fn server_installed<C: ConnectionTrait>(
    db: &C,
    server: &servers::Model,
    successful: bool,
    initial: bool,
    url_root: &str,
) -> Result<(), DbErr> {
    let owner = users::Entity::find_by_id(server.owner_id).one(db).await?;
    let Some(owner) = owner else { return Ok(()) };
    let locale = owner.language.clone();
    let key = match (initial, successful) {
        (true, true) => "notifications.installation_completed",
        (true, false) => "notifications.installation_failed",
        (false, true) => "notifications.reinstallation_completed",
        (false, false) => "notifications.reinstallation_failed",
    };
    let title = crate::i18n::trans(&locale, key, &[]);
    let n = FilamentNotification {
        title,
        body: Some(crate::i18n::trans(&locale, "server_name", &[("name", &server.name)])),
        status: Some(if successful { "success" } else { "danger" }.into()),
        actions: vec![filament_button(
            "exclude_view",
            &crate::i18n::trans(&locale, "notifications.open_server", &[]),
            &format!("{url_root}/server/{}/console", server.uuid_short),
        )],
    };
    n.send(db, owner.id.0).await
}

/// `BackupCompletedListener`: notify owner + subusers who opted in.
pub async fn backup_completed<C: ConnectionTrait>(
    db: &C,
    backup: &pelican_db::entities::backups::Model,
    server: &servers::Model,
    url_root: &str,
) -> Result<(), DbErr> {
    use pelican_db::entities::server_user_settings;
    let mut candidates: Vec<users::Model> = Vec::new();
    if let Some(owner) = users::Entity::find_by_id(server.owner_id).one(db).await? {
        candidates.push(owner);
    }
    for s in subusers::Entity::find().filter(subusers::Column::ServerId.eq(server.id)).all(db).await? {
        if let Some(u) = users::Entity::find_by_id(s.user_id).one(db).await? {
            if !candidates.iter().any(|c| c.id == u.id) {
                candidates.push(u);
            }
        }
    }
    let key = if backup.is_scheduled.0 { "scheduled_backup_notifications" } else { "manual_backup_notifications" };
    for user in candidates {
        let setting = server_user_settings::Entity::find()
            .filter(server_user_settings::Column::ServerId.eq(server.id))
            .filter(server_user_settings::Column::UserId.eq(user.id))
            .one(db)
            .await?;
        let enabled = setting
            .and_then(|s| s.settings)
            .and_then(|j| j.0.get(key).cloned())
            .is_some_and(|v| !crate::php::empty(&v));
        if !enabled {
            continue;
        }
        let ok = backup.is_successful.0;
        let locale = user.language.clone();
        let t = |k: &str| crate::i18n::trans(&locale, k, &[]);
        FilamentNotification {
            title: if ok { t("notifications.backup_completed") } else { t("notifications.backup_failed") },
            body: Some(crate::i18n::trans(
                &locale,
                "notifications.backup_body",
                &[("name", &backup.name), ("server", &server.name)],
            )),
            status: Some(if ok { "success" } else { "danger" }.into()),
            actions: vec![filament_button(
                "exclude_view",
                &t("notifications.view_backups"),
                &format!("{url_root}/server/{}/backups", server.uuid_short),
            )],
        }
        .send(db, user.id.0)
        .await?;
    }
    Ok(())
}
