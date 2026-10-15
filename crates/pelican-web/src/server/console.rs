//! Console page (app/Filament/Server/Pages/Console.php + Widgets/ServerConsole,
//! ServerOverview, charts): the browser talks to Wings over its websocket.

use axum::Json;
use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use minijinja::context;
use pelican_core::state::AppState;
use serde_json::json;

use super::ServerPanel;
use crate::error::{WebError, WebResult};
use crate::server_info::{self, ContainerStatus};

pub fn routes() -> Router<AppState> {
    Router::new().route("/server/{server}", get(page)).route("/server/{server}/console/websocket", get(websocket))
}

fn limit_text(amount_mib: i64, binary: bool) -> String {
    if amount_mib > 0 {
        format!(" / {}", crate::views::bytes((server_info::limit_bytes(amount_mib, binary)) as i64, binary))
    } else {
        " / \u{221E}".into()
    }
}

/// `GET /server/{server}`.
async fn page(p: ServerPanel) -> WebResult {
    let st = &p.st;
    let locale = p.locale();
    let details = server_info::details(st, &p.server, &p.node).await;
    let status = server_info::status(&details);
    let (condition, _, _) = server_info::condition(&p.server, &details, &locale);
    let binary = st.config.use_binary_prefix;
    let address = p.allocation().await?.map(|a| crate::models::display_address(&a)).unwrap_or_else(|| "None".into());
    let custom = crate::models::customization(st, &p.user.user);
    let statuses: serde_json::Map<String, serde_json::Value> = [
        "created",
        "starting",
        "running",
        "restarting",
        "exited",
        "paused",
        "dead",
        "removing",
        "stopping",
        "offline",
        "missing",
    ]
    .iter()
    .filter_map(|s| ContainerStatus::parse(s))
    .map(|c| (c.value().to_string(), json!(c.label(&locale))))
    .collect();
    let font = custom.get("console_font").and_then(|v| v.as_str()).unwrap_or("monospace").to_string();
    let prelude = st.config.app_name.to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-");
    let cfg = json!({
        "uuid": p.server.uuid,
        "websocket": format!("{}/console/websocket", p.base_url()),
        "name": p.server.name,
        "prelude": prelude,
        "font": font,
        "fontSize": custom.get("console_font_size").and_then(|v| v.as_i64()).unwrap_or(14),
        "rows": custom.get("console_rows").and_then(|v| v.as_i64()).unwrap_or(30),
        "period": custom.get("console_graph_period").and_then(|v| v.as_i64()).unwrap_or(30),
        "binaryPrefix": binary,
        "status": status.value(),
        "statusIsLive": p.server.status.is_none(),
        "conflict": p.conflict(),
        "canCommand": p.can("control.console"),
        "statusLabels": statuses,
        "offlineLabel": ContainerStatus::Offline.label(&locale),
        "unknown": "\u{2014}",
        "strings": {
            "command": p.t("server/console.command"),
            "commandBlocked": p.t("server/console.command_blocked"),
            "commandBlockedTitle": p.t("server/console.command_blocked_title"),
            "wsErrorTitle": p.t("server/console.websocket_error.title"),
            "wsErrorBody": p.t("server/console.websocket_error.body"),
        },
    });
    let power = |perm: &str| p.can(perm);
    // PHP plugins' console actions (ADR-0020): links to the bridge's host page.
    let sidecar = pelican_api::sidecar::get(&p.st);
    let plugin_actions: Vec<_> = pelican_api::sidecar::console_actions(&p.st, p.user.id(), &p.server.uuid_short)
        .await
        .into_iter()
        .map(|a| {
            // Signed for this user and server: the action runs on the first click.
            let run = sidecar
                .as_ref()
                .map(|sc| format!("&rs_run={}", sc.console_run_token(p.user.id(), &p.server.uuid_short, &a.name)))
                .unwrap_or_default();
            context! {
                url => format!("{}/rs-console-action?action={}{run}", p.base_url(), super::rawurlencode(&a.name)),
                label => a.label,
                icon => crate::sidecar_proxy::icon(a.icon.as_deref()),
            }
        })
        .collect();
    p.page(
        "",
        "server/console.html",
        context! {
            title => p.t("server/console.title"),
            cfg => cfg.to_string(),
            condition => condition,
            address => address,
            cpu_limit => if p.server.cpu.0 > 0 { format!(" / {} %", p.server.cpu.0) } else { " / \u{221E}".into() },
            memory_limit => limit_text(p.server.memory.0, binary),
            disk_limit => limit_text(p.server.disk.0, binary),
            can => context! {
                start => power("control.start"),
                restart => power("control.restart"),
                stop => power("control.stop"),
                command => p.can("control.console"),
            },
            font => font,
            plugin_actions => plugin_actions,
        },
    )
}

/// `GET /server/{server}/console/websocket`: `ServerConsole::getToken()` + `getSocket()`.
async fn websocket(p: ServerPanel) -> Result<Response, WebError> {
    if !p.can("websocket.connect") {
        return Err(WebError::Forbidden);
    }
    let st = &p.st;
    let conn = st.node_conn(&p.node).map_err(WebError::internal)?;
    let now = chrono::Utc::now().timestamp();
    let permissions = pelican_core::permissions::user_permissions(&st.db, &p.server, &p.user).await?;
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: conn.connection_address(),
        identified_by: format!("{}{}", p.user.id(), p.server.uuid),
        issued_at: now,
        expires_at: now + 600,
        subject: None,
        claims: vec![("server_uuid".into(), json!(p.server.uuid)), ("permissions".into(), permissions)],
        user_uuid: Some(p.user.user.uuid.clone()),
        scope: "websocket".into(),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&conn.token)
    .map_err(WebError::internal)?;
    let socket = conn.connection_address().replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    Ok(Json(json!({ "token": token, "socket": format!("{socket}/api/servers/{}/ws", p.server.uuid) })).into_response())
}
