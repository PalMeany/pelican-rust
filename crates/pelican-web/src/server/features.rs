//! Console "server features" modals (app/Extensions/Features/Schemas/* +
//! `Console::mountFeature()`): Wings reports console-output lines matching a
//! registered listener and sends a websocket `feature match` event; the browser
//! then loads the feature's action modal here and POSTs it back after the user
//! confirms (`GET|POST /server/{server}/console/feature/{key}`).

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use minijinja::Value as Mj;
use minijinja::context;
use pelican_api::model_events;
use pelican_api::plugin_routes::{server_json, user_json};
use pelican_api::services::LogActivity;
use pelican_core::state::AppState;
use pelican_db::entities::{eggs, server_variables};
use pelican_wings::WingsError;
use pelican_wings::client::{Body, Call, Method};
use sea_orm::ColumnTrait;
use sea_orm::EntityTrait;
use sea_orm::QueryFilter;
use serde_json::json;

use super::ServerPanel;
use super::startup;
use crate::error::{WebError, WebResult};
use crate::forms::Form;

#[cfg(test)]
use pelican_db::Json as DbJson;

pub fn routes() -> Router<AppState> {
    Router::new().route("/server/{server}/console/feature/{key}", get(modal).post(perform))
}

/// The `{key}` segment of `/server/{server}/console/feature/{key}`
/// (`Path<String>` cannot be used: the route has two path parameters).
pub struct FeatureKey(pub String);

impl<S: Send + Sync> FromRequestParts<S> for FeatureKey {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &S) -> Result<Self, Self::Rejection> {
        let Path(params): Path<Vec<(String, String)>> =
            Path::from_request_parts(parts, st).await.map_err(|_| WebError::NotFound.into_response())?;
        params
            .iter()
            .find(|(k, _)| k == "key")
            .and_then(|(_, v)| (!v.is_empty()).then(|| v.clone()))
            .map(FeatureKey)
            .ok_or_else(|| WebError::NotFound.into_response())
    }
}

/// The five built-in schemas (`FeatureSchemaInterface` implementations).
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Eula,
    JavaVersion,
    GslToken,
    PidLimit,
    SteamDiskSpace,
}

impl Kind {
    fn parse(id: &str) -> Option<Kind> {
        match id {
            "eula" => Some(Kind::Eula),
            "java_version" => Some(Kind::JavaVersion),
            "gsl_token" => Some(Kind::GslToken),
            "pid_limit" => Some(Kind::PidLimit),
            "steam_disk_space" => Some(Kind::SteamDiskSpace),
            _ => None,
        }
    }

    /// `$schema->authorize($user, $server)`; informational schemas return true.
    fn permission(&self) -> Option<&'static str> {
        match self {
            Kind::Eula => Some("file.update"),
            Kind::JavaVersion => Some("startup.docker-image"),
            Kind::GslToken => Some("startup.update"),
            Kind::PidLimit | Kind::SteamDiskSpace => None,
        }
    }
}

/// `FeatureService::getActiveSchemas($egg->features)`: the egg lists the feature
/// ids it wants; an egg without a features list offers every feature.
fn feature_active(egg: &eggs::Model, key: &str) -> bool {
    match &egg.features {
        None => true,
        Some(j) => j.0.as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some(key))),
    }
}

async fn validate_state(p: &ServerPanel, key: &str) -> Result<Kind, WebError> {
    let egg = startup::egg_of(p).await?;
    if !feature_active(&egg, key) {
        return Err(WebError::NotFound);
    }
    let kind = Kind::parse(key).ok_or(WebError::NotFound)?;
    if let Some(permission) = kind.permission() {
        p.authorize(permission)?;
    }
    Ok(kind)
}

/// Backend URL for both actions of one feature.
fn feature_url(p: &ServerPanel, key: &str) -> String {
    format!("{}/console/feature/{}", p.base_url(), super::rawurlencode(key))
}

/// Blade `<x-filament::link href="…" target="…">text</x-filament::link>` inside
/// translated strings (rendered by `Blade::render` in the original) → anchor.
fn blade_link(s: &str) -> String {
    const TAG_OPEN: &str = "<x-filament::link";
    const TAG_CLOSE: &str = "</x-filament::link>";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        // Leave anything without a well-formed link untouched.
        let Some(i) = rest.find(TAG_OPEN) else { break };
        let Some(tail) = rest[i..].find(TAG_CLOSE) else { break };
        let segment = &rest[i..i + tail + TAG_CLOSE.len()];
        let Some(open_end_rel) = segment.find('>') else { break };
        let attrs = &segment[TAG_OPEN.len()..open_end_rel];
        let Some(open_end) = rest[i..].find('>') else { break };
        let inner_start = i + open_end + 1;
        let inner_end = i + tail;
        let href = tag_attr(attrs, "href").unwrap_or_default();
        let target = tag_attr(attrs, "target").unwrap_or_default();
        out.push_str(&rest[..i]);
        out.push_str(&format!("<a class=\"btn-link\" href=\"{href}\"{target}>{}</a>", &rest[inner_start..inner_end]));
        rest = &rest[inner_end + TAG_CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

/// One attribute value of a rendered Blade tag (`key="value"` pairs); `target`
/// keeps its leading space so it can be appended to an anchor verbatim.
fn tag_attr(attrs: &str, name: &str) -> Option<String> {
    attrs
        .split_whitespace()
        .find_map(|tok| {
            let (k, v) = tok.split_once('=')?;
            (k == name).then(|| v.trim_matches('"').to_string())
        })
        .map(|v| if name == "target" && !v.is_empty() { format!(" target=\"{v}\"") } else { v })
}

/// The browser's notification payload for this endpoint (`window.notify`); a
/// `None` title means "just close the modal".
fn notify_json(status: &str, title: Option<String>, body: Option<String>) -> Result<Response, WebError> {
    Ok(Json(json!({ "status": status, "title": title, "body": body })).into_response())
}

/// A Wings `DaemonFileRepository::putContent`.
async fn daemon_put_file(p: &ServerPanel, file: &str, content: &[u8]) -> Result<(), WingsError> {
    let conn = p.st.node_conn(&p.node).map_err(|e| WingsError::Connection(e.to_string()))?;
    let path = format!("/api/servers/{}/files/write", p.server.uuid);
    let query = [("file", file.to_string())];
    let mut c = Call::new(Method::POST, &path).query(&query);
    c.body = Body::Raw(content.to_vec());
    p.st.wings.send(&conn, c).await.map(|_| ())
}

/// A Wings `DaemonServerRepository::power('restart')`.
async fn daemon_restart(p: &ServerPanel) -> Result<(), WingsError> {
    let conn = p.st.node_conn(&p.node).map_err(|e| WingsError::Connection(e.to_string()))?;
    p.st.wings.power(&conn, &p.server.uuid, "restart").await
}

/// The STEAM_ACC egg variable with its stored value for this server.
async fn gsl_variable(
    p: &ServerPanel,
) -> Result<Option<(pelican_db::entities::egg_variables::Model, Option<String>)>, WebError> {
    use pelican_db::entities::egg_variables;
    let var = egg_variables::Entity::find()
        .filter(egg_variables::Column::EggId.eq(p.server.egg_id))
        .filter(egg_variables::Column::EnvVariable.eq("STEAM_ACC"))
        .one(&p.st.db)
        .await?;
    let Some(var) = var else { return Ok(None) };
    let row = server_variables::Entity::find()
        .filter(server_variables::Column::ServerId.eq(p.server.id))
        .filter(server_variables::Column::VariableId.eq(var.id))
        .one(&p.st.db)
        .await?;
    Ok(Some((var, row.map(|r| r.variable_value))))
}

/// The fragment view-model handed to `templates/server/feature.html`.
#[derive(Default)]
struct ModalView {
    kind_sel: &'static str,
    heading: String,
    /// Trusted translation markup (links and paragraphs only).
    description_html: Option<String>,
    submit: Option<String>,
    options: Vec<(String, String)>,
    current: String,
    select_disabled: bool,
    env: String,
    field_label: String,
    field_value: String,
    field_hint: String,
    errors: Vec<(String, String)>,
}

fn base_view() -> ModalView {
    ModalView::default()
}

impl ModalView {
    fn ctx(&self, action: &str) -> Mj {
        context! {
            action => action,
            kind_sel => self.kind_sel,
            heading => self.heading.clone(),
            description_html => self.description_html.clone(),
            submit => self.submit.clone(),
            options => Mj::from_serialize(&self.options),
            current => self.current.clone(),
            select_disabled => self.select_disabled,
            env => self.env.clone(),
            field_label => self.field_label.clone(),
            field_value => self.field_value.clone(),
            field_hint => self.field_hint.clone(),
            // First message per field (`MessageBag::first`), as `forms.html` expects.
            errors => Mj::from_serialize(self.errors.iter().rev().cloned().collect::<BTreeMap<String, String>>()),
        }
    }

    async fn render(self, p: &ServerPanel, key: &str) -> WebResult<Response> {
        p.render("server/feature.html", self.ctx(&feature_url(p, key)))
    }
}

/// `GET …` — the feature's action-modal HTML fragment for the console page.
///
/// Plugin-registered features (`registration.features`) render the plugin's own
/// `feature-{id}` page instead; their authorize is always true.
async fn modal(p: ServerPanel, FeatureKey(key): FeatureKey) -> WebResult<Response> {
    // A plugin may register this id; its modal body comes from the plugin's page
    // `feature-{key}` rendered on the server panel.
    if let Some(rt) = pelican_plugins::runtime::global() {
        if let Some(plugin_id) =
            rt.loaded().iter().find_map(|l| l.registration.features.iter().any(|f| f.id == key).then(|| l.id.clone()))
        {
            return plugin_modal(&rt, &p, &plugin_id, &key).await;
        }
    }

    match validate_state(&p, &key).await? {
        Kind::PidLimit => {
            ModalView {
                kind_sel: "info",
                heading: admin_aware(&p, "pid_limit.heading_admin", "pid_limit.heading_user"),
                description_html: Some(admin_aware_html(
                    &p,
                    "pid_limit.description_admin",
                    "pid_limit.description_user",
                )),
                ..base_view()
            }
            .render(&p, &key)
            .await
        }
        Kind::SteamDiskSpace => {
            ModalView {
                kind_sel: "info",
                heading: p.t("server/feature.steam_disk_space.heading"),
                description_html: Some(admin_aware_html(
                    &p,
                    "steam_disk_space.description_admin",
                    "steam_disk_space.description_user",
                )),
                ..base_view()
            }
            .render(&p, &key)
            .await
        }
        Kind::Eula => {
            ModalView {
                kind_sel: "confirm",
                heading: p.t("server/feature.eula.heading"),
                description_html: Some(blade_link(&p.t("server/feature.eula.description"))),
                submit: Some(p.t("server/feature.eula.accept")),
                ..base_view()
            }
            .render(&p, &key)
            .await
        }
        Kind::JavaVersion => {
            let b = startup::bundle(&p).await?;
            java_view(&p, &b, b.server.image.clone(), Vec::new()).render(&p, &key).await
        }
        Kind::GslToken => {
            let (var, value) = gsl_variable(&p).await?.ok_or(WebError::NotFound)?;
            let field = crate::variables::VariableField::new(&var, value.as_deref());
            ModalView {
                kind_sel: "gsl",
                heading: p.t("server/feature.gsl_token.heading"),
                description_html: Some(blade_link(&p.t("server/feature.gsl_token.info"))),
                submit: Some(p.t("server/feature.gsl_token.submit")),
                env: field.env,
                field_label: field.name,
                field_value: field.value,
                field_hint: if field.description.is_empty() { "\u{2014}".to_string() } else { field.description },
                ..base_view()
            }
            .render(&p, &key)
            .await
        }
    }
}

/// JavaVersionSchema's modal: the egg's images, `current` selected.
fn java_view(
    p: &ServerPanel,
    b: &pelican_core::server_config::ServerBundle,
    current: String,
    errors: Vec<(String, String)>,
) -> ModalView {
    let options = startup::flip_options(b.egg.docker_images.as_ref().map(|j| &j.0).unwrap_or(&serde_json::Value::Null));
    ModalView {
        kind_sel: "java",
        heading: p.t("server/feature.java_version.heading"),
        description_html: Some(format!(
            "<p>{}</p><p class=\"field-label\">{}</p>",
            crate::views::escape(&p.t("server/feature.java_version.description")),
            crate::views::escape(&p.t("server/feature.java_version.select_version"))
        )),
        submit: Some(p.t("server/feature.java_version.submit")),
        options,
        select_disabled: false,
        current,
        errors,
        ..base_view()
    }
}

/// Heading/body picking between the admin and user texts of one schema.
fn admin_aware(p: &ServerPanel, admin_key: &str, user_key: &str) -> String {
    let key =
        if p.user.is_admin() { format!("server/feature.{admin_key}") } else { format!("server/feature.{user_key}") };
    p.t(&key)
}

fn admin_aware_html(p: &ServerPanel, admin_key: &str, user_key: &str) -> String {
    bladeless_html(&admin_aware(p, admin_key, user_key))
}

/// Descriptions arrive as raw trusted HTML strings in the original
/// (`HtmlString`); keep them as-is here (only links are rewritten).
fn bladeless_html(s: &str) -> String {
    s.to_string()
}

/// The plugin-feature fragment: the plugin's page HTML without a submit button.
async fn plugin_modal(
    rt: &std::sync::Arc<pelican_plugins::runtime::Runtime>,
    p: &ServerPanel,
    plugin_id: &str,
    key: &str,
) -> WebResult<Response> {
    let page_id = format!("feature-{key}");
    let req = pelican_plugins::runtime::Request {
        method: "GET".into(),
        path: format!("/{page_id}"),
        query: p.query.clone().unwrap_or_default(),
        headers: Vec::new(),
        body: Vec::new(),
        locale: p.locale(),
        user: Some(user_json(&p.user.user, p.user.is_root_admin())),
        server: Some(server_json(&p.server)),
    };
    // The plugin's page record carries the display title when registered.
    let title = rt
        .get(plugin_id)
        .and_then(|l| {
            l.registration
                .pages
                .iter()
                .find(|pg| pg.panel == pelican_plugins::runtime::Panel::Server && pg.id == page_id)
                .cloned()
        })
        .map(|pg| crate::plugin_pages::label(&p.st, plugin_id, &p.locale(), &pg.title))
        .unwrap_or_else(|| p.t_with("filament-actions::modal.heading", &[("title", key)]));
    let html = match rt.render_page(plugin_id, pelican_plugins::runtime::Panel::Server, &page_id, req).await {
        Ok(Ok(html)) => html,
        Ok(Err(e)) => format!("<div class=\"banner danger\"><p>{}</p></div>", e),
        Err(e) => format!("<div class=\"banner danger\"><p>{}</p></div>", e.to_string()),
    };
    p.render("plugins/feature.html", context! { heading => title, html })
}

/// `POST …` — perform the feature's action like `$schema->getAction()->action()`.
///
/// Success/failure are returned as notification payloads for `window.notify`
/// (Filament sends them live over Livewire; there is no page navigation to
/// carry session flashes on).
async fn perform(p: ServerPanel, FeatureKey(key): FeatureKey, form: Form) -> WebResult<Response> {
    match validate_state(&p, &key).await? {
        Kind::Eula => eula_action(&p).await,
        Kind::JavaVersion => java_action(&p, &form).await,
        Kind::GslToken => gsl_action(&p, &form, &key).await,
        // Informational schemas have no action (`->action(fn () => null)`); their
        // modals have no submit button either.
        Kind::PidLimit | Kind::SteamDiskSpace => notify_json("success", None, None),
    }
}

/// MinecraftEulaSchema: write `eula=true` through Wings, then restart.
async fn eula_action(p: &ServerPanel) -> WebResult<Response> {
    let outcome: Result<(), String> = async {
        daemon_put_file(p, "eula.txt", b"eula=true").await.map_err(|e| e.to_string())?;
        daemon_restart(p).await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            notify_json("success", Some(p.t("server/feature.eula.accepted")), Some(p.t("server/feature.restart_now")))
        }
        Err(msg) => notify_json("danger", Some(p.t("server/feature.eula.failed")), Some(msg)),
    }
}

/// JavaVersionSchema: save the chosen docker image, activity, restart.
async fn java_action(p: &ServerPanel, form: &Form) -> WebResult<Response> {
    let state = form.get("image");
    // Select rules (`required`, the egg's options, `notIn($server->image)`) fail inline
    // like Filament's form validation; the attribute is the lcfirst label.
    let b = startup::bundle(p).await?;
    let options = startup::flip_options(b.egg.docker_images.as_ref().map(|j| &j.0).unwrap_or(&serde_json::Value::Null));
    let label = p.t("server/feature.java_version.docker_image");
    let attribute = match label.chars().next() {
        Some(c) => c.to_lowercase().chain(label.chars().skip(1)).collect::<String>(),
        None => label.clone(),
    };
    let rule = if state.is_empty() {
        Some("validation.required")
    } else if !options.iter().any(|(v, _)| *v == state) {
        Some("validation.in")
    } else if state == b.server.image {
        Some("validation.not_in")
    } else {
        None
    };
    if let Some(rule) = rule {
        let msg = p.t_with(rule, &[("attribute", &attribute)]);
        return java_view(p, &b, state.to_string(), vec![("image".into(), msg)]).render(p, "java_version").await;
    }
    let original = b.server.image.clone();
    let outcome: Result<(), String> = async {
        startup::save_server_field(p, None, Some(state.to_string())).await.map_err(|e| e.to_string())?;
        if original != state {
            p.activity_for_server("server:startup.image")
                .property("old", original.clone())
                .property("new", state)
                .log(&p.st)
                .await
                .map_err(|e| format!("activity log: {e:?}"))?;
        }
        daemon_restart(p).await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => notify_json(
            "success",
            Some(p.t("server/feature.java_version.updated")),
            Some(p.t("server/feature.restart_now")),
        ),
        Err(msg) => notify_json("danger", Some(p.t("server/feature.java_version.failed")), Some(msg)),
    }
}

/// GSLTokenSchema: validate against the egg variable's rules, store the new
/// STEAM_ACC value, activity, restart.
async fn gsl_action(p: &ServerPanel, form: &Form, key: &str) -> WebResult<Response> {
    let (var, existing) = gsl_variable(p).await?.ok_or(WebError::NotFound)?;
    let state = form.get("gsltoken").to_string();

    // Rules from the egg variable run on the submitted value (the closure-based
    // Validator of the original, message attribute replaced with the name).
    let field = crate::variables::VariableField::new(&var, None);
    let messages: Vec<(String, String)> = crate::variables::validate(std::slice::from_ref(&field), |_| state.clone())
        .into_iter()
        .map(|(_, msg)| ("gsltoken".to_string(), msg))
        .collect();
    if !messages.is_empty() {
        // Re-render the fragment with the entered value and field errors.
        return ModalView {
            kind_sel: "gsl",
            heading: p.t("server/feature.gsl_token.heading"),
            description_html: Some(blade_link(&p.t("server/feature.gsl_token.info"))),
            submit: Some(p.t("server/feature.gsl_token.submit")),
            env: var.env_variable.clone(),
            field_label: var.name.clone(),
            field_value: state,
            field_hint: if var.description.is_empty() { "\u{2014}".to_string() } else { var.description.clone() },
            options: Vec::new(),
            current: String::new(),
            select_disabled: false,
            errors: messages,
        }
        .render(p, key)
        .await;
    }

    // `$serverVariable->update()`, activity when changed, restart.
    let now = startup::ts_now();
    let outcome: Result<(), String> = async {
        let row = server_variables::Entity::find()
            .filter(server_variables::Column::ServerId.eq(p.server.id))
            .filter(server_variables::Column::VariableId.eq(var.id))
            .one(&p.st.db)
            .await
            .map_err(|e| e.to_string())?;
        let changed = existing.as_deref() != Some(state.as_str());
        let id = match row {
            Some(row) => {
                use sea_orm::sea_query::Expr;
                server_variables::Entity::update_many()
                    .col_expr(server_variables::Column::VariableValue, Expr::value(state.clone()))
                    .col_expr(server_variables::Column::UpdatedAt, Expr::value(now))
                    .filter(server_variables::Column::Id.eq(row.id))
                    .exec(&p.st.db)
                    .await
                    .map_err(|e| e.to_string())?;
                Some(row.id.0)
            }
            None => {
                use sea_orm::ActiveValue::Set;
                let am = server_variables::ActiveModel {
                    server_id: Set(p.server.id),
                    variable_id: Set(var.id),
                    variable_value: Set(state.clone()),
                    created_at: Set(Some(now)),
                    updated_at: Set(Some(now)),
                    ..Default::default()
                };
                pelican_db::write::insert_id(&p.st.db, &am).await.map_err(|e| e.to_string())?
            }
        };
        if changed {
            if let Some(id) = id {
                model_events::fire_id::<server_variables::Entity>(&p.st, model_events::Action::Updated, id).await;
            }
            p.activity_for_server("server:startup.edit")
                .property("variable", var.env_variable.clone())
                .property("old", existing.clone().unwrap_or_default())
                .property("new", state.clone())
                .log(&p.st)
                .await
                .map_err(|e| format!("activity log: {e:?}"))?;
        }
        daemon_restart(p).await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => notify_json(
            "success",
            Some(p.t("server/feature.gsl_token.updated")),
            Some(p.t("server/feature.restart_now")),
        ),
        Err(msg) => notify_json("danger", Some(p.t("server/feature.gsl_token.failed")), Some(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blade_links_are_rewritten() {
        let s = "By pressing you agree to <x-filament::link href=\"https://minecraft.net/eula\" target=\"_blank\">Minecraft EULA </x-filament::link>.";
        assert_eq!(
            blade_link(s),
            "By pressing you agree to <a class=\"btn-link\" href=\"https://minecraft.net/eula\" target=\"_blank\">Minecraft EULA </a>."
        );
        assert_eq!(blade_link("plain text"), "plain text");
        // unterminated tags pass through untouched
        assert_eq!(blade_link("<x-filament::link href="), "<x-filament::link href=");
    }

    #[test]
    fn kind_permissions_match_the_schemas() {
        assert_eq!(Kind::parse("eula").unwrap().permission(), Some("file.update"));
        assert_eq!(Kind::parse("java_version").unwrap().permission(), Some("startup.docker-image"));
        assert_eq!(Kind::parse("gsl_token").unwrap().permission(), Some("startup.update"));
        assert_eq!(Kind::parse("pid_limit").unwrap().permission(), None);
        assert_eq!(Kind::parse("steam_disk_space").unwrap().permission(), None);
        assert!(Kind::parse("unknown_feature").is_none());
    }

    #[test]
    fn feature_activation_semantics() {
        let egg_none = test_egg(None);
        assert!(feature_active(&egg_none, "eula"), "no list → every feature");
        let egg_list = test_egg(Some(DbJson(json!(["eula", "gsl_token"]))));
        assert!(feature_active(&egg_list, "eula"));
        assert!(!feature_active(&egg_list, "java_version"));
        assert!(!feature_active(&egg_list, ""));
    }

    fn test_egg(features: Option<DbJson>) -> eggs::Model {
        use pelican_db::{Bool, Int};
        eggs::Model {
            id: Int(1),
            name: "Egg".into(),
            description: None,
            created_at: None,
            updated_at: None,
            config_from: None,
            config_stop: None,
            config_logs: None,
            config_startup: None,
            config_files: None,
            script_install: None,
            script_is_privileged: Bool(false),
            script_entry: String::new(),
            script_container: String::new(),
            copy_script_from: None,
            uuid: None,
            author: String::new(),
            features,
            docker_images: None,
            update_url: None,
            file_denylist: None,
            force_outgoing_ip: Bool(false),
            tags: None,
            startup_commands: None,
        }
    }

    #[test]
    fn feature_modal_template_renders_for_every_kind() {
        for kind_sel in ["confirm", "info", "java", "gsl"] {
            let view = ModalView {
                kind_sel: kind_sel,
                heading: "Heading".into(),
                description_html: Some("<span>x</span>".into()),
                submit: if kind_sel == "info" || kind_sel == "plugin" { None } else { Some("Accept".into()) },
                options: vec![("img:a".into(), "Image A".into()), ("img:b".into(), "Image B".into())],
                current: "img:b".into(),
                select_disabled: false,
                env: "STEAM_ACC".into(),
                field_label: "GSL Token".into(),
                field_value: "old-token".into(),
                field_hint: "Steam account".into(),
                errors: vec![],
            };
            crate::views::render("server/feature.html", view.ctx("/server/x/console/feature/eula")).expect("renders");
        }
    }

    #[test]
    fn gsl_error_rerender_template_has_errors() {
        let view = ModalView {
            kind_sel: "gsl",
            heading: "h".into(),
            field_label: "GSL Token".into(),
            errors: vec![("gsltoken".into(), "The GSL Token field is required.".into())],
            ..base_view()
        };
        let html = crate::views::render("server/feature.html", view.ctx("/x")).expect("renders");
        assert!(html.contains("has-error") || html.contains("is required"), "error surfaced: {html}");
    }
}
