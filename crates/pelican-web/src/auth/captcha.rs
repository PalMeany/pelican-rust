//! Captcha extension (app/Extensions/Captcha): Cloudflare Turnstile, the only
//! schema registered by default (`CAPTCHA_TURNSTILE_*`).

use pelican_core::state::AppState;
use serde_json::{Value, json};

/// The active schema's site key, when Turnstile is enabled.
pub fn site_key(st: &AppState) -> Option<String> {
    st.config
        .env
        .bool_or("CAPTCHA_TURNSTILE_ENABLED", false)
        .then(|| st.config.env.string_or("CAPTCHA_TURNSTILE_SITE_KEY", ""))
}

/// `TurnstileSchema::validateResponse()`; the error is the rule's failure message.
pub async fn validate(st: &AppState, response: &str) -> Result<(), String> {
    let fail = |m: &str| Err(format!("Captcha validation failed: {m}"));
    let secret = st.config.env.string_or("CAPTCHA_TURNSTILE_SECRET_KEY", "");
    if secret.is_empty() {
        return fail("Turnstile secret key is not defined.");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post("https://challenges.cloudflare.com/turnstile/v0/siteverify")
        .json(&json!({ "secret": secret, "response": response }))
        .send()
        .await
        .map_err(|e| format!("Captcha validation failed: {e}"))?;
    if !resp.status().is_success() {
        return fail(&format!("HTTP request returned status code {}", resp.status().as_u16()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("Captcha validation failed: {e}"))?;
    if !body.get("success").and_then(Value::as_bool).unwrap_or(false) {
        let code = body.get("error-codes").and_then(|c| c.get(0)).and_then(Value::as_str).unwrap_or("");
        return fail(match code {
            "missing-input-secret" => "The secret parameter was not passed.",
            "invalid-input-secret" => {
                "The secret parameter was invalid, did not exist, or is a testing secret key with a non-testing response."
            }
            "missing-input-response" => "The response parameter (token) was not passed.",
            "invalid-input-response" => "The response parameter (token) is invalid or has expired.",
            "bad-request" => "The request was rejected because it was malformed.",
            "timeout-or-duplicate" => "The response parameter (token) has already been validated before.",
            _ => "An internal error happened while validating the response.",
        });
    }
    // verifyDomain(): the hostname Cloudflare saw must be this panel's host.
    if st.config.env.bool_or("CAPTCHA_TURNSTILE_VERIFY_DOMAIN", true) {
        let host = reqwest::Url::parse(&st.config.app_url).ok().and_then(|u| u.host_str().map(String::from));
        if body.get("hostname").and_then(Value::as_str) != host.as_deref() {
            return fail("Domain verification failed.");
        }
    }
    Ok(())
}
