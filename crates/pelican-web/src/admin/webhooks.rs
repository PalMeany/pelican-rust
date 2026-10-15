//! Admin › Webhooks (app/Filament/Admin/Resources/Webhooks): list with
//! scope tabs, type/server filters and server grouping; create/edit/view of
//! `WebhookConfiguration` (always global scope on this panel), replicate,
//! delete (soft), test delivery (`WebhookConfiguration::run()`).
//!
//! Only the core webhook type (`regular`) is registered in the original panel
//! (see pelican_api::application::webhooks::REGULAR), so endpoint-based type
//! detection is a no-op here and payload editing is limited to the headers.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::application::webhooks as api_webhooks;
use pelican_api::model_events;
use pelican_api::validation::{CustomRule, Validator, php_string};
use pelican_core::state::AppState;
use pelican_db::entities::{servers, webhook_configurations};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Tab, Table};

const SLUG: &str = "webhooks";
/// Test event for global webhooks (`WebhookConfiguration::run()` default).
const TEST_EVENT_GLOBAL: &str = r"eloquent.created: App\Models\Server";

/// `RegularSchema::DEFAULT_HEADERS`.
fn default_headers() -> Vec<(String, String)> {
    vec![("X-Webhook-Event".into(), "{{event}}".into())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_shown_without_scheme() {
        assert_eq!(endpoint_display("https://example.com/hook/long-url"), "example.com/hook/long-url");
        assert_eq!(endpoint_display("http://a.b"), "a.b");
        assert_eq!(endpoint_display("not-a-url"), "not-a-url");
    }

    #[test]
    fn endpoint_is_limited_to_60_chars() {
        let long = format!("{}{}", "https://x.com/", "y".repeat(80));
        let shown = endpoint_display(&long);
        assert_eq!(shown.chars().count(), 60);
    }

    #[test]
    fn headers_are_saved_with_dashes() {
        let mut m = normalize_headers(&[("X Custom Header".into(), "v".into()), ("Plain".into(), "w".into())]);
        // Key order matters for the JSON column.
        let keys: Vec<&String> = m.keys().collect();
        assert_eq!(keys, ["X-Custom-Header", "Plain"]);
        m.remove("X-Custom-Header");
    }

    /// The form template renders in every mode (create / edit with and without
    /// a registered type, view) without template errors.
    #[test]
    fn form_template_renders() {
        for mode in ["create", "edit", "view"] {
            let ctx = context! {
                title => "Webhook",
                action => "/admin/webhooks/1/edit",
                mode => mode,
                readonly => (mode == "view"),
                can_delete => true,
                can_test => true,
                has_events => (mode != "create"),
                record => context! { id => 1 },
                banners => vec![context! { status => "info", title => "Help", body => "Wrap variables in braces" }],
                v => FormData::ctx(&FormData {
                    name: "Hook".into(),
                    description: "d".into(),
                    endpoint: "https://example.com/hook".into(),
                    r#type: if mode == "edit" { "discord".into() } else { api_webhooks::REGULAR.into() },
                    events: vec!["server:file.write".into()],
                    headers: vec![("X-Webhook-Event".into(), "{{event}}".into())],
                }),
                headers => Mj::from_serialize(&vec![("X-Webhook-Event".to_string(), "{{event}}".to_string())]),
                headers_default => Mj::from_serialize(&default_headers()),
                errors => Mj::from_serialize(&Errors::default()),
                events_selected => vec!["server:file.write"],
                events_options => event_options("global"),
                type_options => vec![
                    ("regular".to_string(), "Regular".to_string()),
                    ("discord".to_string(), "discord (unavailable)".to_string()),
                ],
                unavailable_type_text => (mode == "edit").then(|| "unavailable text"),
                server_options => Vec::<(String, String)>::new(),
                server_id => "",
                scope => "global",
            };
            crate::views::render("admin/webhooks/form.html", ctx).expect("template renders");
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/webhooks", get(index))
        .route("/admin/webhooks/create", get(create_form).post(store))
        .route("/admin/webhooks/{id}", get(view))
        .route("/admin/webhooks/{id}/edit", get(edit_form).post(update))
        .route("/admin/webhooks/{id}/delete", post(delete))
        .route("/admin/webhooks/bulk-delete", post(bulk_delete))
        .route("/admin/webhooks/{id}/replicate", post(replicate))
        .route("/admin/webhooks/{id}/test", post(test))
}

fn t(admin: &Admin) -> impl Fn(&str) -> String + '_ {
    |k: &str| admin.t(&format!("admin/webhook.{k}"))
}

async fn find(admin: &Admin, id: i64) -> Result<webhook_configurations::Model, WebError> {
    // SoftDeletes scope: deleted rows never bind again.
    webhook_configurations::Entity::find_by_id(Int(id))
        .filter(webhook_configurations::Column::DeletedAt.is_null())
        .one(&admin.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// Event catalogue of `filamentCheckboxList(scope)` (keys = event, values =
/// display names dumped from panel-orig; order matters).
fn event_options(scope: &str) -> Vec<(String, String)> {
    api_webhooks::EVENTS
        .get(scope)
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
        .unwrap_or_default()
}

/// `str($state)->after('://')` + Filament `limit(60)`.
fn endpoint_display(endpoint: &str) -> String {
    let s = match endpoint.split_once("://") {
        Some((_, rest)) => rest,
        None => endpoint,
    };
    s.chars().take(60).collect()
}

/// Types offered by the type selector (`WebhookForm::typeSelector`): the core
/// registry plus the stored type when no plugin provides it anymore.
fn type_options(admin: &Admin, current: Option<&str>) -> Vec<(String, String)> {
    let mut opts = vec![(api_webhooks::REGULAR.to_string(), admin.t("admin/webhook.regular"))];
    if let Some(ty) = current.filter(|t| !t.is_empty() && *t != api_webhooks::REGULAR) {
        opts.push((ty.to_string(), admin.t_with("admin/webhook.unavailable_type_option", &[("type", ty)])));
    }
    opts
}

async fn server_options(admin: &Admin) -> Result<Vec<(String, String)>, WebError> {
    Ok(servers::Entity::find()
        .order_by_asc(servers::Column::Id)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .map(|s| (s.id.0.to_string(), s.id.0.to_string()))
        .collect())
}

/// GET /admin/webhooks (ListWebhookConfigurations).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList webhook")?;
    let t = t(&admin);
    let opts = [10u64, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);

    // Tabs per WebhookScope (`<scope>-webhooks`).
    let mut badges: HashMap<String, i64> = HashMap::new();
    let mut tabs = Vec::new();
    for scope in ["global", "server"] {
        let n = webhook_configurations::Entity::find()
            .filter(webhook_configurations::Column::DeletedAt.is_null())
            .filter(webhook_configurations::Column::Scope.eq(scope))
            .count(&admin.st.db)
            .await? as i64;
        badges.insert(scope.into(), n);
        tabs.push(Tab {
            key: format!("{scope}-webhooks"),
            label: admin.t(&format!("admin/webhook.tabs.{scope}")),
            badge: Some(n),
            url: String::new(),
            active: false,
        });
    }
    // Active tab narrows the query by scope.
    let scope_tab =
        p.tab.as_deref().and_then(|t| t.strip_suffix("-webhooks")).filter(|t| matches!(*t, "global" | "server"));

    let server_names: HashMap<i64, String> =
        servers::Entity::find().all(&admin.st.db).await?.into_iter().map(|s| (s.id.0, s.name)).collect();

    let mut q = webhook_configurations::Entity::find().filter(webhook_configurations::Column::DeletedAt.is_null());
    if let Some(scope) = scope_tab {
        q = q.filter(webhook_configurations::Column::Scope.eq(scope));
    }
    if let Some(ty) = p.filter("type").filter(|v| !v.is_empty()) {
        q = q.filter(webhook_configurations::Column::Type.eq(ty));
    }
    match p.filter("server_id").filter(|v| !v.is_empty()).and_then(|v| v.parse::<i64>().ok()) {
        Some(id) => q = q.filter(webhook_configurations::Column::ServerId.eq(Int(id))),
        // An unset filter is fine; a malformed one matches nothing.
        None if p.filter("server_id").is_some_and(|v| !v.is_empty()) => {
            q = q.filter(webhook_configurations::Column::Id.eq(Int(0)));
        }
        _ => {}
    }
    let all = q.order_by_asc(webhook_configurations::Column::Id).all(&admin.st.db).await?;

    // Grouping by server name (Table::$groups 'server.name').
    let grouped = p.filter("group") == Some("server");
    let mut rows_models: Vec<webhook_configurations::Model> = all;
    if grouped {
        rows_models.sort_by(|a, b| {
            let sa = a.server_id.and_then(|i| server_names.get(&i.0)).cloned().unwrap_or_default();
            let sb = b.server_id.and_then(|i| server_names.get(&i.0)).cloned().unwrap_or_default();
            (sa, a.id.0).cmp(&(sb, b.id.0))
        });
    }

    let can_update = admin.can("update webhook");
    let can_create = admin.can("create webhook");
    let has_rows = !rows_models.is_empty();
    let total = rows_models.len() as u64;
    let page_rows: Vec<webhook_configurations::Model> = match p.limit_offset() {
        Some((limit, offset)) => rows_models.iter().skip(offset as usize).take(limit as usize).cloned().collect(),
        None => rows_models,
    };

    let mut table = Table::new("webhooks", "/admin/webhooks");
    table.columns = vec![
        Column::new("name", t("name")),
        Column::new("description", t("table.description")),
        Column::new("type", t("type")),
        Column::new("server", t("server")),
        Column::new("endpoint", t("endpoint")),
    ];
    let mut last_heading = String::new();
    for m in &page_rows {
        let url =
            if can_update { format!("/admin/webhooks/{}/edit", m.id.0) } else { format!("/admin/webhooks/{}", m.id.0) };
        let type_known = m.r#type.as_deref() == Some(api_webhooks::REGULAR);
        let mut row = Row::new(m.id.0.to_string()).url(url.clone());
        row = row.cell("name", Cell::text(m.name.clone()));
        row = row.cell("description", Cell::text(m.description.clone().unwrap_or_default()));
        row = row.cell(
            "type",
            if type_known {
                Cell::Icon { icon: "world-www".into(), color: "primary".into(), tooltip: Some(t("regular")) }
            } else {
                Cell::Icon { icon: "puzzle-off".into(), color: "gray".into(), tooltip: Some(t("unavailable_type")) }
            },
        );
        let server_name = m.server_id.and_then(|i| server_names.get(&i.0)).cloned();
        row = row.cell(
            "server",
            match &server_name {
                Some(n) => Cell::Badge {
                    text: n.clone(),
                    color: "info".into(),
                    icon: Some("server-2".into()),
                    tooltip: None,
                    copy: None,
                },
                None => Cell::text("—"),
            },
        );
        row = row.cell("endpoint", Cell::text(endpoint_display(&m.endpoint)));
        if grouped {
            let heading = server_name.unwrap_or_else(|| "—".into());
            if heading != last_heading {
                last_heading = heading.clone();
                row.heading = Some((heading, None));
            }
        }
        if can_update {
            row.actions.push(Action::link(admin.t("filament-actions::edit.single.label"), "pencil", url.clone()));
        } else {
            row.actions.push(Action::link(admin.t("filament-actions::view.single.label"), "eye", url.clone()));
        }
        if can_create {
            // ReplicateAction: modal-free, copies without timestamps, redirect to edit.
            row.group.push(Action::post(
                admin.t("filament-actions::replicate.single.label"),
                "box-multiple",
                format!("/admin/webhooks/{}/replicate", m.id.0),
            ));
        }
        table.rows.push(row);
    }
    if admin.can("delete webhook") {
        table.bulk.push(
            Action::post(admin.t("filament-actions::delete.multiple.label"), "trash", "/admin/webhooks/bulk-delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
        );
    }

    table.tabs = tabs;
    let mut filters = vec![
        Filter {
            name: "type".into(),
            label: t("type"),
            options: vec![(api_webhooks::REGULAR.into(), t("regular"))],
            value: p.filter("type").unwrap_or("").into(),
        },
        Filter {
            name: "server_id".into(),
            label: t("server"),
            options: server_options(&admin).await?,
            value: p.filter("server_id").unwrap_or("").into(),
        },
    ];
    if has_rows || grouped {
        filters.push(Filter {
            name: "group".into(),
            label: admin.t("filament-tables::table.grouping.fields.group.label"),
            options: vec![("server".into(), t("server"))],
            value: if grouped { "server" } else { "" }.into(),
        });
    }
    table.filters = filters;
    table.empty_icon = "webhook".into();
    table.empty_heading = t("no_webhooks");
    table.finish(&p, total, &opts, &admin.locale());

    let create = (can_create && scope_tab != Some("server")).then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => "/admin/webhooks/create",
        }
    });
    admin
        .page(
            SLUG,
            "admin/list.html",
            context! {
                title => t("model_label_plural"),
                table => Mj::from_serialize(&table),
                create => create,
            },
        )
        .await
}

// --- form ------------------------------------------------------------------

struct FormData {
    name: String,
    description: String,
    endpoint: String,
    r#type: String,
    events: Vec<String>,
    headers: Vec<(String, String)>,
}

impl FormData {
    fn from_model(m: &webhook_configurations::Model) -> FormData {
        let mut headers = Vec::new();
        if let Value::Object(map) =
            m.headers.as_ref().map(|j| pelican_core::server_config::php_assoc(j.0.clone())).unwrap_or(Value::Null)
        {
            for (k, v) in map {
                headers.push((k, php_string(&v)));
            }
        }
        let events = match pelican_core::server_config::php_assoc(m.events.0.clone()) {
            Value::Array(a) => a.iter().map(|v| php_string(v)).collect(),
            other => vec![php_string(&other)],
        };
        FormData {
            name: m.name.clone(),
            description: m.description.clone().unwrap_or_default(),
            endpoint: m.endpoint.clone(),
            r#type: m.r#type.clone().unwrap_or_else(|| api_webhooks::REGULAR.into()),
            events,
            headers,
        }
    }

    fn from_form(f: &Form) -> FormData {
        let keys = f.list("headers_key");
        let vals = f.list("headers_val");
        let headers =
            keys.into_iter().zip(vals).filter(|(k, v)| !(k.trim().is_empty() && v.trim().is_empty())).collect();
        FormData {
            name: f.get("name").trim().into(),
            description: f.get("description").trim().into(),
            endpoint: f.get("endpoint").trim().into(),
            r#type: {
                let ty = f.get("type");
                if ty.is_empty() { api_webhooks::REGULAR.into() } else { ty.to_string() }
            },
            events: f.list("events"),
            headers,
        }
    }

    fn ctx(&self) -> Mj {
        Mj::from_serialize(json!({
            "name": self.name, "description": self.description, "endpoint": self.endpoint,
            "type": self.r#type, "events": self.events, "headers": self.headers,
        }))
    }
}

/// Selected event strings in template form.
fn selected_events(data: &FormData) -> Vec<String> {
    data.events.clone()
}

async fn render_form(
    admin: &Admin,
    record: Option<&webhook_configurations::Model>,
    data: &FormData,
    errors: &Errors,
) -> WebResult {
    let t = t(admin);
    let mode = match record {
        Some(_) if admin.can("update webhook") => "edit",
        Some(_) => "view",
        None => "create",
    };
    let scope = match record.and_then(|m| m.server_id) {
        Some(_) => "server",
        _ => "global",
    };
    let events_opts = event_options(scope);
    let (title, action) = match record {
        Some(m) => (
            admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &m.name)]),
            format!("/admin/webhooks/{}/edit", m.id.0),
        ),
        None => (
            admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
            "/admin/webhooks/create".into(),
        ),
    };
    let readonly = mode == "view";
    let can_delete = record.is_some() && admin.can("delete webhook");
    let can_test = record.is_some() && admin.can("update webhook");
    let unknown_type = record.is_some() && data.r#type != api_webhooks::REGULAR;
    let banners = if mode == "create" || mode == "edit" {
        vec![context! { status => "info", title => t("help"), body => t("help_text") }]
    } else {
        vec![]
    };
    admin
        .page(
            SLUG,
            "admin/webhooks/form.html",
            context! {
                title => title,
                action => action,
                mode => mode,
                tab => admin.tab(&["information", "payload", "events"]),
                readonly => readonly,
                can_delete => can_delete,
                can_test => can_test,
                has_events => !data.events.is_empty(),
                record => record.map(|m| context! { id => m.id.0 }),
                banners => banners,
                v => data.ctx(),
                headers => Mj::from_serialize(&data.headers),
                headers_default => Mj::from_serialize(&default_headers()),
                errors => Mj::from_serialize(errors),
                events_selected => selected_events(data),
                events_options => events_opts,
                type_options => type_options(admin, Some(&data.r#type)),
                unavailable_type_text =>
                    unknown_type.then(|| admin.t_with("admin/webhook.unavailable_type_text", &[("type", &data.r#type)])),
                server_options => server_options(admin).await?,
                server_id => record.and_then(|m| m.server_id).map(|i| i.0.to_string()).unwrap_or_default(),
                scope => scope,
            },
        )
        .await
}

/// Form rules (Filament components) — WebhookConfiguration has no extra
/// `$validationRules`.
async fn validate(admin: &Admin, data: &FormData, scope: &str) -> Result<Errors, WebError> {
    let t = t(admin);
    let mut headers_map = serde_json::Map::new();
    for (k, v) in &data.headers {
        headers_map.insert(k.clone(), json!(v));
    }
    let null_if_blank = |s: &str| if s.is_empty() { Value::Null } else { Value::from(s) };
    let input = json!({
        "name": null_if_blank(&data.name),
        "description": null_if_blank(&data.description),
        "endpoint": null_if_blank(&data.endpoint),
        "type": null_if_blank(&data.r#type),
        "events": Value::Array(data.events.iter().map(|e| Value::from(e.clone())).collect()),
        "headers": Value::Object(headers_map),
    });
    let mut v = Validator::for_value(&input);
    for (f, label) in [
        ("name", t("name")),
        ("description", t("description")),
        ("endpoint", t("endpoint")),
        ("type", t("type")),
        ("events", t("events")),
        ("headers", t("headers")),
    ] {
        v.attribute_name(f, &label.to_lowercase());
    }
    v.rules("name", &["required", "string"]);
    v.rules("description", &["required", "string"]);
    v.rules("endpoint", &["required", "string"]);
    v.rules("type", &["sometimes", "string"]);
    v.rules("headers.*", &["string", r"regex:/\A[^\r\n]*\z/"]);
    if data.events.is_empty() {
        // CheckboxList required: an empty selection fails.
        v.custom(
            "events",
            CustomRule {
                name: "required".into(),
                check: Box::new(|_, _| Some("The events field is required.".into())),
            },
        );
    } else {
        // Every checked event must exist in the scope catalogue.
        let allowed = event_options(scope);
        for (i, e) in data.events.iter().enumerate() {
            if !allowed.iter().any(|(name, _)| name == e) {
                v.add_error("events", &format!("The selected events.{i} is invalid."));
                break;
            }
        }
    }
    v.run();
    Ok(Errors::from_validator(&v))
}

/// RegularSchema::mutateFormDataBeforeSave — header names are saved with
/// spaces replaced by dashes.
fn normalize_headers(headers: &[(String, String)]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (k, v) in headers {
        out.insert(k.replace(' ', "-"), json!(v));
    }
    out
}

/// GET/POST /admin/webhooks/create.
async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create webhook")?;
    let data = FormData {
        name: String::new(),
        description: String::new(),
        endpoint: String::new(),
        r#type: api_webhooks::REGULAR.into(),
        events: vec![],
        headers: default_headers(),
    };
    render_form(&admin, None, &data, &Errors::default()).await
}

async fn store(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create webhook")?;
    let data = FormData::from_form(&form);
    // CreateWebhookConfiguration::mutateFormDataBeforeCreate forces GLOBAL and
    // drops any submitted server_id on the admin panel.
    let errors = validate(&admin, &data, "global").await?;
    if !errors.is_empty() {
        return render_form(&admin, None, &data, &errors).await;
    }
    let now = Timestamp(pelican_core::time::now());
    let wh = pelican_db::write::insert(
        &admin.st.db,
        webhook_configurations::ActiveModel {
            name: Set(data.name.clone()),
            description: Set(Some(data.description.clone())),
            endpoint: Set(data.endpoint.clone()),
            r#type: Set(Some(if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })),
            scope: Set("global".into()),
            server_id: Set(None),
            events: Set(Json(json!(data.events))),
            headers: Set(Some(Json(Value::Object(normalize_headers(&data.headers))))),
            payload: Set(None),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // CreateWebhookConfiguration → `WebhookConfiguration::create($data)`
    // (created event; model_events also resets the watched-webhooks cache).
    model_events::created(&admin.st, &wh).await;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("/admin/webhooks/{}/edit", wh.id.0)))
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view webhook")?;
    let m = find(&admin, id).await?;
    render_form(&admin, Some(&m), &FormData::from_model(&m), &Errors::default()).await
}

async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update webhook")?;
    let m = find(&admin, id).await?;
    render_form(&admin, Some(&m), &FormData::from_model(&m), &Errors::default()).await
}

async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update webhook")?;
    let rec = find(&admin, id).await?;
    let data = FormData::from_form(&form);
    // The hidden scope mirrors EditRecord state (derived from the disabled
    // server_id); the scope/server_id themselves never change through edit.
    let errors = validate(&admin, &data, &rec.scope).await?;
    if !errors.is_empty() {
        return render_form(&admin, Some(&rec), &data, &errors).await;
    }
    let am = webhook_configurations::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(rec.id),
        name: Set(data.name.clone()),
        description: Set(Some(data.description.clone())),
        endpoint: Set(data.endpoint.clone()),
        r#type: Set(Some(if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })),
        events: Set(Json(json!(data.events))),
        headers: Set(Some(Json(Value::Object(normalize_headers(&data.headers))))),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    // EditRecord → `$record->update($data)`; fires only on actual change.
    let changed = data.name != rec.name
        || rec.description != Some(data.description.clone())
        || data.endpoint != rec.endpoint
        || rec.r#type.clone().unwrap_or_default()
            != (if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })
        || rec.events.clone().0 != json!(data.events)
        || rec.headers.as_ref().map(|h| h.0.clone()) != Some(Value::Object(normalize_headers(&data.headers)));
    pelican_db::write::update(&admin.st.db, &am).await?;
    if changed {
        model_events::fire_id::<webhook_configurations::Entity>(&admin.st, model_events::Action::Updated, id).await;
    }
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&format!("/admin/webhooks/{id}/edit")))
}

/// POST /admin/webhooks/{id}/delete — DeleteAction + SoftDeletes.
async fn delete(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("delete webhook")?;
    delete_one(&admin, id).await?;
    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(admin.redirect("/admin/webhooks"))
}

/// `DeleteBulkAction('exclude_bulk_delete')` — soft deletes.
async fn bulk_delete(admin: Admin, form: Form) -> WebResult {
    admin.authorize("delete webhook")?;
    let ids = super::bulk_ids(&form);
    let mut failed = 0;
    for id in &ids {
        if delete_one(&admin, *id).await.is_err() {
            failed += 1;
        }
    }
    super::bulk_delete_notify(&admin, ids.len(), failed);
    Ok(admin.redirect(&form.back("/admin/webhooks")))
}

/// Webhook deletion (SoftDeletes): `deleted_at` stamp only.
async fn delete_one(admin: &Admin, id: i64) -> Result<(), WebError> {
    let wh = find(admin, id).await?;
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::update(
        &admin.st.db,
        &webhook_configurations::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(wh.id),
            deleted_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // SoftDeletes → `$record->delete()`; row now carries deleted_at.
    model_events::fire_id::<webhook_configurations::Entity>(&admin.st, model_events::Action::Deleted, id).await;
    Ok(())
}

/// POST /admin/webhooks/{id}/replicate — ReplicateAction (no modal): copies
/// every attribute except id/timestamps, appends " Copy <Y-m-d H:i:s>" and
/// redirects to the replica's edit page.
async fn replicate(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("create webhook")?;
    let wh = find(&admin, id).await?;
    let stamp = pelican_core::time::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let now = Timestamp(pelican_core::time::now());
    let replica = pelican_db::write::insert(
        &admin.st.db,
        webhook_configurations::ActiveModel {
            name: Set(format!("{} Copy {}", wh.name, stamp)),
            description: Set(wh.description.clone()),
            endpoint: Set(wh.endpoint.clone()),
            r#type: Set(wh.r#type.clone()),
            scope: Set(wh.scope.clone()),
            server_id: Set(wh.server_id),
            events: Set(wh.events.clone()),
            headers: Set(wh.headers.clone()),
            payload: Set(wh.payload.clone()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // ReplicateAction → `$replica->save()` (created event).
    model_events::created(&admin.st, &replica).await;
    admin.flash_success(admin.t("filament-actions::replicate.single.notifications.replicated.title"));
    Ok(admin.redirect(&format!("/admin/webhooks/{}/edit", replica.id.0)))
}

/// POST /admin/webhooks/{id}/test — EditRecord `test_now` action calling
/// `WebhookConfiguration::run()`; delivery runs inline (QUEUE_CONNECTION=sync).
async fn test(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update webhook")?;
    let wh = find(&admin, id).await?;
    let (event, sample) = if wh.scope == "server" {
        ("server:file.write", &*api_webhooks::SERVER_SAMPLE)
    } else {
        (TEST_EVENT_GLOBAL, &*api_webhooks::GLOBAL_SAMPLE)
    };
    api_webhooks::process_webhook_in(&admin.st, &admin.st.db, &wh, event, sample)
        .await
        .map_err(|_| WebError::Internal("webhook delivery failed".into()))?;
    Ok(admin.redirect(&format!("/admin/webhooks/{id}/edit")))
}
