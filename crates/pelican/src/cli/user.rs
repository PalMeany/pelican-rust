//! `p:user:make`, `p:user:delete`, `p:user:disable2fa` (app/Console/Commands/User).

use clap::{Args, Subcommand};
use pelican_api::application::users::{NewUser, create, delete_user, validate_new_user};
use pelican_core::state::AppState;
use pelican_db::entities::users;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter};

use super::{Ctx, api_error};
use crate::console::{Console, t, truthy};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Creates a user on the system via the CLI.
    #[command(name = "p:user:make")]
    Make(MakeArgs),
    /// Deletes a user from the Panel if no servers are attached to their account.
    #[command(name = "p:user:delete")]
    Delete(DeleteArgs),
    /// Disable two-factor authentication for a specific user in the Panel.
    #[command(name = "p:user:disable2fa")]
    Disable2fa(Disable2faArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    let st = ctx.state().await?;
    Ok(match cmd {
        Command::Make(a) => make(&st, io, a).await,
        Command::Delete(a) => delete(&st, io, a).await,
        Command::Disable2fa(a) => disable_2fa(&st, io, a).await,
    })
}

#[derive(Args, Debug)]
pub struct MakeArgs {
    #[arg(long)]
    email: Option<String>,
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long)]
    admin: Option<String>,
    #[arg(long = "no-password")]
    no_password: bool,
}

/// MakeUserCommand.
pub async fn make(st: &AppState, io: &Console, a: MakeArgs) -> i32 {
    let root_admin = match a.admin {
        Some(v) => truthy(&v),
        None => io.confirm(&t("command/messages.user.ask_admin", &[]), false),
    };
    let email = a.email.or_else(|| io.ask(&t("command/messages.user.ask_email", &[]), None)).unwrap_or_default();
    let username =
        a.username.or_else(|| io.ask(&t("command/messages.user.ask_username", &[]), None)).unwrap_or_default();
    let mut password = a.password;
    if password.is_none() && !a.no_password {
        io.warn(&t("command/messages.user.ask_password_help", &[]));
        io.line(&t("command/messages.user.ask_password_tip", &[]));
        password = io.secret(&t("command/messages.user.ask_password", &[]));
    }
    let data = NewUser {
        email,
        username,
        password,
        root_admin,
        language: "en".into(),
        timezone: st.config.env.string_or("APP_TIMEZONE", "UTC"),
        ..Default::default()
    };
    if let Err(e) = validate_new_user(st, &data).await {
        io.error(&api_error(&e));
        return 1;
    }
    let user = match create(st, data).await {
        Ok(u) => u,
        Err(e) => {
            io.error(&api_error(&e));
            return 1;
        }
    };
    let admin = pelican_core::auth::is_root_admin(&st.db, user.id.0).await.unwrap_or(false);
    io.table(
        &["Field", "Value"],
        &[
            vec!["UUID".into(), user.uuid.clone()],
            vec!["Email".into(), user.email.clone()],
            vec!["Username".into(), user.username.clone()],
            vec!["Admin".into(), if admin { "Yes" } else { "No" }.into()],
        ],
    );
    0
}

#[derive(Args, Debug)]
pub struct DeleteArgs {
    #[arg(long)]
    user: Option<String>,
}

/// `id LIKE 'x%' OR username LIKE 'x%' OR email LIKE 'x%'` (PostgreSQL needs the id cast).
async fn search(st: &AppState, term: &str) -> Result<Vec<users::Model>, sea_orm::DbErr> {
    let like = format!("{term}%");
    let id_like = if st.db.get_database_backend() == DbBackend::Postgres {
        Expr::cust_with_values("CAST(\"id\" AS TEXT) LIKE $1", [like.clone()])
    } else {
        Expr::cust_with_values("`id` LIKE $1", [like.clone()])
    };
    users::Entity::find()
        .filter(
            Condition::any()
                .add(id_like)
                .add(users::Column::Username.like(like.clone()))
                .add(users::Column::Email.like(like)),
        )
        .all(&st.db)
        .await
}

/// DeleteUserCommand.
pub async fn delete(st: &AppState, io: &Console, a: DeleteArgs) -> i32 {
    let mut option = a.user;
    loop {
        let term = option.take().or_else(|| io.ask(&t("command/messages.user.search_users", &[]), None));
        let Some(term) = term.filter(|s| !s.is_empty()) else {
            io.error("Search term should not be empty.");
            return 1;
        };
        let results = match search(st, &term).await {
            Ok(r) => r,
            Err(e) => {
                io.error(&e.to_string());
                return 1;
            }
        };
        if results.is_empty() {
            io.error(&t("command/messages.user.no_users_found", &[]));
            if io.interactive() {
                continue;
            }
            return 1;
        }
        let target = if io.interactive() {
            let rows: Vec<Vec<String>> =
                results.iter().map(|u| vec![u.id.0.to_string(), u.email.clone(), u.username.clone()]).collect();
            io.table(&["User ID", "Email", "Name"], &rows);
            let pick = io.ask(&t("command/messages.user.select_search_user", &[]), None).unwrap_or_default();
            if pick.is_empty() || pick == "0" {
                continue;
            }
            match pick.parse::<i64>().ok().map(|id| results.iter().find(|u| u.id.0 == id).cloned()) {
                Some(Some(u)) => u,
                _ => match users::Entity::find_by_id(pelican_db::Int(pick.parse().unwrap_or(0))).one(&st.db).await {
                    Ok(Some(u)) => u,
                    _ => {
                        io.error("No query results for model [App\\Models\\User].");
                        return 1;
                    }
                },
            }
        } else {
            if results.len() > 1 {
                io.error(&t("command/messages.user.multiple_found", &[]));
                return 1;
            }
            results[0].clone()
        };
        if io.confirm(&t("command/messages.user.confirm_delete", &[]), false) || !io.interactive() {
            if let Err(e) = delete_user(st, &target, None).await {
                io.error(&api_error(&e));
                return 1;
            }
            io.info(&t("command/messages.user.deleted", &[]));
        }
        return 0;
    }
}

#[derive(Args, Debug)]
pub struct Disable2faArgs {
    #[arg(long)]
    email: Option<String>,
}

/// DisableTwoFactorCommand.
pub async fn disable_2fa(st: &AppState, io: &Console, a: Disable2faArgs) -> i32 {
    if io.interactive() {
        io.warning_block(&t("command/messages.user.2fa_help_text", &[]));
    }
    let email = a.email.or_else(|| io.ask(&t("command/messages.user.ask_email", &[]), None)).unwrap_or_default();
    let user = match users::Entity::find().filter(users::Column::Email.eq(email)).one(&st.db).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            io.error("No query results for model [App\\Models\\User].");
            return 1;
        }
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    let changed = user.mfa_app_secret.is_some() || user.mfa_app_recovery_codes.is_some() || user.mfa_email_enabled.0;
    if changed {
        let am = users::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(user.id),
            mfa_app_secret: Set(None),
            mfa_app_recovery_codes: Set(None),
            mfa_email_enabled: Set(pelican_db::Bool(false)),
            updated_at: Set(Some(pelican_db::Timestamp(pelican_core::time::now()))),
            ..Default::default()
        };
        if let Err(e) = pelican_db::write::update(&st.db, &am).await {
            io.error(&e.to_string());
            return 1;
        }
        // `$user->update([...])` (eloquent.updated when something changed).
        pelican_api::model_events::fire_id::<users::Entity>(st, pelican_api::model_events::Action::Updated, user.id.0)
            .await;
    }
    io.info(&t("command/messages.user.2fa_disabled", &[("email", &user.email)]));
    0
}
