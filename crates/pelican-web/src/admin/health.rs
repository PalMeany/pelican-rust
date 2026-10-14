//! Admin › Health (app/Filament/Admin/Pages/Health.php + views/filament/pages/health):
//! latest health check results, refresh action.

use axum::Router;
use axum::routing::{get, post};
use minijinja::context;
use pelican_api::health::{self, Text};
use pelican_core::state::AppState;

use super::Admin;
use crate::error::WebResult;
use crate::forms::Form;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/health", get(index)).route("/admin/health/refresh", post(refresh))
}

/// Messages were produced by a console run: translated in the application locale.
fn text(admin: &Admin, t: &Text) -> String {
    let locale = admin.st.config.env.string_or("APP_LOCALE", "en");
    match t {
        Text::Raw(s) => s.clone(),
        Text::Key(key, params) => crate::i18n::trans(&locale, key, params),
    }
}

async fn index(admin: Admin) -> WebResult {
    admin.authorize("view health")?;
    let results = match health::latest() {
        Some(r) => r,
        None => health::run_checks(&admin.st).await,
    };
    let checks: Vec<_> = results
        .results
        .iter()
        .map(|r| {
            let message = if r.notification_message.is_empty() { &r.short_summary } else { &r.notification_message };
            let (icon, color) = match r.status {
                "ok" => ("circle-check", "success"),
                "warning" => ("exclamation-circle", "warning"),
                "skipped" => ("circle-chevron-right", "info"),
                "failed" | "crashed" => ("circle-x", "danger"),
                _ => ("help-circle", "gray"),
            };
            let key = r.label.to_lowercase().split_whitespace().collect::<String>();
            context! {
                label => admin.t(&format!("admin/health.results.{key}.label")),
                message => text(&admin, message),
                icon => icon, color => color,
            }
        })
        .collect();
    let stale = (chrono::Utc::now() - results.finished_at).num_minutes() > 5;
    admin
        .page(
            "health",
            "admin/health.html",
            context! {
                title => admin.t("admin/health.title"),
                checks => checks,
                last_ran_at => results.finished_at.to_rfc3339(),
                stale => stale,
            },
        )
        .await
}

async fn refresh(admin: Admin, form: Form) -> WebResult {
    admin.authorize("view health")?;
    health::run_checks(&admin.st).await;
    admin.flash_success(admin.t("admin/health.results_refreshed"));
    Ok(admin.redirect(&form.back("/admin/health")))
}
