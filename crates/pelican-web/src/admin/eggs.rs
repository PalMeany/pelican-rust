//! Admin › Eggs (app/Filament/Admin/Resources/Eggs): list, import (files,
//! URLs, pasted content), create/edit tabs (configuration, process management,
//! variables, install script), export, update from `update_url`, replicate, delete.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::{
    model_events,
    validation::{CustomRule, Validator},
};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, egg_variables, eggs, nodes, servers, users};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde::Serialize;
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, Filter, ListParams, Row, Table};

const SLUG: &str = "eggs";
const TABS: [&str; 4] = ["configuration", "process_management", "egg_variables", "install_script"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/eggs", get(index))
        .route("/admin/eggs/import", post(import))
        .route("/admin/eggs/create", get(create_form).post(create))
        .route("/admin/eggs/{id}", get(view))
        .route("/admin/eggs/{id}/edit", get(edit).post(save))
        .route("/admin/eggs/{id}/export/{format}", get(export))
        .route("/admin/eggs/{id}/copy-data", get(copy_data))
        .route("/admin/eggs/{id}/action", post(action))
        .route("/admin/eggs/bulk-delete", post(bulk_delete))
        .route("/admin/eggs/bulk-update", post(bulk_update))
}

/// Server counts per egg id (`servers_count`).
async fn server_counts(st: &AppState, egg_ids: &[i64]) -> Result<HashMap<i64, i64>, WebError> {
    if egg_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let b = st.db.get_database_backend();
    #[derive(sea_orm::FromQueryResult)]
    struct C {
        egg_id: Int,
        c: Int,
    }
    let marks = vec!["?"; egg_ids.len()].join(", ");
    let vals: Vec<sea_orm::Value> = egg_ids.iter().map(|i| sea_orm::Value::from(*i)).collect();
    let sql = format!(
        "select {{egg_id}}, count(*) as {{c}} from {{servers}} where {{egg_id}} in ({marks}) group by {{egg_id}}"
    );
    Ok(<C as sea_orm::FromQueryResult>::find_by_statement(raw::stmt(b, &sql, vals))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.egg_id.0, c.c.0))
        .collect())
}

fn list_of(v: Option<&Json>) -> Vec<String> {
    match v.map(|j| &j.0) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::Object(m)) => m.values().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => serde_json::from_str::<Vec<String>>(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `name => value` pairs of a KeyValue column (`docker_images`, `startup_commands`).
fn pairs_of(v: Option<&Json>) -> Vec<(String, String)> {
    match v.map(|j| &j.0) {
        Some(Value::Object(m)) => {
            m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()
        }
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| (s.to_string(), s.to_string()))).collect(),
        _ => Vec::new(),
    }
}

/// `GET /admin/eggs` (ListEggs).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList egg")?;
    let st = &admin.st;
    let opts = [10, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 25);
    let mut all = eggs::Entity::find().order_by_asc(eggs::Column::Id).all(&st.db).await?;
    if !p.search.is_empty() {
        let q = p.search.to_lowercase();
        all.retain(|e| e.name.to_lowercase().contains(&q));
    }
    if let Some(tag) = p.filter("tag") {
        all.retain(|e| list_of(e.tags.as_ref()).iter().any(|t| t == tag));
    }
    match p.sort.as_deref() {
        Some("name") if p.desc => all.sort_by(|a, b| b.name.cmp(&a.name)),
        Some("name") => all.sort_by(|a, b| a.name.cmp(&b.name)),
        _ => {}
    }
    let counts: HashMap<i64, i64> = {
        let ids: Vec<i64> = all.iter().map(|e| e.id.0).collect();
        server_counts(st, &ids).await?
    };
    let total = all.len() as u64;
    let page: Vec<&eggs::Model> = match p.limit_offset() {
        Some((l, o)) => all.iter().skip(o as usize).take(l as usize).collect(),
        None => all.iter().collect(),
    };
    let t = |k: &str| admin.t(k);
    let can_edit = admin.can("update egg");
    let mut table = Table::new("eggs", "/admin/eggs");
    table.columns = vec![
        Column::new("icon", "").center(),
        Column::new("name", t("admin/egg.name")).sortable(),
        Column::new("servers", t("admin/egg.servers")),
    ];
    let default_icon = crate::assets::url("img/pelican.svg");
    for e in page {
        let icon = e
            .uuid
            .as_deref()
            .and_then(|u| crate::models::icon_url(st, "egg", u))
            .unwrap_or_else(|| default_icon.clone());
        let desc =
            e.description.clone().map(|d| if d.len() > 120 { format!("{}...", truncate_bytes(&d, 120)) } else { d });
        let url = if can_edit { format!("/admin/eggs/{}/edit", e.id.0) } else { format!("/admin/eggs/{}", e.id.0) };
        let mut row = Row::new(e.id.0.to_string())
            .url(url.clone())
            .cell("icon", Cell::Image { src: icon })
            .cell("name", Cell::text_desc(e.name.clone(), desc))
            .cell("servers", Cell::text(counts.get(&e.id.0).copied().unwrap_or(0).to_string()));
        row.actions.push(if can_edit {
            Action::link(t("filament-actions::edit.single.label"), "pencil", url)
        } else {
            Action::link(t("filament-actions::view.single.label"), "eye", url)
        });
        if admin.can("export egg") {
            row.group.push(Action::link(
                admin.t_with("admin/egg.export.as", &[("format", "json")]),
                "download",
                format!("/admin/eggs/{}/export/json", e.id.0),
            ));
            row.group.push(Action::link(
                admin.t_with("admin/egg.export.as", &[("format", "yaml")]),
                "download",
                format!("/admin/eggs/{}/export/yaml", e.id.0),
            ));
        }
        // UpdateEggAction::visible(): an update was found by p:egg:check-updates.
        if admin.can("import egg") && e.uuid.as_deref().is_some_and(pelican_api::egg_index::has_update) {
            row.group.push(
                Action::post(
                    crate::i18n::trans_choice(&admin.locale(), "admin/egg.update", 1.0, &[]),
                    "cloud-download",
                    format!("/admin/eggs/{}/action", e.id.0),
                )
                .field("op", "update")
                .color("success")
                .confirm(crate::i18n::trans_choice(
                    &admin.locale(),
                    "admin/egg.update_description",
                    1.0,
                    &[],
                )),
            );
        }
        if admin.can("create egg") {
            row.group.push(
                Action::post(
                    t("filament-actions::replicate.single.label"),
                    "copy",
                    format!("/admin/eggs/{}/action", e.id.0),
                )
                .field("op", "replicate"),
            );
        }
        table.rows.push(row);
    }
    if admin.can("delete egg") {
        table.bulk.push(
            Action::post(admin.t("filament-actions::delete.multiple.label"), "trash", "/admin/eggs/bulk-delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
        );
    }
    if admin.can("import egg") {
        // UpdateEggBulkAction.
        table.bulk.push(
            Action::post(
                crate::i18n::trans_choice(&admin.locale(), "admin/egg.update", 2.0, &[]),
                "cloud-download",
                "/admin/eggs/bulk-update",
            )
            .color("success")
            .confirm(crate::i18n::trans_choice(&admin.locale(), "admin/egg.update_question", 2.0, &[])),
        );
    }
    let mut tags: Vec<String> = all.iter().flat_map(|e| list_of(e.tags.as_ref())).collect();
    tags.sort();
    tags.dedup();
    table.searchable = true;
    if !tags.is_empty() {
        table.filters = vec![Filter {
            name: "tag".into(),
            label: t("admin/egg.tags"),
            options: tags.into_iter().map(|t| (t.clone(), t)).collect(),
            value: p.filter("tag").unwrap_or("").into(),
        }];
    }
    table.empty_icon = "eggs".into();
    table.empty_heading = t("admin/egg.no_eggs");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create egg").then(|| {
        context! { label => admin.t_with("filament-actions::create.single.label", &[("label", &t("admin/egg.model_label"))]), url => "/admin/eggs/create" }
    });
    admin
        .page(
            SLUG,
            "admin/eggs/list.html",
            context! { title => t("admin/egg.model_label_plural"), table => Mj::from_serialize(&table), create => create, can_import => admin.can("import egg"), egg_index => Mj::from_serialize(pelican_api::egg_index::index_or_fetch(st).await) },
        )
        .await
}

fn truncate_bytes(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `ImportEggAction`: pasted/uploaded contents and URLs (no local addresses).
async fn import(admin: Admin, form: Form) -> WebResult {
    admin.authorize("import egg")?;
    let st = &admin.st;
    // GitHub tab: the selected index eggs install in the background (InstallEgg jobs).
    let github: Vec<String> = form.list("eggs").into_iter().filter(|u| !u.is_empty()).collect();
    if !github.is_empty() {
        for url in &github {
            let (st, url) = (st.clone(), url.clone());
            tokio::spawn(async move { pelican_api::egg_index::install_from_url(&st, &url).await });
        }
        admin.session.flash(
            "success",
            admin.t("installer.egg.background_install_started"),
            Some(admin.t_with("installer.egg.background_install_description", &[("count", &github.len().to_string())])),
        );
    }
    let mut sources: Vec<(String, Result<String, String>)> = Vec::new();
    let mut urls: Vec<String> =
        form.list("urls").into_iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
    urls.dedup();
    for url in urls {
        // GitHub page links are rewritten to raw downloads.
        let url = if url.contains("github.com") { url.replacen("blob", "raw", 1) } else { url };
        let name = egg_name(after_last(&url, "/egg-"));
        let content = fetch_url(&admin, &url).await;
        sources.push((name, content));
    }
    let names = form.list("names");
    for (i, c) in form.list("contents").into_iter().enumerate() {
        if !c.trim().is_empty() {
            let file = names.get(i).cloned().unwrap_or_default();
            sources.push((egg_name(after_last(&file, "egg-")), Ok(c)));
        }
    }
    let total = sources.len();
    let (mut ok, mut failed) = (Vec::new(), Vec::new());
    for (name, content) in sources {
        match content {
            Ok(c) => match pelican_api::application::eggs::import_egg(st, &c).await {
                Ok(_) => ok.push(name),
                Err(e) => {
                    tracing::warn!(egg = %name, error = ?e.errors.first().map(|o| &o.detail), "egg import failed");
                    failed.push(name);
                }
            },
            Err(e) => {
                tracing::warn!(egg = %name, error = %e, "egg download failed");
                failed.push(name);
            }
        }
    }
    if total == 0 {
        return Ok(admin.redirect("/admin/eggs"));
    }
    let mut body = Vec::new();
    if !ok.is_empty() {
        body.push(admin.t_with("admin/egg.import.imported_eggs", &[("eggs", &ok.join(", "))]));
    }
    if !failed.is_empty() {
        body.push(admin.t_with("admin/egg.import.failed_import_eggs", &[("eggs", &failed.join(", "))]));
    }
    let title = admin.t_with(
        "admin/egg.import.import_result",
        &[("success", &ok.len().to_string()), ("failed", &failed.len().to_string()), ("total", &total.to_string())],
    );
    admin.session.flash(
        if failed.is_empty() {
            "success"
        } else if ok.is_empty() {
            "danger"
        } else {
            "warning"
        },
        title,
        Some(body.join(" | ")),
    );
    Ok(admin.redirect(&form.back("/admin/eggs")))
}

fn after_last<'a>(s: &'a str, needle: &str) -> &'a str {
    s.rfind(needle).map_or(s, |i| &s[i + needle.len()..])
}

/// `str($filename)->before('.json')->headline()` of an egg file name.
fn egg_name(file: &str) -> String {
    let ext = after_last(file, ".").to_lowercase();
    let stem = match ext.as_str() {
        "json" | "yaml" | "yml" => file.find(&format!(".{ext}")).map_or(file, |i| &file[..i]),
        _ => file,
    };
    headline(stem)
}

/// `Str::headline`: words split on `-`, `_`, spaces and case changes, each capitalized.
fn headline(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c == '-' || c == '_' || c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .into_iter()
        .map(|w| {
            let mut ch = w.chars();
            ch.next().map(|f| f.to_uppercase().chain(ch).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `EggImporterService::fromUrl`: extension check, 5 s timeout (1 s connect), `throw()` on HTTP errors.
async fn fetch_url(admin: &Admin, url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| admin.t("admin/egg.import.invalid_url"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(admin.t("admin/egg.import.invalid_url"));
    }
    let ext = parsed.path().rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if !matches!(ext.as_str(), "json" | "yaml" | "yml") {
        return Err("Unsupported file format.".into());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(parsed).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP request returned status code {}", resp.status().as_u16()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

#[derive(Serialize, Clone, Default)]
struct VarRow {
    id: Option<i64>,
    name: String,
    description: String,
    env_variable: String,
    default_value: String,
    user_viewable: bool,
    user_editable: bool,
    rules: Vec<String>,
    sort: i64,
}

#[derive(Default)]
struct EggForm {
    name: String,
    description: String,
    author: String,
    force_outgoing_ip: bool,
    startup_commands: Vec<(String, String)>,
    file_denylist: Vec<String>,
    update_url: String,
    features: Vec<String>,
    tags: Vec<String>,
    docker_images: Vec<(String, String)>,
    config_from: String,
    config_stop: String,
    config_startup: String,
    config_files: String,
    config_logs: String,
    copy_script_from: String,
    script_container: String,
    script_entry: String,
    script_install: String,
    script_is_privileged: bool,
    variables: Vec<VarRow>,
}

impl EggForm {
    fn defaults(author: &str) -> EggForm {
        EggForm {
            author: author.into(),
            config_startup: "{}".into(),
            config_files: "{}".into(),
            config_logs: "{}".into(),
            script_container: "ghcr.io/pelican-eggs/installers:debian".into(),
            script_entry: "bash".into(),
            script_is_privileged: true,
            ..Default::default()
        }
    }

    fn from_model(e: &eggs::Model, vars: &[egg_variables::Model]) -> EggForm {
        EggForm {
            name: e.name.clone(),
            description: e.description.clone().unwrap_or_default(),
            author: e.author.clone(),
            force_outgoing_ip: e.force_outgoing_ip.0,
            startup_commands: pairs_of(e.startup_commands.as_ref()),
            file_denylist: list_of(e.file_denylist.as_ref()),
            update_url: e.update_url.clone().unwrap_or_default(),
            features: list_of(e.features.as_ref()),
            tags: list_of(e.tags.as_ref()),
            docker_images: pairs_of(e.docker_images.as_ref()),
            config_from: e.config_from.map(|c| c.0.to_string()).unwrap_or_default(),
            config_stop: e.config_stop.clone().unwrap_or_default(),
            config_startup: e.config_startup.clone().unwrap_or_default(),
            config_files: e.config_files.clone().unwrap_or_default(),
            config_logs: e.config_logs.clone().unwrap_or_default(),
            copy_script_from: e.copy_script_from.map(|c| c.0.to_string()).unwrap_or_default(),
            script_container: e.script_container.clone(),
            script_entry: e.script_entry.clone(),
            script_install: e.script_install.clone().unwrap_or_default(),
            script_is_privileged: e.script_is_privileged.0,
            variables: vars
                .iter()
                .map(|v| VarRow {
                    id: Some(v.id.0),
                    name: v.name.clone(),
                    description: v.description.clone(),
                    env_variable: v.env_variable.clone(),
                    default_value: v.default_value.clone(),
                    user_viewable: v.user_viewable.0,
                    user_editable: v.user_editable.0,
                    rules: crate::variables::rules_of(&v.rules.0),
                    sort: v.sort.map(|s| s.0).unwrap_or(0),
                })
                .collect(),
        }
    }

    fn from_form(f: &Form) -> EggForm {
        let zip = |k: &str, v: &str| -> Vec<(String, String)> {
            f.list(k)
                .into_iter()
                .zip(f.list(v))
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .filter(|(a, b)| !(a.is_empty() && b.is_empty()))
                .collect()
        };
        let ids = f.list("var_id");
        let names = f.list("var_name");
        let descs = f.list("var_description");
        let envs = f.list("var_env");
        let defaults = f.list("var_default");
        let rules = f.list("var_rules");
        let viewable = f.list("var_viewable");
        let editable = f.list("var_editable");
        let variables = (0..names.len())
            .map(|i| VarRow {
                id: ids.get(i).and_then(|s| s.parse().ok()),
                name: names[i].trim().into(),
                description: descs.get(i).cloned().unwrap_or_default(),
                env_variable: envs.get(i).map(|s| s.trim().to_string()).unwrap_or_default(),
                default_value: defaults.get(i).cloned().unwrap_or_default(),
                user_viewable: viewable.get(i).is_some_and(|v| v == "1"),
                user_editable: editable.get(i).is_some_and(|v| v == "1"),
                rules: rules.get(i).map(|r| crate::forms::parse_tags(r)).unwrap_or_default(),
                sort: i as i64 + 1,
            })
            .filter(|v| !(v.name.is_empty() && v.env_variable.is_empty()))
            .collect();
        EggForm {
            name: f.get("name").trim().into(),
            description: f.get("description").into(),
            author: f.get("author").trim().into(),
            force_outgoing_ip: f.bool("force_outgoing_ip"),
            startup_commands: zip("startup_name", "startup_command"),
            file_denylist: f.tags("file_denylist"),
            update_url: f.get("update_url").trim().into(),
            features: f.tags("features"),
            tags: f.tags("tags"),
            docker_images: zip("image_name", "image_uri"),
            config_from: f.get("config_from").into(),
            config_stop: f.get("config_stop").into(),
            config_startup: f.get("config_startup").into(),
            config_files: f.get("config_files").into(),
            config_logs: f.get("config_logs").into(),
            copy_script_from: f.get("copy_script_from").into(),
            script_container: f.get("script_container").trim().into(),
            script_entry: f.get("script_entry").into(),
            script_install: f.get("script_install").replace("\r\n", "\n"),
            script_is_privileged: true,
            variables,
        }
    }

    fn kv_object(pairs: &[(String, String)]) -> Value {
        Value::Object(
            pairs.iter().filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect(),
        )
    }

    /// Field rules + `Egg::$validationRules` + `EggVariable` rules.
    async fn validate(&self, admin: &Admin, egg_id: Option<i64>) -> Result<Errors, WebError> {
        let blank = |s: &str| if s.trim().is_empty() { Value::Null } else { Value::String(s.to_string()) };
        let data = json!({
            "name": self.name, "description": blank(&self.description), "author": self.author,
            "docker_images": Self::kv_object(&self.docker_images), "startup_commands": Self::kv_object(&self.startup_commands),
            "config_from": blank(&self.config_from), "config_stop": blank(&self.config_stop),
            "config_startup": blank(&self.config_startup), "config_logs": blank(&self.config_logs), "config_files": blank(&self.config_files),
            "update_url": blank(&self.update_url), "script_container": self.script_container, "script_entry": self.script_entry,
        });
        let mut v = Validator::for_value(&data);
        for (k, l) in [
            ("name", "admin/egg.name"),
            ("author", "admin/egg.author"),
            ("docker_images", "admin/egg.docker_images"),
            ("startup_commands", "admin/egg.startup_commands"),
            ("config_stop", "admin/egg.stop_command"),
            ("config_startup", "admin/egg.start_config"),
            ("config_files", "admin/egg.config_files"),
            ("config_logs", "admin/egg.log_config"),
            ("script_container", "admin/egg.script_container"),
        ] {
            v.attribute_name(k, &admin.t(l).to_lowercase());
        }
        v.rules("name", &["required", "string", "max:255"]);
        v.rules("description", &["string", "nullable"]);
        v.rules("author", &["required", "string", "email", "max:255"]);
        v.rules("docker_images", &["required", "array", "min:1"]);
        v.rules("docker_images.*", &["required", "string"]);
        v.rules("startup_commands", &["required", "array", "min:1"]);
        v.rules("startup_commands.*", &["required", "string", "distinct"]);
        v.rules("config_from", &["sometimes", "bail", "nullable", "numeric"]);
        v.rules("config_stop", &["required_without:config_from", "nullable", "string", "max:255"]);
        v.rules("config_startup", &["required_without:config_from", "nullable", "json"]);
        v.rules("config_logs", &["required_without:config_from", "nullable", "json"]);
        v.rules("config_files", &["required_without:config_from", "nullable", "json"]);
        v.rules("update_url", &["sometimes", "nullable", "url"]);
        v.rules("script_container", &["required", "string", "max:255"]);
        v.rules("script_entry", &["required", "in:bash,ash,/bin/bash"]);
        if let Ok(from) = self.config_from.parse::<i64>() {
            let exists = eggs::Entity::find_by_id(from).one(&admin.st.db).await?.is_some_and(|_| Some(from) != egg_id);
            v.custom(
                "config_from",
                CustomRule::precomputed("exists", (!exists).then(|| "The selected :attribute is invalid.".into())),
            );
        }
        v.run();
        let mut errors = Errors::from_validator(&v);
        let mut seen_names: Vec<&str> = Vec::new();
        let mut seen_env: Vec<&str> = Vec::new();
        for (i, var) in self.variables.iter().enumerate() {
            let key = |f: &str| format!("var_{f}_{i}");
            if var.name.is_empty() {
                errors.add(
                    &key("name"),
                    admin.t_with("validation.required", &[("attribute", &admin.t("admin/egg.name").to_lowercase())]),
                );
            } else if seen_names.contains(&var.name.as_str()) {
                errors.add(&key("name"), admin.t("admin/egg.error_unique"));
            }
            seen_names.push(&var.name);
            if var.env_variable.is_empty() {
                errors.add(&key("env"), admin.t("admin/egg.error_required"));
            } else if egg_format::import::RESERVED_ENV_NAMES.contains(&var.env_variable.as_str())
                || !var.env_variable.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                || var.env_variable.len() > 255
            {
                errors.add(&key("env"), admin.t("admin/egg.error_reserved"));
            } else if seen_env.contains(&var.env_variable.as_str()) {
                errors.add(&key("env"), admin.t("admin/egg.error_unique"));
            }
            seen_env.push(&var.env_variable);
        }
        Ok(errors)
    }
}

async fn render(
    admin: &Admin,
    egg: Option<&eggs::Model>,
    form: &EggForm,
    errors: &Errors,
    readonly: bool,
) -> WebResult {
    let st = &admin.st;
    let t = |k: &str| admin.t(k);
    let others: Vec<Vec<String>> = eggs::Entity::find()
        .order_by_asc(eggs::Column::Name)
        .all(&st.db)
        .await?
        .into_iter()
        .filter(|e| egg.is_none_or(|x| x.id != e.id))
        .map(|e| vec![e.id.0.to_string(), e.name])
        .collect();
    let tab = admin
        .query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "tab").map(|(_, v)| v.into_owned()))
        .filter(|t| TABS.contains(&t.as_str()))
        .unwrap_or_else(|| "configuration".into());
    let (title, action) = match egg {
        Some(e) if readonly => {
            (admin.t_with("filament-panels::resources/pages/view-record.title", &[("label", &e.name)]), String::new())
        }
        Some(e) => (
            admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &e.name)]),
            format!("/admin/eggs/{}/edit", e.id.0),
        ),
        None => (
            admin.t_with(
                "filament-panels::resources/pages/create-record.title",
                &[("label", &t("admin/egg.model_label"))],
            ),
            "/admin/eggs/create".into(),
        ),
    };
    let (servers, icon) = match egg {
        Some(e) => {
            (server_rows(admin, e.id.0).await?, e.uuid.as_deref().and_then(|u| crate::models::icon_url(st, "egg", u)))
        }
        None => (Vec::new(), None),
    };
    admin
        .page(
            SLUG,
            "admin/eggs/form.html",
            context! {
                title => title, action => action, tab => tab, readonly => readonly, creating => egg.is_none(),
                egg => egg.map(|e| context! { id => e.id.0, uuid => e.uuid.clone(), name => e.name.clone(), update_url => e.update_url.clone() }),
                egg_icon => icon,
                can_delete => egg.is_some() && !readonly && admin.can("delete egg"),
                can_export => egg.is_some() && admin.can("export egg"),
                can_import => egg.is_some() && !readonly && admin.can("import egg"),
                v => context! {
                    name => form.name, description => form.description, author => form.author,
                    force_outgoing_ip => form.force_outgoing_ip, startup_commands => form.startup_commands,
                    file_denylist => form.file_denylist, update_url => form.update_url, features => form.features,
                    tags => form.tags, docker_images => form.docker_images, config_from => form.config_from,
                    config_stop => form.config_stop, config_startup => form.config_startup, config_files => form.config_files,
                    config_logs => form.config_logs, copy_script_from => form.copy_script_from,
                    script_container => form.script_container, script_entry => form.script_entry,
                    script_install => form.script_install,
                    variables => Mj::from_serialize(&form.variables),
                },
                others => others,
                servers => servers,
                errors => Mj::from_serialize(errors),
            },
        )
        .await
}

/// ServersRelationManager rows: owner, name, node, image, primary allocation.
async fn server_rows(admin: &Admin, egg_id: i64) -> Result<Vec<Mj>, WebError> {
    let db = &admin.st.db;
    let list = servers::Entity::find()
        .filter(servers::Column::EggId.eq(egg_id))
        .order_by_asc(servers::Column::Id)
        .all(db)
        .await?;
    let owners: HashMap<i64, users::Model> = users::Entity::find()
        .filter(users::Column::Id.is_in(list.iter().map(|s| s.owner_id.0)))
        .all(db)
        .await?
        .into_iter()
        .map(|u| (u.id.0, u))
        .collect();
    let node_names: HashMap<i64, String> = nodes::Entity::find()
        .filter(nodes::Column::Id.is_in(list.iter().map(|s| s.node_id.0)))
        .all(db)
        .await?
        .into_iter()
        .map(|n| (n.id.0, n.name))
        .collect();
    let allocs: HashMap<i64, String> = allocations::Entity::find()
        .filter(allocations::Column::Id.is_in(list.iter().filter_map(|s| s.allocation_id.map(|a| a.0))))
        .all(db)
        .await?
        .into_iter()
        .map(|a| (a.id.0, crate::models::address(&a)))
        .collect();
    let (edit_user, edit_server, edit_node) =
        (admin.can("update user"), admin.can("update server"), admin.can("update node"));
    Ok(list
        .into_iter()
        .map(|s| {
            let owner = owners.get(&s.owner_id.0);
            context! {
                name => s.name, uuid_short => s.uuid_short, image => s.image,
                url => edit_server.then(|| format!("/admin/servers/{}/edit", s.id.0)),
                owner => owner.map(|u| u.username.clone()),
                owner_url => (edit_user && owner.is_some()).then(|| format!("/admin/users/{}/edit", s.owner_id.0)),
                node => node_names.get(&s.node_id.0).cloned(),
                node_url => edit_node.then(|| format!("/admin/nodes/{}/edit", s.node_id.0)),
                allocation => s.allocation_id.and_then(|a| allocs.get(&a.0).cloned()),
            }
        })
        .collect())
}

async fn find(admin: &Admin, id: i64) -> Result<(eggs::Model, Vec<egg_variables::Model>), WebError> {
    let e = eggs::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    let vars = egg_variables::Entity::find()
        .filter(egg_variables::Column::EggId.eq(id))
        .order_by_asc(egg_variables::Column::Sort)
        .order_by_asc(egg_variables::Column::Id)
        .all(&admin.st.db)
        .await?;
    Ok((e, vars))
}

async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create egg")?;
    render(&admin, None, &EggForm::defaults(&admin.user.user.email), &Errors::default(), false).await
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view egg")?;
    let (e, vars) = find(&admin, id).await?;
    render(&admin, Some(&e), &EggForm::from_model(&e, &vars), &Errors::default(), true).await
}

async fn edit(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update egg")?;
    let (e, vars) = find(&admin, id).await?;
    render(&admin, Some(&e), &EggForm::from_model(&e, &vars), &Errors::default(), false).await
}

fn json_list(items: &[String]) -> Json {
    Json(json!(items))
}

fn opt(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

/// Write egg columns + the variables repeater (create, update, delete missing).
async fn persist(admin: &Admin, id: Option<i64>, f: &EggForm) -> Result<i64, WebError> {
    let st = &admin.st;
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let mut am = eggs::ActiveModel {
        name: Set(f.name.clone()),
        description: Set(opt(&f.description)),
        force_outgoing_ip: Set(Bool(f.force_outgoing_ip)),
        startup_commands: Set(Some(Json(EggForm::kv_object(&f.startup_commands)))),
        file_denylist: Set(Some(json_list(&f.file_denylist))),
        update_url: Set(opt(&f.update_url)),
        features: Set(Some(json_list(&f.features))),
        tags: Set(Some(json_list(&f.tags))),
        docker_images: Set(Some(Json(EggForm::kv_object(&f.docker_images)))),
        config_from: Set(f.config_from.parse::<i64>().ok().map(Int)),
        config_stop: Set(opt(&f.config_stop)),
        config_startup: Set(opt(&f.config_startup)),
        // `dehydrateStateUsing(blank → '{}')`.
        config_files: Set(Some(if f.config_files.trim().is_empty() { "{}".into() } else { f.config_files.clone() })),
        config_logs: Set(opt(&f.config_logs)),
        copy_script_from: Set(f.copy_script_from.parse::<i64>().ok().map(Int)),
        script_container: Set(f.script_container.clone()),
        script_entry: Set(f.script_entry.clone()),
        script_install: Set(opt(&f.script_install)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    // Events fire per model instance after commit (EditRecord / repeater
    // relationship saves in the original).
    let mut egg_created: Option<i64> = None;
    let mut egg_updated = false;
    let egg_id = match id {
        Some(id) => {
            am.id = sea_orm::ActiveValue::Unchanged(Int(id));
            egg_updated = pelican_db::write::update(&txn, &am).await?;
            id
        }
        None => {
            am.uuid = Set(Some(uuid::Uuid::new_v4().to_string()));
            am.author = Set(f.author.clone());
            am.script_is_privileged = Set(Bool(f.script_is_privileged));
            am.created_at = Set(Some(now));
            egg_created = pelican_db::write::insert_id(&txn, &am).await?;
            egg_created.unwrap_or_default()
        }
    };
    let mut var_created: Vec<i64> = Vec::new();
    let mut var_updated: Vec<i64> = Vec::new();
    let mut kept: Vec<i64> = Vec::new();
    for var in &f.variables {
        let rules: Vec<Value> = var.rules.iter().map(|r| json!(r)).collect();
        let mut row = egg_variables::ActiveModel {
            name: Set(var.name.clone()),
            description: Set(var.description.clone()),
            env_variable: Set(var.env_variable.clone()),
            default_value: Set(var.default_value.clone()),
            user_viewable: Set(SmallBool(var.user_viewable)),
            user_editable: Set(SmallBool(var.user_editable)),
            rules: Set(Json(Value::Array(rules))),
            sort: Set(Some(Int(var.sort))),
            updated_at: Set(Some(now)),
            ..Default::default()
        };
        let existing = match var.id {
            Some(vid) => egg_variables::Entity::find_by_id(vid).one(&txn).await?.filter(|v| v.egg_id.0 == egg_id),
            None => None,
        };
        match existing {
            Some(v) => {
                row.id = sea_orm::ActiveValue::Unchanged(v.id);
                if pelican_db::write::update(&txn, &row).await? {
                    var_updated.push(v.id.0);
                }
                kept.push(v.id.0);
            }
            None => {
                row.egg_id = Set(Int(egg_id));
                row.created_at = Set(Some(now));
                if let Some(new) = pelican_db::write::insert_id(&txn, &row).await? {
                    kept.push(new);
                    var_created.push(new);
                }
            }
        }
    }
    // Repeater relationship: rows removed from the form are deleted
    // (`$record->delete()` per removed variable in the original). Read their
    // pre-delete state first — the deleted events carry it.
    let b = txn.get_database_backend();
    let mut sql = String::from("delete from {egg_variables} where {egg_id} = ?");
    let mut vals: Vec<sea_orm::Value> = vec![egg_id.into()];
    let mut removed_q = egg_variables::Entity::find().filter(egg_variables::Column::EggId.eq(Int(egg_id)));
    if !kept.is_empty() {
        sql.push_str(&format!(" and {{id}} not in ({})", vec!["?"; kept.len()].join(", ")));
        vals.extend(kept.iter().map(|i| sea_orm::Value::from(*i)));
        removed_q = removed_q.filter(egg_variables::Column::Id.is_not_in(kept.clone()));
    }
    let removed = removed_q.all(&txn).await?;
    txn.execute_raw(raw::stmt(b, &sql, vals)).await?;
    txn.commit().await?;
    // eloquent events (order: egg, variable updates, creates, removals).
    if egg_updated {
        model_events::fire_id::<eggs::Entity>(st, model_events::Action::Updated, egg_id).await;
    }
    if let Some(cid) = egg_created {
        model_events::fire_id::<eggs::Entity>(st, model_events::Action::Created, cid).await;
    }
    for vid in var_updated {
        model_events::fire_id::<egg_variables::Entity>(st, model_events::Action::Updated, vid).await;
    }
    for vid in var_created {
        model_events::fire_id::<egg_variables::Entity>(st, model_events::Action::Created, vid).await;
    }
    for m in &removed {
        model_events::deleted(st, m).await;
    }
    Ok(egg_id)
}

async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create egg")?;
    let data = EggForm::from_form(&form);
    let errors = data.validate(&admin, None).await?;
    if !errors.is_empty() {
        return render(&admin, None, &data, &errors, false).await;
    }
    let id = persist(&admin, None, &data).await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("/admin/eggs/{id}/edit")))
}

async fn save(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update egg")?;
    let (e, _) = find(&admin, id).await?;
    let mut data = EggForm::from_form(&form);
    data.author = e.author.clone();
    let errors = data.validate(&admin, Some(id)).await?;
    if !errors.is_empty() {
        return render(&admin, Some(&e), &data, &errors, false).await;
    }
    persist(&admin, Some(id), &data).await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    let tab = form.opt("_tab").filter(|t| TABS.contains(t)).unwrap_or("configuration");
    Ok(admin.redirect(&format!("/admin/eggs/{id}/edit?tab={tab}")))
}

/// Values copied by the `CopyFrom` selects (process + install script).
async fn copy_data(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view egg")?;
    let e = eggs::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    Ok(axum::Json(json!({
        "config_stop": e.config_stop, "config_startup": e.config_startup, "config_files": e.config_files, "config_logs": e.config_logs,
        "script_container": e.script_container, "script_entry": e.script_entry, "script_install": e.script_install,
    }))
    .into_response())
}

/// `ExportEggAction`: `egg-<name>.json|yaml` download.
async fn export(admin: Admin, Path((id, format)): Path<(i64, String)>) -> Result<Response, WebError> {
    admin.authorize("export egg")?;
    let (e, _) = find(&admin, id).await?;
    let doc = pelican_api::application::eggs::export_document(&admin.st, &e)
        .await
        .map_err(|err| WebError::internal(format!("{err:?}")))?;
    let (body, ext) = match format.as_str() {
        "json" => (egg_format::export::to_json(&doc).ok_or_else(|| WebError::internal("json"))?, "json"),
        _ => (egg_format::export::to_yaml(&doc), "yaml"),
    };
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&format!("application/{ext}")) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) =
        HeaderValue::from_str(&format!("attachment; filename=egg-{}.{ext}", egg_format::export::kebab_name(&e.name)))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// Delete / update from URL / replicate.
async fn action(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let (e, vars) = find(&admin, id).await?;
    let st = &admin.st;
    match form.get("op") {
        "delete" => {
            admin.authorize("delete egg")?;
            match pelican_api::application::eggs::delete_egg(st, &e).await {
                Ok(()) => {
                    admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
                    Ok(admin.redirect("/admin/eggs"))
                }
                Err(err) => {
                    admin.flash_danger(err.errors.first().map(|o| o.detail.clone()).unwrap_or_default(), None);
                    Ok(admin.redirect(&format!("/admin/eggs/{id}/edit")))
                }
            }
        }
        "update" => {
            admin.authorize("import egg")?;
            // UpdateEggAction: EggImporterService::fromUrl($egg->update_url, $egg).
            match update_from_url(&admin, &e).await {
                Ok(url) => {
                    let body = admin.t_with("admin/egg.updated_from", &[("url", &url)]);
                    admin.session.flash(
                        "success",
                        admin.t_with("admin/egg.update_success", &[("egg", &e.name)]),
                        Some(body),
                    );
                }
                Err(msg) => admin.flash_danger(
                    admin.t_with("admin/egg.update_failed", &[("egg", &e.name)]),
                    Some(admin.t_with("admin/egg.update_error", &[("error", &msg)])),
                ),
            }
            Ok(admin.redirect(&form.back(&format!("/admin/eggs/{id}/edit"))))
        }
        "replicate" => {
            admin.authorize("create egg")?;
            // ReplicateAction: new uuid, author = current user, " Copy" suffix, variables copied.
            let mut f = EggForm::from_model(&e, &vars);
            f.name = format!("{} Copy", e.name);
            f.author = admin.user.user.email.clone();
            f.update_url.clear();
            for v in &mut f.variables {
                v.id = None;
            }
            let new_id = persist(&admin, None, &f).await?;
            admin.flash_success(admin.t("filament-actions::replicate.single.notifications.replicated.title"));
            Ok(admin.redirect(&format!("/admin/eggs/{new_id}/edit")))
        }
        _ => Err(WebError::Unprocessable),
    }
}

/// `EggImporterService::fromUrl($egg->update_url, $egg)` — shared by the
/// single `update` action and `UpdateEggBulkAction`; returns the update URL on
/// success or the human-readable failure message.
async fn update_from_url(admin: &Admin, e: &eggs::Model) -> Result<String, String> {
    let url = e.update_url.clone().unwrap_or_default();
    match fetch_url(admin, &url).await {
        Ok(content) => match pelican_api::application::eggs::import_egg_into(&admin.st, &content, Some(e.id.0)).await {
            Ok(_) => {
                if let Some(uuid) = &e.uuid {
                    pelican_api::egg_index::forget_update(uuid);
                }
                Ok(url)
            }
            Err(err) => Err(err.errors.first().map(|o| o.detail.clone()).unwrap_or_default()),
        },
        Err(msg) => Err(msg),
    }
}

/// `DeleteBulkAction('exclude_bulk_delete')`: the `before()` hook pulls eggs
/// with servers (danger notification), the rest is deleted.
async fn bulk_delete(admin: Admin, form: Form) -> WebResult {
    admin.authorize("delete egg")?;
    let st = &admin.st;
    let ids = super::bulk_ids(&form);
    let eggs = if ids.is_empty() {
        Vec::new()
    } else {
        eggs::Entity::find().filter(eggs::Column::Id.is_in(ids)).all(&st.db).await?
    };
    let counts = server_counts(st, &eggs.iter().map(|e| e.id.0).collect::<Vec<_>>()).await?;
    let mut blocked: Vec<String> = Vec::new();
    let mut failed = 0usize;
    for e in &eggs {
        // The `sprintf('%s (%d server%s)', …)` shape of the notification body.
        let n = counts.get(&e.id.0).copied().unwrap_or(0);
        if n > 0 {
            blocked.push(format!("{} ({} server{})", e.name, n, if n > 1 { "s" } else { "" }));
            continue;
        }
        if pelican_api::application::eggs::delete_egg(st, e).await.is_err() {
            failed += 1;
        }
    }
    if !blocked.is_empty() {
        admin.flash_danger(
            admin.t_with("admin/egg.cannot_delete", &[("count", &blocked.len().to_string())]),
            Some(admin.t_with("admin/egg.eggs_have_servers", &[("eggs", &blocked.join(", "))])),
        );
    }
    if blocked.len() < eggs.len() {
        super::bulk_delete_notify(&admin, eggs.len() - blocked.len(), failed);
    }
    Ok(admin.redirect(&form.back("/admin/eggs")))
}

/// `UpdateEggBulkAction('exclude_bulk_update')`: eggs without `update_url` are
/// skipped with a warning (`before()`), then each egg is updated like the
/// single `UpdateEggAction`.
async fn bulk_update(admin: Admin, form: Form) -> WebResult {
    admin.authorize("import egg")?;
    let st = &admin.st;
    let ids = super::bulk_ids(&form);
    let mut records = if ids.is_empty() {
        Vec::new()
    } else {
        eggs::Entity::find().filter(eggs::Column::Id.is_in(ids)).order_by_asc(eggs::Column::Id).all(&st.db).await?
    };
    // `before()`: pull eggs without update URL and warn about them.
    let no_url: Vec<String> = records.iter().filter(|e| e.update_url.is_none()).map(|e| e.name.clone()).collect();
    if !no_url.is_empty() {
        admin.session.flash(
            "warning",
            admin.t_with("admin/egg.cannot_update", &[("count", &no_url.len().to_string())]),
            Some(admin.t_with("admin/egg.no_update_url", &[("eggs", &no_url.join(", "))])),
        );
    }
    records.retain(|e| e.update_url.is_some());
    if records.is_empty() {
        return Ok(admin.redirect(&form.back("/admin/eggs")));
    }
    let total = records.len();
    let (mut ok, mut failed): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for e in &records {
        match update_from_url(&admin, e).await {
            Ok(_) => ok.push(e.name.clone()),
            Err(msg) => {
                // `report($exception)` of the original action.
                tracing::warn!(egg = %e.name, error = %msg, "bulk egg update failed");
                failed.push(e.name.clone());
            }
        }
    }
    let mut body = Vec::new();
    if !ok.is_empty() {
        body.push(admin.t_with("admin/egg.updated_eggs", &[("eggs", &ok.join(", "))]));
    }
    if !failed.is_empty() {
        body.push(admin.t_with("admin/egg.failed_eggs", &[("eggs", &failed.join(", "))]));
    }
    let title = crate::i18n::trans_choice(
        &admin.locale(),
        "admin/egg.updated",
        2.0,
        &[("count".into(), ok.len().to_string()), ("total".into(), total.to_string())],
    );
    admin.session.flash(if failed.is_empty() { "success" } else { "warning" }, title, Some(body.join(" | ")));
    Ok(admin.redirect(&form.back("/admin/eggs")))
}
