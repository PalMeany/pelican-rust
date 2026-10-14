//! Admin › Settings (app/Filament/Admin/Pages/Settings.php): the six-tab
//! `.env` editor (general/captcha/mail/backup/oauth/misc). Values come from
//! `st.config.env` like Laravel's `env()`/`config()` defaults; saving ports
//! `Settings::save()` → `EnvironmentWriterTrait::writeToEnvironment`
//! (`Env::writeVariables`, ADR-0014 reload) plus the test-mail hint action.

use std::collections::BTreeMap;

use axum::Router;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::validation::Validator;
use pelican_core::env::{Env, EnvValue, EnvWrite, php_intval};
use pelican_core::mail::{MailConfig, MailMessage};
use pelican_core::state::AppState;
use serde_json::{Map, Value};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::i18n;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/settings/cloudflare-ips", get(cloudflare_ips))
        .route("/admin/settings", get(page).post(save))
        .route("/admin/settings/test-mail", post(test_mail))
}

/// Field kinds: `.env` string, boolean (`true`/`false` in .env), number
/// (written raw), optional number or a comma-joined TagsInput value. The
/// bundled value is the Laravel config fallback shown/written when neither
/// the form nor `.env` has a value.
#[derive(Clone, Copy)]
enum Kind {
    Str(&'static str),
    Bool(bool),
    Int(i64),
    IntOpt,
    Tags,
}

const FIELDS: &[(&str, Kind)] = &[
    // generalSettings()
    ("APP_NAME", Kind::Str("Pelican")),
    ("APP_LOGO", Kind::Str("")),
    ("APP_FAVICON", Kind::Str("/pelican.ico")),
    ("APP_DEBUG", Kind::Bool(false)),
    ("FILAMENT_AVATAR_PROVIDER", Kind::Str("gravatar")),
    ("FILAMENT_UPLOADABLE_AVATARS", Kind::Bool(false)),
    ("PANEL_USE_BINARY_PREFIX", Kind::Int(1)),
    ("FILAMENT_DEFAULT_NAVIGATION", Kind::Str("sidebar")),
    ("APP_2FA_REQUIRED", Kind::Int(0)),
    ("FILAMENT_WIDTH", Kind::Str("screen-2xl")),
    ("TRUSTED_PROXIES", Kind::Tags),
    // captchaSettings(): BaseSchema + TurnstileSchema (registered by default)
    ("CAPTCHA_TURNSTILE_ENABLED", Kind::Bool(false)),
    ("CAPTCHA_TURNSTILE_SITE_KEY", Kind::Str("")),
    ("CAPTCHA_TURNSTILE_SECRET_KEY", Kind::Str("")),
    ("CAPTCHA_TURNSTILE_VERIFY_DOMAIN", Kind::Bool(true)),
    // mailSettings()
    ("MAIL_MAILER", Kind::Str("log")),
    ("MAIL_FROM_ADDRESS", Kind::Str("hello@example.com")),
    ("MAIL_FROM_NAME", Kind::Str("Example")),
    ("MAIL_HOST", Kind::Str("127.0.0.1")),
    ("MAIL_PORT", Kind::Int(2525)),
    ("MAIL_USERNAME", Kind::Str("")),
    ("MAIL_PASSWORD", Kind::Str("")),
    ("MAIL_SCHEME", Kind::Str("smtp")),
    ("MAILGUN_DOMAIN", Kind::Str("")),
    ("MAILGUN_SECRET", Kind::Str("")),
    ("MAILGUN_ENDPOINT", Kind::Str("api.mailgun.net")),
    // backupSettings()
    ("BACKUP_THROTTLE_LIMIT", Kind::Int(2)),
    ("BACKUP_THROTTLE_PERIOD", Kind::Int(600)),
    // oauthSettings(): registration order of OAuthServiceProvider, keys per
    // OAuthSchema/OAuthSchemaInterface
    ("OAUTH_FACEBOOK_ENABLED", Kind::Bool(false)),
    ("OAUTH_FACEBOOK_CLIENT_ID", Kind::Str("")),
    ("OAUTH_FACEBOOK_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_FACEBOOK_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_FACEBOOK_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_X_ENABLED", Kind::Bool(false)),
    ("OAUTH_X_CLIENT_ID", Kind::Str("")),
    ("OAUTH_X_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_X_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_X_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_LINKEDIN_ENABLED", Kind::Bool(false)),
    ("OAUTH_LINKEDIN_CLIENT_ID", Kind::Str("")),
    ("OAUTH_LINKEDIN_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_LINKEDIN_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_LINKEDIN_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GOOGLE_ENABLED", Kind::Bool(false)),
    ("OAUTH_GOOGLE_CLIENT_ID", Kind::Str("")),
    ("OAUTH_GOOGLE_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_GOOGLE_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GOOGLE_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GITHUB_ENABLED", Kind::Bool(false)),
    ("OAUTH_GITHUB_CLIENT_ID", Kind::Str("")),
    ("OAUTH_GITHUB_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_GITHUB_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GITHUB_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GITLAB_ENABLED", Kind::Bool(false)),
    ("OAUTH_GITLAB_CLIENT_ID", Kind::Str("")),
    ("OAUTH_GITLAB_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_GITLAB_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GITLAB_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_GITLAB_HOST", Kind::Str("")),
    ("OAUTH_BITBUCKET_ENABLED", Kind::Bool(false)),
    ("OAUTH_BITBUCKET_CLIENT_ID", Kind::Str("")),
    ("OAUTH_BITBUCKET_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_BITBUCKET_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_BITBUCKET_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_SLACK_ENABLED", Kind::Bool(false)),
    ("OAUTH_SLACK_CLIENT_ID", Kind::Str("")),
    ("OAUTH_SLACK_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_SLACK_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_SLACK_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_AUTHENTIK_ENABLED", Kind::Bool(false)),
    ("OAUTH_AUTHENTIK_CLIENT_ID", Kind::Str("")),
    ("OAUTH_AUTHENTIK_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_AUTHENTIK_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_AUTHENTIK_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_AUTHENTIK_BASE_URL", Kind::Str("")),
    ("OAUTH_AUTHENTIK_DISPLAY_NAME", Kind::Str("Authentik")),
    ("OAUTH_AUTHENTIK_DISPLAY_COLOR", Kind::Str("#fd4b2d")),
    ("OAUTH_DISCORD_ENABLED", Kind::Bool(false)),
    ("OAUTH_DISCORD_CLIENT_ID", Kind::Str("")),
    ("OAUTH_DISCORD_CLIENT_SECRET", Kind::Str("")),
    ("OAUTH_DISCORD_SHOULD_CREATE_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_DISCORD_SHOULD_LINK_MISSING_USERS", Kind::Bool(false)),
    ("OAUTH_STEAM_ENABLED", Kind::Bool(false)),
    ("OAUTH_STEAM_CLIENT_SECRET", Kind::Str("")),
    // miscSettings()
    ("PANEL_CLIENT_ALLOCATIONS_ENABLED", Kind::Bool(false)),
    ("PANEL_CLIENT_ALLOCATIONS_CREATE_NEW", Kind::Bool(true)),
    ("PANEL_CLIENT_ALLOCATIONS_RANGE_START", Kind::IntOpt),
    ("PANEL_CLIENT_ALLOCATIONS_RANGE_END", Kind::IntOpt),
    ("PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION", Kind::Bool(true)),
    ("PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION", Kind::Bool(true)),
    ("PANEL_SEND_REMOVED_FROM_SERVER_NOTIFICATION", Kind::Bool(true)),
    ("PANEL_SEND_INSTALL_NOTIFICATION", Kind::Bool(true)),
    ("PANEL_SEND_REINSTALL_NOTIFICATION", Kind::Bool(true)),
    ("GUZZLE_TIMEOUT", Kind::Int(15)),
    ("GUZZLE_CONNECT_TIMEOUT", Kind::Int(5)),
    ("APP_ACTIVITY_PRUNE_DAYS", Kind::Int(90)),
    ("APP_ACTIVITY_HIDE_ADMIN", Kind::Bool(false)),
    ("APP_API_CLIENT_RATELIMIT", Kind::Int(256)),
    ("APP_API_CLIENT_RATELIMIT_PERIOD", Kind::Int(1)),
    ("APP_API_APPLICATION_RATELIMIT", Kind::Int(256)),
    ("APP_API_APPLICATION_RATELIMIT_PERIOD", Kind::Int(1)),
    ("APP_API_PASSWORD_RESET_RATELIMIT", Kind::Int(2)),
    ("APP_API_PASSWORD_RESET_RATELIMIT_PERIOD", Kind::Int(1)),
    ("APP_API_WEBSOCKET_RATELIMIT", Kind::Int(5)),
    ("APP_API_WEBSOCKET_RATELIMIT_PERIOD", Kind::Int(1)),
    ("APP_API_BACKUP_RESTORE_RATELIMIT", Kind::Int(3)),
    ("APP_API_BACKUP_RESTORE_RATELIMIT_PERIOD", Kind::Int(15)),
    ("APP_API_DATABASE_CREATE_RATELIMIT", Kind::Int(2)),
    ("APP_API_DATABASE_CREATE_RATELIMIT_PERIOD", Kind::Int(1)),
    ("APP_API_SUBUSER_CREATE_RATELIMIT", Kind::Int(10)),
    ("APP_API_SUBUSER_CREATE_RATELIMIT_PERIOD", Kind::Int(15)),
    ("APP_API_FILE_PULL_RATELIMIT", Kind::Int(5)),
    ("APP_API_FILE_PULL_RATELIMIT_PERIOD", Kind::Int(10)),
    ("APP_API_DEFAULT_RATELIMIT", Kind::Int(2)),
    ("APP_API_DEFAULT_RATELIMIT_PERIOD", Kind::Int(1)),
    ("PANEL_EDITABLE_SERVER_DESCRIPTIONS", Kind::Bool(true)),
    ("APP_WEBHOOK_PRUNE_DAYS", Kind::Int(30)),
];

fn kind_of(key: &str) -> Option<Kind> {
    FIELDS.iter().find(|(k, _)| *k == key).map(|(_, kind)| *kind)
}

/// The default registered OAuth providers (`OAuthServiceProvider::register()`).
const OAUTH_PROVIDERS: &[(&str, &str, &str)] = &[
    ("facebook", "Facebook", "brand-facebook-filled"),
    ("x", "X", "brand-x"),
    ("linkedin", "Linkedin", "brand-linkedin-filled"),
    ("google", "Google", "brand-google-filled"),
    ("github", "Github", "brand-github-filled"),
    ("gitlab", "Gitlab", "brand-gitlab"),
    ("bitbucket", "Bitbucket", "brand-bitbucket-filled"),
    ("slack", "Slack", "brand-slack"),
    ("authentik", "Authentik", "brand-oauth"),
    ("discord", "Discord", "brand-discord-filled"),
    ("steam", "Steam", "brand-steam-filled"),
];

/// Filament `Width::class` options: value + case name label (`Width` has no
/// HasLabel implementation, so Filament shows the case name).
const WIDTHS: &[&str] = &[
    "3xs",
    "2xs",
    "xs",
    "sm",
    "md",
    "lg",
    "xl",
    "2xl",
    "3xl",
    "4xl",
    "5xl",
    "6xl",
    "7xl",
    "none",
    "full",
    "min",
    "max",
    "fit",
    "prose",
    "container",
    "screen-sm",
    "screen-md",
    "screen-lg",
    "screen-xl",
    "screen-2xl",
    "screen",
];
const WIDTH_NAMES: &[(&str, &str)] = &[
    ("3xs", "ThreeExtraSmall"),
    ("2xs", "TwoExtraSmall"),
    ("xs", "ExtraSmall"),
    ("sm", "Small"),
    ("md", "Medium"),
    ("lg", "Large"),
    ("xl", "ExtraLarge"),
    ("2xl", "TwoExtraLarge"),
    ("3xl", "ThreeExtraLarge"),
    ("4xl", "FourExtraLarge"),
    ("5xl", "FiveExtraLarge"),
    ("6xl", "SixExtraLarge"),
    ("7xl", "SevenExtraLarge"),
    ("none", "None"),
    ("full", "Full"),
    ("min", "MinContent"),
    ("max", "MaxContent"),
    ("fit", "FitContent"),
    ("prose", "Prose"),
    ("container", "Container"),
    ("screen-sm", "ScreenSmall"),
    ("screen-md", "ScreenMedium"),
    ("screen-lg", "ScreenLarge"),
    ("screen-xl", "ScreenExtraLarge"),
    ("screen-2xl", "ScreenTwoExtraLarge"),
    ("screen", "Screen"),
];

/// Rate-limit field pairs of misc › API (`config/http.php` order) with their
/// translation slugs.
const RATE_LIMITS: &[(&str, &str, &str)] = &[
    ("client", "APP_API_CLIENT_RATELIMIT", "APP_API_CLIENT_RATELIMIT_PERIOD"),
    ("application", "APP_API_APPLICATION_RATELIMIT", "APP_API_APPLICATION_RATELIMIT_PERIOD"),
    ("password_reset", "APP_API_PASSWORD_RESET_RATELIMIT", "APP_API_PASSWORD_RESET_RATELIMIT_PERIOD"),
    ("websocket", "APP_API_WEBSOCKET_RATELIMIT", "APP_API_WEBSOCKET_RATELIMIT_PERIOD"),
    ("backup_restore", "APP_API_BACKUP_RESTORE_RATELIMIT", "APP_API_BACKUP_RESTORE_RATELIMIT_PERIOD"),
    ("database_create", "APP_API_DATABASE_CREATE_RATELIMIT", "APP_API_DATABASE_CREATE_RATELIMIT_PERIOD"),
    ("subuser_create", "APP_API_SUBUSER_CREATE_RATELIMIT", "APP_API_SUBUSER_CREATE_RATELIMIT_PERIOD"),
    ("file_pull", "APP_API_FILE_PULL_RATELIMIT", "APP_API_FILE_PULL_RATELIMIT_PERIOD"),
    ("default", "APP_API_DEFAULT_RATELIMIT", "APP_API_DEFAULT_RATELIMIT_PERIOD"),
];

/// Current values of every schema key: an exact submitted field wins, else the
/// Laravel-converted env value, else the empty string.
struct Snapshot<'a> {
    env: &'a Env,
    form: Option<&'a Form>,
}

impl Snapshot<'_> {
    /// Laravel `env($key)`: missing/`(null)` → None.
    fn env_str(&self, key: &str) -> Option<String> {
        match self.env.value(key) {
            Some(EnvValue::Str(s)) => Some(s),
            Some(EnvValue::Bool(b)) => Some(if b { "1".into() } else { "0".into() }),
            _ => None,
        }
    }

    fn sub(&self, key: &str) -> Option<String> {
        self.form.and_then(|f| f.opt(key)).map(|s| s.trim().to_string())
    }

    fn cur(&self, key: &str) -> String {
        if let Some(s) = self.sub(key) {
            return s;
        }
        match self.env.value(key) {
            Some(EnvValue::Str(s)) => s,
            Some(EnvValue::Bool(b)) => (if b { "1" } else { "0" }).into(),
            // Like `env($key, $default)`: unset/null keys show their config fallback.
            _ => match kind_of(key) {
                Some(Kind::Str(d)) => d.to_string(),
                _ => String::new(),
            },
        }
    }

    /// Like [`Self::cur`] but with a fallback default (test-mail values).
    fn cur_or(&self, key: &str, default: &str) -> String {
        let v = self.cur(key);
        if v.is_empty() { default.to_string() } else { v }
    }

    /// Boolean state (`BooleanStateCast`) with the Laravel config fallback.
    fn bool_cur(&self, key: &str) -> bool {
        if let Some(s) = self.sub(key) {
            return !(s.is_empty() || s == "0" || s.eq_ignore_ascii_case("false"));
        }
        match self.env.value(key) {
            Some(EnvValue::Bool(b)) => b,
            Some(EnvValue::Str(s)) => !(s.is_empty() || s == "0"),
            Some(EnvValue::Null) => false,
            None => matches!(kind_of(key), Some(Kind::Bool(d)) if d),
        }
    }

    /// Numeric state (`numeric()` cast) with the config fallback.
    fn int_cur(&self, key: &str) -> i64 {
        if let Some(s) = self.sub(key) {
            return php_intval(&s);
        }
        match self.env.value(key) {
            Some(EnvValue::Str(s)) => php_intval(&s),
            Some(EnvValue::Bool(b)) => i64::from(b),
            Some(EnvValue::Null) => 0,
            None => match kind_of(key) {
                Some(Kind::Int(d)) => d,
                _ => 0,
            },
        }
    }

    fn enabled(&self, upper: &str) -> bool {
        self.bool_cur(&format!("OAUTH_{upper}_ENABLED"))
    }

    /// Non-blank string (mail credentials are optional).
    fn non_blank(s: &str) -> Option<String> {
        (!s.is_empty()).then(|| s.to_string())
    }
}

/// `{KEY: display value}` of every form field as strings (booleans as "0"/"1").
fn render_values(snap: &Snapshot<'_>) -> Mj {
    let mut m = Map::new();
    for (key, kind) in FIELDS {
        let v = match kind {
            Kind::Tags | Kind::IntOpt => continue,
            Kind::Bool(_) => Value::from(if snap.bool_cur(key) { "1" } else { "0" }),
            Kind::Int(_) => Value::from(snap.int_cur(key).to_string()),
            Kind::Str(_) => Value::String(snap.cur(key)),
        };
        m.insert((*key).into(), v);
    }
    Mj::from_serialize(&m)
}

/// Write values, one per schema key in Filament dehydration order
/// (`Settings::save()`: bools → `'true'`/`'false'`, enums back to scalars).
fn write_values(snap: &Snapshot<'_>) -> Vec<(String, EnvWrite)> {
    FIELDS
        .iter()
        .map(|(key, kind)| {
            let raw = snap.cur(key);
            let value = match kind {
                // TagsInput::separator(','): the form posts a JSON array.
                Kind::Tags => {
                    let tags = match snap.form.and_then(|f| f.opt(key)) {
                        Some(posted) => crate::forms::parse_tags(posted),
                        None => raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect(),
                    };
                    EnvWrite::Str(tags.join(","))
                }
                Kind::Bool(_) => EnvWrite::Str(if snap.bool_cur(key) { "true".into() } else { "false".into() }),
                Kind::Int(_) => EnvWrite::Raw(snap.int_cur(key).to_string()),
                // Never-saved range fields stay empty (`KEY=`) instead of inventing values.
                Kind::IntOpt => match snap.form.and_then(|f| f.opt(key)).is_none() && snap.env.value(key).is_none() {
                    true => EnvWrite::Null,
                    false => EnvWrite::Raw(snap.int_cur(key).to_string()),
                },
                // Never-set anywhere (and no default) → `KEY=` stays empty.
                Kind::Str(_) => match raw.is_empty() && snap.sub(key).is_none() && snap.env_str(key).is_none() {
                    true => EnvWrite::Null,
                    false => EnvWrite::Str(raw),
                },
            };
            ((*key).to_string(), value)
        })
        .collect()
}

/// TagsInput hydration: split on the separator (`,`), like the form widget.
fn tag_list(snap: &Snapshot<'_>) -> Vec<String> {
    snap.cur("TRUSTED_PROXIES").split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Filament rules (`required`, `numeric()` min/max, Select option checks);
/// conditional sections only validate while visible/enabled.
fn validate(admin: &Admin, snap: &Snapshot<'_>, trusted_proxies: &[String]) -> Result<Errors, WebError> {
    let mut input = Map::new();
    for (key, kind) in FIELDS {
        let v = match kind {
            Kind::Tags => Value::Array(trusted_proxies.iter().map(|s| Value::from(s.as_str())).collect()),
            _ => Value::String(snap.cur(key)),
        };
        input.insert((*key).into(), v);
    }
    let input = Value::Object(input);
    let mut v = Validator::for_value(&input);
    for (key, name) in attribute_names(admin) {
        v.attribute_name(&key, &name);
    }
    v.rules("APP_NAME", &["required", "string"]);
    v.rules("APP_FAVICON", &["required", "string"]);
    v.rules("FILAMENT_AVATAR_PROVIDER", &["in:gravatar,uiavatars"]);
    v.rules("FILAMENT_DEFAULT_NAVIGATION", &["in:sidebar,topbar,mixed"]);
    v.rules("PANEL_USE_BINARY_PREFIX", &["in:0,1"]);
    v.rules("APP_2FA_REQUIRED", &["in:0,1,2"]);
    v.rules("FILAMENT_WIDTH", &["in:3xs,2xs,xs,sm,md,lg,xl,2xl,3xl,4xl,5xl,6xl,7xl,none,full,min,max,fit,prose,container,screen-sm,screen-md,screen-lg,screen-xl,screen-2xl,screen"]);
    // mail
    v.rules("MAIL_MAILER", &["required", "in:log,smtp,mailgun,mandrill,postmark,sendmail"]);
    v.rules("MAIL_FROM_ADDRESS", &["required", "email"]);
    v.rules("MAIL_FROM_NAME", &["required", "string"]);
    if snap.cur("MAIL_MAILER") == "smtp" {
        v.rules("MAIL_HOST", &["required", "string"]);
        v.rules("MAIL_PORT", &["required", "numeric", "between:1,65535"]);
        v.rules("MAIL_SCHEME", &["in:smtp,smtps"]);
    }
    if snap.cur("MAIL_MAILER") == "mailgun" {
        for f in ["MAILGUN_DOMAIN", "MAILGUN_SECRET", "MAILGUN_ENDPOINT"] {
            v.rules(f, &["required", "string"]);
        }
    }
    // captcha provider keys are required while the provider is on
    if snap.bool_cur("CAPTCHA_TURNSTILE_ENABLED") {
        v.rules("CAPTCHA_TURNSTILE_SITE_KEY", &["required", "string"]);
        v.rules("CAPTCHA_TURNSTILE_SECRET_KEY", &["required", "string"]);
    }
    // backup throttle
    v.rules("BACKUP_THROTTLE_LIMIT", &["required", "numeric", "min:1"]);
    v.rules("BACKUP_THROTTLE_PERIOD", &["required", "numeric", "min:0"]);
    // oauth provider fields are required while that provider is enabled
    for (id, ..) in OAUTH_PROVIDERS {
        let up = id.to_ascii_uppercase();
        if !snap.enabled(&up) {
            continue;
        }
        if *id == "steam" {
            v.rules("OAUTH_STEAM_CLIENT_SECRET", &["required", "string"]);
            continue;
        }
        v.rules(&format!("OAUTH_{up}_CLIENT_ID"), &["required", "string"]);
        v.rules(&format!("OAUTH_{up}_CLIENT_SECRET"), &["required", "string"]);
    }
    if snap.enabled("AUTHENTIK") {
        v.rules("OAUTH_AUTHENTIK_BASE_URL", &["required", "url"]);
        v.rules("OAUTH_AUTHENTIK_DISPLAY_COLOR", &["regex:^#([[:xdigit:]]{3}|[[:xdigit:]]{6})$"]);
    }
    // misc
    if snap.bool_cur("PANEL_CLIENT_ALLOCATIONS_ENABLED") {
        v.rules("PANEL_CLIENT_ALLOCATIONS_RANGE_START", &["required", "numeric", "between:1024,65535"]);
        v.rules("PANEL_CLIENT_ALLOCATIONS_RANGE_END", &["required", "numeric", "between:1024,65535"]);
    }
    v.rules("GUZZLE_TIMEOUT", &["required", "numeric", "between:15,60"]);
    v.rules("GUZZLE_CONNECT_TIMEOUT", &["required", "numeric", "between:5,60"]);
    v.rules("APP_ACTIVITY_PRUNE_DAYS", &["required", "numeric", "between:1,365"]);
    v.rules("APP_WEBHOOK_PRUNE_DAYS", &["required", "numeric", "between:1,365"]);
    for (_, limit, period) in RATE_LIMITS {
        v.rules(limit, &["required", "numeric", "min:1"]);
        v.rules(period, &["required", "numeric", "min:1"]);
    }
    v.run();
    Ok(Errors::from_validator(&v))
}

/// Humanized names for validation messages, mirroring the field labels.
fn attribute_names(admin: &Admin) -> BTreeMap<String, String> {
    let t = |k: &str| admin.t(k).to_lowercase();
    let mut m = BTreeMap::new();
    for pair in [
        ("APP_NAME", t("admin/setting.general.app_name")),
        ("APP_FAVICON", t("admin/setting.general.app_favicon")),
        ("MAIL_FROM_ADDRESS", t("admin/setting.mail.from_address")),
        ("MAIL_FROM_NAME", t("admin/setting.mail.from_name")),
        ("MAIL_HOST", t("admin/setting.mail.smtp.host")),
        ("MAIL_PORT", t("admin/setting.mail.smtp.port")),
        ("MAIL_USERNAME", t("admin/setting.mail.smtp.username")),
        ("MAILGUN_DOMAIN", t("admin/setting.mail.mailgun.domain")),
        ("MAILGUN_SECRET", t("admin/setting.mail.mailgun.secret")),
        ("MAILGUN_ENDPOINT", t("admin/setting.mail.mailgun.endpoint")),
        ("BACKUP_THROTTLE_LIMIT", t("admin/setting.backup.limit")),
        ("BACKUP_THROTTLE_PERIOD", t("admin/setting.backup.period")),
        ("GUZZLE_TIMEOUT", t("admin/setting.misc.connections.request_timeout")),
        ("GUZZLE_CONNECT_TIMEOUT", t("admin/setting.misc.connections.connection_timeout")),
        ("APP_ACTIVITY_PRUNE_DAYS", t("admin/setting.misc.activity_log.prune_age")),
        ("APP_WEBHOOK_PRUNE_DAYS", t("admin/setting.misc.webhook.prune_age")),
        ("PANEL_CLIENT_ALLOCATIONS_RANGE_START", t("admin/setting.misc.auto_allocation.start")),
        ("PANEL_CLIENT_ALLOCATIONS_RANGE_END", t("admin/setting.misc.auto_allocation.end")),
        ("CAPTCHA_TURNSTILE_SITE_KEY", t("admin/setting.captcha.site_key")),
        ("CAPTCHA_TURNSTILE_SECRET_KEY", t("admin/setting.captcha.secret_key")),
        ("OAUTH_STEAM_CLIENT_SECRET", t("admin/setting.oauth.web_api_key")),
        ("OAUTH_AUTHENTIK_BASE_URL", "base url".into()),
        ("OAUTH_AUTHENTIK_DISPLAY_COLOR", "display color".into()),
    ] {
        m.insert(pair.0.into(), pair.1);
    }
    for (_, limit, period) in RATE_LIMITS {
        m.insert((*limit).into(), t("admin/setting.misc.api.requests_per_period"));
        m.insert((*period).into(), t("admin/setting.misc.api.period_length"));
    }
    for (id, ..) in OAUTH_PROVIDERS {
        if *id == "steam" {
            continue;
        }
        let up = id.to_ascii_uppercase();
        m.insert(format!("OAUTH_{up}_CLIENT_ID"), t("admin/setting.oauth.client_id"));
        m.insert(format!("OAUTH_{up}_CLIENT_SECRET"), t("admin/setting.oauth.client_secret"));
    }
    m
}

/// Template context for the settings page (pure; shared by GET, the error
/// re-render and the render test).
fn settings_ctx(tab: &str, t: &dyn Fn(&str) -> String, readonly: bool, snap: &Snapshot<'_>, errors: &Errors) -> Mj {
    let providers: Vec<Mj> = OAUTH_PROVIDERS
        .iter()
        .map(|(id, name, icon)| {
            let up = id.to_ascii_uppercase();
            let display =
                if *id == "authentik" { snap.cur("OAUTH_AUTHENTIK_DISPLAY_NAME") } else { (*name).to_string() };
            context! { id => id, upper => up, name => display, icon => icon }
        })
        .collect();
    let rate_limits: Vec<Mj> = RATE_LIMITS
        .iter()
        .map(|(slug, limit, period)| {
            context! {
                limit_key => limit,
                period_key => period,
                title => t(&format!("admin/setting.misc.api.{slug}_title")),
                helper => t(&format!("admin/setting.misc.api.{slug}_helper")),
            }
        })
        .collect();
    let width_options: Vec<(String, String)> = WIDTHS
        .iter()
        .map(|w| {
            (
                (*w).to_string(),
                WIDTH_NAMES.iter().find(|(v, _)| v == w).map_or_else(|| (*w).to_string(), |(_, n)| humanize_case(n)),
            )
        })
        .collect();
    context! {
        tab => tab,
        readonly => readonly,
        can_update => !readonly,
        action => "/admin/settings",
        v => render_values(snap),
        proxies => tag_list(snap),
        oauth_providers => providers,
        rate_limits => rate_limits,
        width_options => width_options,
        mailer_options => vec![
            ("log", "/storage/logs Directory"),
            ("smtp", "SMTP Server"),
            ("mailgun", "Mailgun"),
            ("mandrill", "Mandrill"),
            ("postmark", "Postmark"),
            ("sendmail", "sendmail (PHP)"),
        ],
        avatar_options => vec![("gravatar", "Gravatar"), ("uiavatars", "UI Avatars")],
        nav_options => vec![
            ("sidebar", t("admin/setting.general.sidebar")),
            ("topbar", t("admin/setting.general.topbar")),
            ("mixed", t("admin/setting.general.mixed")),
        ],
        twofa_options => vec![
            ("0", t("admin/setting.general.not_required")),
            ("1", t("admin/setting.general.admins_only")),
            ("2", t("admin/setting.general.all_users")),
        ],
        scheme_options => vec![("smtp", "SMTP"), ("smtps", "SMTPS")],
        errors => Mj::from_serialize(errors),
    }
}

/// Context of the settings page (shared by GET and the error re-render).
async fn page_ctx(admin: &Admin, snap: &Snapshot<'_>, errors: &Errors) -> WebResult {
    let t = |k: &str| admin.t(k);
    let readonly = !admin.can("update settings");
    let ctx = settings_ctx(&admin.tab(TAB_NAMES), &t, readonly, snap, errors);
    let ctx = minijinja::context! { ..ctx, ..minijinja::context! { title => admin.t("admin/setting.title") } };
    admin.page("settings", "admin/settings.html", ctx).await
}

/// `CamelCase` enum label → "Camel case".
fn humanize_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        if i == 0 {
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

const TAB_NAMES: &[&str] = &["general", "captcha", "mail", "backup", "oauth", "misc"];

/// `GET /admin/settings`: `Settings::canAccess()` gate (`view settings`),
/// everything disabled without `update settings`.
async fn page(admin: Admin) -> WebResult {
    admin.authorize("view settings")?;
    let snap = Snapshot { env: &admin.st.config.env, form: None };
    Ok(page_ctx(&admin, &snap, &Errors::default()).await?)
}

/// Redirect preserving the active tab (`persistTabInQueryString`).
fn redirect_back(form: &Form) -> String {
    match form.opt("_tab").filter(|t| TAB_NAMES.iter().any(|x| *x == *t)) {
        Some(t) => format!("/admin/settings?tab={t}"),
        None => "/admin/settings".into(),
    }
}

/// `POST /admin/settings` (`Settings::save()`): authorize, validate, write
/// `.env`, request runtime reload (ADR-0014), flash success/failure.
async fn save(admin: Admin, form: Form) -> WebResult {
    admin.authorize("update settings")?;
    let snap = Snapshot { env: &admin.st.config.env, form: Some(&form) };
    let proxies = form.tags("TRUSTED_PROXIES");
    let errors = validate(&admin, &snap, &proxies)?;
    if !errors.is_empty() {
        return Ok(page_ctx(&admin, &snap, &errors).await?);
    }
    let values = write_values(&snap);
    match pelican_core::env::write_variables(&admin.st.config.base_path.join(".env"), &values, true) {
        Err(e) => admin.flash_danger(admin.t("admin/setting.save_failed"), Some(e.to_string())),
        Ok(()) => {
            // Artisan queue:restart has no Rust counterpart; ADR-0014 handles
            // configuration reload from the next request.
            admin.st.request_reload();
            admin.flash_success(admin.t("admin/setting.save_success"));
        }
    }
    Ok(admin.redirect(&redirect_back(&form)))
}

/// Test-mail hint action (`Settings::save()` mail tab): builds `MailConfig`
/// over the submitted values instead of the saved ones and notifies the user.
async fn test_mail(admin: Admin, form: Form) -> WebResult {
    admin.authorize("update settings")?;
    let snap = Snapshot { env: &admin.st.config.env, form: Some(&form) };
    let cfg = MailConfig {
        mailer: snap.cur("MAIL_MAILER"),
        host: snap.cur_or("MAIL_HOST", "127.0.0.1"),
        port: snap.int_cur("MAIL_PORT").clamp(1, 65535) as u16,
        username: Snapshot::non_blank(&snap.cur("MAIL_USERNAME")),
        password: Snapshot::non_blank(&snap.cur("MAIL_PASSWORD")),
        scheme: snap.cur_or("MAIL_SCHEME", "smtp").to_lowercase(),
        from_address: snap.cur_or("MAIL_FROM_ADDRESS", "hello@example.com"),
        from_name: snap.cur_or("MAIL_FROM_NAME", "Example"),
        app_name: admin.st.config.app_name.clone(),
    };
    let locale = admin.locale();
    let msg = MailMessage {
        outro: Vec::new(),
        subject: i18n::trans(&locale, "mail.mail_tested.subject", &[]),
        error: false,
        greeting: Some(i18n::trans(&locale, "mail.greeting", &[("name".into(), admin.user.user.username.clone())])),
        lines: vec![i18n::trans(&locale, "mail.mail_tested.body", &[])],
        action: None,
    };
    match pelican_core::mail::send(&cfg, &admin.user.user.username, &admin.user.user.email, &msg).await {
        Ok(()) => admin.flash_success(admin.t("admin/setting.mail.test_mail_sent")),
        Err(e) => admin.flash_danger(admin.t("admin/setting.mail.test_mail_failed"), Some(e.to_string())),
    }
    Ok(admin.redirect(&redirect_back(&form)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(map: &[(&str, &str)]) -> Snapshot<'static> {
        let vars = map.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        let env: &'static Env = Box::leak(Box::new(Env::from_map(vars)));
        Snapshot { env, form: None }
    }

    /// The settings page renders in every tab without template errors.
    #[test]
    fn template_renders() {
        let s = snap(&[]);
        for tab in TAB_NAMES {
            let ctx = settings_ctx(tab, &|k| i18n::trans("en", k, &[]), false, &s, &Errors::default());
            crate::views::render("admin/settings.html", ctx).expect("template renders");
        }
    }

    #[test]
    fn booleans_fall_back_to_config_defaults() {
        // PANEL_SEND_* default true, OAUTH defaults false like `env(k, false)`.
        let s = snap(&[("OAUTH_X_ENABLED", "true")]);
        assert!(s.bool_cur("PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION"));
        assert!(!s.bool_cur("OAUTH_GOOGLE_ENABLED"));
        assert!(s.bool_cur("OAUTH_X_ENABLED"));
        // Config fallbacks of the misc section.
        assert_eq!(s.int_cur("GUZZLE_TIMEOUT"), 15);
        assert_eq!(s.int_cur("APP_ACTIVITY_PRUNE_DAYS"), 90);
        // Env value conversion like `env()` ("(true)").
        let s2 = snap(&[("APP_DEBUG", "(true)")]);
        assert!(s2.bool_cur("APP_DEBUG"));
    }

    #[test]
    fn write_values_map_types_like_save() {
        let s = snap(&[("TRUSTED_PROXIES", "10.0.0.1, 10.0.0.2"), ("PANEL_USE_BINARY_PREFIX", "1")]);
        let values = write_values(&s);
        let get = |k: &str| values.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        // strings verbatim (with their config fallback), tags joined, numbers raw,
        // bools as true/false
        assert_eq!(get("APP_NAME"), Some(EnvWrite::Str("Pelican".into())));
        assert_eq!(get("FILAMENT_UPLOADABLE_AVATARS"), Some(EnvWrite::Str("false".into())));
        assert_eq!(get("CAPTCHA_TURNSTILE_VERIFY_DOMAIN"), Some(EnvWrite::Str("true".into())));
        assert_eq!(get("PANEL_USE_BINARY_PREFIX"), Some(EnvWrite::Raw("1".into())));
        assert_eq!(get("TRUSTED_PROXIES"), Some(EnvWrite::Str("10.0.0.1,10.0.0.2".into())));
        // never-saved allocation ranges stay empty
        assert_eq!(get("PANEL_CLIENT_ALLOCATIONS_RANGE_START"), Some(EnvWrite::Null));
        // endpoints keep their config fallback
        assert_eq!(get("MAILGUN_ENDPOINT"), Some(EnvWrite::Str("api.mailgun.net".into())));
        // schema order: first APP_NAME, last APP_WEBHOOK_PRUNE_DAYS
        assert_eq!(values.first().unwrap().0, "APP_NAME");
        assert_eq!(values.last().unwrap().0, "APP_WEBHOOK_PRUNE_DAYS");
    }
}

/// `hint_cloudflare`: the current Cloudflare ranges (an empty list when the request fails).
async fn cloudflare_ips(admin: Admin) -> WebResult {
    admin.authorize("update settings")?;
    let mut ips: Vec<String> = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .connect_timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| WebError::internal(e.to_string()))?;
    if let Ok(resp) = client.get("https://api.cloudflare.com/client/v4/ips").send().await {
        if resp.status().as_u16() == 200 {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                for key in ["ipv4_cidrs", "ipv6_cidrs"] {
                    for ip in
                        body.get("result").and_then(|r| r.get(key)).and_then(|v| v.as_array()).into_iter().flatten()
                    {
                        if let Some(s) = ip.as_str() {
                            ips.push(s.to_string());
                        }
                    }
                }
            }
        }
    }
    Ok(axum::response::IntoResponse::into_response(axum::Json(ips)))
}
