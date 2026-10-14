//! `/servers/{server}/files/*` — `FileController`, `FileUploadController`
//! (client-api.md §8.5); Wings calls as `DaemonFileRepository`.

use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use pelican_core::server_config::php_assoc;
use pelican_core::state::AppState;
use pelican_db::entities::{nodes, servers};
use pelican_wings::client::{Body, Call, Method};
use pelican_wings::{Node, WingsError};
use sea_orm::EntityTrait;
use serde_json::{Map, Value, json};

use crate::envelope::{item, list};
use crate::error::{ApiError, ApiResult};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::validation::Validator;

/// `$server->node` (not transfer-aware) and its decrypted connection.
async fn node(st: &AppState, server: &servers::Model) -> ApiResult<Node> {
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))
}

fn wings_err(e: WingsError) -> ApiError {
    ApiError::internal(e.code())
}

/// Wings 400 → `FileExistsException` (write, create-directory).
fn exists_err(e: WingsError) -> ApiError {
    match e {
        WingsError::Request { status: 400, .. } => ApiError::internal("FileExistsException"),
        e => wings_err(e),
    }
}

async fn call(st: &AppState, server: &servers::Model, c: Call<'_>) -> ApiResult<pelican_wings::client::WingsResponse> {
    let conn = node(st, server).await?;
    st.wings.send(&conn, c).await.map_err(wings_err)
}

fn path(server: &servers::Model, action: &str) -> String {
    format!("/api/servers/{}/files/{action}", server.uuid)
}

/// `$request->input($key)` (null when absent).
fn input(i: &Input, key: &str) -> Value {
    i.get(key).cloned().unwrap_or(Value::Null)
}

/// `$value ?? '/'`.
fn or_root(v: &Value) -> Value {
    if v.is_null() { json!("/") } else { v.clone() }
}

/// `$request->get($key)` for a validated string.
fn get_str(i: &Input, key: &str) -> String {
    i.get_query_first(key).map(pelican_core::php::string).unwrap_or_default()
}

/// PHP `rawurldecode`: `%XX` sequences only (`+` is kept).
fn rawurldecode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `convert_to_utf8()` (app/helpers.php): UTF-8 unchanged, UTF-16 by BOM, else ISO-8859-1.
fn convert_to_utf8(body: Vec<u8>) -> Vec<u8> {
    let body = match String::from_utf8(body) {
        Ok(s) => return s.into_bytes(),
        Err(e) => e.into_bytes(),
    };
    let le = body.starts_with(b"\xFF\xFE");
    if le || body.starts_with(b"\xFE\xFF") {
        // mbstring "UTF-16": the BOM picks the byte order and is consumed; bad units → '?'.
        let units: Vec<u16> = body[2..]
            .chunks(2)
            .map(|c| match c {
                [a, b] if le => u16::from_le_bytes([*a, *b]),
                [a, b] => u16::from_be_bytes([*a, *b]),
                _ => 0xFFFF,
            })
            .collect();
        let mut out = String::new();
        for r in char::decode_utf16(units.iter().copied()) {
            match r {
                Ok('\u{FFFF}') | Err(_) => out.push('?'),
                Ok(c) => out.push(c),
            }
        }
        return out.into_bytes();
    }
    body.iter().map(|b| *b as char).collect::<String>().into_bytes()
}

/// `Carbon::parse($v)->toAtomString()` for Wings' RFC 3339 timestamps.
fn atom(v: Option<&Value>) -> ApiResult<String> {
    let s = match v {
        None | Some(Value::Null) => String::new(),
        Some(v) => pelican_core::php::string(v),
    };
    if s.trim().is_empty() {
        return Ok(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
    }
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(&s) {
        return Ok(d.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
    }
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
        return Ok(d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
    }
    Err(ApiError::internal("InvalidFormatException"))
}

/// Coercion of a PHP typed property (`?string`, non-strict mode).
fn php_str(v: Option<&Value>, nullable: bool) -> ApiResult<Value> {
    match v {
        None | Some(Value::Null) if nullable => Ok(Value::Null),
        None | Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => {
            Err(ApiError::internal("TypeError"))
        }
        Some(v) => Ok(Value::String(pelican_core::php::string(v))),
    }
}

/// `FileObjectData::fromModel()` (§9.13).
fn file_object(model: &Value) -> ApiResult<Value> {
    let Value::Object(m) = model else { return Err(ApiError::internal("TypeError")) };
    let size = match m.get("size") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::Number(n)) if n.is_i64() => Value::Number(n.clone()),
        Some(Value::Number(n)) => json!(n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::Bool(b)) => json!(*b as i64),
        Some(Value::String(s)) if pelican_core::php::is_numeric_str(s) => {
            json!(s.trim().parse::<f64>().unwrap_or(0.0) as i64)
        }
        Some(_) => return Err(ApiError::internal("TypeError")),
    };
    let boolean = |key: &str, default: bool| -> ApiResult<bool> {
        match m.get(key) {
            None => Ok(default),
            Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => Err(ApiError::internal("TypeError")),
            Some(v) => Ok(!pelican_core::php::empty(v)),
        }
    };
    let mut a = Map::new();
    a.insert("name".into(), php_str(m.get("name"), true)?);
    a.insert("mode".into(), php_str(m.get("mode"), true)?);
    a.insert("mode_bits".into(), m.get("mode_bits").cloned().unwrap_or(Value::Null));
    a.insert("size".into(), size);
    a.insert("is_file".into(), json!(boolean("file", true)?));
    a.insert("is_symlink".into(), json!(boolean("symlink", false)?));
    let mime = match m.get("mime") {
        None => json!("application/octet-stream"),
        v => php_str(v, false)?,
    };
    a.insert("mimetype".into(), mime);
    a.insert("created_at".into(), json!(atom(m.get("created"))?));
    a.insert("modified_at".into(), json!(atom(m.get("modified"))?));
    Ok(item("file_object", Value::Object(a), vec![]))
}

/// `GET /files/list`.
pub async fn list_directory(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(i): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "file.read").await?;
    let mut v = Validator::new(&i);
    v.rules("directory", &["sometimes", "nullable", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let dir = match i.get_query_first("directory") {
        None | Some(Value::Null) => "/".to_string(),
        Some(d) => pelican_core::php::string(d),
    };
    let p = path(&server, "list-directory");
    let q = [("directory", dir)];
    let resp = call(&st, &server, Call::new(Method::GET, &p).query(&q)).await?;
    let items: Vec<Value> = match resp.json() {
        Some(Value::Array(a)) => a,
        Some(Value::Object(m)) => m.into_iter().map(|(_, v)| v).collect(),
        _ => return Err(ApiError::internal("TypeError")),
    };
    let data = items.iter().map(file_object).collect::<ApiResult<Vec<_>>>()?;
    Ok(PhpJson::ok(list(data)).into_response())
}

/// `GET /files/contents`.
pub async fn contents(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.read-content").await?;
    let mut v = Validator::new(&i);
    v.rules("file", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let file = get_str(&i, "file");
    let conn = node(&st, &server).await?;
    let p = path(&server, "contents");
    let q = [("file", file.clone())];
    let resp = match st.wings.send(&conn, Call::new(Method::GET, &p).query(&q)).await {
        Ok(r) => r,
        Err(WingsError::Request { status: 400, .. }) => return Err(ApiError::internal("FileNotEditableException")),
        Err(WingsError::Request { status: 404, .. }) => return Err(ApiError::internal("FileNotFoundException")),
        Err(e) => return Err(wings_err(e)),
    };
    let max = st.config.files_max_edit_size;
    if max != 0 && resp.content_length.is_some_and(|l| l as i128 > max as i128) {
        return Err(ApiError::display(
            "FileSizeTooLargeException",
            400,
            "The file you are attempting to open is too large to view in the file editor.",
        ));
    }
    ctx.activity("server:file.read").property("file", file).save(&st.db).await?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], convert_to_utf8(resp.body))
        .into_response())
}

/// Node JWT for `/download/file` and `/upload/file` (NodeJWTService, 15 minutes).
fn signed_url(
    st: &AppState,
    ctx: &Ctx,
    conn: &Node,
    server: &servers::Model,
    kind: &str,
    claims: Vec<(String, Value)>,
) -> ApiResult<Response> {
    let user = ctx.user()?;
    let now = chrono::Utc::now().timestamp();
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: conn.connection_address(),
        identified_by: format!("{}{}", user.id(), server.uuid),
        issued_at: now,
        expires_at: now + 15 * 60,
        subject: None,
        claims,
        user_uuid: Some(user.user.uuid.clone()),
        scope: format!("file-{kind}"),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&conn.token)
    .map_err(|_| ApiError::internal("InvalidKeyProvided"))?;
    let url = format!("{}/{kind}/file?token={token}", conn.connection_address());
    Ok(PhpJson::ok(json!({"object": "signed_url", "attributes": {"url": url}})).into_response())
}

/// `GET /files/download`.
pub async fn download(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.read-content").await?;
    let mut v = Validator::new(&i);
    v.rules("file", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let file = get_str(&i, "file");
    // lcobucci's encoder throws on invalid UTF-8 (JSON_THROW_ON_ERROR).
    let file_path = String::from_utf8(rawurldecode(&file)).map_err(|_| ApiError::internal("CannotEncodeContent"))?;
    let conn = node(&st, &server).await?;
    let claims = vec![("file_path".into(), json!(file_path)), ("server_uuid".into(), json!(server.uuid))];
    let resp = signed_url(&st, &ctx, &conn, &server, "download", claims)?;
    ctx.activity("server:file.download").property("file", file).save(&st.db).await?;
    Ok(resp)
}

/// `GET /files/upload`.
pub async fn upload(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let server = ctx.bound.server()?.clone();
    let conn = node(&st, &server).await?;
    signed_url(&st, &ctx, &conn, &server, "upload", vec![("server_uuid".into(), json!(server.uuid))])
}

/// `POST /files/write` — the raw request body is the file content.
pub async fn write(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let mut v = Validator::new(&i);
    v.rules("file", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let file = get_str(&i, "file");
    let conn = node(&st, &server).await?;
    let p = path(&server, "write");
    let q = [("file", file.clone())];
    let mut c = Call::new(Method::POST, &p).query(&q);
    c.body = Body::Raw(i.raw.to_vec());
    st.wings.send(&conn, c).await.map_err(exists_err)?;
    ctx.activity("server:file.write").property("file", file).save(&st.db).await?;
    Ok(no_content())
}

/// `POST /files/create-folder`.
pub async fn create_folder(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(i): In,
) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["sometimes", "nullable", "string"]);
    v.rules("name", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let name = input(&i, "name");
    // `$request->input('root', '/')`: the default applies only when absent; null → TypeError.
    let root = match i.get("root") {
        None => json!("/"),
        Some(Value::Null) => return Err(ApiError::internal("TypeError")),
        Some(r) => r.clone(),
    };
    let conn = node(&st, &server).await?;
    let p = path(&server, "create-directory");
    let c = Call::new(Method::POST, &p).json(json!({"name": name, "path": root}));
    st.wings.send(&conn, c).await.map_err(exists_err)?;
    ctx.activity("server:file.create-directory")
        .property("name", name)
        .property("directory", input(&i, "root"))
        .save(&st.db)
        .await?;
    Ok(no_content())
}

/// `PUT /files/rename`.
pub async fn rename(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.update").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["required", "nullable", "string"]);
    v.rules("files", &["required", "array"]);
    v.rules("files.*", &["array"]);
    v.rules("files.*.to", &["required", "string"]);
    v.rules("files.*.from", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let root = input(&i, "root");
    let files = php_assoc(input(&i, "files"));
    let p = path(&server, "rename");
    call(&st, &server, Call::new(Method::PUT, &p).json(json!({"root": or_root(&root), "files": files}))).await?;
    // `$files[0]['to']`: a missing key raises a warning → ErrorException.
    let first = match &files {
        Value::Array(a) => a.first().cloned(),
        Value::Object(m) => m.get("0").cloned(),
        _ => None,
    };
    let (Some(to), Some(from)) =
        (first.as_ref().and_then(|f| f.get("to")).cloned(), first.as_ref().and_then(|f| f.get("from")).cloned())
    else {
        return Err(ApiError::internal("ErrorException"));
    };
    ctx.activity("server:file.rename")
        .property("directory", root)
        .property("files", files)
        .property("to", to)
        .property("from", from)
        .save(&st.db)
        .await?;
    Ok(no_content())
}

/// `POST /files/copy`.
pub async fn copy(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let mut v = Validator::new(&i);
    v.rules("location", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let location = input(&i, "location");
    let p = path(&server, "copy");
    call(&st, &server, Call::new(Method::POST, &p).json(json!({"location": location}))).await?;
    ctx.activity("server:file.copy").property("file", location).save(&st.db).await?;
    Ok(no_content())
}

/// `POST /files/compress`.
pub async fn compress(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.archive").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["sometimes", "nullable", "string"]);
    v.rules("files", &["required", "array"]);
    v.rules("files.*", &["string"]);
    v.rules("name", &["sometimes", "nullable", "string"]);
    v.rules("extension", &["sometimes", "in:zip,tgz,tar.gz,txz,tar.xz,tbz2,tar.bz2"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let root = input(&i, "root");
    let files = php_assoc(input(&i, "files"));
    let or_empty = |v: Value| if v.is_null() { json!("") } else { v };
    let body = json!({
        "root": or_root(&root),
        "files": files,
        "name": or_empty(input(&i, "name")),
        "extension": or_empty(input(&i, "extension")),
    });
    let p = path(&server, "compress");
    let mut c = Call::new(Method::POST, &p).json(body);
    c.timeout = Some(Duration::from_secs(15 * 60));
    let resp = call(&st, &server, c).await?;
    let file = resp.json().unwrap_or(Value::Null);
    let Some(name) = file.get("name").cloned() else { return Err(ApiError::internal("ErrorException")) };
    ctx.activity("server:file.compress")
        .property("name", name)
        .property("directory", root)
        .property("files", files)
        .save(&st.db)
        .await?;
    Ok(PhpJson::ok(file_object(&file)?).into_response())
}

/// `POST /files/decompress`.
pub async fn decompress(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["sometimes", "nullable", "string"]);
    v.rules("file", &["required", "string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let root = input(&i, "root");
    let file = input(&i, "file");
    let p = path(&server, "decompress");
    let mut c = Call::new(Method::POST, &p).json(json!({"root": or_root(&root), "file": file}));
    c.timeout = Some(Duration::from_secs(15 * 60));
    call(&st, &server, c).await?;
    ctx.activity("server:file.decompress").property("directory", root).property("file", file).save(&st.db).await?;
    Ok(no_content())
}

/// `POST /files/delete`.
pub async fn delete(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.delete").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["required", "nullable", "string"]);
    v.rules("files", &["required", "array"]);
    v.rules("files.*", &["string"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let root = input(&i, "root");
    let files = php_assoc(input(&i, "files"));
    let p = path(&server, "delete");
    call(&st, &server, Call::new(Method::POST, &p).json(json!({"root": or_root(&root), "files": files.clone()})))
        .await?;
    ctx.activity("server:file.delete").property("directory", root).property("files", files).save(&st.db).await?;
    Ok(no_content())
}

/// `POST /files/chmod` (no activity).
pub async fn chmod(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.update").await?;
    let mut v = Validator::new(&i);
    v.rules("root", &["required", "nullable", "string"]);
    v.rules("files", &["required", "array"]);
    v.rules("files.*.file", &["required", "string"]);
    v.rules("files.*.mode", &["required", "numeric"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let root = input(&i, "root");
    let files = php_assoc(input(&i, "files"));
    let p = path(&server, "chmod");
    call(&st, &server, Call::new(Method::POST, &p).json(json!({"root": or_root(&root), "files": files}))).await?;
    Ok(no_content())
}

/// `POST /files/pull`.
pub async fn pull(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(i): In) -> ApiResult<Response> {
    ctx.authorize(&st, "file.create").await?;
    let mut v = Validator::new(&i);
    v.rules("url", &["required", "string", "url"]);
    v.rules("directory", &["nullable", "string"]);
    v.rules("filename", &["nullable", "string"]);
    v.rules("use_header", &["boolean"]);
    v.rules("foreground", &["boolean"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let url = input(&i, "url");
    let directory = input(&i, "directory");
    let mut body = Map::new();
    body.insert("url".into(), url.clone());
    body.insert("root".into(), or_root(&directory));
    // `$request->safe([...])` + array_filter(!is_null).
    for (from, to) in [("filename", "file_name"), ("use_header", "use_header"), ("foreground", "foreground")] {
        if let Some(val) = i.get(from).filter(|v| !v.is_null()) {
            body.insert(to.into(), php_assoc(val.clone()));
        }
    }
    let p = path(&server, "pull");
    call(&st, &server, Call::new(Method::POST, &p).json(Value::Object(body))).await?;
    ctx.activity("server:file.pull").property("directory", directory).property("url", url).save(&st.db).await?;
    Ok(no_content())
}
