//! Mock Wings: records every request a panel sends to the node and answers
//! with configured responses. Reachable as `wings.test:8080` on the difftest
//! network (DNS alias of the harness container).

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::scenario::WingsResponse;

pub const PORT: u16 = 8080;

/// Wings' User-Agent: the panel treats non-2xx responses as RequestException
/// only when the token id in it matches the node (models-services.md §0.10).
pub const DEFAULT_TOKEN_ID: &str = "difftestTokenId0";

#[derive(Debug, Clone)]
pub struct WingsCall {
    pub method: String,
    pub path_and_query: String,
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub accept: Option<String>,
    pub body: Bytes,
}

#[derive(Default)]
struct Inner {
    calls: Vec<WingsCall>,
    responses: Vec<WingsResponse>,
}

#[derive(Clone, Default)]
pub struct MockWings {
    inner: Arc<Mutex<Inner>>,
}

impl MockWings {
    pub async fn start() -> anyhow::Result<MockWings> {
        let me = MockWings::default();
        let app = axum::Router::new().fallback(handle).with_state(me.clone());
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", PORT)).await?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(me)
    }

    /// Reset recorded calls and install the responses for the next run.
    pub fn reset(&self, responses: Vec<WingsResponse>) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.calls.clear();
        g.responses = responses;
    }

    pub fn take_calls(&self) -> Vec<WingsCall> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut g.calls)
    }
}

async fn handle(State(w): State<MockWings>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let path = uri.path().to_string();
    let call = WingsCall {
        method: method.to_string(),
        path_and_query: uri.path_and_query().map(|p| p.to_string()).unwrap_or_else(|| path.clone()),
        authorization: h("authorization"),
        content_type: h("content-type"),
        accept: h("accept"),
        body,
    };
    let resp = {
        let mut g = w.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.calls.push(call);
        g.responses
            .iter()
            .find(|r| {
                r.method.as_deref().is_none_or(|m| m.eq_ignore_ascii_case(method.as_str()))
                    && path_matches(&r.path, &path)
            })
            .cloned()
    };
    let ua = format!("Pelican Wings/v1.0.0 (id:{DEFAULT_TOKEN_ID})");
    match resp {
        None => (StatusCode::NO_CONTENT, [("user-agent", ua)]).into_response(),
        Some(r) => {
            let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let (ctype, body) = match (&r.json, &r.body) {
                (Some(j), _) => ("application/json", serde_json::to_vec(j).unwrap_or_default()),
                (None, Some(b)) => ("text/plain", b.clone().into_bytes()),
                (None, None) => ("text/plain", Vec::new()),
            };
            let mut builder = Response::builder().status(status).header("content-type", ctype).header("user-agent", ua);
            for (k, v) in &r.headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            builder
                .body(axum::body::Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

/// `*` matches one segment, `**` matches the remainder.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let s: Vec<&str> = path.trim_matches('/').split('/').collect();
    for (i, seg) in p.iter().enumerate() {
        if *seg == "**" {
            return true;
        }
        match s.get(i) {
            Some(x) if *seg == "*" || seg == x => {}
            _ => return false,
        }
    }
    p.len() == s.len()
}
