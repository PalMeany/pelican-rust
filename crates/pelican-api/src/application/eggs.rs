//! `/api/application/eggs` — `EggController`, `EggExporterService`,
//! `EggImporterService` (spec §3.4, docs/spec/eggs.md).

use axum::Extension;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use egg_format::import::ImportError;
use pelican_core::state::AppState;
use pelican_db::entities::{egg_variables, eggs, servers};
use pelican_db::{Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use super::data::{egg_item, egg_variable_attributes};
use super::{READ, WRITE};
use crate::envelope::{Includes, list};
use crate::error::{ApiError, ApiResult};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::validation::Validator;

fn bound(ctx: &Ctx) -> ApiResult<eggs::Model> {
    ctx.bound.egg.clone().ok_or_else(ApiError::model_not_found)
}

/// `GET /eggs` (`Egg::all()`, unpaginated).
pub async fn index(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("egg", READ)?;
    let all = eggs::Model::find_by_statement(raw::stmt(st.db.get_database_backend(), "select * from {eggs}", vec![]))
        .all(&st.db)
        .await?;
    let inc = Includes::parse(&input);
    let mut data = Vec::new();
    for e in &all {
        data.push(egg_item(&st, &ctx, e, &inc).await?);
    }
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `GET /eggs/{egg}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("egg", READ)?;
    let e = bound(&ctx)?;
    Ok(PhpJson::ok(egg_item(&st, &ctx, &e, &Includes::parse(&input)).await?).into_response())
}

fn json_col(v: &Option<Json>) -> Value {
    v.as_ref().map(|j| egg_format::export::php_assoc(j.0.clone())).unwrap_or(Value::Null)
}

/// `EggExporterService::getEggIconAsBase64()`.
fn icon_data_uri(st: &AppState, e: &eggs::Model) -> Value {
    let Some(uuid) = &e.uuid else { return Value::Null };
    for (ext, mime) in [("png", "image/png"), ("jpg", "image/jpeg"), ("webp", "image/webp")] {
        let path = st.config.base_path.join("storage/app/public/icons/egg").join(format!("{uuid}.{ext}"));
        if let Ok(bytes) = std::fs::read(&path) {
            return json!(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)));
        }
    }
    Value::Null
}

/// `GET /eggs/{egg}/export?format=yaml|json` (default yaml).
pub async fn export(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("egg", READ)?;
    let mut v = Validator::new(&input);
    v.rules("format", &["nullable", "string", "in:yaml,json"]);
    v.validate()?;
    let json_format = input.get("format").and_then(Value::as_str) == Some("json");
    let e = bound(&ctx)?;
    let parent = |id: Option<Int>| {
        let st = st.clone();
        async move {
            match id {
                Some(id) => eggs::Entity::find_by_id(id).one(&st.db).await,
                None => Ok(None),
            }
        }
    };
    let config_from = parent(e.config_from).await?;
    let script_from = parent(e.copy_script_from).await?;
    let inherit = |own: &Option<String>, f: fn(&eggs::Model) -> Option<String>| -> Option<String> {
        if own.is_some() || e.config_from.is_none() { own.clone() } else { config_from.as_ref().and_then(f) }
    };
    let copy = |own: Option<String>, f: fn(&eggs::Model) -> Option<String>| -> Option<String> {
        if own.as_deref().is_some_and(|s| !s.is_empty() && s != "0") || e.copy_script_from.is_none() {
            own
        } else {
            script_from.as_ref().and_then(f)
        }
    };
    let denylist_src = if e.config_from.is_none() {
        e.file_denylist.as_ref().map(|j| j.0.clone())
    } else {
        config_from.as_ref().and_then(|c| c.file_denylist.as_ref().map(|j| j.0.clone()))
    };
    let denylist: Vec<Value> = match denylist_src {
        Some(Value::Array(a)) => a,
        Some(Value::Object(m)) => m.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|v| !pelican_core::php::empty(v))
    .collect();
    let vars = egg_variables::Model::find_by_statement(raw::stmt(
        st.db.get_database_backend(),
        "select * from {egg_variables} where {egg_variables.egg_id} = ? and {egg_variables.egg_id} is not null",
        vec![e.id.into()],
    ))
    .all(&st.db)
    .await?;
    let variables: Vec<Value> = vars
        .iter()
        .map(|v| {
            let mut a = egg_variable_attributes(st.driver, v, None);
            for k in ["id", "egg_id", "created_at", "updated_at"] {
                a.shift_remove(k);
            }
            Value::Object(a)
        })
        .collect();
    let doc = json!({
        "_comment": "DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL",
        "meta": {"version": "PLCN_v3", "update_url": e.update_url},
        "exported_at": pelican_core::time::atom(&pelican_core::time::now()),
        "name": e.name,
        "author": e.author,
        "uuid": e.uuid,
        "description": e.description,
        "icon": icon_data_uri(&st, &e),
        "tags": json_col(&e.tags),
        "features": json_col(&e.features),
        "docker_images": json_col(&e.docker_images),
        "file_denylist": denylist,
        "startup_commands": json_col(&e.startup_commands),
        "config": {
            "files": inherit(&e.config_files, |c| c.config_files.clone()),
            "startup": inherit(&e.config_startup, |c| c.config_startup.clone()),
            "logs": inherit(&e.config_logs, |c| c.config_logs.clone()),
            "stop": inherit(&e.config_stop, |c| c.config_stop.clone()),
        },
        "scripts": {"installation": {
            "script": copy(e.script_install.clone(), |c| c.script_install.clone()),
            "container": copy(Some(e.script_container.clone()), |c| Some(c.script_container.clone())),
            "entrypoint": copy(Some(e.script_entry.clone()), |c| Some(c.script_entry.clone())),
        }},
        "variables": variables,
    });
    let (body, ext) = if json_format {
        (egg_format::export::to_json(&doc).ok_or_else(|| ApiError::internal("JsonException"))?, "json")
    } else {
        (egg_format::export::to_yaml(&doc), "yaml")
    };
    let filename = format!("egg-{}.{ext}", egg_format::export::kebab_name(&e.name));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, format!("application/{ext}")),
            (header::CONTENT_DISPOSITION, format!("attachment; filename={filename}")),
        ],
        body,
    )
        .into_response())
}

fn import_err(e: ImportError) -> ApiError {
    match e {
        ImportError::Parse(_) | ImportError::UnknownVersion => {
            ApiError::display("InvalidFileUploadException", 400, e.to_string())
        }
        ImportError::Php(code) => ApiError::internal(code),
    }
}

fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Bool(b)) => Some(if *b { "1".into() } else { String::new() }),
        Some(other) => Some(pelican_core::php::string(other)),
    }
}

/// `json_encode(json_decode($text), JSON_PRETTY_PRINT)`: objects kept, invalid → `"null"`.
fn repretty(v: Option<&Value>) -> String {
    use laravel_compat::json::{JSON_PRETTY_PRINT, to_string_with_flags};
    let text = opt_str(v);
    let decoded = text.as_deref().and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
    to_string_with_flags(&decoded, JSON_PRETTY_PRINT).unwrap_or_else(|_| "null".into())
}

fn json_or_null(v: Option<&Value>) -> Option<Json> {
    v.filter(|v| !v.is_null()).map(|v| Json(egg_format::export::php_assoc(v.clone())))
}

/// `POST /eggs/import` (raw body parsed as YAML).
pub async fn import(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("egg", WRITE)?;
    let content = String::from_utf8_lossy(&input.raw).into_owned();
    let parsed = egg_format::import::parse(&content).map_err(import_err)?;
    let vars: Vec<Value> = match parsed.get("variables") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        // count(null): TypeError.
        _ => return Err(ApiError::internal("TypeError")),
    };
    let uuid = opt_str(parsed.get("uuid")).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let existing = eggs::Entity::find().filter(eggs::Column::Uuid.eq(uuid.clone())).one(&txn).await?;

    // Icon: written before the commit; failures are reported and ignored.
    if let Some(Value::String(icon)) = parsed.get("icon") {
        save_icon(&st, &uuid, icon);
    }
    let denylist: Value = match parsed.get("file_denylist") {
        Some(Value::Array(a)) => {
            let kept: Map<String, Value> = a
                .iter()
                .enumerate()
                .filter(|(_, v)| !pelican_core::php::empty(v))
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect();
            egg_format::export::php_assoc(Value::Object(kept))
        }
        Some(Value::Object(m)) => egg_format::export::php_assoc(Value::Object(
            m.iter().filter(|(_, v)| !pelican_core::php::empty(v)).map(|(k, v)| (k.clone(), v.clone())).collect(),
        )),
        _ => json!([]),
    };
    let required = |v: Option<String>| v.ok_or_else(|| ApiError::internal("QueryException"));
    let mut am = eggs::ActiveModel {
        uuid: Set(Some(uuid.clone())),
        author: Set(required(opt_str(parsed.get("author")))?),
        copy_script_from: Set(None),
        config_from: Set(None),
        name: Set(required(opt_str(parsed.get("name")))?),
        description: Set(opt_str(parsed.get("description"))),
        tags: Set(Some(Json(egg_format::export::php_assoc(parsed.get("tags").cloned().unwrap_or(json!([])))))),
        features: Set(json_or_null(parsed.get("features"))),
        docker_images: Set(json_or_null(parsed.get("docker_images"))),
        file_denylist: Set(Some(Json(denylist))),
        update_url: Set(opt_str(get(&parsed, "meta.update_url"))),
        config_files: Set(Some(repretty(get(&parsed, "config.files")))),
        config_startup: Set(Some(repretty(get(&parsed, "config.startup")))),
        config_logs: Set(Some(repretty(get(&parsed, "config.logs")))),
        config_stop: Set(opt_str(get(&parsed, "config.stop"))),
        startup_commands: Set(json_or_null(parsed.get("startup_commands"))),
        script_install: Set(opt_str(get(&parsed, "scripts.installation.script"))),
        script_entry: Set(required(opt_str(get(&parsed, "scripts.installation.entrypoint")))?),
        script_container: Set(required(opt_str(get(&parsed, "scripts.installation.container")))?),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let egg = match existing {
        Some(e) => {
            am.id = sea_orm::ActiveValue::Unchanged(e.id);
            pelican_db::write::update(&txn, &am).await?;
            e.id
        }
        None => {
            am.created_at = Set(Some(now));
            pelican_db::write::insert(&txn, am).await?.id
        }
    };
    let mut imported = Vec::new();
    for var in &vars {
        let Value::Object(m) = var else { return Err(ApiError::internal("TypeError")) };
        let env = opt_str(m.get("env_variable")).unwrap_or_default();
        imported.push(env.clone());
        let rules = match m.get("rules") {
            Some(Value::Array(a)) => Value::Array(a.clone()),
            Some(Value::Null) | None => return Err(ApiError::internal("ErrorException")),
            Some(other) => Value::Array(pelican_core::php::string(other).split('|').map(|s| json!(s)).collect()),
        };
        let mut row = egg_variables::ActiveModel { updated_at: Set(Some(now)), ..Default::default() };
        for (k, v) in m {
            match k.as_str() {
                "egg_id" | "env_variable" | "field_type" | "rules" => {}
                "name" => row.name = Set(opt_str(Some(v)).unwrap_or_default()),
                "description" => row.description = Set(opt_str(Some(v)).unwrap_or_default()),
                "default_value" => row.default_value = Set(opt_str(Some(v)).unwrap_or_default()),
                "user_viewable" => row.user_viewable = Set(SmallBool(!pelican_core::php::empty(v))),
                "user_editable" => row.user_editable = Set(SmallBool(!pelican_core::php::empty(v))),
                "sort" => {
                    row.sort =
                        Set(v.as_i64().map(Int).or_else(|| opt_str(Some(v)).and_then(|s| s.parse().ok()).map(Int)))
                }
                // Unknown keys are written as columns by the unguarded model: SQL error.
                _ => return Err(ApiError::internal("QueryException")),
            }
        }
        row.rules = Set(Json(rules));
        let found = egg_variables::Entity::find()
            .filter(egg_variables::Column::EggId.eq(egg))
            .filter(egg_variables::Column::EnvVariable.eq(env.clone()))
            .one(&txn)
            .await?;
        match found {
            Some(f) => {
                row.id = sea_orm::ActiveValue::Unchanged(f.id);
                pelican_db::write::update(&txn, &row).await?;
            }
            None => {
                row.egg_id = Set(egg);
                row.env_variable = Set(env);
                row.created_at = Set(Some(now));
                pelican_db::write::insert(&txn, row).await?;
            }
        }
    }
    // Variables not in the file are removed (server values cascade).
    let b = st.db.get_database_backend();
    if imported.is_empty() {
        txn.execute_raw(raw::stmt(
            b,
            "delete from {egg_variables} where {egg_variables.egg_id} = ? and {egg_variables.egg_id} is not null",
            vec![egg.into()],
        ))
        .await?;
    } else {
        let marks = vec!["?"; imported.len()].join(", ");
        let mut vals: Vec<sea_orm::Value> = vec![egg.into()];
        vals.extend(imported.iter().map(|s| sea_orm::Value::from(s.clone())));
        txn.execute_raw(raw::stmt(
            b,
            &format!("delete from {{egg_variables}} where {{egg_variables.egg_id}} = ? and {{egg_variables.egg_id}} is not null and {{env_variable}} not in ({marks})"),
            vals,
        ))
        .await?;
    }
    txn.commit().await?;
    let fresh = eggs::Entity::find_by_id(egg).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::with_status(StatusCode::CREATED, egg_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?)
        .into_response())
}

/// `saveEggIconFromBase64()` → `HasIcon::writeIcon()` on the public disk.
fn save_icon(st: &AppState, uuid: &str, data_uri: &str) {
    let Some(rest) = data_uri.strip_prefix("data:image/") else { return };
    let Some((ext, b64)) = rest.split_once(";base64,") else { return };
    let ext = match ext.to_lowercase().as_str() {
        "jpeg" | "jpg" => "jpg",
        "png" => "png",
        "webp" => "webp",
        _ => return,
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else { return };
    let dir = st.config.base_path.join("storage/app/public/icons/egg");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for other in ["png", "jpg", "jpeg", "webp", "svg"] {
        let _ = std::fs::remove_file(dir.join(format!("{uuid}.{other}")));
    }
    let _ = std::fs::write(dir.join(format!("{uuid}.{ext}")), bytes);
}

/// `DELETE /eggs/{egg}` and `/eggs/uuid/{uuid}`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.acl("egg", WRITE)?;
    let e = bound(&ctx)?;
    if servers::Entity::find().filter(servers::Column::EggId.eq(e.id)).one(&st.db).await?.is_some() {
        return Err(ApiError::display(
            "HasActiveServersException",
            400,
            "An Egg with active servers attached to it cannot be deleted from the Panel.",
        ));
    }
    if eggs::Entity::find().filter(eggs::Column::ConfigFrom.eq(e.id)).one(&st.db).await?.is_some() {
        return Err(ApiError::display(
            "HasChildrenException",
            400,
            "This Egg is a parent to one or more other Eggs. Please delete those Eggs before deleting this Egg.",
        ));
    }
    eggs::Entity::delete_by_id(e.id).exec(&st.db).await?;
    Ok(no_content())
}
