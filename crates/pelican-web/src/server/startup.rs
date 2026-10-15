//! Server panel › Startup (app/Filament/Server/Pages/Startup.php): the
//! startup-command and docker-image selects with live saving, the
//! `PreviewStartupAction` over the resolved-command textarea and the
//! user-viewable variables grid — each variable is its own form standing in
//! for `StartupVariable::afterStateUpdated` → `Startup::update()`.

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::context;
use pelican_api::binding::Bound;
use pelican_api::model_events;
use pelican_api::pipeline::Ctx;
use pelican_api::services::{LogActivity, server_saved};
use pelican_core::activity::morph;
use pelican_core::server_config::{ServerBundle, php_assoc};
use pelican_db::entities::{egg_variables, server_variables};
use pelican_db::{Int, Timestamp};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::forms::Form;
use pelican_core::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/startup", get(page))
        .route("/server/{server}/startup/command", post(command))
        .route("/server/{server}/startup/image", post(image))
        .route("/server/{server}/startup/variable/{id}", post(variable))
}

/// A client-API `Ctx` for the domain functions (`server_saved`): ip and actor
/// like this web session, default subject = the server.
pub(crate) fn api_ctx(p: &ServerPanel) -> Result<Ctx, WebError> {
    let ip: IpAddr = p.ip.parse().map_err(|_| WebError::internal("invalid request ip"))?;
    Ok(Ctx {
        ip,
        user: Some(Arc::new(p.user.clone())),
        bound: Arc::new(Bound { server: Some(p.server.clone()), ..Default::default() }),
        node: None,
        root: p.st.config.app_url.clone(),
        subject: Some((morph::SERVER, p.server.id.0)),
    })
}

pub(crate) fn ts_now() -> Timestamp {
    Timestamp(pelican_core::time::now())
}

/// `StartupCommandService`'s bundle of this panel's server.
pub(crate) async fn bundle(p: &ServerPanel) -> Result<ServerBundle, WebError> {
    let server = p.server.clone();
    ServerBundle::load(&p.st.db, server).await.map_err(|e| match e {
        pelican_core::server_config::ConfigError::Db(d) => d.into(),
        pelican_core::server_config::ConfigError::Php(c) => WebError::internal(c),
    })
}

/// PHP `array_flip` over an egg JSON map/list: object `{label: value}` yields
/// `(value, label)` select options in insertion order; a plain list maps each
/// entry to itself.
pub(crate) fn flip_options(j: &Value) -> Vec<(String, String)> {
    match php_assoc(j.clone()) {
        Value::Object(m) => m.into_iter().map(|(k, v)| (pelican_core::php::string(&v), k)).collect(),
        Value::Array(a) => a
            .into_iter()
            .map(|v| {
                let s = pelican_core::php::string(&v);
                (s.clone(), s)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `$startups[$command]`-style label lookup for activity properties.
fn option_label(options: &[(String, String)], value: &str) -> String {
    options.iter().find(|(v, _)| v == value).map(|(_, l)| l.clone()).unwrap_or_default()
}

/// The egg behind the panel's server (`ErrorException` when missing).
pub(crate) async fn egg_of(p: &ServerPanel) -> Result<pelican_db::entities::eggs::Model, WebError> {
    use pelican_db::entities::eggs;
    eggs::Entity::find_by_id(p.server.egg_id).one(&p.st.db).await?.ok_or(WebError::internal("ErrorException"))
}

/// Back-link POSTs return to; `_back` only when it targets this panel.
fn back_url(form: &Form, p: &ServerPanel) -> String {
    form.back(&format!("{}/startup", p.base_url()))
}

/// Select data rendered into the command/image fields (`options` = [value,
/// label] pairs from array_flip; `current` = the selected value).
#[derive(serde::Serialize)]
struct SelectField {
    options: Vec<(String, String)>,
    current: String,
    enabled: bool,
}

/// `Server::ensureVariablesExist()`: firstOrCreate rows with the default values.
/// A side effect of loading the repeater relationship in the original.
async fn ensure_variables_exist<C: ConnectionTrait>(db: &C, p: &ServerPanel, b: &ServerBundle) -> Result<(), WebError> {
    let now = ts_now();
    for (var, value) in &b.variables {
        if value.is_some() {
            continue; // joined row exists (`variable_value` is NOT NULL)
        }
        let am = server_variables::ActiveModel {
            server_id: Set(p.server.id),
            variable_id: Set(var.id),
            variable_value: Set(var.default_value.clone()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        };
        // `ServerVariable::firstOrCreate(...)` in `Server::ensureVariablesExist()`
        // (created event for the missing rows).
        if let Some(new_id) = pelican_db::write::insert_id(db, &am).await? {
            model_events::fire_id::<server_variables::Entity>(&p.st, model_events::Action::Created, new_id).await;
        }
    }
    Ok(())
}

/// User-viewable variables ordered by `sort`
/// (`orderByPowerJoins('variable.sort')`; MySQL puts NULLs first).
fn visible_vars(b: &ServerBundle) -> Vec<crate::variables::VariableField> {
    let mut vars: Vec<&(egg_variables::Model, Option<String>)> =
        b.variables.iter().filter(|(v, _)| v.user_viewable.0).collect();
    vars.sort_by_key(|(v, _)| v.sort.map(|s| (0i8, s.0)).unwrap_or((-1i8, 0)));
    vars.into_iter().map(|(v, sv)| crate::variables::VariableField::new(v, sv.as_deref())).collect()
}

/// Data per variable field of the grid (`StartupVariable` component).
#[derive(serde::Serialize)]
struct VarView {
    id: i64,
    name: String,
    description: String,
    env: String,
    value: String,
    default: String,
    kind: crate::variables::Kind,
    /// `in:` select options as [value, label] pairs.
    options: Vec<(String, String)>,
    required: bool,
    disabled: bool,
    rules: String,
    /// extra input attributes of number fields (`min=`/`max=`).
    attrs: String,
}

impl From<crate::variables::VariableField> for VarView {
    fn from(f: crate::variables::VariableField) -> VarView {
        let attrs = match (f.min, f.max) {
            (Some(min), Some(max)) => format!("min=\"{min}\" max=\"{max}\""),
            (Some(min), None) => format!("min=\"{min}\""),
            (None, Some(max)) => format!("max=\"{max}\""),
            (None, None) => String::new(),
        };
        VarView {
            id: f.id,
            name: f.name,
            description: f.description,
            env: f.env,
            value: f.value,
            default: f.default,
            kind: f.kind,
            options: f.options.iter().map(|o| (o.clone(), o.clone())).collect(),
            required: f.required,
            disabled: !f.editable,
            rules: f.rules.join("|"),
            attrs,
        }
    }
}

/// `GET /server/{server}/startup`.
async fn page(p: ServerPanel) -> WebResult {
    p.authorize("startup.read")?;
    let b = bundle(&p).await?;
    ensure_variables_exist(&p.st.db, &p, &b).await?;

    let egg = egg_of(&p).await?;
    let empty = json!([]);
    let empty_map = json!({});
    let command_options = flip_options(egg.startup_commands.as_ref().map(|j| &j.0).unwrap_or(&empty));
    let image_options = flip_options(egg.docker_images.as_ref().map(|j| &j.0).unwrap_or(&empty_map));
    // Select vs readonly "Custom …" display (`in_array($x, $cmds)` checks the
    // JSON values).
    let command_visible = command_options.iter().any(|(v, _)| *v == p.server.startup);
    let image_visible = image_options.iter().any(|(v, _)| *v == p.server.image);

    let can_update = p.can("startup.update");
    let can_image = p.can("startup.docker-image");

    // Repeater is disabled without `startup.update`; fields additionally
    // disabled when the variable itself is not editable.
    let vars: Vec<VarView> =
        visible_vars(&b).into_iter().map(|f| VarView { disabled: !can_update || !f.editable, ..f.into() }).collect();

    p.page(
        "startup",
        "server/startup.html",
        context! {
            title => p.t("server/startup.title"),
            base => p.base_url(),
            raw_startup => p.server.startup.clone(),
            preview_startup => pelican_api::client::data::startup_command(&b, false),
            cmd_select => if command_visible {
                Some(SelectField { options: command_options, current: p.server.startup.clone(), enabled: can_update })
            } else {
                None
            },
            img_select => if image_visible {
                Some(SelectField { options: image_options, current: p.server.image.clone(), enabled: can_image })
            } else {
                None
            },
            can_update => can_update,
            can_image => can_image,
            vars => vars,
        },
    )
}

/// `forceFill([...])->saveOrFail()` on this server plus its `saved` hook.
pub(crate) async fn save_server_field(
    p: &ServerPanel,
    startup: Option<String>,
    image: Option<String>,
) -> Result<(), WebError> {
    use pelican_db::entities::servers;
    // Eloquent isDirty: `forceFill` only marks a field dirty when its value
    // actually differs from the stored one.
    let changed = startup.as_ref().is_some_and(|s| *s != p.server.startup)
        || image.as_ref().is_some_and(|i| *i != p.server.image);
    let now = ts_now();
    let am = servers::ActiveModel {
        id: Set(p.server.id),
        startup: startup.map(Set).unwrap_or(Set(p.server.startup.clone())),
        image: image.map(Set).unwrap_or_else(|| Set(p.server.image.clone())),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    pelican_db::write::update(&p.st.db, &am).await?;
    if changed {
        // `$server->forceFill([...])->saveOrFail()` → updated Server (a clean
        // model would not write at all).
        model_events::fire_id::<servers::Entity>(&p.st, model_events::Action::Updated, p.server.id.0).await;
    }
    // Model hooks fire through `saveOrFail`; failures surface like an uncaught
    // exception in the original.
    let ctx = api_ctx(p)?;
    server_saved(&p.st, &ctx, &p.st.db, &p.server).await.map_err(|_| WebError::internal("Server save hook failed"))?;
    Ok(())
}

/// `POST /server/{server}/startup/command` — the live startup select.
async fn command(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("startup.update")?;
    let back = back_url(&form, &p);
    let state = form.get("startup");
    let empty = json!([]);
    let egg = egg_of(&p).await?;
    let options = flip_options(egg.startup_commands.as_ref().map(|j| &j.0).unwrap_or(&empty));
    // Unknown submitted values cannot come from the rendered select.
    if !options.iter().any(|(v, _)| v == state) {
        return Err(WebError::Unprocessable);
    }

    let original = p.server.startup.clone();
    save_server_field(&p, Some(state.to_string()), None).await?;
    if original != state {
        // Properties map through array_flip(startup_commands).
        p.activity_for_server("server:startup.command")
            .property("old", option_label(&options, &original))
            .property("new", option_label(&options, state))
            .log(&p.st)
            .await
            .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
    }
    p.session.flash(
        "success",
        p.t("server/startup.notification_startup"),
        Some(p.t("server/startup.notification_startup_body")),
    );
    Ok(p.redirect(&back))
}

/// `POST /server/{server}/startup/image` — the live docker-image select.
async fn image(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("startup.docker-image")?;
    let back = back_url(&form, &p);
    let state = form.get("image");
    let empty_map = json!({});
    let egg = egg_of(&p).await?;
    let options = flip_options(egg.docker_images.as_ref().map(|j| &j.0).unwrap_or(&empty_map));
    if !options.iter().any(|(v, _)| v == state) {
        return Err(WebError::Unprocessable);
    }

    let original = p.server.image.clone();
    save_server_field(&p, None, Some(state.to_string())).await?;
    if original != state {
        p.activity_for_server("server:startup.image")
            .property("old", original)
            .property("new", state)
            .log(&p.st)
            .await
            .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
    }
    p.session.flash(
        "success",
        p.t("server/startup.notification_docker"),
        Some(p.t("server/startup.notification_docker_body")),
    );
    Ok(p.redirect(&back))
}

/// `POST …/variable/{id}` — one live variable field → `Startup::update()`.
async fn variable(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("startup.update")?;
    let back = back_url(&form, &p);
    // The repeater binds records to the panel's tenant; check membership.
    let var = egg_variables::Entity::find_by_id(Int(id))
        .one(&p.st.db)
        .await?
        .filter(|v| v.egg_id == p.server.egg_id && v.user_viewable.0)
        .ok_or(WebError::Forbidden)?;
    // Read-only variables silently keep their value.
    if !var.user_editable.0 {
        return Ok(p.redirect(&back));
    }

    // BooleanStateCast before `update()`: toggles arrive as '1'/'0'.
    let state = form.get("value").to_string();

    // Validation failures are notifications, not error pages
    // (`body(implode(', ', $validator->errors()->all()))`).
    let field = crate::variables::VariableField::new(&var, None);
    let messages: Vec<String> = crate::variables::validate(std::slice::from_ref(&field), |_| state.clone())
        .into_iter()
        .map(|(_, m)| m)
        .collect();
    if !messages.is_empty() {
        p.session.flash(
            "danger",
            p.t_with("server/startup.validation_fail", &[("variable", &var.name)]),
            Some(messages.join(", ")),
        );
        return Ok(p.redirect(&back));
    }

    // The repeater's records exist after a page load; a direct POST may miss
    // one (original = null then).
    let existing = server_variables::Entity::find()
        .filter(server_variables::Column::ServerId.eq(p.server.id))
        .filter(server_variables::Column::VariableId.eq(var.id))
        .one(&p.st.db)
        .await?;
    let original = existing.as_ref().map(|r| r.variable_value.clone());
    let stored = state.clone(); // empty strings stay '' (`$state ?? ''` after casts)

    // try { … } catch: every failure becomes the `fail` notification.
    let result: Result<bool, WebError> = async {
        match existing {
            Some(row) => {
                if row.variable_value != stored {
                    // `updateOrCreate` on an existing row (updated; dirty guard
                    // matches Eloquent).
                    let now = ts_now();
                    pelican_db::entities::server_variables::Entity::update_many()
                        .col_expr(
                            server_variables::Column::VariableValue,
                            sea_orm::sea_query::Expr::value(stored.clone()),
                        )
                        .col_expr(server_variables::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now))
                        .filter(server_variables::Column::Id.eq(row.id))
                        .exec(&p.st.db)
                        .await?;
                    model_events::fire_id::<server_variables::Entity>(&p.st, model_events::Action::Updated, row.id.0)
                        .await;
                }
            }
            None => {
                let am = server_variables::ActiveModel {
                    server_id: Set(p.server.id),
                    variable_id: Set(var.id),
                    variable_value: Set(stored.clone()),
                    created_at: Set(Some(ts_now())),
                    updated_at: Set(Some(ts_now())),
                    ..Default::default()
                };
                // `updateOrCreate` inserting a new row (created event).
                if let Some(new_id) = pelican_db::write::insert_id(&p.st.db, &am).await? {
                    model_events::fire_id::<server_variables::Entity>(&p.st, model_events::Action::Created, new_id)
                        .await;
                }
            }
        }
        // `$original !== $state` (strict PHP comparison on the stored value).
        Ok(match &original {
            Some(o) => o != state.as_str(),
            None => !state.is_empty(),
        })
    }
    .await;
    match result {
        Ok(changed) => {
            if changed {
                p.activity_for_server("server:startup.edit")
                    .property("variable", var.env_variable.clone())
                    .property("old", original.clone().unwrap_or_default())
                    .property("new", stored.clone())
                    .log(&p.st)
                    .await
                    .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
            }
            let body = format!("{} -> {}", original.unwrap_or_default(), stored);
            p.session.flash("success", p.t_with("server/startup.update", &[("variable", &var.name)]), Some(body));
            Ok(p.redirect(&back))
        }
        Err(e) => {
            tracing::warn!(error=?e, "startup variable update failed");
            p.session.flash("danger", p.t_with("server/startup.fail", &[("variable", &var.name)]), Some(e.to_string()));
            Ok(p.redirect(&back))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    fn sel() -> SelectField {
        SelectField {
            options: vec![("java -jar x.jar".into(), "Default".into())],
            current: "java -jar x.jar".into(),
            enabled: true,
        }
    }

    fn var(i: u8) -> VarView {
        VarView {
            id: i as i64 + 10,
            name: format!("Var{i}"),
            description: "desc".into(),
            env: format!("VAR_{i}"),
            value: if i == 2 { "1".into() } else { "val".into() },
            default: "def".into(),
            kind: match i {
                0 => crate::variables::Kind::Select,
                1 => crate::variables::Kind::Number,
                2 => crate::variables::Kind::Toggle,
                _ => crate::variables::Kind::Text,
            },
            options: if i == 0 { vec![("a".into(), "a".into()), ("b".into(), "b".into())] } else { Vec::new() },
            required: i == 0,
            disabled: i == 3,
            rules: "in:a,b".into(),
            attrs: String::new(),
        }
    }

    #[test]
    fn array_flip_semantics() {
        // object {label: value} → (value, label); list → identity.
        assert_eq!(
            flip_options(&json!({"Java 21": "ghcr.io/img:j21", "Default": "cmd"})),
            vec![("ghcr.io/img:j21".to_string(), "Java 21".to_string()), ("cmd".to_string(), "Default".to_string())]
        );
        assert_eq!(
            flip_options(&json!(["a", "b"])),
            vec![("a".to_string(), "a".to_string()), ("b".to_string(), "b".to_string())]
        );
        assert_eq!(flip_options(&json!("plain")), Vec::<(String, String)>::new());
        let opts = flip_options(&json!({"Cmd A": "c1", "Cmd B": "c2"}));
        assert_eq!(option_label(&opts, "c2"), "Cmd B");
        assert_eq!(option_label(&opts, "zz"), "");
    }

    #[test]
    fn select_membership_in_array_semantics() {
        // in_array checks the VALUES of the json field.
        let opts = flip_options(&json!({"Label": "img:tag"}));
        assert!(opts.iter().any(|(v, _)| v == "img:tag"));
        assert!(!opts.iter().any(|(v, _)| v == "Label"));
    }

    #[test]
    fn startup_page_template_renders() {
        for cmd_visible in [true, false] {
            for img_visible in [true, false] {
                for can_update in [true, false] {
                    let ctx = context! {
                        title => "Startup",
                        base => "/server/abcd1234",
                        raw_startup => "java {{SERVER_MEMORY}} {{APP_X}}",
                        preview_startup => "java 1024 hiddenvalue",
                        cmd_select => (cmd_visible).then(|| sel()),
                        img_select => (img_visible).then(|| sel()),
                        can_update => can_update,
                        can_image => true,
                        vars => vec![var(0), var(1), var(2), var(3)],
                    };
                    crate::views::render("server/startup.html", ctx).expect("template renders");
                }
            }
        }
    }
}
