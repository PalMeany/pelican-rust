//! The WASM plugin runtime of a running panel (ADR-0018): host services and startup.

use std::sync::Arc;

use pelican_api::services::LogActivity;
use pelican_core::state::Live;
use pelican_plugins::runtime::{BoxFuture, Runtime, Services};
use serde_json::{Map, Value};

struct PanelServices {
    live: Live,
}

impl Services for PanelServices {
    fn trans(&self, plugin: &str, locale: &str, key: &str, replace: &[(String, String)]) -> String {
        let cfg = pelican_api::application::plugins::config(&self.live.get());
        pelican_plugins::trans_line(&cfg, plugin, locale, key, replace)
            .unwrap_or_else(|| pelican_web::i18n::trans(locale, key, replace))
    }

    fn activity<'a>(&'a self, plugin: &'a str, event: &'a str, properties: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let st = self.live.get();
            let mut a =
                pelican_core::activity::Activity::new(event, "127.0.0.1".to_string()).property("plugin", plugin);
            if let Value::Object(m) = properties {
                for (k, v) in m {
                    a = a.property(&k, v);
                }
            }
            if let Err(e) = a.log(&st).await {
                tracing::warn!(plugin, error = ?e.errors.first().map(|o| &o.code), "plugin activity failed");
            }
        })
    }

    fn api<'a>(
        &'a self,
        plugin: &'a str,
        grants: &'a Map<String, Value>,
        method: &'a str,
        path: &'a str,
        body: Option<String>,
    ) -> BoxFuture<'a, Result<(u16, String), String>> {
        Box::pin(async move {
            use tower::ServiceExt;
            if !path.starts_with('/') || path.contains("..") {
                return Err("invalid path".into());
            }
            let st = self.live.get();
            let token = plugin_key(&st, plugin, grants).await?;
            let req = axum::http::Request::builder()
                .method(method)
                .uri(format!("/api/application{path}"))
                .header("authorization", format!("Bearer {token}"))
                .header("accept", "application/json")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.unwrap_or_default()))
                .map_err(|e| e.to_string())?;
            let resp = pelican_api::router(st).oneshot(req).await.map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20).await.map_err(|e| e.to_string())?;
            Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
        })
    }
}

/// A plugin runtime whose host services act on `live` (the server's, or a CLI command's
/// own for install/uninstall hooks).
pub fn runtime(live: &Live) -> Result<Arc<Runtime>, String> {
    let storage = live.get().config.base_path.join("storage").join("plugins");
    Runtime::new(Arc::new(PanelServices { live: live.clone() }), storage).map_err(|e| format!("{e:#}"))
}

/// Create the runtime and load the enabled plugins in the background.
pub fn start(live: &Live) {
    let st = live.get();
    let cfg = pelican_api::application::plugins::config(&st);
    if cfg.safe_mode {
        tracing::info!("PANEL_PLUGIN_SAFE_MODE: plugins are not loaded");
        return;
    }
    // PHP plugins run in the sidecar (ADR-0020): its plugins directory, then the manifest.
    if pelican_api::sidecar::get(&st).is_some() {
        if let Err(e) = pelican_api::sidecar::sync_plugins_dir(&st, &cfg.plugins()) {
            tracing::error!(error = %e, "preparing the sidecar's plugins directory failed");
        }
        pelican_api::sidecar::spawn_refresher(st.clone());
    }
    match runtime(live) {
        Ok(rt) => {
            pelican_plugins::runtime::set_global(rt.clone());
            pelican_core::server_config::set_extra_features(pelican_api::sidecar::all_feature_listeners);
            tokio::spawn(watch(live.clone(), rt));
        }
        Err(e) => tracing::error!(error = %e, "starting the plugin runtime failed"),
    }
}

/// Load the plugins, then follow changes made outside this process (`p:plugin:*`, files
/// copied in): reload the WASM plugins, re-link new or removed PHP plugins for the sidecar.
async fn watch(live: Live, rt: Arc<Runtime>) {
    let st = live.get();
    rt.load_all(&pelican_api::application::plugins::config(&st)).await;
    let php_ids = |plugins: &[pelican_plugins::Plugin]| -> Vec<String> {
        plugins.iter().filter(|p| p.wasm.is_none()).map(|p| p.id.clone()).collect()
    };
    let mut linked = php_ids(&pelican_api::application::plugins::config(&st).plugins());
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let st = live.get();
        let cfg = pelican_api::application::plugins::config(&st);
        rt.reload_if_changed(&cfg).await;
        if pelican_api::sidecar::get(&st).is_some() {
            let plugins = cfg.plugins();
            let ids = php_ids(&plugins);
            if ids != linked {
                match pelican_api::sidecar::sync_plugins_dir(&st, &plugins) {
                    Ok(()) => linked = ids,
                    Err(e) => tracing::warn!(error = %e, "re-linking the sidecar's plugins failed"),
                }
            }
        }
    }
}

/// The plugin's own application API key (memo `Plugin: <id>`, owned by the first Root Admin,
/// permissions = the manifest's `capabilities.api`): visible and revocable in the admin
/// API key list. The secret is kept in `storage/plugins/<id>/api-key.json`, outside the
/// plugin's reach. Returns the bearer token.
async fn plugin_key(
    st: &pelican_core::state::AppState,
    plugin: &str,
    grants: &Map<String, Value>,
) -> Result<String, String> {
    use pelican_db::entities::api_keys;
    use pelican_db::{Int, Json, Timestamp};
    use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
    let file = st.config.base_path.join("storage").join("plugins").join(plugin).join("api-key.json");
    let stored: Option<(String, String)> = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| Some((v.get("identifier")?.as_str()?.to_string(), v.get("secret")?.as_str()?.to_string())));
    let perms = Value::Object(grants.clone());
    if let Some((identifier, secret)) = stored {
        let row = api_keys::Entity::find()
            .filter(api_keys::Column::Identifier.eq(identifier.clone()))
            .one(&st.db)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(row) = row {
            if row.permissions.as_ref().map(|p| &p.0) != Some(&perms) {
                api_keys::Entity::update_many()
                    .col_expr(api_keys::Column::Permissions, sea_orm::sea_query::Expr::value(Some(Json(perms))))
                    .filter(api_keys::Column::Id.eq(row.id))
                    .exec(&st.db)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            return Ok(format!("{identifier}{secret}"));
        }
    }
    // The first Root Admin owns plugin keys.
    let owner: Option<i64> = {
        #[derive(sea_orm::FromQueryResult)]
        struct Owner {
            model_id: Int,
        }
        let b = st.db.get_database_backend();
        <Owner as sea_orm::FromQueryResult>::find_by_statement(pelican_db::raw::stmt(
            b,
            "select {model_has_roles.model_id} as {model_id} from {model_has_roles} inner join {roles} on {roles.id} = {model_has_roles.role_id} \
             where {roles.name} = ? and {model_has_roles.model_type} = ? order by {model_has_roles.model_id} limit 1",
            vec![pelican_core::auth::ROOT_ADMIN_ROLE.into(), "user".into()],
        ))
        .one(&st.db)
        .await
        .map_err(|e| e.to_string())?
        .map(|o| o.model_id.0)
    };
    let owner = owner.ok_or("no Root Admin to own the plugin API key")?;
    let identifier = format!("papp_{}", pelican_wings::jwt::str_random(11));
    let secret = pelican_wings::jwt::str_random(32);
    let now = Timestamp(pelican_core::time::now());
    pelican_db::write::insert(
        &st.db,
        api_keys::ActiveModel {
            user_id: Set(Some(Int(owner))),
            key_type: Set(Int(2)),
            identifier: Set(Some(identifier.clone())),
            token: Set(st.enc.encrypt_string(&secret)),
            permissions: Set(Some(Json(perms))),
            allowed_ips: Set(Json(Value::Array(vec![]))),
            memo: Set(Some(format!("Plugin: {plugin}"))),
            last_used_at: Set(None),
            expires_at: Set(None),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&file, serde_json::json!({"identifier": identifier, "secret": secret}).to_string())
        .map_err(|e| e.to_string())?;
    Ok(format!("{identifier}{secret}"))
}
