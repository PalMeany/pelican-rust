//! Update feeds (`Plugin::getUpdateData/getUpdateEntry/isUpdateAvailable/getUpdateDownload`)
//! and `HubCredentials` (the Hub key goes only to the Hub's https origin, no redirects).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{Config, Plugin};

/// `cache()->remember("plugins.<id>.update", 10 minutes)`.
static FEEDS: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);
const FEED_TTL: Duration = Duration::from_secs(600);

/// Lower-cased `host:port` of an https URL (`HubCredentials::origin`).
fn origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| {
        let (scheme, rest) = url.split_once("://")?;
        scheme.eq_ignore_ascii_case("https").then_some(rest)
    })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (h, rest) = v6.split_once(']')?;
            (format!("[{h}]"), rest.strip_prefix(':').unwrap_or("443").to_string())
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => (authority.to_string(), "443".to_string()),
        },
    };
    (!host.is_empty()).then(|| format!("{}:{port}", host.to_lowercase()))
}

/// `HubCredentials::request($url)`: a client, with `X-Panel-Api-Key` and redirects off
/// when the URL is the configured Hub.
pub fn client(cfg: &Config, url: &str, timeout: Duration, connect: Duration) -> reqwest::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().timeout(timeout).connect_timeout(connect).user_agent("GuzzleHttp/7");
    let hub =
        cfg.hub_api_key.as_deref().filter(|_| origin(&cfg.hub_url).is_some() && origin(url) == origin(&cfg.hub_url));
    if let Some(key) = hub {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(key) {
            headers.insert("X-Panel-Api-Key", v);
        }
        b = b.default_headers(headers).redirect(reqwest::redirect::Policy::none());
    }
    b.build()
}

/// The feed for this plugin (`[]` on errors, cached like the original).
async fn feed(cfg: &Config, p: &Plugin) -> Value {
    let Some(url) = p.update_url.as_deref().filter(|u| !u.is_empty()) else { return Value::Null };
    if let Some((at, v)) = FEEDS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&p.id).cloned())) {
        if at.elapsed() < FEED_TTL {
            return v;
        }
    }
    let fetched = async {
        let resp = client(cfg, url, Duration::from_secs(5), Duration::from_secs(1))?.get(url).send().await?;
        resp.error_for_status()?.json::<Value>().await
    }
    .await;
    let mut data = match fetched {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(plugin = %p.id, error = %e, "fetching the plugin update feed failed");
            Value::Array(vec![])
        }
    };
    // Feeds that cover several plugins are keyed by plugin id.
    if let Some(own) = data.get(&p.id).cloned() {
        data = own;
    }
    if let Ok(mut g) = FEEDS.lock() {
        g.get_or_insert_with(HashMap::new).insert(p.id.clone(), (Instant::now(), data.clone()));
    }
    data
}

pub fn forget(id: &str) {
    if let Ok(mut g) = FEEDS.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(id);
        }
    }
}

pub fn forget_all() {
    if let Ok(mut g) = FEEDS.lock() {
        *g = None;
    }
}

/// `getUpdateEntry()`: the entry for the current panel version, else `*`.
async fn entry(cfg: &Config, p: &Plugin) -> Option<Value> {
    let version = cfg.panel_version.as_deref()?;
    let data = feed(cfg, p).await;
    data.get(version).or_else(|| data.get("*")).cloned()
}

/// `isUpdateAvailable()`.
pub async fn update_available(cfg: &Config, p: &Plugin) -> bool {
    let Some(e) = entry(cfg, p).await else { return false };
    let latest = e.get("version").map(pelican_core::php::string).unwrap_or_default();
    pelican_core::php::version_compare(&latest, p.version.as_deref().unwrap_or("")) == std::cmp::Ordering::Greater
}

/// `getUpdateDownload()`: `(url, sha256)` from the same entry.
pub async fn update_download(cfg: &Config, p: &Plugin) -> Option<(String, Option<String>)> {
    let e = entry(cfg, p).await?;
    let url = e.get("download_url").map(pelican_core::php::string).filter(|u| !u.trim().is_empty())?;
    let sha = e.get("sha256").map(pelican_core::php::string);
    Some((url, sha))
}

#[cfg(test)]
mod tests {
    use super::origin;

    #[test]
    fn hub_origin() {
        assert_eq!(origin("https://Hub.Pelican.dev/api/x").as_deref(), Some("hub.pelican.dev:443"));
        assert_eq!(origin("https://hub.pelican.dev:8443").as_deref(), Some("hub.pelican.dev:8443"));
        assert_eq!(origin("http://hub.pelican.dev"), None);
    }
}
