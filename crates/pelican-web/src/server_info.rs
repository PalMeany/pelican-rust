//! Live server state for the UI: `ContainerStatus` / `ServerState` enums,
//! cached Wings details and resource formatting (app/Enums/*, Server.php).

use std::time::Duration;

use pelican_core::state::AppState;
use pelican_db::entities::{nodes, servers};
use serde_json::{Value, json};

use crate::i18n;

/// app/Enums/ContainerStatus.php.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerStatus {
    Created,
    Starting,
    Running,
    Restarting,
    Exited,
    Paused,
    Dead,
    Removing,
    Stopping,
    Offline,
    Missing,
}

impl ContainerStatus {
    pub fn parse(s: &str) -> Option<ContainerStatus> {
        use ContainerStatus::*;
        Some(match s {
            "created" => Created,
            "starting" => Starting,
            "running" => Running,
            "restarting" => Restarting,
            "exited" => Exited,
            "paused" => Paused,
            "dead" => Dead,
            "removing" => Removing,
            "stopping" => Stopping,
            "offline" => Offline,
            "missing" => Missing,
            _ => return None,
        })
    }

    pub fn value(self) -> &'static str {
        use ContainerStatus::*;
        match self {
            Created => "created",
            Starting => "starting",
            Running => "running",
            Restarting => "restarting",
            Exited => "exited",
            Paused => "paused",
            Dead => "dead",
            Removing => "removing",
            Stopping => "stopping",
            Offline => "offline",
            Missing => "missing",
        }
    }

    pub fn icon(self) -> &'static str {
        use ContainerStatus::*;
        match self {
            Created => "heart-plus",
            Starting => "heart-up",
            Running => "heartbeat",
            Restarting => "heart-bolt",
            Exited => "heart-exclamation",
            Paused => "heart-pause",
            Dead | Offline => "heart-x",
            Removing => "heart-down",
            Missing => "heart-search",
            Stopping => "heart-minus",
        }
    }

    pub fn color(self) -> &'static str {
        use ContainerStatus::*;
        match self {
            Created => "primary",
            Starting | Paused | Removing | Stopping => "warning",
            Running => "success",
            Restarting => "info",
            Exited | Dead | Missing | Offline => "danger",
        }
    }

    pub fn label(self, locale: &str) -> String {
        i18n::trans(locale, &format!("server/console.status.{}", self.value()), &[])
    }

    pub fn is_offline(self) -> bool {
        matches!(self, ContainerStatus::Offline | ContainerStatus::Missing)
    }

    pub fn is_startable(self) -> bool {
        use ContainerStatus::*;
        !matches!(self, Running | Starting | Stopping | Restarting | Missing)
    }

    pub fn is_restartable(self) -> bool {
        self.is_startable() || !self.is_offline()
    }

    pub fn is_stoppable(self) -> bool {
        use ContainerStatus::*;
        !matches!(self, Starting | Stopping | Restarting | Exited | Offline | Missing)
    }

    pub fn is_killable(self) -> bool {
        use ContainerStatus::*;
        !matches!(self, Offline | Running | Exited | Missing)
    }
}

/// app/Enums/ServerState.php (the `servers.status` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    Installing,
    InstallFailed,
    ReinstallFailed,
    Suspended,
    RestoringBackup,
}

impl ServerState {
    pub fn parse(s: Option<&str>) -> Option<ServerState> {
        Some(match s? {
            "installing" => ServerState::Installing,
            "install_failed" => ServerState::InstallFailed,
            "reinstall_failed" => ServerState::ReinstallFailed,
            "suspended" => ServerState::Suspended,
            "restoring_backup" => ServerState::RestoringBackup,
            _ => return None,
        })
    }

    pub fn icon(self) -> &'static str {
        match self {
            ServerState::Installing => "heart-bolt",
            ServerState::InstallFailed | ServerState::ReinstallFailed => "heart-x",
            ServerState::Suspended => "heart-cancel",
            ServerState::RestoringBackup => "heart-up",
        }
    }

    pub fn color(self) -> &'static str {
        match self {
            ServerState::Installing | ServerState::RestoringBackup => "primary",
            ServerState::InstallFailed | ServerState::ReinstallFailed => "danger",
            ServerState::Suspended => "warning",
        }
    }

    /// `str($this->value)->headline()`.
    pub fn label(self) -> &'static str {
        match self {
            ServerState::Installing => "Installing",
            ServerState::InstallFailed => "Install Failed",
            ServerState::ReinstallFailed => "Reinstall Failed",
            ServerState::Suspended => "Suspended",
            ServerState::RestoringBackup => "Restoring Backup",
        }
    }
}

/// `DaemonServerRepository::getDetails()` cached 15 s (`servers.<uuid>.status/resources`).
pub async fn details(st: &AppState, server: &servers::Model, node: &nodes::Model) -> Value {
    if node.maintenance_mode.0 {
        return json!({ "state": "missing", "utilization": {} });
    }
    let key = format!("servers.{}.details", server.uuid);
    crate::cache::remember(&key, Duration::from_secs(15), || async {
        let Ok(conn) = st.node_conn(node) else { return json!({ "state": "missing" }) };
        let path = format!("/api/servers/{}", server.uuid);
        let mut call = pelican_wings::client::Call::new(pelican_wings::client::Method::GET, &path);
        call.timeout = Some(Duration::from_secs(1));
        call.connect_timeout = Some(Duration::from_secs(1));
        match st.wings.send(&conn, call).await {
            Ok(r) => r.json().unwrap_or_else(|| json!({ "state": "missing" })),
            Err(_) => json!({ "state": "missing" }),
        }
    })
    .await
}

async fn wings_get(st: &AppState, node: &nodes::Model, path: &str, timeout: u64, connect: u64) -> Option<Value> {
    let conn = st.node_conn(node).ok()?;
    let mut call = pelican_wings::client::Call::new(pelican_wings::client::Method::GET, path);
    call.timeout = Some(Duration::from_secs(timeout));
    call.connect_timeout = Some(Duration::from_secs(connect));
    st.wings.send(&conn, call).await.ok()?.json()
}

/// `Node::statistics()` (`/api/system/utilization`, zeros when unreachable).
pub async fn node_statistics(st: &AppState, node: &nodes::Model) -> Value {
    let key = format!("nodes.{}.statistics", node.id.0);
    crate::cache::remember(&key, Duration::from_secs(5), || async {
        match wings_get(st, node, "/api/system/utilization", 1, 1).await {
            Some(v) if v.get("memory_total").is_some_and(|m| m.as_f64().unwrap_or(0.0) > 0.0) => v,
            _ => json!({
                "memory_total": 0, "memory_used": 0, "swap_total": 0, "swap_used": 0,
                "load_average1": 0.0, "load_average5": 0.0, "load_average15": 0.0,
                "cpu_percent": 0.0, "disk_total": 0, "disk_used": 0,
            }),
        }
    })
    .await
}

/// `Node::systemInformation()` (`/api/system`; `{exception}` when unreachable).
pub async fn node_system_information(st: &AppState, node: &nodes::Model) -> Value {
    let key = format!("nodes.{}.system_information", node.id.0);
    crate::cache::remember(&key, Duration::from_secs(30), || async {
        match wings_get(st, node, "/api/system", 15, 3).await {
            Some(v) if v.get("cpu_count").is_some() => v,
            _ => json!({ "exception": "Could not connect" }),
        }
    })
    .await
}

pub fn forget_details(uuid: &str) {
    crate::cache::forget(&format!("servers.{uuid}.details"));
}

/// `retrieveStatus()`.
pub fn status(details: &Value) -> ContainerStatus {
    details.get("state").and_then(Value::as_str).and_then(ContainerStatus::parse).unwrap_or(ContainerStatus::Missing)
}

/// `retrieveResources()[key] ?? 0`.
pub fn resource(details: &Value, key: &str) -> f64 {
    details.get("utilization").and_then(|u| u.get(key)).and_then(Value::as_f64).unwrap_or(0.0)
}

/// `$server->condition`: the DB state wins over the container status.
pub fn condition(server: &servers::Model, details: &Value, locale: &str) -> (String, &'static str, &'static str) {
    match ServerState::parse(server.status.as_deref()) {
        Some(s) => (s.label().to_string(), s.icon(), s.color()),
        None => {
            let c = status(details);
            (c.label(locale), c.icon(), c.color())
        }
    }
}

/// `isInstalled()`: not installing and not a failed (re)install.
pub fn is_installed(server: &servers::Model) -> bool {
    !matches!(
        ServerState::parse(server.status.as_deref()),
        Some(ServerState::Installing | ServerState::InstallFailed | ServerState::ReinstallFailed)
    )
}

/// `isInConflictState()` (transfer check needs the caller's lookup).
pub fn in_conflict_state(server: &servers::Model, node: &nodes::Model, has_transfer: bool) -> bool {
    matches!(ServerState::parse(server.status.as_deref()), Some(ServerState::Suspended | ServerState::RestoringBackup))
        || node.maintenance_mode.0
        || !is_installed(server)
        || has_transfer
}

/// Limit in bytes (`ServerResourceType::getResourceAmount` for memory/disk limits).
pub fn limit_bytes(mib: i64, binary: bool) -> f64 {
    mib as f64 * if binary { 1024.0 * 1024.0 } else { 1000.0 * 1000.0 }
}

/// `formatResource()` for byte amounts: `convert_bytes_to_readable($n, $precision, base: 3)`.
pub fn format_bytes_gib(bytes: f64, precision: usize, binary: bool) -> String {
    let suffix = if binary { "GiB" } else { "GB" };
    if bytes <= 0.0 {
        return format!("0 {}", "Bytes");
    }
    let unit: f64 = if binary { 1024.0 } else { 1000.0 };
    format!("{:.*} {suffix}", precision, bytes / unit.powi(3))
}

/// `formatResource()` for a limit of 0: the infinity sign.
pub const UNLIMITED: &str = "\u{221E}";

/// `Uptime` formatting: `diffForHumans(DIFF_ABSOLUTE, short, parts: 4)` of `uptime` ms.
pub fn format_uptime(ms: f64) -> String {
    let mut s = (ms / 1000.0) as u64;
    let mut parts = Vec::new();
    for (unit, len) in [("w", 604_800u64), ("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if s >= len && parts.len() < 4 {
            parts.push(format!("{}{unit}", s / len));
            s %= len;
        }
    }
    if parts.is_empty() { "1s".into() } else { parts.join(" ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_rules() {
        assert!(ContainerStatus::Offline.is_startable());
        assert!(!ContainerStatus::Running.is_startable());
        assert!(ContainerStatus::Running.is_stoppable());
        assert!(!ContainerStatus::Running.is_killable());
        assert!(ContainerStatus::Starting.is_killable());
        assert!(ContainerStatus::Running.is_restartable());
        assert!(!ContainerStatus::Missing.is_restartable());
    }

    #[test]
    fn formatting() {
        assert_eq!(format_bytes_gib(1024.0 * 1024.0 * 1024.0 * 1.5, 2, true), "1.50 GiB");
        assert_eq!(format_uptime(3_723_000.0), "1h 2m 3s");
    }
}
