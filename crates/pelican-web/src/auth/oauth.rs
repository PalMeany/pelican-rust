//! OAuth sign-in and account linking (app/Extensions/OAuth + OAuthController) over
//! the Socialite drivers the original registers: facebook, x, linkedin, google,
//! github, gitlab, bitbucket, slack (Socialite) and authentik, discord, steam
//! (socialiteproviders). Configuration comes from `OAUTH_<ID>_*`.

use axum::extract::Path;
use axum::response::Response;
use pelican_api::model_events;
use pelican_api::services::LogActivity;
use pelican_core::activity::morph;
use pelican_core::env::Env;
use pelican_core::state::AppState;
use pelican_db::entities::users;
use pelican_db::{Json, Timestamp};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};

use crate::ctx::Web;
use crate::error::{WebError, WebResult};

/// Registration order of `OAuthServiceProvider` (login buttons, profile tab).
pub const PROVIDERS: [&str; 11] =
    ["facebook", "x", "linkedin", "google", "github", "gitlab", "bitbucket", "slack", "authentik", "discord", "steam"];

#[derive(Clone, Debug, serde::Serialize)]
pub struct Schema {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
}

fn upper(id: &str) -> String {
    id.to_uppercase()
}

/// `OAuthSchema::isEnabled()` (`OAUTH_<ID>_ENABLED`).
pub fn is_enabled(env: &Env, id: &str) -> bool {
    PROVIDERS.contains(&id) && env.bool_or(&format!("OAUTH_{}_ENABLED", upper(id)), false)
}

/// `getName()` / `getIcon()` / `getHexColor()` of each schema.
pub fn schema(env: &Env, id: &'static str) -> Schema {
    let (icon, color) = match id {
        "facebook" => (Some("brand-facebook-filled"), Some("#1877f2")),
        "x" => (Some("brand-x"), Some("#1da1f2")),
        "linkedin" => (Some("brand-linkedin-filled"), Some("#0a66c2")),
        "google" => (Some("brand-google-filled"), Some("#4285f4")),
        "github" => (Some("brand-github-filled"), Some("#4078c0")),
        "gitlab" => (Some("brand-gitlab"), Some("#fca326")),
        "bitbucket" => (Some("brand-bitbucket-filled"), Some("#205081")),
        "slack" => (Some("brand-slack"), Some("#6ecadc")),
        "discord" => (Some("brand-discord-filled"), Some("#5865F2")),
        "steam" => (Some("brand-steam-filled"), Some("#00adee")),
        _ => (None, None),
    };
    let (name, color) = if id == "authentik" {
        (
            env.string_or("OAUTH_AUTHENTIK_DISPLAY_NAME", "Authentik"),
            Some(env.string_or("OAUTH_AUTHENTIK_DISPLAY_COLOR", "#fd4b2d")),
        )
    } else {
        // Str::title($id)
        let mut c = id.chars();
        (c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default(), color.map(String::from))
    };
    Schema { id: id.to_string(), name, icon: icon.map(String::from), color }
}

/// `OAuthService::getEnabled()`.
/// Enabled providers: the built-in ones, then those registered by PHP plugins (ADR-0020).
pub fn enabled(env: &Env) -> Vec<Schema> {
    let mut out: Vec<Schema> = PROVIDERS.iter().filter(|id| is_enabled(env, id)).map(|id| schema(env, id)).collect();
    if let Some(m) = pelican_api::sidecar::manifest() {
        out.extend(m.oauth_providers.iter().filter(|p| p.enabled).map(|p| Schema {
            id: p.id.clone(),
            name: p.name.clone(),
            icon: p.icon.as_deref().map(|i| i.trim_start_matches("tabler-").to_string()),
            color: p.color.clone(),
        }));
    }
    out
}

/// A plugin provider's OAuth flow runs in the PHP sidecar (Socialite there); a sign-in comes
/// back through the proxy's login hand-off.
fn plugin_driver(driver: &str) -> bool {
    find_id(driver).is_none() && pelican_api::sidecar::oauth_provider(driver).is_some_and(|p| p.enabled)
}

fn find_id(id: &str) -> Option<&'static str> {
    PROVIDERS.iter().copied().find(|p| *p == id)
}

/// `config('services.<id>.redirect')` = `/auth/oauth/callback/<id>`, made absolute.
fn redirect_url(st: &AppState, id: &str) -> String {
    format!("{}/auth/oauth/callback/{id}", st.config.app_url.trim_end_matches('/'))
}

fn client(env: &Env, id: &str) -> (String, String) {
    let u = upper(id);
    (env.string_or(&format!("OAUTH_{u}_CLIENT_ID"), ""), env.string_or(&format!("OAUTH_{u}_CLIENT_SECRET"), ""))
}

fn gitlab_host(env: &Env) -> String {
    let h = env.string_or("OAUTH_GITLAB_HOST", "");
    if h.is_empty() { "https://gitlab.com".into() } else { h.trim_end_matches('/').to_string() }
}

fn authentik_base(env: &Env) -> String {
    env.string_or("OAUTH_AUTHENTIK_BASE_URL", "").trim_end_matches('/').to_string()
}

/// Authorization endpoint, scopes and scope separator of a driver.
fn auth_spec(env: &Env, id: &str) -> (String, &'static [&'static str], &'static str) {
    match id {
        "facebook" => ("https://www.facebook.com/v23.0/dialog/oauth".into(), &["email"], ","),
        "x" => ("https://x.com/i/oauth2/authorize".into(), &["users.read", "users.email", "tweet.read"], " "),
        "linkedin" => {
            ("https://www.linkedin.com/oauth/v2/authorization".into(), &["r_liteprofile", "r_emailaddress"], " ")
        }
        "google" => ("https://accounts.google.com/o/oauth2/auth".into(), &["openid", "profile", "email"], " "),
        "github" => ("https://github.com/login/oauth/authorize".into(), &["user:email"], ","),
        "gitlab" => (format!("{}/oauth/authorize", gitlab_host(env)), &["read_user"], " "),
        "bitbucket" => ("https://bitbucket.org/site/oauth2/authorize".into(), &["email"], " "),
        "slack" => (
            "https://slack.com/oauth/v2/authorize".into(),
            &["identity.basic", "identity.email", "identity.team", "identity.avatar"],
            ",",
        ),
        "authentik" => (
            format!("{}/application/o/authorize/", authentik_base(env)),
            &["openid goauthentik.io/api profile email"],
            " ",
        ),
        _ => ("https://discord.com/api/oauth2/authorize".into(), &["identify", "email"], " "),
    }
}

fn token_url(env: &Env, id: &str) -> String {
    match id {
        "facebook" => "https://graph.facebook.com/v23.0/oauth/access_token".into(),
        "x" => "https://api.x.com/2/oauth2/token".into(),
        "linkedin" => "https://www.linkedin.com/oauth/v2/accessToken".into(),
        "google" => "https://www.googleapis.com/oauth2/v4/token".into(),
        "github" => "https://github.com/login/oauth/access_token".into(),
        "gitlab" => format!("{}/oauth/token", gitlab_host(env)),
        "bitbucket" => "https://bitbucket.org/site/oauth2/access_token".into(),
        "slack" => "https://slack.com/api/oauth.v2.access".into(),
        "authentik" => format!("{}/application/o/token/", authentik_base(env)),
        _ => "https://discord.com/api/oauth2/token".into(),
    }
}

fn str_random(n: usize) -> String {
    pelican_wings::jwt::str_random(n)
}

/// `http_build_query($fields, '', '&', PHP_QUERY_RFC1738)`.
fn build_query(fields: &[(&str, String)]) -> String {
    form_urlencoded::Serializer::new(String::new()).extend_pairs(fields.iter().map(|(k, v)| (*k, v.as_str()))).finish()
}

/// The provider URL a sign-in or link starts at (`Socialite::driver($id)->redirect()`).
fn authorization_url(web: &Web, id: &'static str) -> String {
    let st = &web.st;
    let env = &st.config.env;
    if id == "steam" {
        // OpenID 2.0, stateless; realm = scheme://host of the panel.
        let realm = reqwest::Url::parse(&st.config.app_url)
            .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default()))
            .unwrap_or_default();
        let fields = [
            ("openid.ns", "http://specs.openid.net/auth/2.0".to_string()),
            ("openid.mode", "checkid_setup".into()),
            ("openid.return_to", redirect_url(st, id)),
            ("openid.realm", realm),
            ("openid.identity", "http://specs.openid.net/auth/2.0/identifier_select".into()),
            ("openid.claimed_id", "http://specs.openid.net/auth/2.0/identifier_select".into()),
        ];
        return format!("https://steamcommunity.com/openid/login?{}", build_query(&fields));
    }
    let (base, scopes, sep) = auth_spec(env, id);
    let state = str_random(40);
    let mut fields: Vec<(&str, String)> = vec![
        ("client_id", client(env, id).0),
        ("redirect_uri", redirect_url(st, id)),
        ("scope", scopes.join(sep)),
        ("response_type", "code".into()),
        ("state", state.clone()),
    ];
    let mut verifier = None;
    if id == "x" {
        // usesPKCE: S256 challenge of a 96-character verifier.
        use sha2::Digest;
        let v = str_random(96);
        let challenge = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            sha2::Sha256::digest(v.as_bytes()),
        );
        fields.push(("code_challenge", challenge));
        fields.push(("code_challenge_method", "S256".into()));
        verifier = Some(v);
    }
    if id == "slack" {
        // Requested as user scopes.
        let user_scope = scopes.join(sep);
        if let Some(f) = fields.iter_mut().find(|(k, _)| *k == "scope") {
            f.1 = String::new();
        }
        fields.push(("user_scope", user_scope));
    }
    if id == "discord" {
        fields.push(("prompt", "none".into()));
    }
    web.session.update(|d| {
        d.extra.insert("oauth_state".into(), json!(state));
        match &verifier {
            Some(v) => d.extra.insert("oauth_code_verifier".into(), json!(v)),
            None => d.extra.remove("oauth_code_verifier"),
        };
    });
    format!("{base}?{}", build_query(&fields))
}

/// Socialite `User`: the fields the controller uses.
#[derive(Debug)]
struct OAuthUser {
    id: Value,
    nickname: Option<String>,
    email: Option<String>,
    /// Only drivers that map it (Authentik).
    email_verified: Option<Value>,
}

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(pelican_core::php::string(other)),
    })
}

fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, k| cur.get(k))
}

type Fail = String;

fn http() -> Result<reqwest::Client, Fail> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("GuzzleHttp/7")
        .build()
        .map_err(|e| e.to_string())
}

async fn json_of(resp: reqwest::Response) -> Result<Value, Fail> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "HTTP request returned status code {}: {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

/// `AbstractProvider::user()` for the OAuth 2 drivers.
async fn oauth2_user(web: &Web, id: &str, code: &str, verifier: Option<String>) -> Result<OAuthUser, Fail> {
    let st = &web.st;
    let env = &st.config.env;
    let (client_id, secret) = client(env, id);
    let http = http()?;
    let mut fields: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".into()),
        ("client_id", client_id.clone()),
        ("client_secret", secret.clone()),
        ("code", code.into()),
        ("redirect_uri", redirect_url(st, id)),
    ];
    if let Some(v) = verifier {
        fields.push(("code_verifier", v));
    }
    let mut req = http.post(token_url(env, id)).form(&fields);
    if id != "facebook" {
        req = req.header("Accept", "application/json");
    }
    if matches!(id, "bitbucket" | "x") {
        req = req.basic_auth(&client_id, Some(&secret));
    }
    let mut tokens = json_of(req.send().await.map_err(|e| e.to_string())?).await?;
    if id == "slack" {
        tokens = tokens.get("authed_user").cloned().unwrap_or(Value::Null);
    }
    let token = s(tokens.get("access_token")).ok_or("No access token was returned.")?;
    let bearer = format!("Bearer {token}");
    let user = match id {
        "github" => {
            let auth = format!("token {token}");
            let get_gh = |url: &'static str| {
                http.get(url)
                    .header("Accept", "application/vnd.github.v3+json")
                    .header("Authorization", auth.clone())
                    .send()
            };
            let u = json_of(get_gh("https://api.github.com/user").await.map_err(|e| e.to_string())?).await?;
            // getEmailByToken(): the primary verified address (failures leave it null).
            let email = match get_gh("https://api.github.com/user/emails").await {
                Ok(r) => json_of(r).await.ok().and_then(|list| {
                    list.as_array()?
                        .iter()
                        .find(|e| e.get("primary") == Some(&json!(true)) && e.get("verified") == Some(&json!(true)))
                        .and_then(|e| s(e.get("email")))
                }),
                Err(_) => None,
            };
            OAuthUser {
                id: u.get("id").cloned().unwrap_or(Value::Null),
                nickname: s(u.get("login")),
                email,
                email_verified: None,
            }
        }
        "gitlab" => {
            let u = json_of(
                http.get(format!("{}/api/v3/user", gitlab_host(env)))
                    .query(&[("access_token", token.as_str())])
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: u.get("id").cloned().unwrap_or(Value::Null),
                nickname: s(u.get("username")),
                email: s(u.get("email")),
                email_verified: None,
            }
        }
        "bitbucket" => {
            let u = json_of(
                http.get("https://api.bitbucket.org/2.0/user")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            let email = match http
                .get("https://api.bitbucket.org/2.0/user/emails")
                .header("Authorization", &bearer)
                .send()
                .await
            {
                Ok(r) => json_of(r).await.ok().and_then(|e| {
                    e.get("values")?
                        .as_array()?
                        .iter()
                        .find(|m| {
                            m.get("type") == Some(&json!("email"))
                                && m.get("is_primary") == Some(&json!(true))
                                && m.get("is_confirmed") == Some(&json!(true))
                        })
                        .and_then(|m| s(m.get("email")))
                }),
                Err(_) => None,
            };
            OAuthUser {
                id: u.get("uuid").cloned().unwrap_or(Value::Null),
                nickname: s(u.get("username")),
                email,
                email_verified: None,
            }
        }
        "google" => {
            let u = json_of(
                http.get("https://www.googleapis.com/oauth2/v3/userinfo")
                    .query(&[("prettyPrint", "false")])
                    .header("Accept", "application/json")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: u.get("sub").cloned().unwrap_or(Value::Null),
                nickname: s(u.get("nickname")),
                email: s(u.get("email")),
                email_verified: None,
            }
        }
        "facebook" => {
            use hmac::Mac;
            let mut q = vec![
                ("access_token", token.clone()),
                ("fields", "name,email,gender,verified,link,picture.width(1920)".to_string()),
            ];
            if !secret.is_empty() {
                let proof = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
                    .map(|mut m| {
                        m.update(token.as_bytes());
                        hex::encode(m.finalize().into_bytes())
                    })
                    .map_err(|e| e.to_string())?;
                q.push(("appsecret_proof", proof));
            }
            let u = json_of(
                http.get("https://graph.facebook.com/v23.0/me")
                    .query(&q)
                    .header("Accept", "application/json")
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: u.get("id").cloned().unwrap_or(Value::Null),
                nickname: None,
                email: s(u.get("email")),
                email_verified: None,
            }
        }
        "linkedin" => {
            let get_li = |url: &'static str, q: Vec<(&'static str, &'static str)>| {
                http.get(url)
                    .header("Authorization", bearer.clone())
                    .header("X-RestLi-Protocol-Version", "2.0.0")
                    .query(&q)
                    .send()
            };
            let profile = json_of(
                get_li(
                    "https://api.linkedin.com/v2/me",
                    vec![(
                        "projection",
                        "(id,firstName,lastName,profilePicture(displayImage~:playableStreams),vanityName)",
                    )],
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .await?;
            let email = json_of(
                get_li(
                    "https://api.linkedin.com/v2/emailAddress",
                    vec![("q", "members"), ("projection", "(elements*(handle~))")],
                )
                .await
                .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: profile.get("id").cloned().unwrap_or(Value::Null),
                nickname: None,
                email: s(get(&email, "elements")
                    .and_then(|e| e.get(0))
                    .and_then(|e| e.get("handle~"))
                    .and_then(|h| h.get("emailAddress"))),
                email_verified: None,
            }
        }
        "slack" => {
            let u = json_of(
                http.get("https://slack.com/api/users.identity")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: get(&u, "user.id").cloned().unwrap_or(Value::Null),
                nickname: None,
                email: s(get(&u, "user.email")),
                email_verified: None,
            }
        }
        "x" => {
            let u = json_of(
                http.get("https://api.x.com/2/users/me")
                    .query(&[("user.fields", "profile_image_url,confirmed_email")])
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            let d = u.get("data").cloned().unwrap_or(Value::Null);
            OAuthUser {
                id: d.get("id").cloned().unwrap_or(Value::Null),
                nickname: s(d.get("username")),
                email: s(d.get("confirmed_email")),
                email_verified: None,
            }
        }
        "authentik" => {
            let u = json_of(
                http.get(format!("{}/application/o/userinfo/", authentik_base(env)))
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            OAuthUser {
                id: u.get("sub").cloned().unwrap_or(Value::Null),
                nickname: s(u.get("nickname")),
                email: s(u.get("email")),
                email_verified: Some(u.get("email_verified").cloned().unwrap_or(Value::Null)),
            }
        }
        _ => {
            let u = json_of(
                http.get("https://discord.com/api/users/@me")
                    .header("Authorization", &bearer)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?,
            )
            .await?;
            let username = s(u.get("username")).unwrap_or_default();
            let disc = s(u.get("discriminator")).unwrap_or_default();
            OAuthUser {
                id: u.get("id").cloned().unwrap_or(Value::Null),
                nickname: Some(if disc != "0" { format!("{username}#{disc}") } else { username }),
                email: s(u.get("email")),
                email_verified: None,
            }
        }
    };
    Ok(user)
}

/// The Steam provider: OpenID 2.0 `check_authentication`, then the player summary.
async fn steam_user(web: &Web, params: &[(String, String)]) -> Result<OAuthUser, Fail> {
    let p = |k: &str| {
        let alt = k.replacen("openid.", "openid_", 1);
        params.iter().find(|(n, _)| n == k || *n == alt).map(|(_, v)| v.clone())
    };
    if p("openid.assoc_handle").is_none()
        || p("openid.signed").is_none()
        || p("openid.sig").is_none()
        || p("openid.return_to").is_none()
    {
        return Err("A critical openid parameter is missing from the request".into());
    }
    let allowed = web.st.config.app_url.replace("http://", "").replace("https://", "");
    let return_host =
        p("openid.return_to").and_then(|u| reqwest::Url::parse(&u).ok()).and_then(|u| u.host_str().map(String::from));
    if return_host.as_deref() != Some(allowed.trim_end_matches('/')) {
        return Err("Invalid return_to host".into());
    }
    let mut form = vec![
        ("openid.assoc_handle".to_string(), p("openid.assoc_handle").unwrap_or_default()),
        ("openid.signed".into(), p("openid.signed").unwrap_or_default()),
        ("openid.sig".into(), p("openid.sig").unwrap_or_default()),
        ("openid.ns".into(), "http://specs.openid.net/auth/2.0".into()),
        ("openid.mode".into(), "check_authentication".into()),
    ];
    if let Some(e) = p("openid.error") {
        form.push(("openid.error".into(), e));
    }
    for item in p("openid.signed").unwrap_or_default().split(',') {
        form.push((format!("openid.{item}"), p(&format!("openid.{item}")).unwrap_or_default()));
    }
    let http = http()?;
    let body = http
        .post("https://steamcommunity.com/openid/login")
        .header("referer", "https://steamcommunity.com/")
        .header("origin", "https://steamcommunity.com")
        .form(&form)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let valid = body.lines().any(|l| l.trim() == "is_valid:true");
    if !valid {
        return Err(format!(
            "Failed to validate OpenID login: {}",
            p("openid.error").unwrap_or_else(|| "unknown error".into())
        ));
    }
    let claimed = p("openid.claimed_id").unwrap_or_default();
    let steam_id = claimed
        .strip_prefix("https://steamcommunity.com/openid/id/")
        .or_else(|| claimed.strip_prefix("http://steamcommunity.com/openid/id/"))
        .filter(|id| (17..=25).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or("0")
        .to_string();
    let key = web.st.config.env.string_or("OAUTH_STEAM_CLIENT_SECRET", "");
    if key.is_empty() {
        return Err("The Steam API key has not been specified.".into());
    }
    let info = json_of(
        http.get(format!(
            "http://api.steampowered.com/ISteamUser/GetPlayerSummaries/v0002/?key={key}&steamids={steam_id}"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?,
    )
    .await?;
    let player = get(&info, "response.players").and_then(|p| p.get(0)).cloned().unwrap_or(Value::Null);
    Ok(OAuthUser {
        id: player.get("steamid").cloned().unwrap_or(Value::Null),
        nickname: s(player.get("personaname")),
        email: None,
        email_verified: None,
    })
}

/// `GET /auth/oauth/redirect/{driver}` (guest route).
pub async fn redirect(web: Web, Path(driver): Path<String>, req: axum::extract::Request) -> WebResult {
    if plugin_driver(&driver) && web.user.is_none() {
        return Ok(crate::sidecar_proxy::proxy(&web, req, None).await);
    }
    let Some(id) = find_id(&driver).filter(|id| is_enabled(&web.st.config.env, id)) else {
        return Ok(web.redirect("/login"));
    };
    if web.user.is_some() {
        return Ok(web.redirect("/"));
    }
    Ok(external(&authorization_url(&web, id)))
}

/// `POST /profile/oauth/{driver}/link`: the profile tab's link action.
pub async fn link(web: Web, Path(driver): Path<String>, req: axum::extract::Request) -> WebResult {
    if web.user.is_none() {
        return Ok(web.redirect("/login"));
    }
    if plugin_driver(&driver) {
        // The original links from the profile through Socialite's redirect; its callback
        // links to the signed-in user (the bridge passes ours along).
        return Ok(crate::sidecar_proxy::proxy(&web, req, Some(&format!("/auth/oauth/redirect/{driver}"))).await);
    }
    let Some(id) = find_id(&driver).filter(|id| is_enabled(&web.st.config.env, id)) else {
        return Err(WebError::NotFound);
    };
    Ok(external(&authorization_url(&web, id)))
}

/// A redirect leaving the panel (full navigation, also from htmx).
fn external(url: &str) -> Response {
    let mut resp = axum::response::Redirect::to(url).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(url) {
        resp.headers_mut().insert("HX-Redirect", v);
    }
    resp
}

use axum::response::IntoResponse;

fn error_redirect(web: &Web, error: Option<&str>) -> Response {
    // Persistent danger notification (`errorRedirect()`).
    web.session.flash(
        "danger",
        if error.is_some() { "Something went wrong" } else { "No linked User found" },
        error.map(String::from),
    );
    web.redirect("/login")
}

/// `GET /auth/oauth/callback/{driver}` (`OAuthController::callback`).
pub async fn callback(web: Web, Path(driver): Path<String>, req: axum::extract::Request) -> WebResult {
    if plugin_driver(&driver) {
        return Ok(crate::sidecar_proxy::proxy(&web, req, None).await);
    }
    let st = web.st.clone();
    let Some(id) = find_id(&driver).filter(|id| is_enabled(&st.config.env, id)) else {
        return Ok(web.redirect("/login"));
    };
    let params: Vec<(String, String)> =
        web.query.as_deref().map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect()).unwrap_or_default();
    let param = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    if let Some(error) = param("error").filter(|e| !e.is_empty()) {
        tracing::warn!(driver = id, error = %param("error_description").unwrap_or_else(|| error.clone()), "oauth error");
        return Ok(error_redirect(&web, Some(&error)));
    }
    let result = if id == "steam" {
        steam_user(&web, &params).await
    } else {
        // hasInvalidState(): the session state is pulled and must match.
        let data = web.session.data();
        let expected = data.extra.get("oauth_state").and_then(Value::as_str).map(String::from);
        let verifier = data.extra.get("oauth_code_verifier").and_then(Value::as_str).map(String::from);
        web.session.update(|d| {
            d.extra.remove("oauth_state");
            d.extra.remove("oauth_code_verifier");
        });
        let state_ok = expected.is_some_and(|e| !e.is_empty() && Some(e) == param("state"));
        if !state_ok {
            return Err(WebError::internal("InvalidStateException"));
        }
        oauth2_user(&web, id, &param("code").unwrap_or_default(), verifier).await
    };
    let oauth_user = match result {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(driver = id, error = %e, "oauth user lookup failed");
            return Err(WebError::internal(e));
        }
    };
    // Signed-in users link the account from their profile.
    if let Some(user) = &web.user {
        link_user(&st, &user.user, id, &oauth_user.id).await?;
        return Ok(web.redirect("/profile?tab=oauth"));
    }
    if let Some(user) = find_linked(&st, id, &oauth_user.id).await? {
        return login_user(&web, user).await;
    }
    handle_missing_user(&web, id, oauth_user).await
}

/// `User::whereJsonContains("oauth->$driver", $id)->first()`.
async fn find_linked(st: &AppState, id: &str, oauth_id: &Value) -> Result<Option<users::Model>, WebError> {
    let candidates = users::Entity::find().filter(users::Column::Oauth.is_not_null()).all(&st.db).await?;
    Ok(candidates.into_iter().find(|u| {
        u.oauth
            .as_ref()
            .and_then(|j| j.0.get(id))
            .is_some_and(|v| v == oauth_id || v.as_array().is_some_and(|a| a.contains(oauth_id)))
    }))
}

/// `OAuthService::linkUser()`.
pub async fn link_user(st: &AppState, user: &users::Model, id: &str, oauth_id: &Value) -> Result<(), WebError> {
    let prev = user.oauth.as_ref().map(|j| j.0.clone());
    let mut oauth = match user.oauth.as_ref().map(|j| j.0.clone()) {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    oauth.insert(id.into(), oauth_id.clone());
    // `$user->update(['oauth' => $oauth])`: Eloquent fires only when the value changed.
    let dirty = prev.as_ref() != Some(&json!(oauth));
    set_oauth(st, user.id.0, Value::Object(oauth)).await?;
    if dirty {
        model_events::fire_id::<users::Entity>(st, model_events::Action::Updated, user.id.0).await;
    }
    Ok(())
}

/// `OAuthService::unlinkUser()` / the profile's unlink action.
pub async fn unlink_user(st: &AppState, user: &users::Model, id: &str) -> Result<bool, WebError> {
    let Some(Value::Object(mut oauth)) = user.oauth.as_ref().map(|j| j.0.clone()) else { return Ok(false) };
    if oauth.remove(id).is_none() {
        return Ok(false);
    }
    // `$user->update(['oauth' => $oauth])` — removing the key always changes it.
    set_oauth(st, user.id.0, Value::Object(oauth)).await?;
    model_events::fire_id::<users::Entity>(st, model_events::Action::Updated, user.id.0).await;
    Ok(true)
}

async fn set_oauth(st: &AppState, user_id: i64, oauth: Value) -> Result<(), WebError> {
    use sea_orm::ActiveValue::{Set, Unchanged};
    pelican_db::write::update(
        &st.db,
        &users::ActiveModel {
            id: Unchanged(pelican_db::Int(user_id)),
            oauth: Set(Some(Json(oauth))),
            updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

async fn handle_missing_user(web: &Web, id: &'static str, oauth_user: OAuthUser) -> WebResult {
    let st = &web.st;
    let env = &st.config.env;
    let Some(email) = oauth_user.email.clone().filter(|e| !e.is_empty()) else {
        return Ok(error_redirect(web, Some("No email was linked to your account on the OAuth provider.")));
    };
    if let Some(v) = &oauth_user.email_verified {
        // isset() + FILTER_VALIDATE_BOOLEAN
        let verified = match v {
            Value::Null => true,
            Value::Bool(b) => *b,
            other => matches!(pelican_core::php::string(other).to_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        };
        if !verified {
            return Ok(error_redirect(web, Some("Email not verified on OAuth provider.")));
        }
    }
    let u = upper(id);
    let existing = users::Entity::find().filter(users::Column::Email.eq(email.clone())).one(&st.db).await?;
    let user = match existing {
        Some(user) => {
            if !env.bool_or(&format!("OAUTH_{u}_SHOULD_LINK_MISSING_USERS"), false) {
                return Ok(error_redirect(web, None));
            }
            link_user(st, &user, id, &oauth_user.id).await?;
            user
        }
        None => {
            if !env.bool_or(&format!("OAUTH_{u}_SHOULD_CREATE_MISSING_USERS"), false) {
                return Ok(error_redirect(web, None));
            }
            let mut oauth = serde_json::Map::new();
            oauth.insert(id.into(), oauth_user.id.clone());
            let oauth = Value::Object(oauth);
            match pelican_api::application::users::create_user(
                st,
                &oauth_user.nickname.clone().unwrap_or_default(),
                &email,
                oauth,
            )
            .await
            {
                Ok(user) => user,
                Err(e) => {
                    tracing::warn!(driver = id, error = ?e.errors.first().map(|o| &o.detail), "oauth user creation failed");
                    return Ok(error_redirect(web, None));
                }
            }
        }
    };
    login_user(web, user).await
}

/// `auth()->login($user, true)` (Login event → `auth:success`), no MFA challenge.
async fn login_user(web: &Web, user: users::Model) -> WebResult {
    web.session.login(user.id.0, true);
    let _ =
        web.activity("auth:success").actor(morph::USER, user.id.0).subject(morph::USER, user.id.0).log(&web.st).await;
    Ok(web.redirect("/"))
}
