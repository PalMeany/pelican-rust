//! Web installer (app/Livewire/Installer/PanelInstaller.php, ADR-0016): served while
//! `APP_INSTALLED` is false, before the panel has a database. Steps: requirements,
//! environment, database, eggs, cache, queue, session; each step's values are written
//! to `.env` after validation (`writeToEnvironment`) and carried as hidden fields.
//! Finishing installs the schema, seeds the Root Admin role, creates the admin user,
//! queues the selected eggs, sets `APP_INSTALLED=true` and hands a booted state to
//! the binary, which then serves the panel on the same listener.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use indexmap::IndexMap;
use minijinja::{Value as Mj, context};
use pelican_core::config::Config;
use pelican_core::env::{Env, EnvWrite, write_variables};
use pelican_core::state::AppState;
use pelican_db::install::SchemaState;
use serde_json::Value;

use crate::forms::{Errors, Form};

pub const STEPS: [&str; 7] = ["requirements", "environment", "database", "egg", "cache", "queue", "session"];
const DRIVERS: [(&str, &str); 4] = [("sqlite", "SQLite"), ("mariadb", "MariaDB"), ("mysql", "MySQL"), ("pgsql", "PostgreSQL")];

/// Called once with the booted state when installation finishes. It must switch the
/// listener to the panel before returning: the browser follows the redirect at once.
pub type OnFinished = Box<dyn FnOnce(AppState) + Send>;

#[derive(Clone)]
pub struct Installer {
    base: PathBuf,
    /// Double-submit CSRF secret (cookie + form field).
    token: String,
    done: Arc<Mutex<Option<OnFinished>>>,
}

/// One-time sign-in for the admin created by the installer (`auth()->login($user, true)`).
static LOGIN: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// Redeemed by `/installer/complete` once the panel serves requests.
pub fn take_login(token: &str) -> Option<i64> {
    let mut g = LOGIN.lock().ok()?;
    match g.as_ref() {
        Some((t, id)) if t == token => {
            let id = *id;
            *g = None;
            Some(id)
        }
        _ => None,
    }
}

pub fn router(base: PathBuf, done: OnFinished) -> Router {
    let inst = Installer { base, token: pelican_wings::jwt::str_random(40), done: Arc::new(Mutex::new(Some(done))) };
    Router::new()
        .route("/installer", get(show).post(submit))
        .route("/assets/{*path}", get(crate::assets::serve))
        .route(
            "/pelican.ico",
            get(|| crate::assets::serve(axum::extract::Path("pelican.ico".into()), axum::extract::RawQuery(None))),
        )
        // RedirectIfNotInstalled
        .fallback(|| async { Redirect::to("/installer") })
        .with_state(inst)
}

fn env_of(inst: &Installer) -> Env {
    Env::load(&inst.base).unwrap_or_default()
}

/// The submitted wizard state: every non-meta field.
fn data_of(form: &Form) -> IndexMap<String, String> {
    let v = form.to_value();
    let mut out = IndexMap::new();
    if let Value::Object(m) = v {
        for (k, v) in m {
            if matches!(k.as_str(), "_token" | "_install_token" | "step" | "go") {
                continue;
            }
            match v {
                Value::Array(a) => {
                    for (i, x) in a.into_iter().enumerate() {
                        out.insert(format!("{k}[{i}]"), pelican_core::php::string(&x));
                    }
                }
                other => {
                    out.insert(k, pelican_core::php::string(&other));
                }
            }
        }
    }
    out
}

fn list_of(data: &IndexMap<String, String>, key: &str) -> Vec<String> {
    let prefix = format!("{key}[");
    data.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(_, v)| v.clone()).collect()
}

struct Page<'a> {
    inst: &'a Installer,
    step: &'a str,
    data: IndexMap<String, String>,
    errors: Errors,
    notice: Option<(String, String, Option<String>)>,
}

fn t(locale: &str, key: &str) -> String {
    crate::i18n::trans(locale, key, &[])
}

async fn render(p: Page<'_>) -> Response {
    let env = env_of(p.inst);
    let locale = p.data.get("language").cloned().filter(|l| !l.is_empty()).unwrap_or_else(|| "en".into());
    let get = |k: &str, default: &str| p.data.get(k).cloned().unwrap_or_else(|| default.to_string());
    let idx = STEPS.iter().position(|s| *s == p.step).unwrap_or(0);
    // Fields shown on this step are edited; the others travel as hidden inputs.
    let shown: &[&str] = match p.step {
        "environment" => &["APP_NAME", "APP_URL", "user_email", "user_username", "user_password"],
        "database" => &["DB_CONNECTION", "DB_DATABASE", "DB_HOST", "DB_PORT", "DB_USERNAME", "DB_PASSWORD"],
        "egg" => &["eggs"],
        "cache" => &["CACHE_STORE", "REDIS_HOST", "REDIS_PORT", "REDIS_USERNAME", "REDIS_PASSWORD"],
        "queue" => &["QUEUE_CONNECTION"],
        "session" => &["SESSION_DRIVER"],
        _ => &[],
    };
    let hidden: Vec<(String, String)> = p
        .data
        .iter()
        .filter(|(k, _)| k.as_str() != "language" && !shown.iter().any(|s| *k == s || k.starts_with(&format!("{s}["))))
        .map(|(k, v)| {
            // Arrays go back as `name[]`.
            let name = match k.find('[') {
                Some(i) => format!("{}[]", &k[..i]),
                None => k.clone(),
            };
            (name, v.clone())
        })
        .collect();
    let driver = get("DB_CONNECTION", &env.string_or("DB_CONNECTION", "sqlite"));
    let mut index: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut index_error = None;
    if p.step == "egg" {
        let url = env.string_or("PANEL_EGG_INDEX_URL", pelican_api::egg_index::DEFAULT_INDEX_URL);
        match pelican_api::egg_index::fetch_index(&url).await {
            Ok(i) => {
                index = i.into_iter().map(|(nest, eggs)| (nest, {
                    let mut e: Vec<(String, String)> = eggs.into_iter().collect();
                    e.sort_by(|a, b| a.1.cmp(&b.1));
                    e
                })).collect()
            }
            Err(e) => index_error = Some(e),
        }
    }
    let requirements = requirement_checks(&p.inst.base);
    let ctx = context! {
        title => t(&locale, "installer.title"),
        app_name => env.string_or("APP_NAME", "Pelican"),
        locale => locale.clone(),
        csrf => p.inst.token.clone(),
        flash => Mj::from_serialize(Vec::<()>::new()),
        steps => STEPS.iter().map(|s| context! { id => *s, label => t(&locale, &format!("installer.{s}.title")) }).collect::<Vec<_>>(),
        step => p.step,
        step_index => idx,
        last => idx + 1 == STEPS.len(),
        hidden => hidden,
        errors => Mj::from_serialize(&p.errors),
        notice => p.notice.map(|(s, title, body)| context! { status => s, title => title, body => body }),
        languages => crate::app::languages(),
        language => locale,
        requirements => requirements,
        v => context! {
            APP_NAME => get("APP_NAME", &env.string_or("APP_NAME", "Pelican")),
            APP_URL => get("APP_URL", &env.string_or("APP_URL", "http://localhost")),
            user_email => get("user_email", ""),
            user_username => get("user_username", ""),
            user_password => get("user_password", ""),
            DB_CONNECTION => driver.clone(),
            DB_DATABASE => get("DB_DATABASE", &if driver == "sqlite" { "database.sqlite".to_string() } else { env.string_or("DB_DATABASE", "panel") }),
            DB_HOST => get("DB_HOST", &env.string_or("DB_HOST", "127.0.0.1")),
            DB_PORT => get("DB_PORT", &env.string_or("DB_PORT", if driver == "pgsql" { "5432" } else { "3306" })),
            DB_USERNAME => get("DB_USERNAME", &env.string_or("DB_USERNAME", "pelican")),
            DB_PASSWORD => get("DB_PASSWORD", ""),
            CACHE_STORE => get("CACHE_STORE", &env.string_or("CACHE_STORE", "file")),
            REDIS_HOST => get("REDIS_HOST", &env.string_or("REDIS_HOST", "127.0.0.1")),
            REDIS_PORT => get("REDIS_PORT", &env.string_or("REDIS_PORT", "6379")),
            REDIS_USERNAME => get("REDIS_USERNAME", ""),
            REDIS_PASSWORD => get("REDIS_PASSWORD", ""),
            QUEUE_CONNECTION => get("QUEUE_CONNECTION", &env.string_or("QUEUE_CONNECTION", "database")),
            SESSION_DRIVER => get("SESSION_DRIVER", &env.string_or("SESSION_DRIVER", "file")),
            eggs => list_of(&p.data, "eggs"),
        },
        drivers => DRIVERS.iter().map(|(k, l)| vec![k.to_string(), l.to_string()]).collect::<Vec<_>>(),
        index => index,
        index_error => index_error,
    };
    let html = match crate::views::render("installer/page.html", ctx) {
        Ok(h) => h,
        Err(e) => return crate::error::WebError::internal(e.to_string()).into_response(),
    };
    let mut resp = axum::response::Html(html).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("pelican_installer={}; Path=/; HttpOnly; SameSite=Lax", p.inst.token)) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// Folder permission checks (`RequirementsStep`): the Rust build has no PHP version or extensions.
fn requirement_checks(base: &Path) -> Vec<Mj> {
    let writable = |p: &Path| {
        let probe = p.join(".pelican-write-test");
        let ok = std::fs::create_dir_all(p).is_ok() && std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    };
    let folders = [("Storage", base.join("storage")), ("Database", base.join("database"))];
    let bad: Vec<&str> = folders.iter().filter(|(_, p)| !writable(p)).map(|(n, _)| *n).collect();
    let env_ok = {
        let f = base.join(".env");
        std::fs::OpenOptions::new().append(true).create(true).open(&f).is_ok()
    };
    vec![context! {
        title => "permissions",
        names => folders.iter().map(|(n, _)| *n).chain(std::iter::once(".env")).collect::<Vec<_>>().join(", "),
        ok => bad.is_empty() && env_ok,
        bad => bad.iter().copied().chain((!env_ok).then_some(".env")).collect::<Vec<_>>().join(", "),
    }]
}

fn installed(inst: &Installer) -> bool {
    env_of(inst).bool_or("APP_INSTALLED", true)
}

async fn show(State(inst): State<Installer>, axum::extract::RawQuery(q): axum::extract::RawQuery) -> Response {
    if installed(&inst) {
        return crate::error::WebError::NotFound.into_response();
    }
    let step = q
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "step").map(|(_, v)| v.into_owned()))
        .filter(|s| STEPS.contains(&s.as_str()))
        .unwrap_or_else(|| "requirements".into());
    // Only the first step can be opened directly (earlier steps' values travel in the form).
    let _ = step;
    render(Page { inst: &inst, step: "requirements", data: IndexMap::new(), errors: Errors::default(), notice: None }).await
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers.get(header::COOKIE)?.to_str().ok()?.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == "pelican_installer").then(|| v.to_string())
    })
}

async fn submit(State(inst): State<Installer>, headers: HeaderMap, form: Form) -> Response {
    if installed(&inst) {
        return crate::error::WebError::NotFound.into_response();
    }
    if form.get("_install_token") != inst.token || cookie_token(&headers).as_deref() != Some(inst.token.as_str()) {
        return crate::error::WebError::PageExpired.into_response();
    }
    let step = STEPS.iter().copied().find(|s| *s == form.get("step")).unwrap_or("requirements");
    let data = data_of(&form);
    let locale = data.get("language").cloned().unwrap_or_else(|| "en".into());
    let idx = STEPS.iter().position(|s| *s == step).unwrap_or(0);
    let go = form.get("go");
    if go == "back" {
        let prev = STEPS[idx.saturating_sub(1)];
        return render(Page { inst: &inst, step: prev, data, errors: Errors::default(), notice: None }).await;
    }
    if go == "language" {
        return render(Page { inst: &inst, step, data, errors: Errors::default(), notice: None }).await;
    }
    // afterValidation of the current step.
    let (errors, notice) = validate_step(&inst, step, &data, &locale).await;
    if !errors.is_empty() || notice.is_some() {
        return render(Page { inst: &inst, step, data, errors, notice }).await;
    }
    if idx + 1 < STEPS.len() {
        return render(Page { inst: &inst, step: STEPS[idx + 1], data, errors: Errors::default(), notice: None }).await;
    }
    match finish(&inst, &data, &locale).await {
        Ok(token) => Redirect::to(&format!("/installer/complete?token={token}")).into_response(),
        Err((title, body)) => {
            render(Page { inst: &inst, step, data, errors: Errors::default(), notice: Some(("danger".into(), title, Some(body))) })
                .await
        }
    }
}

fn required(errors: &mut Errors, locale: &str, data: &IndexMap<String, String>, key: &str, label_key: &str) -> bool {
    let empty = data.get(key).is_none_or(|v| v.trim().is_empty());
    if empty {
        let label = t(locale, label_key).to_lowercase();
        errors.add(key, crate::i18n::trans(locale, "validation.required", &[("attribute".into(), label)]));
    }
    !empty
}

/// `writeToEnv($key)`: the step's values without empty ones (`array_filter`).
fn write_env(inst: &Installer, values: &[(&str, Option<&String>)]) -> Result<(), String> {
    let vals: Vec<(String, EnvWrite)> = values
        .iter()
        .filter_map(|(k, v)| v.filter(|s| !s.is_empty()).map(|s| (k.to_string(), EnvWrite::Str(s.clone()))))
        .collect();
    write_variables(&inst.base.join(".env"), &vals, true).map_err(|e| e.to_string())
}

type Notice = Option<(String, String, Option<String>)>;

async fn validate_step(inst: &Installer, step: &str, data: &IndexMap<String, String>, locale: &str) -> (Errors, Notice) {
    let mut errors = Errors::default();
    let fail_env = |e: String| Some(("danger".to_string(), t(locale, "installer.exceptions.write_env"), Some(e)));
    match step {
        "requirements" => {
            let ok = requirement_checks(&inst.base).iter().all(|c| c.get_attr("ok").map(|v| v.is_true()).unwrap_or(false));
            if !ok {
                return (errors, Some(("danger".into(), t(locale, "installer.requirements.exception"), None)));
            }
        }
        "environment" => {
            let p = "installer.environment.fields";
            required(&mut errors, locale, data, "APP_NAME", &format!("{p}.app_name"));
            required(&mut errors, locale, data, "APP_URL", &format!("{p}.app_url"));
            if required(&mut errors, locale, data, "user_email", &format!("{p}.account.email")) {
                let email = data.get("user_email").map(String::as_str).unwrap_or_default();
                let input = serde_json::json!({ "email": email });
                let mut v = pelican_api::validation::Validator::for_value(&input);
                v.rules("email", &["email"]);
                v.attribute_name("email", &t(locale, &format!("{p}.account.email")).to_lowercase());
                v.run();
                for (_, m) in v.error_messages() {
                    errors.add("user_email", m);
                }
            }
            required(&mut errors, locale, data, "user_username", &format!("{p}.account.username"));
            required(&mut errors, locale, data, "user_password", &format!("{p}.account.password"));
            if errors.is_empty() {
                if let Err(e) = write_env(inst, &[("APP_NAME", data.get("APP_NAME")), ("APP_URL", data.get("APP_URL"))]) {
                    return (errors, fail_env(e));
                }
            }
        }
        "database" => {
            let driver = data.get("DB_CONNECTION").cloned().unwrap_or_default();
            if !DRIVERS.iter().any(|(k, _)| *k == driver) {
                errors.add("DB_CONNECTION", crate::i18n::trans(locale, "validation.required", &[("attribute".into(), t(locale, "installer.database.driver").to_lowercase())]));
            }
            let f = "installer.database.fields";
            required(&mut errors, locale, data, "DB_DATABASE", &format!("{f}.{}", if driver == "sqlite" { "path" } else { "name" }));
            if driver != "sqlite" {
                required(&mut errors, locale, data, "DB_HOST", &format!("{f}.host"));
                if required(&mut errors, locale, data, "DB_PORT", &format!("{f}.port")) {
                    let ok = data.get("DB_PORT").and_then(|p| p.parse::<u32>().ok()).is_some_and(|p| (1..=65535).contains(&p));
                    if !ok {
                        errors.add("DB_PORT", crate::i18n::trans(locale, "validation.between.numeric", &[("attribute".into(), t(locale, &format!("{f}.port")).to_lowercase()), ("min".into(), "1".into()), ("max".into(), "65535".into())]));
                    }
                }
                required(&mut errors, locale, data, "DB_USERNAME", &format!("{f}.username"));
            }
            if !errors.is_empty() {
                return (errors, None);
            }
            if driver != "sqlite" {
                let lookup = |k: &str| match k {
                    "DB_CONNECTION" | "DB_HOST" | "DB_PORT" | "DB_DATABASE" | "DB_USERNAME" | "DB_PASSWORD" => data.get(k).cloned(),
                    _ => None,
                };
                let tested = match pelican_db::connect::DbConfig::from_lookup(lookup, &inst.base) {
                    Ok(cfg) => pelican_db::connect(&cfg).await.map(|_| ()).map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                if let Err(e) = tested {
                    return (errors, Some(("danger".into(), t(locale, "installer.database.exceptions.connection"), Some(e))));
                }
            }
            let keys = ["DB_CONNECTION", "DB_DATABASE", "DB_HOST", "DB_PORT", "DB_USERNAME", "DB_PASSWORD"];
            let vals: Vec<(&str, Option<&String>)> = keys.iter().map(|k| (*k, data.get(*k))).collect();
            if let Err(e) = write_env(inst, &vals) {
                return (errors, fail_env(e));
            }
        }
        "cache" => {
            let store = data.get("CACHE_STORE").cloned().unwrap_or_default();
            if store == "redis" {
                let f = "installer.cache.fields";
                required(&mut errors, locale, data, "REDIS_HOST", &format!("{f}.host"));
                required(&mut errors, locale, data, "REDIS_PORT", &format!("{f}.port"));
                if !errors.is_empty() {
                    return (errors, None);
                }
                if let Err(e) = redis_ping(data).await {
                    return (errors, Some(("danger".into(), t(locale, "installer.cache.exception"), Some(e))));
                }
            }
            let keys = ["CACHE_STORE", "REDIS_HOST", "REDIS_PORT", "REDIS_USERNAME", "REDIS_PASSWORD"];
            let vals: Vec<(&str, Option<&String>)> =
                keys.iter().map(|k| (*k, if store == "redis" || *k == "CACHE_STORE" { data.get(*k) } else { None })).collect();
            if let Err(e) = write_env(inst, &vals) {
                return (errors, fail_env(e));
            }
        }
        "queue" => {
            // The scheduler and queue run inside the panel process, so there is no crontab or
            // queue worker to set up (the original hides those fields in containers too).
            if let Err(e) = write_env(inst, &[("QUEUE_CONNECTION", data.get("QUEUE_CONNECTION"))]) {
                return (errors, fail_env(e));
            }
        }
        _ => {}
    }
    (errors, None)
}

/// `RedisManager(...)->connection()->command('ping')` over a plain RESP connection.
async fn redis_ping(data: &IndexMap<String, String>) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let host = data.get("REDIS_HOST").cloned().unwrap_or_default();
    let port = data.get("REDIS_PORT").cloned().unwrap_or_default();
    let mut s = tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::TcpStream::connect(format!("{host}:{port}")))
        .await
        .map_err(|_| "Connection timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let cmd = |parts: &[&str]| {
        let mut out = format!("*{}\r\n", parts.len());
        for p in parts {
            out.push_str(&format!("${}\r\n{p}\r\n", p.len()));
        }
        out
    };
    let pass = data.get("REDIS_PASSWORD").cloned().unwrap_or_default();
    let user = data.get("REDIS_USERNAME").cloned().unwrap_or_default();
    let mut payload = String::new();
    if !pass.is_empty() {
        payload.push_str(&if user.is_empty() { cmd(&["AUTH", &pass]) } else { cmd(&["AUTH", &user, &pass]) });
    }
    payload.push_str(&cmd(&["PING"]));
    s.write_all(payload.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut buf))
        .await
        .map_err(|_| "Read timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
    if reply.contains("+PONG") { Ok(()) } else { Err(reply.trim().to_string()) }
}

/// `PanelInstaller::submit()`.
async fn finish(inst: &Installer, data: &IndexMap<String, String>, locale: &str) -> Result<String, (String, String)> {
    let env = env_of(inst);
    let (config, enc) = Config::from_env(env, &inst.base).map_err(|e| (t(locale, "installer.exceptions.migration"), e.to_string()))?;
    let migration = |e: String| (t(locale, "installer.database.exceptions.migration"), e);
    // runMigrations(): our schema comes from the reference dump (ADR-0003).
    let db = pelican_db::connect(&config.db).await.map_err(|e| migration(e.to_string()))?;
    match pelican_db::install::schema_state(&db).await.map_err(|e| migration(e.to_string()))? {
        SchemaState::Empty => pelican_db::install::install_fresh(&db, config.db.driver).await.map_err(|e| migration(e.to_string()))?,
        SchemaState::Ready { .. } => {}
        SchemaState::Outdated { missing } => return Err(migration(format!("{} migrations missing", missing.len()))),
    }
    let st = AppState::new(config, enc, db);
    // DatabaseSeeder: Role::firstOrCreate(['name' => 'Root Admin']).
    pelican_api::application::users::ensure_root_admin_role(&st).await.map_err(|e| migration(format!("{:?}", e.errors.first())))?;
    let user = pelican_api::application::users::create_admin(
        &st,
        data.get("user_username").map(String::as_str).unwrap_or_default(),
        data.get("user_email").map(String::as_str).unwrap_or_default(),
        data.get("user_password").map(String::as_str).unwrap_or_default(),
    )
    .await
    .map_err(|e| (t(locale, "installer.exceptions.create_user"), e.errors.first().map(|o| o.detail.clone()).unwrap_or_default()))?;
    let secure = st.config.app_url.starts_with("https://");
    let session = vec![
        ("SESSION_DRIVER".to_string(), EnvWrite::Str(data.get("SESSION_DRIVER").cloned().unwrap_or_else(|| "file".into()))),
        ("SESSION_SECURE_COOKIE".to_string(), EnvWrite::Str(if secure { "true".into() } else { "false".into() })),
    ];
    write_variables(&inst.base.join(".env"), &session, true).map_err(|e| (t(locale, "installer.exceptions.write_env"), e.to_string()))?;
    for url in list_of(data, "eggs") {
        let st = st.clone();
        tokio::spawn(async move { pelican_api::egg_index::install_from_url(&st, &url).await });
    }
    write_variables(&inst.base.join(".env"), &[("APP_INSTALLED".into(), EnvWrite::Str("true".into()))], true)
        .map_err(|e| (t(locale, "installer.exceptions.write_env"), e.to_string()))?;
    let token = pelican_wings::jwt::str_random(40);
    if let Ok(mut g) = LOGIN.lock() {
        *g = Some((token.clone(), user.id.0));
    }
    // Re-read the final .env for the panel.
    let env = Env::load(&inst.base).unwrap_or_default();
    let st = match Config::from_env(env, &inst.base) {
        Ok((config, enc)) => st.rebuild(config, enc),
        Err(_) => st,
    };
    if let Some(done) = inst.done.lock().ok().and_then(|mut g| g.take()) {
        done(st);
    }
    Ok(token)
}
