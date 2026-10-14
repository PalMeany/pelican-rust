//! Profile page (app/Filament/Pages/Auth/EditProfile.php): account,
//! multi-factor authentication, API keys, SSH keys, activity, customization.

use std::sync::LazyLock;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::services::LogActivity;
use pelican_api::validation::{CustomRule, Validator};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{activity_logs, api_keys, user_ssh_keys, users};
use pelican_db::{Int, Json, Timestamp, raw};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde_json::{Value, json};

use crate::auth::totp;
use crate::ctx::Authed;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};

static PHP_LISTS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../pelican-api/data/php-lists.json")).unwrap_or(Value::Null)
});

const TABS: [&str; 7] = ["account", "2fa", "api_keys", "ssh_keys", "activity", "customization", "oauth"];
const MFA_APP: &str = "filament-panels::auth/multi-factor/app";
const MFA_EMAIL: &str = "filament-panels::auth/multi-factor/email";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/profile", get(show).post(save))
        .route("/profile/api-keys", post(api_key_create))
        .route("/profile/api-keys/{id}/delete", post(api_key_delete))
        .route("/profile/ssh-keys", post(ssh_key_create))
        .route("/profile/ssh-keys/{id}/delete", post(ssh_key_delete))
        .route("/profile/mfa/app/setup", get(app_setup_form).post(app_setup))
        .route("/profile/mfa/app/disable", post(app_disable))
        .route("/profile/mfa/app/recovery-codes", post(app_regenerate))
        .route("/profile/mfa/email/setup", get(email_setup_form).post(email_setup))
        .route("/profile/mfa/email/resend", post(email_resend))
        .route("/profile/mfa/email/disable", post(email_disable))
        .route("/profile/avatar", post(avatar_upload).layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)))
        .route("/profile/avatar/delete", post(avatar_delete))
        .route("/profile/oauth/{driver}/link", post(crate::auth::oauth::link))
        .route("/profile/oauth/{driver}/unlink", post(oauth_unlink))
}

/// `FileUpload::make('avatar')` (png only, stored as `avatars/<id>.png` on the public disk).
async fn avatar_upload(web: Authed, body: axum::body::Bytes) -> WebResult {
    if !web.st.config.env.bool_or("FILAMENT_UPLOADABLE_AVATARS", false) {
        return Err(WebError::Forbidden);
    }
    if !body.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(WebError::Unprocessable);
    }
    let path = crate::models::avatar_path(&web.st, web.user.id());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| WebError::internal(e.to_string()))?;
    }
    std::fs::write(&path, &body).map_err(|e| WebError::internal(e.to_string()))?;
    Ok(axum::response::IntoResponse::into_response(axum::http::StatusCode::NO_CONTENT))
}

/// The profile tab's unlink action (`UserUpdateService::handle(['oauth' => …])`).
async fn oauth_unlink(web: Authed, axum::extract::Path(driver): axum::extract::Path<String>) -> WebResult {
    let schema = crate::auth::oauth::enabled(&web.st.config.env)
        .into_iter()
        .find(|s| s.id == driver)
        .ok_or(WebError::NotFound)?;
    if crate::auth::oauth::unlink_user(&web.st, &web.user.user, schema.id).await? {
        web.flash_success(web.t_with("profile.unlinked", &[("name", &schema.name)]));
    }
    Ok(web.redirect("/profile?tab=oauth"))
}

async fn avatar_delete(web: Authed) -> WebResult {
    let _ = std::fs::remove_file(crate::models::avatar_path(&web.st, web.user.id()));
    Ok(web.redirect("/profile"))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn timezones() -> Vec<String> {
    PHP_LISTS["timezones"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// `title_case(Locale::getDisplayName($code, $code))` for the shipped locales.
fn native_name(code: &str) -> &str {
    match code {
        "ar" => "العربية",
        "be" => "Беларуская",
        "bg" => "Български",
        "cs" => "Čeština",
        "da" => "Dansk",
        "de" => "Deutsch",
        "el" => "Ελληνικά",
        "en" => "English",
        "es" => "Español",
        "fi" => "Suomi",
        "fr" => "Français",
        "hu" => "Magyar",
        "id" => "Indonesia",
        "it" => "Italiano",
        "ja" => "日本語",
        "ko" => "한국어",
        "lt" => "Lietuvių",
        "nl" => "Nederlands",
        "no" => "Norsk",
        "pl" => "Polski",
        "pt" => "Português",
        "pt_BR" => "Português (Brasil)",
        "ro" => "Română",
        "ru" => "Русский",
        "sk" => "Slovenčina",
        "sr" => "Српски",
        "sv" => "Svenska",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "vi" => "Tiếng Việt",
        "zh_CN" => "中文 (中国)",
        "zh_TW" => "中文 (台灣)",
        other => other,
    }
}

/// `LanguageService::getAvailableLanguages()`: code → display name.
fn languages() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = crate::i18n::locales()
        .into_iter()
        .map(|code| {
            let name = crate::i18n::trans(&code, "profile.current_language_name", &[]);
            let name = if name == "profile.current_language_name" { native_name(&code).to_string() } else { name };
            (code, name)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn tab_of(web: &Authed) -> String {
    let q = web.query.as_deref().unwrap_or("");
    form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == "tab")
        .map(|(_, v)| v.into_owned())
        .filter(|t| TABS.contains(&t.as_str()))
        .unwrap_or_else(|| "account".into())
}

struct Account {
    username: String,
    email: String,
    timezone: String,
    language: String,
    custom: serde_json::Map<String, Value>,
}

impl Account {
    fn from_user(web: &Authed) -> Account {
        let u = &web.user.user;
        let mut custom = crate::models::customization(&web.st, u);
        // Migration from boolean to string navigation types (mutateFormDataBeforeFill).
        if let Some(Value::Bool(b)) = custom.get("top_navigation") {
            let v = if *b { "topbar" } else { "sidebar" };
            custom.insert("top_navigation".into(), json!(v));
        }
        Account {
            username: u.username.clone(),
            email: u.email.clone(),
            timezone: u.timezone.clone(),
            language: u.language.clone(),
            custom,
        }
    }

    fn from_form(f: &Form) -> Account {
        let mut custom = serde_json::Map::new();
        for k in ["console_font", "dashboard_layout", "top_navigation"] {
            custom.insert(k.into(), json!(f.get(k)));
        }
        for k in ["console_font_size", "console_rows", "console_graph_period"] {
            custom.insert(k.into(), json!(f.get(k)));
        }
        custom.insert("button_style".into(), json!(f.bool("button_style")));
        if f.opt("redirect_to_admin").is_some() {
            custom.insert("redirect_to_admin".into(), json!(f.bool("redirect_to_admin")));
        }
        Account {
            username: f.get("username").trim().into(),
            email: f.get("email").trim().into(),
            timezone: f.get("timezone").into(),
            language: f.get("language").into(),
            custom,
        }
    }
}

/// Fonts offered for the console: `monospace` + `storage/app/public/fonts/*.ttf`.
fn console_fonts(st: &AppState) -> Vec<String> {
    let mut fonts = vec!["monospace".to_string()];
    let dir = crate::models::public_disk(st).join("fonts");
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut found: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                (p.extension().and_then(|x| x.to_str()) == Some("ttf"))
                    .then(|| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                    .flatten()
            })
            .collect();
        found.sort();
        fonts.extend(found);
    }
    fonts
}

async fn render(web: &Authed, account: &Account, errors: &Errors, extra: Mj) -> WebResult {
    let st = &web.st;
    let u = &web.user.user;
    let keys = api_keys::Entity::find()
        .filter(api_keys::Column::UserId.eq(u.id.0))
        .filter(api_keys::Column::KeyType.eq(1))
        .order_by_asc(api_keys::Column::Id)
        .all(&st.db)
        .await?;
    let ssh = user_ssh_keys::Entity::find()
        .filter(user_ssh_keys::Column::UserId.eq(u.id.0))
        .filter(user_ssh_keys::Column::DeletedAt.is_null())
        .order_by_asc(user_ssh_keys::Column::Id)
        .all(&st.db)
        .await?;
    let logs = activity_logs::Entity::find()
        .filter(activity_logs::Column::ActorType.eq("user"))
        .filter(activity_logs::Column::ActorId.eq(u.id.0))
        .order_by_desc(activity_logs::Column::Timestamp)
        .limit(50)
        .all(&st.db)
        .await?;
    let see_ips = web.user.gate_before("seeIps activityLog");
    let activity = crate::activity::entries(&st.db, logs, &web.locale(), see_ips).await?;
    let app_enabled = u.mfa_app_secret.as_deref().is_some_and(|s| !s.is_empty());
    let language_help = (!["en"].contains(&account.language.as_str()))
        .then(|| web.t_with("profile.language_help", &[("state", &account.language)]));
    web.render(
        "app/profile.html",
        context! {
            title => web.t("profile.title"),
            tab => tab_of(web),
            managed => u.is_managed_externally.0,
            is_admin => web.user.is_admin(),
            oauth => crate::auth::oauth::enabled(&web.st.config.env)
                .into_iter()
                .map(|s| {
                    let linked = u.oauth.as_ref().and_then(|j| j.0.get(s.id)).is_some();
                    context! { id => s.id, name => s.name, icon => s.icon, color => s.color, linked => linked }
                })
                .collect::<Vec<_>>(),
            avatars_uploadable => web.st.config.env.bool_or("FILAMENT_UPLOADABLE_AVATARS", false),
            has_avatar => crate::models::avatar_path(&web.st, web.user.id()).exists(),
            avatar_url => crate::models::avatar_url(&web.st, u),
            v => context! {
                username => account.username, email => account.email,
                timezone => account.timezone, language => account.language,
                custom => Mj::from_serialize(&account.custom),
            },
            errors => Mj::from_serialize(errors),
            timezones => timezones().into_iter().map(|t| vec![t.clone(), t]).collect::<Vec<_>>(),
            languages => languages().into_iter().map(|(c, n)| vec![c, n]).collect::<Vec<_>>(),
            language_help => language_help,
            fonts => console_fonts(st).into_iter().map(|f| vec![f.clone(), f]).collect::<Vec<_>>(),
            api_keys => keys.iter().map(|k| context! {
                id => k.id.0,
                identifier => k.identifier.clone().unwrap_or_default(),
                memo => k.memo.clone().unwrap_or_default(),
            }).collect::<Vec<_>>(),
            ssh_keys => ssh.iter().map(|k| context! { id => k.id.0, name => k.name.clone(), fingerprint => k.fingerprint.clone() }).collect::<Vec<_>>(),
            activity => Mj::from_serialize(&activity),
            mfa => context! {
                app => app_enabled,
                email => u.mfa_email_enabled.0,
                recovery_left => if app_enabled { recovery_hashes(web, u).len() } else { 0 },
            },
            ..extra
        },
    )
}

/// `GET /profile`.
async fn show(web: Authed) -> WebResult {
    let account = Account::from_user(&web);
    render(&web, &account, &Errors::default(), context! {}).await
}

async fn unique_taken(web: &Authed, column: users::Column, value: &str) -> Result<bool, WebError> {
    Ok(!value.is_empty()
        && users::Entity::find()
            .filter(column.eq(value))
            .filter(users::Column::Id.ne(web.user.id()))
            .one(&web.st.db)
            .await?
            .is_some())
}

/// `POST /profile`: EditProfile::save (account + customization).
async fn save(web: Authed, form: Form) -> WebResult {
    let account = Account::from_form(&form);
    let managed = web.user.user.is_managed_externally.0;
    let password = form.get("password").to_string();
    let data = json!({
        "username": account.username, "email": account.email,
        "password": password, "passwordConfirmation": form.get("passwordConfirmation"),
        "timezone": account.timezone, "language": account.language,
        "console_font_size": account.custom["console_font_size"], "console_rows": account.custom["console_rows"],
        "console_graph_period": account.custom["console_graph_period"],
        "dashboard_layout": account.custom["dashboard_layout"],
    });
    let mut v = Validator::for_value(&data);
    for (field, key) in [
        ("username", "profile.username"),
        ("email", "profile.email"),
        ("password", "profile.password"),
        ("passwordConfirmation", "profile.password_confirmation"),
        ("timezone", "profile.timezone"),
        ("language", "profile.language"),
        ("console_font_size", "profile.font_size"),
        ("console_rows", "profile.rows"),
        ("console_graph_period", "profile.graph_period"),
        ("dashboard_layout", "profile.dashboard_layout"),
    ] {
        v.attribute_name(field, &web.t(key));
    }
    if !managed {
        v.rules("username", &["required", "max:255"]);
        let taken = unique_taken(&web, users::Column::Username, &account.username).await?;
        v.custom(
            "username",
            CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
        );
        v.rules("email", &["required", "email", "max:255"]);
        let taken = unique_taken(&web, users::Column::Email, &account.email).await?;
        v.custom(
            "email",
            CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
        );
        if !password.is_empty() {
            v.rules("password", &["min:8", "same:passwordConfirmation"]);
            v.rules("passwordConfirmation", &["required"]);
        }
    }
    let tz = timezones().join(",");
    v.rules("timezone", &["required", &format!("in:{tz}")]);
    let langs: Vec<String> = crate::i18n::locales();
    v.rules("language", &["required", &format!("in:{}", langs.join(","))]);
    v.rules("console_font_size", &["required", "numeric", "min:1"]);
    v.rules("console_rows", &["required", "numeric", "min:1"]);
    v.rules("console_graph_period", &["required", "numeric", "min:10", "max:120"]);
    v.rules("dashboard_layout", &["required", "in:grid,table"]);
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        return render(&web, &account, &errors, context! {}).await;
    }

    // mutateFormDataBeforeSave(): only these keys are stored (servers_per_page is dropped).
    let num = |k: &str| account.custom.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let current = crate::models::customization(&web.st, &web.user.user);
    let custom = json!({
        "console_font": account.custom["console_font"],
        "console_font_size": num("console_font_size"),
        "console_rows": num("console_rows"),
        "console_graph_period": num("console_graph_period"),
        "dashboard_layout": account.custom["dashboard_layout"],
        "top_navigation": account.custom["top_navigation"],
        "button_style": account.custom["button_style"],
        "redirect_to_admin": account.custom.get("redirect_to_admin").cloned().unwrap_or_else(|| current.get("redirect_to_admin").cloned().unwrap_or(json!(false))),
    });
    let st = &web.st;
    let b = st.db.get_database_backend();
    let json_set = if b == DbBackend::Postgres { "{customization} = CAST(? AS json)" } else { "{customization} = ?" };
    let mut sets = vec![
        "{timezone} = ?".to_string(),
        "{language} = ?".to_string(),
        json_set.to_string(),
        "{updated_at} = ?".to_string(),
    ];
    let mut vals: Vec<sea_orm::Value> = vec![
        account.timezone.clone().into(),
        account.language.clone().into(),
        laravel_compat::json::to_string(&custom).map_err(WebError::internal)?.into(),
        pelican_core::time::now().into(),
    ];
    if !managed {
        sets.insert(0, "{username} = ?".into());
        vals.insert(0, account.username.to_lowercase().into());
        sets.insert(1, "{email} = ?".into());
        vals.insert(1, account.email.to_lowercase().into());
        if !password.is_empty() {
            let hash = laravel_compat::hashing::make(&password, st.config.bcrypt_rounds).map_err(WebError::internal)?;
            sets.insert(2, "{password} = ?".into());
            vals.insert(2, hash.into());
        }
    }
    vals.push(web.user.id().into());
    st.db
        .execute_raw(raw::stmt(b, &format!("update {{users}} set {} where {{id}} = ?", sets.join(", ")), vals))
        .await?;
    web.flash_success(web.t("filament-panels::auth/pages/edit-profile.notifications.saved.title"));
    Ok(web.redirect(&format!("/profile?tab={}", form.opt("_tab").filter(|t| TABS.contains(t)).unwrap_or("account"))))
}

// ----- API keys (User::createToken) -----

async fn api_key_create(web: Authed, form: Form) -> WebResult {
    let st = &web.st;
    let description = form.get("description").trim().to_string();
    let ips: Vec<String> = form
        .get("allowed_ips")
        .split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if description.is_empty() {
        return Ok(web.redirect("/profile?tab=api_keys"));
    }
    for ip in &ips {
        if pelican_core::net::IpRange::try_parse(ip).is_err() {
            web.flash_danger(format!("\"{ip}\" is not a valid IP address or CIDR range."), None);
            return Ok(web.redirect("/profile?tab=api_keys"));
        }
    }
    let count = api_keys::Entity::find()
        .filter(api_keys::Column::UserId.eq(web.user.id()))
        .filter(api_keys::Column::KeyType.eq(1))
        .all(&st.db)
        .await?
        .len() as i64;
    if count >= st.config.api_key_limit {
        web.flash_danger("You have reached the account limit for number of API keys.", None);
        return Ok(web.redirect("/profile?tab=api_keys"));
    }
    let identifier = format!("pacc_{}", pelican_wings::jwt::str_random(11));
    let secret = pelican_wings::jwt::str_random(32);
    let now = Timestamp(pelican_core::time::now());
    let am = api_keys::ActiveModel {
        user_id: Set(Some(web.user.user.id)),
        key_type: Set(Int(1)),
        identifier: Set(Some(identifier.clone())),
        token: Set(st.enc.encrypt_string(&secret)),
        memo: Set(Some(description)),
        allowed_ips: Set(Json(json!(ips))),
        permissions: Set(Some(Json(json!([])))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&st.db, &am).await?.unwrap_or(0);
    let _ = web
        .activity("user:api-key.create")
        .subject(morph::USER, web.user.id())
        .subject(morph::API_KEY, id)
        .property("identifier", identifier.clone())
        .log(st)
        .await;
    // Shown once, persistent notification (body = identifier . plainTextToken).
    web.session.flash("success", web.t("profile.api_key_created"), Some(format!("{identifier}{secret}")));
    Ok(web.redirect("/profile?tab=api_keys"))
}

async fn api_key_delete(web: Authed, Path(id): Path<i64>) -> WebResult {
    let st = &web.st;
    if let Some(key) = api_keys::Entity::find_by_id(id)
        .filter(api_keys::Column::UserId.eq(web.user.id()))
        .filter(api_keys::Column::KeyType.eq(1))
        .one(&st.db)
        .await?
    {
        api_keys::Entity::delete_by_id(key.id).exec(&st.db).await?;
        let _ = web
            .activity("user:api-key.delete")
            .subject(morph::USER, web.user.id())
            .subject(morph::API_KEY, key.id.0)
            .property("identifier", key.identifier.clone())
            .log(st)
            .await;
    }
    Ok(web.redirect("/profile?tab=api_keys"))
}

// ----- SSH keys (KeyCreationService) -----

async fn ssh_key_create(web: Authed, form: Form) -> WebResult {
    use pelican_core::sshkey::{KeyKind, PublicKey};
    let st = &web.st;
    let name = form.get("name").trim().to_string();
    let public_key = form.get("public_key").trim().to_string();
    if name.is_empty() || public_key.is_empty() {
        return Ok(web.redirect("/profile?tab=ssh_keys"));
    }
    let fail = |msg: &str| {
        web.flash_danger(web.t("profile.could_not_create_ssh_key"), Some(msg.to_string()));
        Ok(web.redirect("/profile?tab=ssh_keys"))
    };
    let Ok(key) = PublicKey::parse(&public_key) else { return fail("The public key provided is not valid.") };
    if matches!(key.kind, KeyKind::Dsa) {
        return fail("DSA keys are not supported.");
    }
    if key.rsa_bits().is_some_and(|b| b < 2048) {
        return fail("RSA keys must be at least 2048 bytes in length.");
    }
    let fingerprint = key.fingerprint_sha256();
    let exists = user_ssh_keys::Entity::find()
        .filter(user_ssh_keys::Column::UserId.eq(web.user.id()))
        .filter(user_ssh_keys::Column::DeletedAt.is_null())
        .filter(user_ssh_keys::Column::Fingerprint.eq(fingerprint.clone()))
        .one(&st.db)
        .await?
        .is_some();
    if exists {
        return fail("The public key provided already exists on your account.");
    }
    let now = Timestamp(pelican_core::time::now());
    let am = user_ssh_keys::ActiveModel {
        user_id: Set(web.user.user.id),
        name: Set(name),
        fingerprint: Set(fingerprint.clone()),
        public_key: Set(key.to_pkcs8_pem()),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&st.db, &am).await?.unwrap_or(0);
    let _ = web
        .activity("user:ssh-key.create")
        .subject(morph::USER, web.user.id())
        .subject(morph::SSH_KEY, id)
        .property("fingerprint", fingerprint.clone())
        .log(st)
        .await;
    web.session.flash("success", web.t("profile.ssh_key_created"), Some(format!("SHA256:{fingerprint}")));
    Ok(web.redirect("/profile?tab=ssh_keys"))
}

async fn ssh_key_delete(web: Authed, Path(id): Path<i64>) -> WebResult {
    let st = &web.st;
    if let Some(key) = user_ssh_keys::Entity::find_by_id(id)
        .filter(user_ssh_keys::Column::UserId.eq(web.user.id()))
        .filter(user_ssh_keys::Column::DeletedAt.is_null())
        .one(&st.db)
        .await?
    {
        let now = Timestamp(pelican_core::time::now());
        user_ssh_keys::Entity::update_many()
            .col_expr(user_ssh_keys::Column::DeletedAt, Expr::value(Some(now)))
            .col_expr(user_ssh_keys::Column::UpdatedAt, Expr::value(now))
            .filter(user_ssh_keys::Column::Id.eq(key.id))
            .exec(&st.db)
            .await?;
        let _ = web
            .activity("user:ssh-key.delete")
            .subject(morph::USER, web.user.id())
            .subject(morph::SSH_KEY, key.id.0)
            .property("fingerprint", key.fingerprint.clone())
            .log(st)
            .await;
    }
    Ok(web.redirect("/profile?tab=ssh_keys"))
}

// ----- Multi-factor authentication (Filament AppAuthentication / EmailAuthentication) -----

fn recovery_hashes(web: &Authed, u: &users::Model) -> Vec<String> {
    u.mfa_app_recovery_codes
        .as_deref()
        .and_then(|c| web.st.enc.decrypt_string(c).ok())
        .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
        .unwrap_or_default()
}

/// `generateRecoveryCodes()`: 8 × `Str::random(10) . '-' . Str::random(10)`.
fn generate_recovery_codes() -> Vec<String> {
    (0..8).map(|_| format!("{}-{}", pelican_wings::jwt::str_random(10), pelican_wings::jwt::str_random(10))).collect()
}

fn hash_codes(web: &Authed, codes: &[String]) -> Result<String, WebError> {
    let hashed: Result<Vec<String>, _> =
        codes.iter().map(|c| laravel_compat::hashing::make(c, web.st.config.bcrypt_rounds)).collect();
    let json = laravel_compat::json::to_string(&hashed.map_err(WebError::internal)?).map_err(WebError::internal)?;
    Ok(web.st.enc.encrypt_string(&json))
}

async fn set_user_columns(web: &Authed, sets: &[&str], mut vals: Vec<sea_orm::Value>) -> Result<(), WebError> {
    let b = web.st.db.get_database_backend();
    let mut cols: Vec<String> = sets.iter().map(|c| format!("{{{c}}} = ?")).collect();
    cols.push("{updated_at} = ?".into());
    vals.push(pelican_core::time::now().into());
    vals.push(web.user.id().into());
    web.st
        .db
        .execute_raw(raw::stmt(b, &format!("update {{users}} set {} where {{id}} = ?", cols.join(", ")), vals))
        .await?;
    Ok(())
}

fn password_ok(web: &Authed, password: &str) -> bool {
    laravel_compat::hashing::check(password, &web.user.user.password).unwrap_or(false)
}

/// 5 attempts per user (Filament `rateLimitAuthenticationAttempt`).
fn rate_limited(web: &Authed, action: &str) -> bool {
    let key = format!("filament-{action}:{}", web.user.id());
    if web.st.limiter.too_many_attempts(&key, 5, now()).is_some() {
        return true;
    }
    web.st.limiter.hit(&key, 60, now());
    false
}

/// QR code of the provisioning URI as an inline SVG.
fn qr_svg(uri: &str) -> String {
    qrcode::QrCode::new(uri.as_bytes())
        .map(|c| c.render::<qrcode::render::svg::Color<'_>>().min_dimensions(192, 192).quiet_zone(true).build())
        .unwrap_or_default()
}

async fn render_app_setup(web: &Authed, errors: &Errors, codes: Option<Vec<String>>) -> WebResult {
    let data = web.session.data();
    let secret = data.extra.get("mfa_setup_secret").and_then(Value::as_str).unwrap_or("").to_string();
    let issuer = web.st.config.app_name.clone();
    let uri = totp::provisioning_uri(&issuer, &web.user.user.email, &secret);
    web.render(
        "app/mfa_app_setup.html",
        context! {
            title => web.t(&format!("{MFA_APP}/actions/set-up.modal.heading")),
            secret => secret,
            qr => Mj::from_safe_string(qr_svg(&uri)),
            errors => Mj::from_serialize(errors),
            recovery_codes => codes,
        },
    )
}

/// `GET /profile/mfa/app/setup`: new secret kept in the session until confirmed.
async fn app_setup_form(web: Authed) -> WebResult {
    if web.user.user.mfa_app_secret.as_deref().is_some_and(|s| !s.is_empty()) {
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    web.session.update(|d| {
        d.extra.insert("mfa_setup_secret".into(), json!(totp::generate_secret()));
    });
    render_app_setup(&web, &Errors::default(), None).await
}

/// `POST /profile/mfa/app/setup`: verify code + current password, store secret and recovery codes.
async fn app_setup(web: Authed, form: Form) -> WebResult {
    let secret = web.session.data().extra.get("mfa_setup_secret").and_then(Value::as_str).map(String::from);
    let Some(secret) = secret else { return Ok(web.redirect("/profile?tab=2fa")) };
    let mut errors = Errors::default();
    let base = format!("{MFA_APP}/actions/set-up.modal.form");
    if rate_limited(&web, "set-up-app-authentication") {
        errors.add("password", web.t(&format!("{base}.code.messages.rate_limited")));
    } else {
        let code = form.get("code").trim();
        if code.is_empty() {
            errors.add(
                "code",
                web.t_with(
                    "validation.required",
                    &[("attribute", &web.t(&format!("{base}.code.validation_attribute")))],
                ),
            );
        } else if !totp::verify(&secret, code, now()) {
            errors.add("code", web.t(&format!("{base}.code.messages.invalid")));
        }
        if !password_ok(&web, form.get("password")) {
            errors.add("password", web.t("validation.current_password"));
        }
    }
    if !errors.is_empty() {
        return render_app_setup(&web, &errors, None).await;
    }
    let codes = generate_recovery_codes();
    set_user_columns(
        &web,
        &["mfa_app_secret", "mfa_app_recovery_codes"],
        vec![web.st.enc.encrypt_string(&secret).into(), hash_codes(&web, &codes)?.into()],
    )
    .await?;
    web.session.update(|d| {
        d.extra.remove("mfa_setup_secret");
    });
    web.flash_success(web.t(&format!("{MFA_APP}/actions/set-up.notifications.enabled.title")));
    render_app_setup(&web, &Errors::default(), Some(codes)).await
}

fn verify_app_code_or_recovery(web: &Authed, form: &Form) -> Result<Option<Vec<String>>, ()> {
    let u = &web.user.user;
    let code = form.get("code").trim();
    if !code.is_empty() {
        let secret = u.mfa_app_secret.as_deref().and_then(|s| web.st.enc.decrypt_string(s).ok()).unwrap_or_default();
        return if totp::verify_once(&secret, code, now()) { Ok(None) } else { Err(()) };
    }
    let recovery = form.get("recovery_code").trim();
    if recovery.is_empty() {
        return Err(());
    }
    let hashes = recovery_hashes(web, u);
    let mut remaining = hashes.clone();
    match hashes.iter().position(|h| laravel_compat::hashing::check(recovery, h).unwrap_or(false)) {
        Some(i) => {
            remaining.remove(i);
            Ok(Some(remaining))
        }
        None => Err(()),
    }
}

/// `POST /profile/mfa/app/disable`.
async fn app_disable(web: Authed, form: Form) -> WebResult {
    let base = format!("{MFA_APP}/actions/disable.modal.form");
    if rate_limited(&web, "disable-app-authentication") {
        web.flash_danger(web.t(&format!("{base}.code.messages.rate_limited")), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    if verify_app_code_or_recovery(&web, &form).is_err() {
        let key = if form.get("recovery_code").trim().is_empty() { "code" } else { "recovery_code" };
        web.flash_danger(web.t(&format!("{base}.{key}.messages.invalid")), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    set_user_columns(
        &web,
        &["mfa_app_secret", "mfa_app_recovery_codes"],
        vec![Option::<String>::None.into(), Option::<String>::None.into()],
    )
    .await?;
    web.flash_success(web.t(&format!("{MFA_APP}/actions/disable.notifications.disabled.title")));
    Ok(web.redirect("/profile?tab=2fa"))
}

/// `POST /profile/mfa/app/recovery-codes`: regenerate (code + password).
async fn app_regenerate(web: Authed, form: Form) -> WebResult {
    let base = format!("{MFA_APP}/actions/regenerate-recovery-codes.modal.form");
    if rate_limited(&web, "regenerate-app-authentication-recovery-codes") {
        web.flash_danger(web.t(&format!("{base}.code.messages.rate_limited")), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    let secret =
        web.user.user.mfa_app_secret.as_deref().and_then(|s| web.st.enc.decrypt_string(s).ok()).unwrap_or_default();
    if !totp::verify_once(&secret, form.get("code").trim(), now()) {
        web.flash_danger(web.t(&format!("{base}.code.messages.invalid")), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    if !password_ok(&web, form.get("password")) {
        web.flash_danger(web.t("validation.current_password"), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    let codes = generate_recovery_codes();
    set_user_columns(&web, &["mfa_app_recovery_codes"], vec![hash_codes(&web, &codes)?.into()]).await?;
    web.flash_success(web.t(&format!("{MFA_APP}/actions/regenerate-recovery-codes.notifications.regenerated.title")));
    web.render(
        "app/mfa_app_setup.html",
        context! {
            title => web.t(&format!("{MFA_APP}/actions/regenerate-recovery-codes.show_new_recovery_codes.modal.heading")),
            recovery_codes => codes,
            regenerated => true,
        },
    )
}

async fn render_email_setup(web: &Authed, errors: &Errors) -> WebResult {
    web.render(
        "app/mfa_email_setup.html",
        context! { title => web.t(&format!("{MFA_EMAIL}/actions/set-up.modal.heading")), errors => Mj::from_serialize(errors) },
    )
}

/// `GET /profile/mfa/email/setup`: send a code to the account email.
async fn email_setup_form(web: Authed) -> WebResult {
    if web.user.user.mfa_email_enabled.0 {
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    crate::auth::send_email_code(&web, &web.user.user).await?;
    render_email_setup(&web, &Errors::default()).await
}

async fn email_setup(web: Authed, form: Form) -> WebResult {
    if !crate::auth::verify_email_code(&web, form.get("code").trim()) {
        let mut errors = Errors::default();
        errors.add("code", web.t(&format!("{MFA_EMAIL}/actions/set-up.modal.form.code.messages.invalid")));
        return render_email_setup(&web, &errors).await;
    }
    set_user_columns(&web, &["mfa_email_enabled"], vec![true.into()]).await?;
    web.flash_success(web.t(&format!("{MFA_EMAIL}/actions/set-up.notifications.enabled.title")));
    Ok(web.redirect("/profile?tab=2fa"))
}

async fn email_resend(web: Authed) -> WebResult {
    let key = format!("{MFA_EMAIL}/actions/set-up.modal.form.code.actions.resend.notifications");
    if crate::auth::send_email_code(&web, &web.user.user).await? {
        web.flash_success(web.t(&format!("{key}.resent.title")));
    } else {
        web.flash_danger(web.t(&format!("{key}.throttled.title")), None);
    }
    if web.user.user.mfa_email_enabled.0 {
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    render_email_setup(&web, &Errors::default()).await
}

/// `POST /profile/mfa/email/disable`: requires a fresh emailed code (sent by `resend`).
async fn email_disable(web: Authed, form: Form) -> WebResult {
    if !crate::auth::verify_email_code(&web, form.get("code").trim()) {
        web.flash_danger(web.t(&format!("{MFA_EMAIL}/actions/disable.modal.form.code.messages.invalid")), None);
        return Ok(web.redirect("/profile?tab=2fa"));
    }
    set_user_columns(&web, &["mfa_email_enabled"], vec![false.into()]).await?;
    web.flash_success(web.t(&format!("{MFA_EMAIL}/actions/disable.notifications.disabled.title")));
    Ok(web.redirect("/profile?tab=2fa"))
}
