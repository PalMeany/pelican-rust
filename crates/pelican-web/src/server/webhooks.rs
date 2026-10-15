//! Server panel › Webhooks (app/Filament/Server/Resources/Webhooks): list,
//! create/edit/view of server-scoped `WebhookConfiguration`, replicate,
//! delete (soft) and test delivery (`WebhookConfiguration::run()`).
//!
//! Mirrors `admin/webhooks.rs` structure, but the resource form has no
//! description/server fields (`WebhookResource::defaultForm`) and access is
//! gated by the spatie abilities of `WebhookConfigurationPolicy`
//! (`DefaultAdminPolicies`), not by subuser permissions.

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::application::webhooks as api_webhooks;
use pelican_api::{
    model_events,
    validation::{CustomRule, Validator, php_string},
};
use pelican_core::state::AppState;
use pelican_db::entities::webhook_configurations;
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect};
use serde_json::{Value, json};

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Table};

/// Test event for server webhooks (`WebhookConfiguration::run()` scope default).
const TEST_EVENT_SERVER: &str = "server:file.write";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/webhook", get(index))
        .route("/server/{server}/webhook/create", get(create_form).post(store))
        .route("/server/{server}/webhook/{id}", get(view_page))
        .route("/server/{server}/webhook/{id}/edit", get(edit_form).post(update))
        .route("/server/{server}/webhook/{id}/delete", post(delete_webhook))
        .route("/server/{server}/webhook/bulk-delete", post(bulk_delete))
        .route("/server/{server}/webhook/{id}/replicate", post(replicate))
        .route("/server/{server}/webhook/{id}/test", post(test))
}

fn t(p: &ServerPanel) -> impl Fn(&str) -> String + '_ {
    |k: &str| p.t(&format!("admin/webhook.{k}"))
}

/// WebhookConfigurationPolicy = DefaultAdminPolicies over the spatie `webhook`
/// permissions (Gate before-callbacks; a model argument is never a Server).
fn can(p: &ServerPanel, ability: &str) -> bool {
    p.user.gate_before(ability)
}

/// Records of this tenant only (Filament binds `{record}` within the tenant);
/// deleted rows never bind again (SoftDeletes global scope).
async fn find(p: &ServerPanel, id: i64) -> Result<webhook_configurations::Model, WebError> {
    webhook_configurations::Entity::find()
        .filter(webhook_configurations::Column::Id.eq(Int(id)))
        .filter(webhook_configurations::Column::DeletedAt.is_null())
        .filter(webhook_configurations::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// Event catalogue of `filamentCheckboxList(WebhookScope::Server)` (keys =
/// event, values = display names dumped from panel-orig; order matters).
fn event_options() -> Vec<(String, String)> {
    api_webhooks::EVENTS
        .get("server")
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
fn type_options(p: &ServerPanel, current: Option<&str>) -> Vec<(String, String)> {
    let mut opts = vec![(api_webhooks::REGULAR.to_string(), p.t("admin/webhook.regular"))];
    if let Some(ty) = current.filter(|t| !t.is_empty() && *t != api_webhooks::REGULAR) {
        opts.push((ty.to_string(), p.t_with("admin/webhook.unavailable_type_option", &[("type", ty)])));
    }
    opts
}

// --- list ------------------------------------------------------------------

/// GET /server/{server}/webhook (ListWebhooks).
async fn index(p: ServerPanel) -> WebResult<Response> {
    if !can(&p, "viewList webhook") {
        return Err(WebError::Forbidden);
    }
    let t = t(&p);
    let base = p.base_url();
    let url = format!("{base}/webhook");
    let opts = [10u64, 25, 50, 0];
    let lp = ListParams::parse(p.query.as_deref(), &opts, 10);

    let can_update = can(&p, "update webhook");
    let can_create = can(&p, "create webhook");
    let type_filter = lp.filter("type").filter(|v| !v.is_empty());

    let q = webhook_configurations::Entity::find()
        .filter(webhook_configurations::Column::DeletedAt.is_null())
        .filter(webhook_configurations::Column::Scope.eq("server"))
        .filter(webhook_configurations::Column::ServerId.eq(p.server.id));
    let total = match &type_filter {
        Some(ty) => q.clone().filter(webhook_configurations::Column::Type.eq(&**ty)).count(&p.st.db).await?,
        None => q.clone().count(&p.st.db).await?,
    };
    // No searchable/sortable columns in WebhookResource::defaultTable; id asc.
    let mut q = match &type_filter {
        Some(ty) => q.filter(webhook_configurations::Column::Type.eq(&**ty)),
        None => q,
    }
    .order_by_asc(webhook_configurations::Column::Id);
    if let Some((limit, offset)) = lp.limit_offset() {
        q = q.limit(limit).offset(offset);
    }
    let rows_models = q.all(&p.st.db).await?;

    let mut table = Table::new("webhooks", &url);
    table.columns =
        vec![Column::new("type", t("type")), Column::new("name", t("name")), Column::new("endpoint", t("endpoint"))];
    for m in &rows_models {
        let url = if can_update { format!("{url}/{}/edit", m.id.0) } else { format!("{url}/{}", m.id.0) };
        let type_known = m.r#type.as_deref() == Some(api_webhooks::REGULAR);
        let mut row = Row::new(m.id.0.to_string()).url(url.clone());
        row = row.cell(
            "type",
            if type_known {
                Cell::Icon { icon: "world-www".into(), color: "primary".into(), tooltip: Some(t("regular")) }
            } else {
                Cell::Icon { icon: "puzzle-off".into(), color: "gray".into(), tooltip: Some(t("unavailable_type")) }
            },
        );
        row = row.cell("name", Cell::text(m.name.clone()));
        row = row.cell("endpoint", Cell::text(endpoint_display(&m.endpoint)));
        if can_update {
            row.group.push(Action::link(p.t("filament-actions::edit.single.label"), "pencil", url.clone()));
        } else {
            row.group.push(Action::link(p.t("filament-actions::view.single.label"), "eye", url.clone()));
        }
        if can_create {
            // ReplicateAction: modal-free, copies without timestamps.
            row.group.push(Action::post(
                p.t("filament-actions::replicate.single.label"),
                "box-multiple",
                format!("{base}/webhook/{}/replicate", m.id.0),
            ));
        }
        table.rows.push(row);
    }

    if can(&p, "delete webhook") {
        table.bulk.push(
            Action::post(p.t("filament-actions::delete.multiple.label"), "trash", format!("{url}/bulk-delete"))
                .color("danger")
                .confirm(p.t("filament-actions::modal.confirmation")),
        );
    }
    table.filters = vec![Filter {
        name: "type".into(),
        label: t("type"),
        options: vec![(api_webhooks::REGULAR.into(), t("regular"))],
        value: type_filter.unwrap_or_default().into(),
    }];
    table.empty_icon = "webhook".into();
    table.empty_heading = t("no_webhooks");
    table.finish(&lp, total, &opts, &p.locale());

    // CreateAction in the header is hidden until there is at least one webhook;
    // on an empty table it lives in the empty state instead.
    p.page(
        "webhook",
        "server/webhooks.html",
        context! {
            title => t("model_label_plural"),
            base => base,
            table => Mj::from_serialize(&table),
            can_create => can_create && total > 0,
            can_create_empty => can_create && total == 0,
            create_label => p.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
        },
    )
}

// --- form ------------------------------------------------------------------

struct FormData {
    name: String,
    endpoint: String,
    r#type: String,
    events: Vec<String>,
    headers: Vec<(String, String)>,
}

impl FormData {
    /// `mutateFormDataBeforeFill`: RegularSchema only ensures headers is an array.
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
            "name": self.name, "endpoint": self.endpoint,
            "type": self.r#type, "events": self.events, "headers": self.headers,
        }))
    }
}

/// `RegularSchema::DEFAULT_HEADERS`.
fn default_headers() -> Vec<(String, String)> {
    vec![("X-Webhook-Event".into(), "{{event}}".into())]
}

/// Form rules (Filament components) — WebhookConfiguration has no extra
/// `$validationRules`; CheckboxList(events) required with membership checks.
fn validate(p: &ServerPanel, data: &FormData) -> Result<Errors, WebError> {
    let t = t(p);
    let mut headers_map = serde_json::Map::new();
    for (k, v) in &data.headers {
        headers_map.insert(k.clone(), json!(v));
    }
    let null_if_blank = |s: &str| if s.is_empty() { Value::Null } else { Value::from(s) };
    let input = json!({
        "name": null_if_blank(&data.name),
        "endpoint": null_if_blank(&data.endpoint),
        "type": null_if_blank(&data.r#type),
        "events": Value::Array(data.events.iter().map(|e| Value::from(e.clone())).collect()),
        "headers": Value::Object(headers_map),
    });
    let mut v = Validator::for_value(&input);
    for (f, label) in [
        ("name", t("name")),
        ("endpoint", t("endpoint")),
        ("type", t("type")),
        ("events", t("events")),
        ("headers", t("headers")),
    ] {
        v.attribute_name(f, &label.to_lowercase());
    }
    v.rules("name", &["required", "string"]);
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
        // Every checked event must exist in the server catalogue.
        let allowed = event_options();
        for e in &data.events {
            if !allowed.iter().any(|(name, _)| name == e) {
                v.add_error("events", &format!("The selected events.{e} is invalid."));
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

async fn render_form(
    p: &ServerPanel,
    record: Option<&webhook_configurations::Model>,
    data: &FormData,
    errors: &Errors,
) -> WebResult<Response> {
    let t = t(p);
    let base = p.base_url();
    let mode = match record {
        Some(_) if can(p, "update webhook") => "edit",
        Some(_) => "view",
        None => "create",
    };
    let (title, action) = match record {
        Some(m) => (
            p.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &m.name)]),
            format!("{base}/webhook/{}/edit", m.id.0),
        ),
        None => (
            p.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
            format!("{base}/webhook/create"),
        ),
    };
    let readonly = mode == "view";
    let can_delete = mode == "edit" && can(p, "delete webhook");
    let has_events = !data.events.is_empty();
    let unknown_type = record.is_some() && data.r#type != api_webhooks::REGULAR;
    p.page(
        "webhook",
        "server/webhooks_form.html",
        context! {
            title => title,
            action => action,
            base => base,
            mode => mode,
            readonly => readonly,
            can_delete => can_delete,
            has_events => has_events,
            record => record.map(|m| context! { id => m.id.0 }),
            v => data.ctx(),
            headers => Mj::from_serialize(&data.headers),
            headers_default => Mj::from_serialize(&default_headers()),
            errors => Mj::from_serialize(errors),
            events_selected => data.events.clone(),
            events_options => event_options(),
            type_options => type_options(p, Some(&data.r#type)),
            unavailable_type_text =>
                unknown_type.then(|| p.t_with("admin/webhook.unavailable_type_text", &[("type", &data.r#type)])),
        },
    )
}

// --- actions ---------------------------------------------------------------

/// GET /server/{server}/webhook/create.
async fn create_form(p: ServerPanel) -> WebResult<Response> {
    if !can(&p, "create webhook") {
        return Err(WebError::Forbidden);
    }
    let data = FormData {
        name: String::new(),
        endpoint: String::new(),
        r#type: api_webhooks::REGULAR.into(),
        events: vec![],
        headers: default_headers(),
    };
    render_form(&p, None, &data, &Errors::default()).await
}

/// POST /server/{server}/webhook/create — CreateRecord::mutateFormDataBeforeCreate
/// pins scope=server and server_id to the current tenant.
async fn store(p: ServerPanel, form: Form) -> WebResult<Response> {
    if !can(&p, "create webhook") {
        return Err(WebError::Forbidden);
    }
    let data = FormData::from_form(&form);
    let errors = validate(&p, &data)?;
    if !errors.is_empty() {
        return render_form(&p, None, &data, &errors).await;
    }
    let now = Timestamp(pelican_core::time::now());
    let wh = pelican_db::write::insert(
        &p.st.db,
        webhook_configurations::ActiveModel {
            name: Set(data.name.clone()),
            description: Set(None),
            endpoint: Set(data.endpoint.clone()),
            r#type: Set(Some(if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })),
            scope: Set("server".into()),
            server_id: Set(Some(Int(p.server.id.0))),
            events: Set(Json(json!(data.events))),
            headers: Set(Some(Json(Value::Object(normalize_headers(&data.headers))))),
            payload: Set(None),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // CreateRecord → `WebhookConfiguration::create($data)`.
    model_events::created(&p.st, &wh).await;
    p.flash_success(p.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(p.redirect(&format!("{}/webhook/{}/edit", p.base_url(), wh.id.0)))
}

/// GET /server/{server}/webhook/{id} (ViewRecord).
async fn view_page(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    if !can(&p, "view webhook") {
        return Err(WebError::Forbidden);
    }
    let m = find(&p, id).await?;
    render_form(&p, Some(&m), &FormData::from_model(&m), &Errors::default()).await
}

/// GET /server/{server}/webhook/{id}/edit.
async fn edit_form(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    if !can(&p, "update webhook") {
        return Err(WebError::Forbidden);
    }
    let m = find(&p, id).await?;
    render_form(&p, Some(&m), &FormData::from_model(&m), &Errors::default()).await
}

/// POST /server/{server}/webhook/{id}/edit — scope and server_id are fixed by
/// the form-less fields of EditWebhook; description is not editable here
/// (`WebhookResource::defaultForm` has no description field).
async fn update(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    if !can(&p, "update webhook") {
        return Err(WebError::Forbidden);
    }
    let rec = find(&p, id).await?;
    let data = FormData::from_form(&form);
    let errors = validate(&p, &data)?;
    if !errors.is_empty() {
        return render_form(&p, Some(&rec), &data, &errors).await;
    }
    let am = webhook_configurations::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(rec.id),
        name: Set(data.name.clone()),
        endpoint: Set(data.endpoint.clone()),
        r#type: Set(Some(if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })),
        events: Set(Json(json!(data.events))),
        headers: Set(Some(Json(Value::Object(normalize_headers(&data.headers))))),
        updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
        ..Default::default()
    };
    // EditRecord → `$record->update($data)`; event only on actual change.
    let changed = data.name != rec.name
        || data.endpoint != rec.endpoint
        || rec.r#type.clone().unwrap_or_default()
            != (if data.r#type.is_empty() { api_webhooks::REGULAR.into() } else { data.r#type.clone() })
        || rec.events.0 != json!(data.events)
        || rec.headers.clone().map(|h| h.0) != Some(Value::Object(normalize_headers(&data.headers)));
    pelican_db::write::update(&p.st.db, &am).await?;
    if changed {
        model_events::fire_id::<webhook_configurations::Entity>(&p.st, model_events::Action::Updated, id).await;
    }
    p.flash_success(p.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(p.redirect(&format!("{}/webhook/{id}/edit", p.base_url())))
}

/// POST /server/{server}/webhook/{id}/delete — DeleteAction + SoftDeletes.
async fn delete_webhook(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    if !can(&p, "delete webhook") {
        return Err(WebError::Forbidden);
    }
    delete_one(&p, id).await?;
    p.flash_success(p.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(p.redirect(&format!("{}/webhook", p.base_url())))
}

/// `DeleteBulkAction` (soft deletes) over this server's webhooks.
async fn bulk_delete(p: ServerPanel, form: Form) -> WebResult<Response> {
    if !can(&p, "delete webhook") {
        return Err(WebError::Forbidden);
    }
    let ids = crate::table::bulk_ids(&form);
    let mut failed = 0;
    for id in &ids {
        if delete_one(&p, *id).await.is_err() {
            failed += 1;
        }
    }
    crate::table::bulk_delete_notify(&p, ids.len(), failed);
    Ok(p.redirect(&form.back(&format!("{}/webhook", p.base_url()))))
}

/// SoftDeletes: stamp `deleted_at` on a webhook of this server.
async fn delete_one(p: &ServerPanel, id: i64) -> Result<(), WebError> {
    let wh = find(p, id).await?;
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::update(
        &p.st.db,
        &webhook_configurations::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(wh.id),
            deleted_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    // SoftDeletes → `$record->delete()`; row now carries deleted_at.
    model_events::fire_id::<webhook_configurations::Entity>(&p.st, model_events::Action::Deleted, id).await;
    Ok(())
}

/// POST /server/{server}/webhook/{id}/replicate — ReplicateAction (no modal):
/// copies every attribute except id/timestamps, appends " Copy <Y-m-d H:i:s>"
/// and redirects to the replica's edit page.
async fn replicate(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    if !can(&p, "create webhook") {
        return Err(WebError::Forbidden);
    }
    let wh = find(&p, id).await?;
    let stamp = pelican_core::time::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let now = Timestamp(pelican_core::time::now());
    let replica = pelican_db::write::insert(
        &p.st.db,
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
    model_events::created(&p.st, &replica).await;
    p.flash_success(p.t("filament-actions::replicate.single.notifications.replicated.title"));
    Ok(p.redirect(&format!("{}/webhook/{}/edit", p.base_url(), replica.id.0)))
}

/// POST /server/{server}/webhook/{id}/test — EditRecord `test_now` action
/// calling `WebhookConfiguration::run()`; server-scope defaults to the
/// file.write sample, delivery runs inline (QUEUE_CONNECTION=sync).
async fn test(p: ServerPanel, RecordId(id): RecordId) -> WebResult<Response> {
    if !can(&p, "update webhook") {
        return Err(WebError::Forbidden);
    }
    let wh = find(&p, id).await?;
    api_webhooks::process_webhook_in(&p.st, &p.st.db, &wh, TEST_EVENT_SERVER, &*api_webhooks::SERVER_SAMPLE)
        .await
        .map_err(|_| WebError::internal("webhook delivery failed"))?;
    Ok(p.redirect(&format!("{}/webhook/{id}/edit", p.base_url())))
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
        assert_eq!(endpoint_display(&long).chars().count(), 60);
    }

    #[test]
    fn headers_are_saved_with_dashes() {
        let mut m = normalize_headers(&[("X Custom Header".into(), "v".into()), ("Plain".into(), "w".into())]);
        // Key order matters for the JSON column.
        let keys: Vec<&String> = m.keys().collect();
        assert_eq!(keys, ["X-Custom-Header", "Plain"]);
        m.remove("X-Custom-Header");
    }

    /// The form template renders in every mode (create / edit / view)
    /// without template errors.
    #[test]
    fn form_template_renders() {
        for mode in ["create", "edit", "view"] {
            let data = FormData {
                name: "Hook".into(),
                endpoint: "https://example.com/hook".into(),
                r#type: api_webhooks::REGULAR.into(),
                events: vec!["server:file.write".into()],
                headers: default_headers(),
            };
            let ctx = context! {
                title => "Webhook",
                action => "/server/abc/webhooks/1/edit",
                base => "/server/abc",
                mode => mode,
                readonly => (mode == "view"),
                can_delete => (mode == "edit"),
                has_events => true,
                record => context! { id => 1 },
                v => data.ctx(),
                headers => Mj::from_serialize(&data.headers),
                headers_default => Mj::from_serialize(&default_headers()),
                errors => Mj::from_serialize(&Errors::default()),
                events_selected => data.events.clone(),
                events_options => event_options(),
                type_options => vec![(api_webhooks::REGULAR.to_string(), "Regular".to_string())],
                unavailable_type_text =>
                    Mj::from_serialize(&Option::<String>::None),
            };
            crate::views::render("server/webhooks_form.html", ctx).expect("template renders");
        }
    }
}
