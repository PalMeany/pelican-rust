//! `Http::daemon($node)` + `DaemonRepository::getHttpClient()` semantics.

use std::sync::LazyLock;
use std::time::Duration;

pub use reqwest::Method;
use serde_json::Value;

/// Connection data of a node (token already decrypted).
#[derive(Clone, Debug)]
pub struct Node {
    pub scheme: String,
    pub fqdn: String,
    pub daemon_connect: i64,
    pub token_id: String,
    pub token: String,
}

impl Node {
    /// `Node::getConnectionAddress()`.
    pub fn connection_address(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.fqdn, self.daemon_connect)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WingsError {
    /// `Illuminate\Http\Client\ConnectionException` (transport error or bad UA).
    #[error("connection: {0}")]
    Connection(String),
    /// `Illuminate\Http\Client\RequestException` (non-2xx, code = status).
    #[error("wings returned HTTP {status}")]
    Request { status: u16, body: String },
}

impl WingsError {
    /// Class basename the original reports in `errors[0].code`.
    pub fn code(&self) -> &'static str {
        match self {
            WingsError::Connection(_) => "ConnectionException",
            WingsError::Request { .. } => "RequestException",
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            WingsError::Request { status, .. } => Some(*status),
            WingsError::Connection(_) => None,
        }
    }
}

/// Request body as the Laravel HTTP client sends it.
pub enum Body {
    /// `delete($url)` / `get()` — no body.
    None,
    /// `post($url, $data)` — `json_encode($data)` (flags 0); empty data → `[]`.
    Json(Value),
    /// `withBody($raw)`.
    Raw(Vec<u8>),
}

#[derive(Clone)]
pub struct WingsClient {
    verify_tls: bool,
    timeout: Duration,
    connect_timeout: Duration,
}

static UA_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^Pelican Wings/v(?:\d+\.\d+\.\d+|develop) \(id:(\w*)\)$").expect("valid regex")
});

pub struct Call<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: &'a [(&'a str, String)],
    pub body: Body,
    pub timeout: Option<Duration>,
    pub connect_timeout: Option<Duration>,
    /// `getHttpClient()` with `throwIf(enforceValidNodeToken)`; false = bare macro.
    pub throw: bool,
}

impl<'a> Call<'a> {
    pub fn new(method: Method, path: &'a str) -> Self {
        Call { method, path, query: &[], body: Body::None, timeout: None, connect_timeout: None, throw: true }
    }

    pub fn json(mut self, v: Value) -> Self {
        self.body = Body::Json(v);
        self
    }

    pub fn query(mut self, q: &'a [(&'a str, String)]) -> Self {
        self.query = q;
        self
    }
}

pub struct WingsResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_length: Option<u64>,
}

impl WingsResponse {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

impl WingsClient {
    /// `verify_tls` = `APP_ENV == production`; timeouts from GUZZLE_* settings.
    pub fn new(verify_tls: bool, timeout_secs: u64, connect_timeout_secs: u64) -> Self {
        WingsClient {
            verify_tls,
            timeout: Duration::from_secs(timeout_secs),
            connect_timeout: Duration::from_secs(connect_timeout_secs),
        }
    }

    fn http(&self, timeout: Duration, connect: Duration) -> Result<reqwest::Client, WingsError> {
        reqwest::Client::builder()
            .danger_accept_invalid_certs(!self.verify_tls)
            .timeout(timeout)
            .connect_timeout(connect)
            .user_agent("GuzzleHttp/8")
            .build()
            .map_err(|e| WingsError::Connection(e.to_string()))
    }

    pub async fn send(&self, node: &Node, call: Call<'_>) -> Result<WingsResponse, WingsError> {
        let url = format!("{}/{}", node.connection_address().trim_end_matches('/'), call.path.trim_start_matches('/'));
        let client =
            self.http(call.timeout.unwrap_or(self.timeout), call.connect_timeout.unwrap_or(self.connect_timeout))?;
        let mut req = client
            .request(call.method.clone(), &url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", node.token));
        if !call.query.is_empty() {
            req = req.query(call.query);
        }
        req = match call.body {
            Body::None => req,
            Body::Json(v) => {
                let v = match v {
                    Value::Null => Value::Array(vec![]),
                    Value::Object(m) if m.is_empty() => Value::Array(vec![]),
                    other => other,
                };
                req.body(laravel_compat::json::to_string(&v).map_err(|e| WingsError::Connection(e.to_string()))?)
            }
            Body::Raw(b) => req.body(b),
        };
        let resp = req.send().await.map_err(|e| WingsError::Connection(format!("cURL error: {e}")))?;
        let status = resp.status().as_u16();
        let ua = resp.headers().get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let content_length = resp.content_length();
        let body = resp.bytes().await.map_err(|e| WingsError::Connection(e.to_string()))?.to_vec();
        if call.throw && !(200..300).contains(&status) {
            // enforceValidNodeToken (DaemonRepository.php:55-67)
            let bad_token = ua.is_empty() || UA_RE.captures(&ua).is_some_and(|c| &c[1] != node.token_id);
            if bad_token {
                return Err(WingsError::Connection(format!("{url} does not match node token_id !")));
            }
            return Err(WingsError::Request { status, body: String::from_utf8_lossy(&body).into_owned() });
        }
        Ok(WingsResponse { status, body, content_length })
    }
}

/// Repository calls used by the panel (remote-api.md §4.4).
impl WingsClient {
    /// S2 `POST /api/servers`.
    pub async fn create_server(&self, node: &Node, uuid: &str, start_on_completion: bool) -> Result<(), WingsError> {
        let body = serde_json::json!({"uuid": uuid, "start_on_completion": start_on_completion});
        self.send(node, Call::new(Method::POST, "/api/servers").json(body)).await.map(|_| ())
    }

    /// S3 `POST /api/servers/{uuid}/sync`.
    pub async fn sync_server(&self, node: &Node, uuid: &str) -> Result<(), WingsError> {
        self.send(node, Call::new(Method::POST, &format!("/api/servers/{uuid}/sync")).json(Value::Null))
            .await
            .map(|_| ())
    }

    /// S4 `DELETE /api/servers/{uuid}`.
    pub async fn delete_server(&self, node: &Node, uuid: &str) -> Result<(), WingsError> {
        self.send(node, Call::new(Method::DELETE, &format!("/api/servers/{uuid}"))).await.map(|_| ())
    }

    /// S5 `POST /api/servers/{uuid}/reinstall`.
    pub async fn reinstall_server(&self, node: &Node, uuid: &str) -> Result<(), WingsError> {
        self.send(node, Call::new(Method::POST, &format!("/api/servers/{uuid}/reinstall")).json(Value::Null))
            .await
            .map(|_| ())
    }

    /// S9 `POST /api/deauthorize-user`.
    pub async fn deauthorize_user(&self, node: &Node, user_uuid: &str, servers: &[String]) -> Result<(), WingsError> {
        let body = serde_json::json!({"user": user_uuid, "servers": servers});
        self.send(node, Call::new(Method::POST, "/api/deauthorize-user").json(body)).await.map(|_| ())
    }

    /// S11 `POST /api/servers/{uuid}/power`.
    pub async fn power(&self, node: &Node, uuid: &str, action: &str) -> Result<(), WingsError> {
        let body = serde_json::json!({"action": action});
        self.send(node, Call::new(Method::POST, &format!("/api/servers/{uuid}/power")).json(body)).await.map(|_| ())
    }

    /// S12 `POST /api/servers/{uuid}/commands` (bare macro: non-2xx ignored).
    pub async fn send_commands(&self, node: &Node, uuid: &str, commands: &[String]) -> Result<(), WingsError> {
        let path = format!("/api/servers/{uuid}/commands");
        let mut call = Call::new(Method::POST, &path).json(serde_json::json!({"commands": commands}));
        call.throw = false;
        self.send(node, call).await.map(|_| ())
    }
}

/// Backup repository calls (B1–B3).
impl WingsClient {
    /// B1 `POST /api/servers/{uuid}/backup`.
    pub async fn create_backup(
        &self,
        node: &Node,
        server: &str,
        adapter: &str,
        backup: &str,
        ignore: &str,
    ) -> Result<(), WingsError> {
        let path = format!("/api/servers/{server}/backup");
        let body = serde_json::json!({"adapter": adapter, "uuid": backup, "ignore": ignore});
        self.send(node, Call::new(Method::POST, &path).json(body)).await.map(|_| ())
    }

    /// B2 `POST /api/servers/{uuid}/backup/{backup}/restore`.
    pub async fn restore_backup(
        &self,
        node: &Node,
        server: &str,
        backup: &str,
        adapter: &str,
        truncate: bool,
        url: &str,
    ) -> Result<(), WingsError> {
        let path = format!("/api/servers/{server}/backup/{backup}/restore");
        let body = serde_json::json!({"adapter": adapter, "truncate_directory": truncate, "download_url": url});
        self.send(node, Call::new(Method::POST, &path).json(body)).await.map(|_| ())
    }

    /// B3 `DELETE /api/servers/{uuid}/backup/{backup}`.
    pub async fn delete_backup(&self, node: &Node, server: &str, backup: &str) -> Result<(), WingsError> {
        let path = format!("/api/servers/{server}/backup/{backup}");
        self.send(node, Call::new(Method::DELETE, &path)).await.map(|_| ())
    }
}
