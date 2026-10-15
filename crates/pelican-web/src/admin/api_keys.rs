//! Admin › API Keys (app/Filament/Admin/Resources/ApiKeys): application keys
//! (`key_type = 2`) only, create-only resource — the table shows the full
//! secret (`identifier . token`) with copy support; no view/edit pages exist.

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::validation::Validator;
use pelican_core::state::AppState;
use pelican_db::entities::{api_keys, users};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde_json::{Map, Value};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const SLUG: &str = "api-keys";
const KEY_TYPE_APPLICATION: i64 = 2;
/// `ApiKey::IDENTIFIER_LENGTH` / `ApiKey::KEY_LENGTH`.
const IDENTIFIER_LENGTH: usize = 16;
const KEY_LENGTH: usize = 32;
/// Length of the `papp_` prefix.
const PREFIX_LEN: usize = 5;

/// `ApiKey::DEFAULT_RESOURCE_NAMES`; order matters (storage and form layout).
const RESOURCES: [&str; 11] =
    ["server", "node", "allocation", "user", "egg", "database_host", "database", "mount", "role", "plugin", "webhook"];

/// Core resources, then those plugins register (`ApiKey::registerCustomResourceName`).
fn resources() -> Vec<String> {
    let mut out: Vec<String> = RESOURCES.iter().map(|r| (*r).to_string()).collect();
    for r in pelican_plugins::runtime::api_resources() {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/api-keys", get(index))
        .route("/admin/api-keys/create", get(create_form).post(store))
        .route("/admin/api-keys/{id}/delete", post(delete))
}

fn t(admin: &Admin) -> impl Fn(&str) -> String + '_ {
    |k: &str| admin.t(&format!("admin/apikey.{k}"))
}

/// `str($resource)->replace('_', ' ')->title()` label of a permission toggle.
fn permission_label(resource: &str) -> String {
    let mut out = String::new();
    for ch in resource.chars() {
        if ch == '_' {
            out.push(' ');
        } else if out.is_empty() || out.ends_with(' ') {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// ToggleButtons option set shared by every permission selector:
/// `(value, label, icon, color)` as in the original form.
fn permission_options(admin: &Admin) -> Vec<(String, String, &'static str, &'static str)> {
    let p = |k: &str| admin.t(&format!("admin/apikey.permissions.{k}"));
    vec![
        ("0".into(), p("none"), "book-off", "success"),
        ("1".into(), p("read"), "book", "warning"),
        ("3".into(), p("read_write"), "writing", "danger"),
    ]
}

fn dt(ts: Option<&Timestamp>) -> Option<String> {
    ts.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// Decrypted token of a stored key (Laravel `encrypted` cast); failure throws.
fn plain_token(st: &AppState, key: &api_keys::Model) -> Result<String, WebError> {
    st.enc.decrypt_string_lenient(&key.token).map_err(|_| WebError::internal("could not decrypt api key token"))
}

/// `GET /admin/api-keys` (ListApiKeys).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList apiKey")?;
    let t = t(&admin);
    let opts = [10u64, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let mut q = api_keys::Entity::find().filter(api_keys::Column::KeyType.eq(KEY_TYPE_APPLICATION));
    q = match p.sort.as_deref() {
        Some("created_at") if p.desc => q.order_by_desc(api_keys::Column::CreatedAt),
        Some("created_at") => q.order_by_asc(api_keys::Column::CreatedAt),
        Some("last_used_at") if p.desc => q.order_by_desc(api_keys::Column::LastUsedAt),
        Some("last_used_at") => q.order_by_asc(api_keys::Column::LastUsedAt),
        _ => q.order_by_asc(api_keys::Column::Id),
    };
    let rows = q.all(&admin.st.db).await?;
    let total = rows.len() as u64;
    let page: Vec<&api_keys::Model> = match p.limit_offset() {
        Some((limit, offset)) => rows.iter().skip(offset as usize).take(limit as usize).collect(),
        None => rows.iter().collect(),
    };

    // `user.username` column (TextColumn with a relation URL).
    let ids: Vec<i64> = page.iter().filter_map(|k| k.user_id.map(|i| i.0)).collect();
    let names: std::collections::HashMap<i64, String> = if ids.is_empty() {
        Default::default()
    } else {
        users::Entity::find()
            .filter(users::Column::Id.is_in(ids.clone()))
            .all(&admin.st.db)
            .await?
            .into_iter()
            .map(|u| (u.id.0, u.username))
            .collect()
    };
    let can_edit_user = admin.can("update user");

    let mut table = Table::new("api-keys", "/admin/api-keys");
    table.columns = vec![
        Column::new("key", t("table.key")),
        Column::new("memo", t("table.description")),
        Column::new("last_used_at", t("table.last_used")).sortable(),
        Column::new("created_at", t("table.created")).sortable(),
        Column::new("user", t("table.created_by")),
    ];
    for k in page {
        let mut row = Row::new(k.id.0.to_string());
        // Key column shows `identifier . token`, copyable from the list.
        let secret = plain_token(&admin.st, k)?;
        let full = format!("{}{}", k.identifier.as_deref().unwrap_or_default(), secret);
        row =
            row.cell("key", Cell::Text { text: full.clone(), desc: None, mono: true, copy: Some(full), tooltip: None });
        row = row.cell("memo", Cell::text(k.memo.as_deref().unwrap_or_default().chars().take(50).collect::<String>()));
        row = row.cell(
            "last_used_at",
            match dt(k.last_used_at.as_ref()) {
                Some(s) => Cell::text(s),
                None => Cell::text(t("table.never_used")),
            },
        );
        row = row.cell("created_at", Cell::text(dt(k.created_at.as_ref()).unwrap_or_default()));
        let owner = k.user_id.and_then(|i| names.get(&i.0)).cloned().unwrap_or_default();
        row = row.cell(
            "user",
            // The original links to the user's edit page when the viewer may
            // update that user; the shared table cell renderer has no link
            // support, so the name renders as plain text either way.
            match (can_edit_user, owner.is_empty()) {
                (true, false) => {
                    Cell::Badge { text: owner, color: "primary".into(), icon: None, tooltip: None, copy: None }
                }
                _ => Cell::text(owner),
            },
        );
        if admin.can("delete apiKey") {
            row.actions.push(
                Action::post(
                    admin.t("filament-actions::delete.single.label"),
                    "trash",
                    format!("/admin/api-keys/{}/delete", k.id.0),
                )
                .color("danger")
                .confirm(
                    admin.t_with("filament-actions::delete.single.modal.heading", &[("label", &t("model_label"))]),
                ),
            );
        }
        table.rows.push(row);
    }

    table.empty_icon = "key".into();
    table.empty_heading = t("empty");
    table.finish(&p, total, &opts, &admin.locale());

    let create = admin.can("create apiKey").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => "/admin/api-keys/create",
        }
    });
    admin
        .page(
            SLUG,
            "admin/list.html",
            context! { title => t("model_label_plural"), table => Mj::from_serialize(&table), create => create },
        )
        .await
}

// --- create -----------------------------------------------------------------

struct FormData {
    memo: String,
    allowed_ips: Vec<String>,
    /// Submitted state per resource, aligned with `RESOURCES`.
    permissions: Vec<String>,
    /// State of the live "set all" selector (default `0`).
    permissions_all: String,
}

impl FormData {
    fn from_form(f: &Form) -> FormData {
        FormData {
            memo: f.get("memo").trim().into(),
            allowed_ips: f.tags("allowed_ips"),
            permissions: resources()
                .iter()
                .map(|r| f.opt(&format!("permission_{r}")).unwrap_or("").to_string())
                .collect(),
            permissions_all: f.opt("permissions_all").unwrap_or("0").to_string(),
        }
    }

    /// Allowed IPs (`TagsInput` entries).
    fn ips(&self) -> Vec<String> {
        self.allowed_ips.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).map(String::from).collect()
    }

    fn ctx(&self) -> Mj {
        let perms: Map<String, Value> =
            resources().iter().zip(&self.permissions).map(|(r, v)| (r.clone(), Value::from(v.clone()))).collect();
        Mj::from_serialize(serde_json::json!({
            "memo": self.memo,
            "allowed_ips": self.allowed_ips,
            "permissions": Value::Object(perms),
            "permissions_all": self.permissions_all,
        }))
    }
}

async fn render_create(admin: &Admin, data: &FormData, errors: &Errors) -> WebResult {
    let t = t(admin);
    let resources: Vec<(String, String)> = resources().iter().map(|r| (r.clone(), permission_label(r))).collect();
    admin
        .page(
            SLUG,
            "admin/api_keys/create.html",
            context! {
                title => admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
                action => "/admin/api-keys/create",
                v => data.ctx(),
                errors => Mj::from_serialize(errors),
                permission_options => permission_options(admin),
                resources => resources,
            },
        )
        .await
}

/// Form field rules + `ApiKey::$validationRules` (memo / allowed_ips part).
fn validate(admin: &Admin, data: &FormData) -> Errors {
    let t = t(admin);
    let mut input = Map::new();
    input.insert("memo".into(), Value::from(data.memo.clone()));
    input.insert("allowed_ips".into(), Value::Array(data.ips().into_iter().map(Value::from).collect()));
    for (r, v) in resources().iter().zip(&data.permissions) {
        input.insert(format!("permission_{r}"), Value::from(v.clone()));
    }
    let input = Value::Object(input);
    let mut v = Validator::for_value(&input);
    v.attribute_name("memo", &t("description").to_lowercase());
    v.attribute_name("allowed_ips", &t("whitelist").to_lowercase());
    v.rules("memo", &["required", "string", "max:500"]);
    v.rules("allowed_ips", &["array"]);
    v.rules("allowed_ips.*", &["string"]);
    for r in resources() {
        let attr = format!("permission_{r}");
        v.attribute_name(&attr, &permission_label(&r).to_lowercase());
        v.rules(&attr, &["required", "integer", "min:0", "max:3"]);
    }
    v.run();
    Errors::from_validator(&v)
}

/// GET /admin/api-keys/create (CreateApiKey).
async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create apiKey")?;
    let data = FormData {
        memo: String::new(),
        allowed_ips: Vec::new(),
        permissions: vec![String::new(); resources().len()],
        permissions_all: "0".into(),
    };
    render_create(&admin, &data, &Errors::default()).await
}

/// POST /admin/api-keys/create (CreateApiKey::handleRecordCreation).
async fn store(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create apiKey")?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data);
    if !errors.is_empty() {
        return render_create(&admin, &data, &errors).await;
    }
    // Generated attributes: identifier/token/user/key type; permissions are
    // collected from the per-resource toggles into the JSON map.
    let identifier = format!("papp_{}", pelican_wings::jwt::str_random(IDENTIFIER_LENGTH - PREFIX_LEN));
    let secret = pelican_wings::jwt::str_random(KEY_LENGTH);
    let mut permissions = Map::new();
    for (r, v) in resources().iter().zip(&data.permissions) {
        if let Ok(n) = v.parse::<i64>() {
            permissions.insert((*r).to_string(), Value::from(n.clamp(0, 3)));
        }
    }
    let now = Timestamp(pelican_core::time::now());
    let am = api_keys::ActiveModel {
        user_id: Set(Some(Int(admin.user.id()))),
        key_type: Set(Int(KEY_TYPE_APPLICATION)),
        identifier: Set(Some(identifier)),
        token: Set(admin.st.enc.encrypt_string(&secret)),
        permissions: Set(Some(Json(Value::Object(permissions)))),
        allowed_ips: Set(Json(Value::Array(data.ips().into_iter().map(Value::from).collect()))),
        memo: Set(Some(data.memo.clone())),
        last_used_at: Set(None),
        expires_at: Set(None),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    pelican_db::write::insert(&admin.st.db, am).await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect("/admin/api-keys"))
}

/// POST /admin/api-keys/{id}/delete (DeleteAction on the list).
async fn delete(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("delete apiKey")?;
    let k = api_keys::Entity::find_by_id(Int(id))
        .filter(api_keys::Column::KeyType.eq(KEY_TYPE_APPLICATION))
        .one(&admin.st.db)
        .await?
        .ok_or(WebError::NotFound)?;
    api_keys::Entity::delete_by_id(k.id).exec(&admin.st.db).await?;
    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
    Ok(admin.redirect(&form.back("/admin/api-keys")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_labels_use_laravel_title_case() {
        assert_eq!(permission_label("server"), "Server");
        assert_eq!(permission_label("database_host"), "Database Host");
        assert_eq!(permission_label("webhook"), "Webhook");
    }

    #[test]
    fn identifier_is_prefix_plus_random_suffix() {
        let id = format!("papp_{}", pelican_wings::jwt::str_random(IDENTIFIER_LENGTH - PREFIX_LEN));
        assert_eq!(id.chars().count(), IDENTIFIER_LENGTH);
        assert!(id.starts_with("papp_"));
    }

    #[test]
    fn allowed_ips_are_trimmed() {
        let data = FormData {
            memo: String::new(),
            allowed_ips: crate::forms::parse_tags(r#"["127.0.0.1", " ", " 192.168.1.1 "]"#),
            permissions: vec![],
            permissions_all: "0".into(),
        };
        assert_eq!(data.ips(), vec!["127.0.0.1", "192.168.1.1"]);
    }

    #[test]
    fn create_template_renders() {
        let data = FormData {
            memo: "Panel script".into(),
            allowed_ips: vec!["127.0.0.1".into()],
            permissions: RESOURCES.iter().map(|_| "1".to_string()).collect(),
            permissions_all: "1".into(),
        };
        let ctx = context! {
            title => "Application API Key",
            action => "/admin/api-keys/create",
            v => data.ctx(),
            errors => Mj::from_serialize(&Errors::default()),
            permission_options => vec![
                ("0".to_string(), "None".to_string(), "book-off", "success"),
                ("1".to_string(), "Read".to_string(), "book", "warning"),
                ("3".to_string(), "Read & Write".to_string(), "writing", "danger"),
            ],
            resources => RESOURCES.iter().map(|r| (r.to_string(), permission_label(r))).collect::<Vec<_>>(),
        };
        crate::views::render("admin/api_keys/create.html", ctx).expect("template renders");
    }
}
