//! `/api/application/plugins` (`Application\Plugins\PluginController`, `PluginData`).
//! The rows come from the `plugins/` directory (the original's Sushi model), refreshed by
//! the status pass `PluginService::loadPlugins()` runs on every request.

use axum::Extension;
use axum::extract::{OriginalUri, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_plugins::{Config, Plugin, Status, lifecycle, updates};
use serde_json::{Map, Value, json};

use super::{Page, READ, WRITE};
use crate::envelope::item;
use crate::error::{ApiError, ApiResult};
use crate::input::{In, php_boolean};
use crate::json::PhpJson;
use crate::pipeline::Ctx;
use crate::query::{FilterValue, filters, sorts};
use crate::validation::Validator;

pub fn config(st: &AppState) -> Config {
    Config::from_env(
        &st.config.env,
        &st.config.base_path,
        crate::versions::current_comparable_version(),
        crate::versions::current_panel_version(),
    )
}

/// Plugin service errors: plain exceptions are 500s, upload problems 400s.
fn service_err(e: pelican_plugins::Error) -> ApiError {
    tracing::error!(error = %e, "plugin operation failed");
    match e {
        pelican_plugins::Error::InvalidUpload(m) => ApiError::display("InvalidFileUploadException", 400, m),
        pelican_plugins::Error::NotFound(_) => ApiError::model_not_found(),
        pelican_plugins::Error::Message(_) => ApiError::internal("Exception"),
    }
}

fn panel_exception() -> ApiError {
    ApiError::internal("PanelException")
}

/// `PluginData::fromModel()`.
pub async fn plugin_data(cfg: &Config, p: &Plugin) -> Value {
    let compatible = pelican_plugins::compat::is_compatible(p, cfg.panel_version.as_deref());
    let mut a = Map::new();
    a.insert("id".into(), json!(p.id));
    a.insert("name".into(), json!(p.name));
    a.insert("author".into(), json!(p.author));
    a.insert("version".into(), json!(p.version));
    a.insert("description".into(), json!(p.description));
    a.insert("category".into(), json!(p.category.as_str()));
    a.insert("url".into(), json!(p.url));
    a.insert("update_url".into(), json!(p.update_url));
    a.insert("namespace".into(), json!(p.namespace));
    a.insert("class".into(), json!(p.class));
    a.insert("panels".into(), json!(p.panel_list()));
    a.insert("panel_version".into(), json!(p.panel_version));
    let packages = p
        .composer_packages
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(pelican_core::server_config::php_assoc)
        .unwrap_or(Value::Null);
    a.insert("composer_packages".into(), packages);
    a.insert(
        "meta".into(),
        json!({
            "status": p.status.as_str(),
            "status_message": p.status_message,
            "load_order": p.load_order,
            "is_compatible": compatible,
            "update_available": updates::update_available(cfg, p).await,
            "can_enable": matches!(p.status, Status::Disabled | Status::Crashed) && compatible,
            "can_disable": matches!(p.status, Status::Enabled | Status::Incompatible),
        }),
    );
    item("plugin", Value::Object(a), vec![])
}

/// Boot (`loadPlugins`) then the request's rows.
fn rows(cfg: &Config) -> Vec<Plugin> {
    cfg.refresh_statuses();
    cfg.plugins()
}

/// `{plugin:id}` route binding: `where('id', $value)->firstOrFail()`.
fn bound(cfg: &Config, ctx: &Ctx) -> ApiResult<Plugin> {
    let id = ctx.bound.param("plugin").unwrap_or_default().to_string();
    rows(cfg).into_iter().find(|p| p.id == id).ok_or_else(ApiError::model_not_found)
}

fn column<'a>(p: &'a Plugin, name: &str) -> Option<&'a str> {
    match name {
        "id" => Some(&p.id),
        "name" => Some(&p.name),
        "author" => Some(&p.author),
        "category" => Some(p.category.as_str()),
        _ => None,
    }
}

/// Spatie's partial filter on the Sushi (SQLite) table: `LOWER(col) LIKE %value%`.
fn matches(value: Option<&str>, f: &FilterValue) -> bool {
    match f {
        FilterValue::List(items) => items.iter().any(|v| matches(value, v)),
        FilterValue::Null => true,
        other => {
            let needle = other.as_php_string().to_lowercase();
            needle.is_empty() || value.is_some_and(|v| v.to_ascii_lowercase().contains(&needle))
        }
    }
}

/// GET /plugins.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    OriginalUri(uri): OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("plugin", READ)?;
    let allowed = ["id", "name", "author", "category"];
    let fs = filters(&input, &allowed)?;
    let ss = sorts(&input, &allowed)?;
    let page = Page::new(&input, 10)?;
    let cfg = config(&st);
    let mut all: Vec<Plugin> =
        rows(&cfg).into_iter().filter(|p| fs.iter().all(|(n, v)| matches(column(p, n), v))).collect();
    // SQLite ORDER BY with BINARY collation; ties keep the scan order.
    for (name, desc) in ss.iter().rev() {
        all.sort_by(|a, b| {
            let o = column(a, name).cmp(&column(b, name));
            if *desc { o.reverse() } else { o }
        });
    }
    let total = all.len() as i64;
    let offset = ((page.page - 1) * page.per_page).max(0) as usize;
    let take = if page.per_page >= 0 { page.per_page as usize } else { usize::MAX };
    let mut data = Vec::new();
    for p in all.iter().skip(offset).take(take) {
        data.push(plugin_data(&cfg, p).await);
    }
    Ok(PhpJson::ok(page.envelope(&ctx, uri.path(), total, data)).into_response())
}

/// GET /plugins/{plugin}.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("plugin", READ)?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    Ok(PhpJson::ok(plugin_data(&cfg, &p).await).into_response())
}

/// After an import: reinstall (without enabling) when the plugin was installed.
fn reinstall_if_installed(cfg: &Config, id: &str) -> ApiResult<()> {
    if lifecycle::is_installed(cfg, id).map_err(service_err)? {
        let p = cfg.find(id).map_err(service_err)?;
        lifecycle::install(cfg, &p, false).map_err(service_err)?;
    }
    Ok(())
}

/// POST /plugins/import/file (multipart field `plugin`).
pub async fn import_file(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    req: Request,
) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let cfg = config(&st);
    cfg.refresh_statuses();
    use axum::extract::FromRequest;
    let mut upload: Option<Vec<u8>> = None;
    if let Ok(mut mp) = axum::extract::Multipart::from_request(req, &st).await {
        while let Ok(Some(field)) = mp.next_field().await {
            if field.name() == Some("plugin") && field.file_name().is_some() {
                upload = field.bytes().await.ok().map(|b| b.to_vec());
                break;
            }
        }
    }
    let Some(bytes) = upload else { return Err(panel_exception()) };
    let tmp = std::env::temp_dir().join(format!("pelican-upload-{}.zip", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &bytes).map_err(|_| ApiError::internal("Exception"))?;
    let result = lifecycle::import_zip(&cfg, &tmp, None);
    let _ = std::fs::remove_file(&tmp);
    let id = result.map_err(service_err)?;
    reinstall_if_installed(&cfg, &id)?;
    Ok(StatusCode::CREATED.into_response())
}

/// POST /plugins/import/url.
pub async fn import_url(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let mut v = Validator::new(&input);
    v.rules("url", &["required", "string"]);
    v.validate()?;
    let cfg = config(&st);
    cfg.refresh_statuses();
    let url = input.get("url").map(pelican_core::php::string).unwrap_or_default();
    let id = lifecycle::import_url(&cfg, &url, None, None).await.map_err(service_err)?;
    reinstall_if_installed(&cfg, &id)?;
    Ok(StatusCode::CREATED.into_response())
}

/// PUT /plugins/hub: store the Hub URL/key in `.env`, drop cached update feeds.
pub async fn hub(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let mut v = Validator::new(&input);
    v.rules("hub_url", &["required", "string", "max:255", "url:https", "regex:/^\\S+$/"]);
    v.rules("api_key", &["required", "string", "max:128", "regex:/^pnl_[A-Za-z0-9]+$/"]);
    v.validate()?;
    let url = input.get("hub_url").map(pelican_core::php::string).unwrap_or_default();
    let key = input.get("api_key").map(pelican_core::php::string).unwrap_or_default();
    use pelican_core::env::EnvWrite;
    pelican_core::env::write_variables(
        &st.config.base_path.join(".env"),
        &[
            ("PANEL_PLUGIN_HUB_URL".into(), EnvWrite::Str(url.trim_end_matches('/').to_string())),
            ("PANEL_PLUGIN_HUB_API_KEY".into(), EnvWrite::Str(key)),
        ],
        true,
    )
    .map_err(|_| ApiError::internal("Exception"))?;
    st.request_reload();
    updates::forget_all();
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn respond(cfg: &Config, id: &str) -> ApiResult<Response> {
    let p = cfg.find(id).map_err(service_err)?;
    Ok(PhpJson::ok(plugin_data(cfg, &p).await).into_response())
}

/// POST /plugins/{plugin}/install.
pub async fn install(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    if p.status != Status::NotInstalled {
        return Err(panel_exception());
    }
    lifecycle::install(&cfg, &p, true).map_err(service_err)?;
    respond(&cfg, &p.id).await
}

/// POST /plugins/{plugin}/update.
pub async fn update(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    if !updates::update_available(&cfg, &p).await {
        return Err(panel_exception());
    }
    let p = lifecycle::update(&cfg, &p).await.map_err(service_err)?;
    Ok(PhpJson::ok(plugin_data(&cfg, &p).await).into_response())
}

/// POST /plugins/{plugin}/uninstall (`delete` boolean).
pub async fn uninstall(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let mut v = Validator::new(&input);
    v.rules("delete", &["boolean"]);
    v.validate()?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    if p.status == Status::NotInstalled {
        return Err(panel_exception());
    }
    let delete = php_boolean(input.get("delete"));
    lifecycle::uninstall(&cfg, &p, delete).map_err(service_err)?;
    // The response transforms the (stale) model instance, deleted or not.
    let mut after = p.clone();
    after.status = Status::NotInstalled;
    after.status_message = None;
    Ok(PhpJson::ok(plugin_data(&cfg, &after).await).into_response())
}

/// POST /plugins/{plugin}/enable.
pub async fn enable(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    let compatible = pelican_plugins::compat::is_compatible(&p, cfg.panel_version.as_deref());
    if !(matches!(p.status, Status::Disabled | Status::Crashed) && compatible) {
        return Err(panel_exception());
    }
    lifecycle::enable(&cfg, &p).map_err(service_err)?;
    respond(&cfg, &p.id).await
}

/// POST /plugins/{plugin}/disable.
pub async fn disable(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("plugin", WRITE)?;
    let cfg = config(&st);
    let p = bound(&cfg, &ctx)?;
    if !matches!(p.status, Status::Enabled | Status::Incompatible) {
        return Err(panel_exception());
    }
    lifecycle::disable(&cfg, &p).map_err(service_err)?;
    respond(&cfg, &p.id).await
}
