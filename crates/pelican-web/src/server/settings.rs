//! Server panel › Settings (app/Filament/Server/Pages/Settings.php): server
//! information (live-saved name/description, icon upload/delete), limits,
//! SFTP details, reinstall with confirmation and the per-user backup
//! notification toggles (`updateNotificationSetting`).

use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Bytes, to_bytes};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::Value;
use minijinja::context;
use pelican_api::binding::Bound;
use pelican_api::pipeline::Ctx;
use pelican_api::services::{LogActivity, server_saved};
use pelican_api::validation::Validator;
use pelican_core::activity::morph;
use pelican_db::entities::{allocations, backups, databases, eggs, server_user_settings, servers};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, TransactionTrait,
};
use serde_json::{Map, json};

use super::ServerPanel;
use crate::error::{WebError, WebResult};
use crate::forms::Form;
use pelican_core::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/settings", get(page))
        .route("/server/{server}/settings/rename", post(rename))
        .route("/server/{server}/settings/description", post(description))
        .route("/server/{server}/settings/reinstall", post(reinstall))
        .route("/server/{server}/settings/notifications", post(notifications))
        .route("/server/{server}/settings/icon/url", post(icon_url))
        .route("/server/{server}/settings/icon/file", post(icon_file))
        .route("/server/{server}/settings/icon/delete", post(icon_delete))
}

/// Back-link POSTs return to; `_back` only when it targets this page.
fn back_url(form: &Form, p: &ServerPanel) -> String {
    form.back(&format!("{}/settings", p.base_url()))
}

fn ts_now() -> Timestamp {
    Timestamp(pelican_core::time::now())
}

/// A client-API `Ctx` for the domain functions (`server_saved`): ip and actor
/// like this web session, default subject = the server.
fn api_ctx(p: &ServerPanel) -> Result<Ctx, WebError> {
    let ip: IpAddr = p.ip.parse().map_err(|_| WebError::internal("invalid request ip"))?;
    Ok(Ctx {
        ip,
        user: Some(Arc::new(p.user.clone())),
        bound: Arc::new(Bound { server: Some(p.server.clone()), ..Default::default() }),
        node: None,
        root: p.st.config.app_url.clone(),
        subject: Some((morph::SERVER, p.server.id.0)),
    })
}

const ICON_DIR: &str = "icons/server"; // Server::getIconStoragePath()
/// HasIcon::$iconFormats keys (`DeleteIcon::getIconFormats()`).
const ICON_FORMATS: [&str; 3] = ["png", "jpg", "webp"];
/// The sweep list of `HasIcon::writeIcon()`.
const SWEEP_FORMATS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "svg"];

/// Eloquent `save()` of one column plus `updated_at`.
async fn touch_column(st: &AppState, server_id: i64, col: servers::Column, value: Expr) -> Result<(), WebError> {
    servers::Entity::update_many()
        .col_expr(col, value)
        .col_expr(servers::Column::UpdatedAt, Expr::value(Timestamp(pelican_core::time::now())))
        .filter(servers::Column::Id.eq(server_id))
        .exec(&st.db)
        .await?;
    Ok(())
}

// ----- GET /settings ----------------------------------------------------------------

/// PHP `Number::format($n)` in the en locale (thousands grouping).
pub(crate) fn format_number(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// One disabled prefix field of the limits display.
#[derive(serde::Serialize)]
struct LimitView {
    icon: &'static str,
    label: String,
    value: String,
}

/// The per-user toggles of this server (`User::getServerSetting`, defaults false).
async fn notification_settings<C: ConnectionTrait>(
    db: &C,
    user_id: i64,
    server_id: i64,
) -> Result<Map<String, serde_json::Value>, sea_orm::DbErr> {
    let row = server_user_settings::Entity::find()
        .filter(server_user_settings::Column::UserId.eq(Int(user_id)))
        .filter(server_user_settings::Column::ServerId.eq(Int(server_id)))
        .one(db)
        .await?;
    Ok(match row.and_then(|r| r.settings).map(|j| j.0) {
        Some(serde_json::Value::Object(m)) => m,
        _ => Map::new(),
    })
}

/// `GET /server/{server}/settings`. Page access needs no subuser permission;
/// every section is gated individually like the original's `->disabled()/->hidden()`.
async fn page(p: ServerPanel) -> WebResult {
    let st = &p.st;
    let binary = st.config.use_binary_prefix;

    let egg = eggs::Entity::find_by_id(p.server.egg_id).one(&st.db).await?;
    let egg_icon = egg.as_ref().and_then(|e| e.uuid.as_deref()).and_then(|u| crate::models::icon_url(st, "egg", u));
    let server_icon = crate::models::icon_url(st, "server", &p.server.uuid);
    // Image::make('', 'icon')->hidden(!icon && !egg.icon).
    let icon_img = match (&server_icon, &egg_icon) {
        (Some(u), _) | (None, Some(u)) => Some(Value::from(u.as_str())),
        _ => None,
    };

    // Limits fieldset (`formatStateUsing` of every disabled input; falsy $state = empty/unlimited text).
    let backup_count =
        backups::Entity::find().filter(backups::Column::ServerId.eq(p.server.id)).count(&st.db).await? as i64;
    let database_count =
        databases::Entity::find().filter(databases::Column::ServerId.eq(p.server.id)).count(&st.db).await? as i64;
    let allocation_count =
        allocations::Entity::find().filter(allocations::Column::ServerId.eq(p.server.id)).count(&st.db).await? as i64;

    let of_max = |n: i64| p.t_with("server/setting.server_info.limits.of", &[("max", &n.to_string())]);
    let readable = |mib: i64| crate::views::bytes(mib * 1024 * 1024, binary); // convert_bytes_to_readable(* 2**20)
    let unlimited_text = p.t("server/setting.server_info.limits.unlimited");
    let limits = [
        LimitView {
            icon: "cpu",
            label: p.t("server/setting.server_info.limits.cpu"),
            value: if p.server.cpu.0 > 0 {
                format!("{}%", format_number(p.server.cpu.0))
            } else {
                unlimited_text.clone()
            },
        },
        LimitView {
            icon: "device-desktop-analytics",
            label: p.t("server/setting.server_info.limits.memory"),
            value: if p.server.memory.0 > 0 { readable(p.server.memory.0) } else { unlimited_text.clone() },
        },
        LimitView {
            icon: "device-sd-card",
            label: p.t("server/setting.server_info.limits.disk"),
            value: if p.server.disk.0 > 0 { readable(p.server.disk.0) } else { unlimited_text },
        },
        LimitView {
            icon: "file-zip",
            label: p.t("server/setting.server_info.limits.backups"),
            value: if p.server.backup_limit.0 > 0 {
                format!("{} {}", backup_count, of_max(p.server.backup_limit.0))
            } else {
                p.t("server/backup.empty")
            },
        },
        LimitView {
            icon: "database",
            label: p.t("server/setting.server_info.limits.databases"),
            value: match p.server.database_limit.map(|l| l.0) {
                Some(n) if n > 0 => format!("{} {}", database_count, of_max(n)),
                _ => p.t("server/database.empty"),
            },
        },
        LimitView {
            icon: "network",
            label: p.t("server/setting.server_info.limits.allocations"),
            value: match p.server.allocation_limit.map(|l| l.0) {
                Some(n) if n > 0 => format!("{} {}", allocation_count, of_max(n)),
                _ => p.t("server/setting.server_info.limits.no_allocations"),
            },
        },
    ];

    // SFTP fieldset (hidden without `file.sftp`); connection = getSftpUrl().
    let can_sftp = p.can("file.sftp");
    let sftp_conn = super::sftp_url(&p, None);
    let sftp_user = format!("{}.{}", p.user.user.username, p.server.uuid_short);

    let notif = notification_settings(&st.db, p.user.id(), p.server.id.0).await?;
    let toggle = |key: &str| notif.get(key).and_then(|v| v.as_bool()).unwrap_or(false);

    p.page(
        "settings",
        "server/settings.html",
        context! {
            title => p.t("server/setting.title"),
            base => p.base_url(),
            can_rename => p.can("settings.rename"),
            can_desc => p.can("settings.description"),
            editable_desc => st.config.env.bool_or("PANEL_EDITABLE_SERVER_DESCRIPTIONS", true),
            can_reinstall => p.can("settings.reinstall"),
            can_icon => p.can("settings.change-icon"),
            s_name => p.server.name,
            s_desc => p.server.description.clone().unwrap_or_default(),
            s_uuid => p.server.uuid,
            s_uuid_short => p.server.uuid_short,
            s_node => p.node.name,
            info_title => p.t("server/setting.server_info.title"),
            information_label => p.t("server/setting.server_info.information"),
            icon_img => icon_img,
            egg_icon_hint => server_icon.is_none() && egg_icon.is_some(),
            icon_tooltip => p.t("server/setting.server_info.icon.tooltip"),
            import_icon_label => p.t("admin/egg.import.import_icon"),
            delete_icon_label => p.t("admin/egg.import.delete_icon"),
            limits_title => p.t("server/setting.server_info.limits.title"),
            limits => limits,
            can_sftp => can_sftp,
            sftp_conn => sftp_conn,
            sftp_user => sftp_user,
            sftp_password_body => p.t("server/setting.server_info.sftp.password_body"),
            reinstall_bodies => vec![
                p.t("server/setting.reinstall.body"),
                p.t("server/setting.reinstall.body2"),
            ],
            reinstall_confirm => p.t("server/setting.reinstall.modal"),
            reinstall_desc => p.t("server/setting.reinstall.modal_description"),
            reinstall_ok => p.t("server/setting.reinstall.yes"),
            notif_manual => toggle("manual_backup_notifications"),
            notif_scheduled => toggle("scheduled_backup_notifications"),
            manual_helper => p.t("server/setting.notifications.backup_manual.helper"),
            scheduled_helper => p.t("server/setting.notifications.backup_scheduled.helper"),
        },
    )
}

/// `forceFill([...])->saveOrFail()` semantics: dirty columns plus the model's
/// `saved` hook; failures surface like an uncaught exception in `try/catch`.
async fn save_server_field(p: &ServerPanel, col: servers::Column, value: Expr) -> Result<(), WebError> {
    touch_column(&p.st, p.server.id.0, col, value).await?;
    let mut server = p.server.clone();
    server.updated_at = Some(ts_now());
    let ctx = api_ctx(p)?;
    server_saved(&p.st, &ctx, &p.st.db, &server)
        .await
        .map_err(|e| WebError::internal(format!("server save hook: {e:?}")))
}

// ----- POST /rename -----------------------------------------------------------------

/// `POST /server/{server}/settings/rename` — `Settings::updateName()`:
/// no request validation in the original besides the DB column, so a rule
/// failure lands in the catch-all danger notification like an exception.
async fn rename(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("settings.rename")?;
    let back = back_url(&form, &p);
    let original = p.server.name.clone();

    // Same rules the client API applies to the same state.
    let value = form.to_value();
    let mut v = Validator::for_value(&value);
    v.rules("name", &["required", "string", "min:1", "max:255"]);
    v.run();
    if v.has_errors() {
        let msgs: Vec<String> = v.error_messages().into_iter().map(|(_, m)| m).collect();
        p.session.flash("danger", p.t("server/setting.server_info.failed"), Some(msgs.join(", ")));
        return Ok(p.redirect(&back));
    }

    let name = form.get("name").to_string();
    // afterStateUpdated only fires when the state actually changed.
    if name != original {
        match save_server_field(&p, servers::Column::Name, Expr::value(name.clone())).await {
            Ok(()) => {
                p.activity_for_server("server:settings.rename")
                    .property("old", original.clone())
                    .property("new", name.clone())
                    .log(&p.st)
                    .await
                    .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
                p.session.flash(
                    "success",
                    p.t("server/setting.server_info.notification_name"),
                    Some(format!("{original} -> {name}")),
                );
            }
            Err(e) => {
                tracing::warn!(error=?e, "server rename failed");
                p.session.flash("danger", p.t("server/setting.server_info.failed"), Some(e.to_string()));
            }
        }
    }
    Ok(p.redirect(&back))
}

// ----- POST /description ------------------------------------------------------------

/// `POST …/description` — `Settings::updateDescription()`; requires
/// `settings.description` AND `PANEL_EDITABLE_SERVER_DESCRIPTIONS`.
async fn description(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("settings.description")?;
    if !p.st.config.env.bool_or("PANEL_EDITABLE_SERVER_DESCRIPTIONS", true) {
        return Err(WebError::Forbidden);
    }
    let back = back_url(&form, &p);
    let original = p.server.description.clone();
    // `$this->updateDescription($state ?? '', …)` stores '' over null.
    let stored = form.get("description").to_string();
    if original.as_deref() != Some(stored.as_str()) {
        match save_server_field(&p, servers::Column::Description, Expr::value(Some(stored.clone()))).await {
            Ok(()) => {
                p.activity_for_server("server:settings.description")
                    .property("old", original.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
                    .property("new", stored.clone())
                    .log(&p.st)
                    .await
                    .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
                p.session.flash(
                    "success",
                    p.t("server/setting.server_info.notification_description"),
                    Some(format!("{} -> {stored}", original.unwrap_or_default())),
                );
            }
            Err(e) => {
                tracing::warn!(error=?e, "server description update failed");
                p.session.flash("danger", p.t("server/setting.server_info.failed"), Some(e.to_string()));
            }
        }
    }
    Ok(p.redirect(&back))
}

// ----- POST /reinstall --------------------------------------------------------------

/// `POST …/reinstall` — `ReinstallServerService::handle()`: status change and
/// the daemon call share a transaction (rolled back when Wings fails).
async fn reinstall(p: ServerPanel, form: Form) -> WebResult<Response> {
    let st = &p.st;
    let back = back_url(&form, &p);
    let result: Result<(), WebError> = async {
        let conn = st.node_conn(&p.node).map_err(|_| WebError::internal("DecryptException"))?;
        let txn = st.db.begin().await?;
        if p.server.status.as_deref() != Some("installing") {
            servers::Entity::update_many()
                .col_expr(servers::Column::Status, Expr::value(Some("installing".to_string())))
                .col_expr(servers::Column::UpdatedAt, Expr::value(ts_now()))
                .filter(servers::Column::Id.eq(p.server.id))
                .exec(&txn)
                .await?;
        }
        st.wings.reinstall_server(&conn, &p.server.uuid).await.map_err(|e| WebError::internal(e.code()))?;
        txn.commit().await?;
        p.activity_for_server("server:reinstall")
            .log(st)
            .await
            .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            p.session.flash("success", p.t("server/setting.reinstall.notification_start"), None);
            Ok(p.redirect(&p.base_url())) // redirect(Console::getUrl())
        }
        Err(e) => {
            tracing::warn!(error=?e, "server reinstall failed"); // report($exception)
            p.session.flash("danger", p.t("server/setting.reinstall.notification_fail"), Some(e.to_string()));
            Ok(p.redirect(&back))
        }
    }
}

// ----- POST /notifications ----------------------------------------------------------

/// `ServerUserSettingKey::cases()` whitelist (`array_intersect_key` target).
const SETTING_KEYS: [&str; 2] = ["manual_backup_notifications", "scheduled_backup_notifications"];

/// `POST …/notifications` — one live toggle → `User::updateServerSetting()`.
async fn notifications(p: ServerPanel, form: Form) -> WebResult<Response> {
    let back = back_url(&form, &p);
    let key = SETTING_KEYS.iter().find(|k| form.opt(k).is_some()).copied().ok_or(WebError::Unprocessable)?;
    // `Toggle::afterStateUpdated(..., (bool) $state)`: hidden field sends 0/1.
    let value = form.bool(key);
    update_server_setting(&p, key, value).await?;
    p.session.flash("success", p.t("server/setting.notifications.saved"), None);
    Ok(p.redirect(&back))
}

/// `User::updateServerSetting()`: merge into the (firstOrNew) row's JSON map
/// and drop keys outside `getDefaultSettings()`, keeping insertion order.
async fn update_server_setting(p: &ServerPanel, key: &str, value: bool) -> Result<(), WebError> {
    let existing = server_user_settings::Entity::find()
        .filter(server_user_settings::Column::UserId.eq(Int(p.user.id())))
        .filter(server_user_settings::Column::ServerId.eq(Int(p.server.id.0)))
        .one(&p.st.db)
        .await?;
    let mut settings =
        existing.as_ref().and_then(|r| r.settings.clone()).and_then(|j| j.0.as_object().cloned()).unwrap_or_default();
    settings.insert(key.into(), json!(value));
    let filtered: Map<String, serde_json::Value> =
        settings.into_iter().filter(|(k, _)| SETTING_KEYS.contains(&k.as_str())).collect();
    let now = ts_now();
    match existing {
        Some(row) => {
            let am = server_user_settings::ActiveModel {
                id: Set(row.id),
                settings: Set(Some(Json(json!(filtered)))),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            pelican_db::write::update(&p.st.db, &am).await?;
        }
        None => {
            let am = server_user_settings::ActiveModel {
                user_id: Set(Int(p.user.id())),
                server_id: Set(Int(p.server.id.0)),
                settings: Set(Some(Json(json!(filtered)))),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            };
            pelican_db::write::insert_id(&p.st.db, &am).await?;
        }
    }
    Ok(())
}

// ----- Icon actions (UploadIcon/DeleteIcon) -----------------------------------------

/// Failure modes of `HasIcon::writeIcon` (translated by the callers).
enum WriteErr {
    UnknownExtension(String),
    Io,
}

/// `HasIcon::writeIcon`: normalized file under `storage/app/public/icons/server/`,
/// other extensions of the same uuid swept away.
async fn write_icon(st: &AppState, ext: &str, bytes: &[u8], uuid: &str) -> Result<(), WriteErr> {
    let ext = match ext.to_lowercase().as_str() {
        "jpeg" | "jpg" => "jpg",
        "png" => "png",
        "webp" => "webp",
        _ => return Err(WriteErr::UnknownExtension(ext.to_string())),
    };
    let dir = crate::models::public_disk(st).join(ICON_DIR); // Storage::disk('public')->put()
    tokio::fs::create_dir_all(&dir).await.map_err(|_| WriteErr::Io)?;
    tokio::fs::write(dir.join(format!("{uuid}.{ext}")), bytes).await.map_err(|_| WriteErr::Io)?;
    for other in SWEEP_FORMATS {
        if other == ext {
            continue;
        }
        let path = dir.join(format!("{uuid}.{other}"));
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            tokio::fs::remove_file(&path).await.ok();
        }
    }
    Ok(())
}

// ----- POST /icon/url ---------------------------------------------------------------

/// `POST …/icon/url` — UploadIcon url branch: validate + fetch + writeIcon.
async fn icon_url(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("settings.change-icon")?;
    let back = back_url(&form, &p);
    let locale = p.locale();
    let url = form.get("icon_url").trim().to_string();
    if url.is_empty() {
        p.session.flash("warning", p.t("admin/egg.import.no_icon"), None);
        return Ok(p.redirect(&back));
    }
    match fetch_icon_from_url(&p.st, &locale, &url, &p.server.uuid).await {
        Ok(()) => p.session.flash("success", p.t("admin/egg.import.icon_updated"), None),
        Err(msg) => p.session.flash("danger", msg, None),
    }
    Ok(p.redirect(&back))
}

/// `validateIconUrl` + `Http::…()->body()` + extension check + writeIcon.
/// Fetch errors surface exactly like Filament's exception notifications.
async fn fetch_icon_from_url(st: &AppState, locale: &str, url: &str, uuid: &str) -> Result<(), String> {
    let msg = |key: &str, params: &[(&str, &str)]| -> String {
        let owned: Vec<(String, String)> = params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        crate::i18n::trans(locale, key, &owned)
    };
    let invalid = || msg("admin/egg.import.invalid_url", &[]);
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid());
    }
    // gethostbyname + FILTER_FLAG_NO_PRIV_RANGE | NO_RES_RANGE: public IPs only.
    let host = parsed.host_str().unwrap_or("").to_string();
    let not_public = || msg("admin/egg.import.no_local_ip", &[]);
    let ip = resolve_ip(&host).await.ok_or_else(not_public)?;
    if !is_public_ip(ip) {
        return Err(not_public());
    }
    // pathinfo(parse_url(…, PHP_URL_PATH), PATHINFO_EXTENSION), lowercased —
    // checked before the request like the original's action() ordering.
    let path = parsed.path().to_string();
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    if ext.is_empty() {
        return Err(invalid());
    }
    // Http::timeout(5)->connectTimeout(1)->withoutRedirecting()->get($url)->body()
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| invalid())?;
    let content = client.get(parsed).send().await.map_err(|_| invalid())?.bytes().await.map_err(|_| invalid())?; // body(); the original does not check the HTTP status either
    if content.is_empty() {
        return Err(invalid());
    }
    if ext.is_empty() {
        return Err(invalid());
    }
    write_icon(st, &ext, &content, uuid).await.map_err(|e| match e {
        WriteErr::UnknownExtension(ext) => msg("admin/egg.import.unknown_extension", &[("extension", &ext)]),
        WriteErr::Io => msg("admin/egg.import.could_not_write", &[]),
    })
}

async fn resolve_ip(host: &str) -> Option<IpAddr> {
    tokio::net::lookup_host((host, 80u16)).await.ok()?.next().map(|a| a.ip())
}

/// PHP `FILTER_FLAG_NO_PRIV_RANGE | FILTER_FLAG_NO_RES_RANGE`: private,
/// loopback, link-local and otherwise reserved addresses are rejected.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
            || v4.is_loopback()
            || v4.is_link_local()
            || v4.is_unspecified()
            || v4.is_broadcast()
            || v4.is_multicast()
            || v4.octets()[0] == 0
            || v4.octets()[0] == 100 && v4.octets()[1] >= 64 && v4.octets()[1] <= 127 // CGNAT 100.64/10
            || (192..199).contains(&v4.octets()[0]) && v4.octets()[1] == 2 // TEST-NET
            || v4.octets()[0] == 198 && (18..=19).contains(&v4.octets()[1])
            || (240..255).contains(&v4.octets()[0]))
        } // reserved/limited broadcast
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || s[0] & 0xffc0 == 0xfe80 // link-local fe80::/10
                || s[0] & 0xfe00 == 0xfc00 // unique-local fc00::/7
                || s[0] == 0x2001 && s[1] == 0xdb8) // documentation 2001:db8::/32
        }
    }
}

// ----- Multipart upload (FileUpload tab) --------------------------------------------

const MAX_ICON_BYTES: usize = 256 * 1024; // FileUpload::maxSize(256) kilobytes

/// The uploaded file part parsed from a browser multipart body.
#[derive(Clone, Debug, PartialEq)]
pub struct UploadedFile {
    pub filename: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// One multipart part of a submitted form.
struct Part {
    name: String,
    file: Option<UploadedFile>,
}

/// The single file part named `name="icon"` from a browser multipart body:
///
/// ```text
/// --BOUND\r\n headers \r\n\r\n content \r\n--BOUND\r\n ... \r\n--BOUND--
/// ```
pub fn multipart_file(content_type: &str, body: &[u8]) -> Option<UploadedFile> {
    let boundary = boundary_of(content_type)?.to_owned();
    let delim = format!("--{boundary}").into_bytes();
    let find = |h: &[u8], n: &[u8], from: usize| -> Option<usize> {
        if n.is_empty() || h.len() < n.len() || from > h.len() - n.len() {
            return None;
        }
        (from..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n)
    };
    // Skip any preamble before the first delimiter.
    let mut pos = find(body, &delim, 0)?;
    loop {
        let mut i = pos + delim.len();
        if i >= body.len() {
            return None;
        }
        if body[i..].starts_with(b"--") {
            return None; // closing delimiter reached without the field
        }
        i += 2; // CRLF after the delimiter (browsers always send one)
        if i > body.len() - 4 {
            return None;
        }
        let head_end = find(body, b"\r\n\r\n", i)? + 4;
        let next = find(body, &delim, head_end)?;
        // Content runs right up to "\r\n--BOUND".
        let (mut content_end, missing_crlf) = (next.checked_sub(2)?, &body[next.saturating_sub(2)..next] != b"\r\n");
        if missing_crlf {
            content_end = next; // tolerate a missing final CRLF before the delimiter
        }
        let part = parse_part(&body[i..head_end - 4], &body[head_end..content_end])?;
        if part.name == "icon" {
            return part.file;
        }
        pos = next;
    }
}

/// Extract `name=` / `filename=` from one part's headers; `filename`
/// present means a file field.
fn parse_part(headers: &[u8], content: &[u8]) -> Option<Part> {
    let text = String::from_utf8_lossy(headers);
    let disp = text.lines().find(|l| l.starts_with("Content-Disposition:"))?;
    let quoted = |key: &str| -> Option<String> {
        let idx = disp.find(key)? + key.len();
        Some(disp[idx..disp[idx..].find('"')? + idx].to_string())
    };
    let name = quoted("name=\"")?;
    let filename = quoted("filename=\"").filter(|f| !f.is_empty());
    let ct = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-type:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string());
    Some(Part {
        name,
        file: filename.map(|filename| UploadedFile { filename, content_type: ct, bytes: content.to_vec() }),
    })
}

/// The `boundary=…` parameter of a `multipart/form-data` header.
fn boundary_of(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|p| {
        let p = p.trim();
        p.strip_prefix("boundary=").map(|b| b.trim().trim_matches('"'))
    })
}

/// `POST …/icon/file` — JS-uploaded multipart form (X-CSRF-TOKEN header set
/// by js/settings.js; the CSRF middleware only buffers urlencoded bodies).
/// Answers JSON; the page reloads on success, shows `message` otherwise.
async fn icon_file(p: ServerPanel, req: Request) -> Response {
    let locale = p.locale();
    if !p.can("settings.change-icon") {
        return (axum::http::StatusCode::FORBIDDEN, axum::Json(json!({"ok": false}))).into_response();
    }
    let too_large = || {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({
                "ok": false,
                "message": crate::i18n::trans(
                    &locale,
                    "validation.max.file",
                    &[("attribute".to_string(), "icon".to_string()), ("max".to_string(), "256".to_string())],
                ),
            })),
        )
            .into_response()
    };
    let ct = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let bytes: Bytes = match to_bytes(req.into_body(), 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return too_large(),
    };
    if !ct.starts_with("multipart/") {
        return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"ok": false}))).into_response();
    }
    if bytes.len() > MAX_ICON_BYTES {
        return too_large(); // Laravel validation runs before storage
    }
    let Some(file) = multipart_file(&ct, &bytes) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"ok": false}))).into_response();
    };
    let ext = file.filename.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let result = write_icon(&p.st, &ext, &file.bytes, &p.server.uuid).await.map_err(|e| match e {
        WriteErr::UnknownExtension(ext) => {
            crate::i18n::trans(&locale, "admin/egg.import.unknown_extension", &[("extension".to_string(), ext)])
        }
        WriteErr::Io => crate::i18n::trans(&locale, "admin/egg.import.could_not_write", &[]),
    });
    match result {
        Ok(()) => axum::Json(json!({"ok": true})).into_response(),
        Err(msg) => {
            (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"ok": false, "message": msg}))).into_response()
        }
    }
}

/// `DELETE`-style icon action: remove every known format, then refresh.
async fn icon_delete(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("settings.change-icon")?;
    let back = back_url(&form, &p);
    for ext in ICON_FORMATS {
        let path = crate::models::public_disk(&p.st).join(ICON_DIR).join(format!("{}.{}", p.server.uuid, ext));
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            tokio::fs::remove_file(&path).await.ok();
        }
    }
    p.session.flash("success", p.t("admin/egg.import.icon_deleted"), None);
    Ok(p.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    #[test]
    fn number_format_matches_php() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(100), "100");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(1_000_000), "1,000,000");
        assert_eq!(format_number(-12_345), "-12,345");
    }

    #[test]
    fn public_ip_filter() {
        // NO_PRIV_RANGE | NO_RES_RANGE: only global unicast passes.
        for good in ["8.8.8.8", "2606:4700::1111"] {
            let ip: IpAddr = good.parse().unwrap();
            assert!(is_public_ip(ip), "{good} should pass");
        }
        for bad in [
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.9",
            "127.0.0.1",
            "169.254.1.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "ff02::1",
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(!is_public_ip(ip), "{bad} should be rejected");
        }
    }

    #[test]
    fn boundary_parameter_parsing() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=----WebKitFormBoundaryabc123"),
            Some("----WebKitFormBoundaryabc123")
        );
        assert_eq!(boundary_of("multipart/form-data; boundary=\"quoted\""), Some("quoted"));
        assert_eq!(boundary_of("application/x-www-form-urlencoded"), None);
    }

    #[test]
    fn multipart_file_parses_browser_body() {
        let ct = "multipart/form-data; boundary=B";
        // Browser-style body: text part first, then the icon with binary content.
        let mut body: Vec<u8> = b"--B\r\n".to_vec();
        body.extend(b"Content-Disposition: form-data; name=\"_back\"\r\n\r\n/server/abcd/settings\r\n--B\r\n");
        body.extend(
            b"Content-Disposition: form-data; name=\"icon\"; filename=\"logo.PNG\"\r\nContent-Type: image/png\r\n\r\n",
        );
        body.extend([0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x2d, 0x2d]);
        body.extend(b"\r\n--B\r\n");
        body.extend(b"Content-Disposition: form-data; name=\"icon\"; filename=\"empty.png\"\r\n\r\n\r\n--B--\r\n");

        let file = multipart_file(ct, &body).expect("first icon part found");
        assert_eq!(file.filename, "logo.PNG");
        assert_eq!(file.content_type.as_deref(), Some("image/png"));
        assert_eq!(&file.bytes[8..10], &[0x2d, 0x2d]); // binary survived intact

        // The empty second file is also parseable when asked alone would match first;
        // verify the binary-lookalike did not split the frame above.
        assert_eq!(file.bytes.len(), 10);
    }

    #[test]
    fn multipart_file_without_icon_field() {
        let ct = "multipart/form-data; boundary=B";
        let body = b"--B\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nx\r\n--B--\r\n";
        assert_eq!(multipart_file(ct, body), None);
        assert_eq!(multipart_file("text/plain", b"body"), None);
    }

    #[test]
    fn settings_page_template_renders() {
        let limit = |i: u8| LimitView {
            icon: "cpu",
            label: format!("L{i}"),
            value: if i == 0 { String::new() } else { format!("{i}") },
        };
        for (can, img) in [(true, true), (false, false)] {
            let ctx = context! {
                title => "Settings",
                base => "/server/abcd1234",
                can_rename => can,
                can_desc => can,
                editable_desc => true,
                can_reinstall => can,
                can_icon => can,
                s_name => "Test",
                s_desc => "",
                s_uuid => "uuid-value",
                s_uuid_short => "abcd1234",
                s_node => "Node 1",
                info_title => "Server Information",
                information_label => "Information",
                icon_img => img.then(|| minijinja::Value::from("/storage/icons/server/u.png")),
                egg_icon_hint => false,
                icon_tooltip => "Using Egg Icon",
                import_icon_label => "Import Icon",
                delete_icon_label => "Delete Icon",
                limits_title => "Limits",
                limits => vec![limit(0), limit(1)],
                can_sftp => can,
                sftp_conn => "sftp://user.abcd@host:2022/",
                sftp_user => "user.abcd1234",
                sftp_password_body => "Your SFTP password",
                reinstall_bodies => vec!["b1", "b2"],
                reinstall_confirm => "Sure?",
                reinstall_desc => "desc",
                reinstall_ok => "Yes",
                notif_manual => false,
                notif_scheduled => true,
                manual_helper => "h",
                scheduled_helper => "s",
            };
            crate::views::render("server/settings.html", ctx).expect("template renders");
        }
    }
}
