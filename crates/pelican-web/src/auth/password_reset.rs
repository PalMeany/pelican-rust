//! Filament's password reset pages (`Auth\Pages\PasswordReset\{RequestPasswordReset,
//! ResetPassword}`) over Laravel's password broker (`pelican_api::passwords`).

use minijinja::{Value, context};
use pelican_api::passwords::{self, Status};
use pelican_api::services::LogActivity;
use pelican_api::validation::Validator;
use pelican_core::activity::morph;
use serde_json::json;

use crate::ctx::Web;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};

const REQ: &str = "filament-panels::auth/pages/password-reset/request-password-reset";
const RESET: &str = "filament-panels::auth/pages/password-reset/reset-password";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn sha1_hex(s: &str) -> String {
    use sha1::Digest;
    hex::encode(sha1::Sha1::digest(s.as_bytes()))
}

/// `WithRateLimiting::rateLimit($max)` for a Livewire component method (per IP, 60 s).
fn rate_limited(web: &Web, component: &str, method: &str, max: u32, group: &str) -> bool {
    let key = format!("livewire-rate-limiter:{}", sha1_hex(&format!("{component}|{method}|{}", web.ip)));
    if let Some(seconds) = web.st.limiter.too_many_attempts(&key, max, now()) {
        throttled(web, group, seconds);
        return true;
    }
    web.st.limiter.hit(&key, 60, now());
    false
}

fn throttled(web: &Web, group: &str, seconds: i64) {
    let p = [("seconds", seconds.to_string()), ("minutes", ((seconds + 59) / 60).to_string())];
    let p: Vec<(&str, &str)> = p.iter().map(|(k, v)| (*k, v.as_str())).collect();
    web.flash_danger(
        web.t_with(&format!("{group}.notifications.throttled.title"), &p),
        Some(web.t_with(&format!("{group}.notifications.throttled.body"), &p)),
    );
}

/// `GET /password-reset/request`.
pub async fn request_form(web: Web) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    web.render(
        "auth/password_request.html",
        context! { errors => Value::from_serialize(Errors::default()), email => "" },
    )
}

/// `POST /password-reset/request`: `RequestPasswordReset::request()`.
pub async fn request(web: Web, form: Form) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    let email = form.get("email").trim().to_string();
    let render = |errors: &Errors, email: &str| {
        web.render("auth/password_request.html", context! { errors => Value::from_serialize(errors), email => email })
    };
    if rate_limited(&web, "Filament\\Auth\\Pages\\PasswordReset\\RequestPasswordReset", "request", 2, REQ) {
        return render(&Errors::default(), &email);
    }
    let data = json!({ "email": email });
    let mut v = Validator::for_value(&data);
    v.attribute_name("email", &web.t(&format!("{REQ}.form.email.label")).to_lowercase());
    v.rules("email", &["required", "email"]);
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        return render(&errors, &email);
    }
    let status = passwords::send_reset_link(&web.st, &email).await.map_err(|e| WebError::internal(format!("{e:?}")))?;
    if status == Status::Sent {
        web.session.flash("success", web.t(status.key()), Some(web.t(&format!("{REQ}.notifications.sent.body"))));
        return render(&Errors::default(), "");
    }
    web.flash_danger(web.t(status.key()), None);
    render(&Errors::default(), &email)
}

/// `hasValidSignature()` for `URL::signedRoute('…password-reset.reset', [email, token])`.
fn valid_signature(web: &Web, email: &str, token: &str, signature: &str) -> bool {
    let expected = pelican_api::services::reset_url(&web.st, email, token);
    expected.rsplit_once("&signature=").is_some_and(|(_, sig)| {
        use subtle_eq::ConstantTimeEq;
        sig.as_bytes().ct_eq(signature.as_bytes())
    })
}

mod subtle_eq {
    pub trait ConstantTimeEq {
        fn ct_eq(&self, other: &Self) -> bool;
    }
    impl ConstantTimeEq for [u8] {
        fn ct_eq(&self, other: &[u8]) -> bool {
            self.len() == other.len() && self.iter().zip(other).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
        }
    }
}

fn query_param(web: &Web, key: &str) -> String {
    web.query
        .as_deref()
        .and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned()))
        .unwrap_or_default()
}

fn render_reset(web: &Web, email: &str, token: &str, signature: &str, errors: &Errors) -> WebResult {
    web.render(
        "auth/password_reset.html",
        context! { errors => Value::from_serialize(errors), email => email, token => token, signature => signature },
    )
}

/// `GET /password-reset/reset?email&token&signature` (signed route).
pub async fn reset_form(web: Web) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    let (email, token, signature) =
        (query_param(&web, "email"), query_param(&web, "token"), query_param(&web, "signature"));
    if !valid_signature(&web, &email, &token, &signature) {
        return Err(WebError::Forbidden);
    }
    render_reset(&web, &email, &token, &signature, &Errors::default())
}

/// `POST /password-reset/reset`: `ResetPassword::resetPassword()`.
pub async fn reset(web: Web, form: Form) -> WebResult {
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    let (email, token, signature) =
        (form.get("email").to_string(), form.get("token").to_string(), form.get("signature").to_string());
    // email/token are #[Locked] properties set from the signed URL.
    if !valid_signature(&web, &email, &token, &signature) {
        return Err(WebError::Forbidden);
    }
    if rate_limited(&web, "Filament\\Auth\\Pages\\PasswordReset\\ResetPassword", "resetPassword", 2, RESET) {
        return render_reset(&web, &email, &token, &signature, &Errors::default());
    }
    // isResetPasswordRateLimited(): two attempts per broker and email.
    if !email.is_empty() {
        let key = format!("filament-reset-password:{}", sha1_hex(&format!("users|{email}")));
        if let Some(seconds) = web.st.limiter.too_many_attempts(&key, 2, now()) {
            throttled(&web, RESET, seconds);
            return render_reset(&web, &email, &token, &signature, &Errors::default());
        }
        web.st.limiter.hit(&key, 60, now());
    }
    let data = json!({ "password": form.get("password"), "passwordConfirmation": form.get("passwordConfirmation") });
    let mut v = Validator::for_value(&data);
    v.attribute_name("password", &web.t(&format!("{RESET}.form.password.validation_attribute")));
    v.attribute_name(
        "passwordConfirmation",
        &web.t(&format!("{RESET}.form.password_confirmation.label")).to_lowercase(),
    );
    v.rules("password", &["required", "string", "min:8", "same:passwordConfirmation"]);
    v.rules("passwordConfirmation", &["required"]);
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        return render_reset(&web, &email, &token, &signature, &errors);
    }
    let (status, user) = passwords::reset(&web.st, &email, &token, form.get("password"))
        .await
        .map_err(|e| WebError::internal(format!("{e:?}")))?;
    if status != Status::Reset {
        web.flash_danger(web.t(status.key()), None);
        return render_reset(&web, &email, &token, &signature, &Errors::default());
    }
    if let Some(u) = user {
        // PasswordResetListener
        let _ = web.activity("auth:password-reset").subject(morph::USER, u.id.0).log(&web.st).await;
    }
    web.flash_success(web.t(status.key()));
    Ok(web.redirect("/login"))
}
