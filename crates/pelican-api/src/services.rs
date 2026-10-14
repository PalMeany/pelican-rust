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
fn reset_url(st: &AppState, email: &str, token: &str) -> String {
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
