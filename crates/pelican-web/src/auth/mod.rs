//! Sign-in, multi-factor challenge and sign-out
//! (app/Filament/Pages/Auth/Login.php over Filament's `Auth\Pages\Login`).

pub mod totp;

use axum::response::Response;
use minijinja::{Value, context};
use pelican_api::services::LogActivity;
use pelican_core::activity::morph;
use pelican_db::entities::users;
use pelican_db::raw;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde_json::json;

use crate::ctx::Web;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};

const LOGIN: &str = "filament-panels::auth/pages/login";
/// How long a started MFA challenge stays valid.
const CHALLENGE_SECONDS: i64 = 15 * 60;
/// `EmailAuthentication::$codeExpiryMinutes`.
const EMAIL_CODE_MINUTES: i64 = 4;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// PHP `filter_var($v, FILTER_VALIDATE_EMAIL)` approximation for choosing the column.
fn looks_like_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else { return false };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
        && !local.contains('@')
}

/// Which MFA provider challenges this user (`getMultiFactorAuthenticationProviders` order).
fn mfa_provider(u: &users::Model) -> Option<&'static str> {
    if u.mfa_app_secret.as_deref().is_some_and(|s| !s.is_empty()) {
        Some("app")
    } else if u.mfa_email_enabled.0 {
        Some("email_code")
    } else {
        None
    }
}

fn render_login(web: &Web, errors: &Errors, login: &str, remember: bool) -> WebResult {
    web.render(
        "auth/login.html",
        context! {
            errors => Value::from_serialize(errors),
            old => context! { login => login, remember => remember },
            password_login => !web.st.config.env.bool_or("PANEL_DISABLE_PASSWORD_LOGIN", false)
                && !web.st.config.env.bool_or("AUTH_DISABLE_PASSWORD_LOGIN", false),
        },
    )
}

fn render_challenge(web: &Web, provider: &str, errors: &Errors, recovery: bool) -> WebResult {
    web.render(
        "auth/challenge.html",
        context! { provider => provider, errors => Value::from_serialize(errors), recovery => recovery },
    )
}

/// `GET /login`.
pub async fn show(web: Web) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    if let Some((_, provider)) = pending(&web).await? {
        return render_challenge(&web, provider, &Errors::default(), false);
    }
    render_login(&web, &Errors::default(), "", false)
}

/// The user waiting for its MFA challenge, if the challenge is still valid.
async fn pending(web: &Web) -> Result<Option<(users::Model, &'static str)>, WebError> {
    let data = web.session.data();
    let Some(id) = data.mfa_pending else { return Ok(None) };
    let started = data.extra.get("mfa_started").and_then(serde_json::Value::as_i64).unwrap_or(0);
    if started < now() - CHALLENGE_SECONDS {
        web.session.update(|d| d.mfa_pending = None);
        return Ok(None);
    }
    let Some(user) = users::Entity::find_by_id(id).one(&web.st.db).await? else { return Ok(None) };
    Ok(mfa_provider(&user).map(|p| (user, p)))
}

/// `POST /login`: `Login::authenticate()`.
pub async fn login(web: Web, form: Form) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    let login = form.get("login").trim().to_string();
    let password = form.get("password").to_string();
    let remember = form.bool("remember");

    // WithRateLimiting::rateLimit(5): per IP, 60 s decay.
    let key = format!(
        "livewire-rate-limiter:{}",
        sha1_hex(&format!("App\\Filament\\Pages\\Auth\\Login|authenticate|{}", web.ip))
    );
    if let Some(seconds) = web.st.limiter.too_many_attempts(&key, 5, now()) {
        web.flash_danger(
            web.t(&format!("{LOGIN}.notifications.throttled.title")),
            Some(web.t_with(
                &format!("{LOGIN}.notifications.throttled.body"),
                &[("seconds", &seconds.to_string()), ("minutes", &((seconds + 59) / 60).to_string())],
            )),
        );
        return render_login(&web, &Errors::default(), &login, remember);
    }
    web.st.limiter.hit(&key, 60, now());

    let mut errors = Errors::default();
    if login.is_empty() {
        errors.add(
            "login",
            web.t_with("validation.required", &[("attribute", &web.t(&format!("{LOGIN}.title")).to_lowercase())]),
        );
    }
    if password.is_empty() {
        errors.add(
            "password",
            web.t_with(
                "validation.required",
                &[("attribute", &web.t(&format!("{LOGIN}.form.password.label")).to_lowercase())],
            ),
        );
    }
    if !errors.is_empty() {
        return render_login(&web, &errors, &login, remember);
    }
    if web.st.config.env.bool_or("AUTH_DISABLE_PASSWORD_LOGIN", false) {
        errors.add("login", web.t("auth.password_login_disabled"));
        return render_login(&web, &errors, &login, remember);
    }

    let (column, value) =
        if looks_like_email(&login) { ("email", login.to_lowercase()) } else { ("username", login.to_lowercase()) };
    let col = if column == "email" { users::Column::Email } else { users::Column::Username };
    let found = users::Entity::find().filter(col.eq(value.clone())).one(&web.st.db).await?;
    let valid = found.as_ref().is_some_and(|u| laravel_compat::hashing::check(&password, &u.password).unwrap_or(false));
    let Some(user) = found.clone().filter(|_| valid) else {
        let mut a = web.activity("auth:fail").property(column, value);
        if let Some(u) = &found {
            a = a.subject(morph::USER, u.id.0);
        }
        let _ = a.log(&web.st).await;
        errors.add("login", web.t(&format!("{LOGIN}.messages.failed")));
        return render_login(&web, &errors, &login, remember);
    };

    if let Some(provider) = mfa_provider(&user) {
        web.session.update(|d| {
            d.mfa_pending = Some(user.id.0);
            d.extra.insert("mfa_started".into(), json!(now()));
            d.extra.insert("mfa_remember".into(), json!(remember));
        });
        if provider == "email_code" {
            send_email_code(&web, &user).await?;
        }
        return render_challenge(&web, provider, &Errors::default(), false);
    }
    complete(&web, user, remember, &password).await
}

/// `POST /login/challenge`: verify the app / email / recovery code.
pub async fn challenge(web: Web, form: Form) -> WebResult {
    let Some((user, provider)) = pending(&web).await? else {
        return Ok(web.redirect("/login"));
    };
    if provider == "app" && form.bool("show_recovery") {
        return render_challenge(&web, provider, &Errors::default(), true);
    }
    let mut errors = Errors::default();
    let use_recovery = form.bool("use_recovery");
    let code = form.get("code").trim().to_string();
    let recovery = form.get("recovery_code").trim().to_string();
    let ok = match provider {
        "app" if use_recovery && !recovery.is_empty() => {
            let ok = use_recovery_code(&web, &user, &recovery).await?;
            if !ok {
                errors.add(
                    "recovery_code",
                    web.t("filament-panels::auth/multi-factor/app/provider.login_form.recovery_code.messages.invalid"),
                );
            }
            ok
        }
        "app" => {
            let secret =
                user.mfa_app_secret.as_deref().and_then(|s| web.st.enc.decrypt_string(s).ok()).unwrap_or_default();
            let ok = !code.is_empty() && totp::verify_once(&secret, &code, now());
            if !ok {
                let msg = if code.is_empty() {
                    web.t_with(
                        "validation.required",
                        &[(
                            "attribute",
                            &web.t(
                                "filament-panels::auth/multi-factor/app/provider.login_form.code.validation_attribute",
                            ),
                        )],
                    )
                } else {
                    web.t("filament-panels::auth/multi-factor/app/provider.login_form.code.messages.invalid")
                };
                errors.add("code", msg);
            }
            ok
        }
        _ => {
            let ok = verify_email_code(&web, &code);
            if !ok {
                errors.add(
                    "code",
                    web.t("filament-panels::auth/multi-factor/email/provider.login_form.code.messages.invalid"),
                );
            }
            ok
        }
    };
    if !ok {
        return render_challenge(&web, provider, &errors, use_recovery);
    }
    let remember = web.session.data().extra.get("mfa_remember").and_then(serde_json::Value::as_bool).unwrap_or(false);
    web.session.update(|d| {
        d.mfa_pending = None;
        d.extra.remove("mfa_started");
        d.extra.remove("mfa_remember");
        d.extra.remove("mfa_email_code");
    });
    complete(&web, user, remember, "").await
}

/// `POST /login/challenge/resend` (email code).
pub async fn resend(web: Web) -> WebResult {
    let Some((user, "email_code")) = pending(&web).await? else {
        return Ok(web.redirect("/login"));
    };
    if send_email_code(&web, &user).await? {
        web.flash_success(web.t("filament-panels::auth/multi-factor/email/provider.login_form.code.actions.resend.notifications.resent.title"));
    } else {
        web.flash_danger(
            web.t("filament-panels::auth/multi-factor/email/provider.login_form.code.actions.resend.notifications.throttled.title"),
            None,
        );
    }
    render_challenge(&web, "email_code", &Errors::default(), false)
}

/// `POST /login/cancel`: back to the credentials form.
pub async fn cancel(web: Web) -> Response {
    web.session.update(|d| d.mfa_pending = None);
    web.redirect("/login")
}

/// `EmailAuthentication::sendCode()`: 2 sends per minute, code bound to this session.
async fn send_email_code(web: &Web, user: &users::Model) -> Result<bool, WebError> {
    let key = format!("filament-email-authentication:{}", user.id.0);
    if web.st.limiter.too_many_attempts(&key, 2, now()).is_some() {
        return Ok(false);
    }
    web.st.limiter.hit(&key, 60, now());
    let code = format!("{:06}", rand::random_range(0..1_000_000u32));
    let hash = laravel_compat::hashing::make(&code, web.st.config.bcrypt_rounds).map_err(WebError::internal)?;
    web.session.update(|d| {
        d.extra.insert("mfa_email_code".into(), json!({ "hash": hash, "expires": now() + EMAIL_CODE_MINUTES * 60 }));
    });
    let locale = user.language.clone();
    let group = "filament-panels::auth/multi-factor/email/notifications/verify-email-authentication";
    let params = vec![("code".to_string(), code.clone()), ("minutes".to_string(), EMAIL_CODE_MINUTES.to_string())];
    let line =
        |i: u8| crate::i18n::trans_choice(&locale, &format!("{group}.lines.{i}"), EMAIL_CODE_MINUTES as f64, &params);
    let msg = pelican_core::mail::MailMessage {
        subject: crate::i18n::trans(&locale, &format!("{group}.subject"), &[]),
        lines: vec![line(0), line(1)],
        ..Default::default()
    };
    pelican_api::services::notify(&web.st, user, msg).await.map_err(|e| WebError::internal(format!("{e:?}")))?;
    Ok(true)
}

fn verify_email_code(web: &Web, code: &str) -> bool {
    let data = web.session.data();
    let Some(entry) = data.extra.get("mfa_email_code") else { return false };
    let hash = entry.get("hash").and_then(|h| h.as_str()).unwrap_or("");
    let expires = entry.get("expires").and_then(|e| e.as_i64()).unwrap_or(0);
    if code.is_empty() || expires < now() || !laravel_compat::hashing::check(code, hash).unwrap_or(false) {
        return false;
    }
    web.session.update(|d| {
        d.extra.remove("mfa_email_code");
    });
    true
}

/// `AppAuthentication::verifyRecoveryCode()`: consume one bcrypt-hashed code.
async fn use_recovery_code(web: &Web, user: &users::Model, code: &str) -> Result<bool, WebError> {
    let Some(stored) = user.mfa_app_recovery_codes.as_deref() else { return Ok(false) };
    let Ok(json) = web.st.enc.decrypt_string(stored) else { return Ok(false) };
    let hashes: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
    let mut remaining = Vec::with_capacity(hashes.len());
    let mut valid = false;
    for h in hashes {
        if !valid && laravel_compat::hashing::check(code, &h).unwrap_or(false) {
            valid = true;
            continue;
        }
        remaining.push(h);
    }
    if !valid {
        return Ok(false);
    }
    let encoded = laravel_compat::json::to_string(&remaining).map_err(WebError::internal)?;
    let b = web.st.db.get_database_backend();
    web.st
        .db
        .execute_raw(raw::stmt(
            b,
            "update {users} set {mfa_app_recovery_codes} = ?, {updated_at} = ? where {id} = ?",
            vec![web.st.enc.encrypt_string(&encoded).into(), pelican_core::time::now().into(), user.id.0.into()],
        ))
        .await?;
    Ok(true)
}

/// Successful sign-in: new session id, `auth:success`, password rehash, redirect.
async fn complete(web: &Web, user: users::Model, remember: bool, password: &str) -> WebResult {
    if !password.is_empty() && laravel_compat::hashing::needs_rehash(&user.password, web.st.config.bcrypt_rounds) {
        if let Ok(hash) = laravel_compat::hashing::make(password, web.st.config.bcrypt_rounds) {
            let b = web.st.db.get_database_backend();
            web.st
                .db
                .execute_raw(raw::stmt(
                    b,
                    "update {users} set {password} = ?, {updated_at} = ? where {id} = ?",
                    vec![hash.into(), pelican_core::time::now().into(), user.id.0.into()],
                ))
                .await?;
        }
    }
    web.session.login(user.id.0, remember);
    let _ =
        web.activity("auth:success").actor(morph::USER, user.id.0).subject(morph::USER, user.id.0).log(&web.st).await;
    let intended = web.session.data().intended.filter(|p| p.starts_with('/') && !p.starts_with("//"));
    web.session.update(|d| d.intended = None);
    Ok(web.redirect(intended.as_deref().unwrap_or("/")))
}

/// `POST /logout`.
pub async fn logout(web: Web) -> Response {
    web.session.logout();
    web.redirect("/login")
}

fn sha1_hex(s: &str) -> String {
    use sha1::Digest;
    hex::encode(sha1::Sha1::digest(s.as_bytes()))
}
