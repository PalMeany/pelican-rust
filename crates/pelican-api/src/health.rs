//! Health checks (spatie/laravel-health with the checks registered in
//! `AppServiceProvider::boot` and `app/Checks/*`). Results live in memory like the
//! original's cache result store; the scheduler refreshes them every five minutes
//! (`RunHealthChecksCommand`) and records the schedule/queue heartbeats every minute.

use std::sync::RwLock;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use pelican_core::state::AppState;
use pelican_db::entities::nodes;
use sea_orm::{ConnectionTrait, EntityTrait};
use serde::Serialize;
use serde_json::{Value, json};

/// A message as the check produced it; `Key` is an `admin/health.results.*` line
/// translated by the page in the application locale (as the console run would).
#[derive(Clone, Debug, Serialize)]
pub enum Text {
    Key(String, Vec<(String, String)>),
    Raw(String),
}

impl Text {
    pub fn is_empty(&self) -> bool {
        matches!(self, Text::Raw(s) if s.is_empty())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    /// `Check::getName()` (`DebugMode`, `UsedDiskSpace`, …).
    pub name: String,
    /// `Check::getLabel()` (`Debug Mode`, …).
    pub label: String,
    /// `ok`, `warning`, `failed`, `crashed` or `skipped`.
    pub status: &'static str,
    pub notification_message: Text,
    pub short_summary: Text,
    pub meta: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoredResults {
    pub finished_at: DateTime<Utc>,
    pub results: Vec<CheckResult>,
}

impl StoredResults {
    pub fn failed(&self) -> Vec<&CheckResult> {
        self.results.iter().filter(|r| r.status == "failed").collect()
    }

    /// `containsFailingCheck()`: anything not ok/skipped… as spatie counts it (not ok).
    pub fn contains_failing(&self) -> bool {
        self.results.iter().any(|r| r.status != "ok")
    }
}

static LATEST: RwLock<Option<StoredResults>> = RwLock::new(None);
static SCHEDULE_HEARTBEAT: AtomicI64 = AtomicI64::new(0);
static QUEUE_HEARTBEAT: AtomicI64 = AtomicI64::new(0);

/// `ScheduleCheckHeartbeatCommand` + `DispatchQueueCheckJobsCommand` (queue jobs run in-process).
pub fn record_heartbeats(now: DateTime<Utc>) {
    SCHEDULE_HEARTBEAT.store(now.timestamp(), Ordering::Relaxed);
    QUEUE_HEARTBEAT.store(now.timestamp(), Ordering::Relaxed);
}

/// `ResultStore::latestResults()`.
pub fn latest() -> Option<StoredResults> {
    LATEST.read().ok().and_then(|g| g.clone())
}

/// `RunHealthChecksCommand`: run every registered check and store the results.
pub async fn run_checks(st: &AppState) -> StoredResults {
    let t = |k: &str, p: &[(&str, &str)]| {
        Text::Key(format!("admin/health.results.{k}"), p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect())
    };
    let mut results = Vec::new();
    if st.config.is_production() {
        results.push(debug_mode(st));
    }
    results.push(environment(st, &t));
    results.push(cache(st, &t));
    results.push(database(st, &t).await);
    results.push(queue());
    results.push(schedule(&t));
    results.push(used_disk_space());
    results.push(panel_version(st, &t).await);
    results.push(node_versions(st, &t).await);
    let stored = StoredResults { finished_at: Utc::now(), results };
    if let Ok(mut g) = LATEST.write() {
        *g = Some(stored.clone());
    }
    stored
}

type T<'a> = &'a (dyn Fn(&str, &[(&str, &str)]) -> Text + Sync);

fn raw(s: impl Into<String>) -> Text {
    Text::Raw(s.into())
}

fn result(name: &str, status: &'static str, message: Text, summary: Option<Text>, meta: Value) -> CheckResult {
    // Result::getShortSummary(): the status in title case when none was set.
    let short = summary.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let mut c = status.chars();
        raw(c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default())
    });
    CheckResult {
        name: name.into(),
        label: label(name),
        status,
        notification_message: message,
        short_summary: short,
        meta,
    }
}

/// `Str::of($name)->snake()->replace('_', ' ')->title()`.
fn label(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn word(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// `DebugModeCheck` (registered only in production; expects debug off).
fn debug_mode(st: &AppState) -> CheckResult {
    let actual = st.config.app_debug;
    let meta = json!({ "actual": actual, "expected": false });
    if !actual {
        result("DebugMode", "ok", raw(""), Some(raw(word(actual))), meta)
    } else {
        let msg = Text::Key(
            "admin/health.results.debugmode.failed".into(),
            vec![("actual".into(), word(actual).into()), ("expected".into(), word(false).into())],
        );
        result("DebugMode", "failed", msg, Some(raw(word(actual))), meta)
    }
}

fn environment(st: &AppState, t: T<'_>) -> CheckResult {
    let actual = st.config.app_env.as_str();
    let meta = json!({ "actual": actual, "expected": "production" });
    if actual == "production" {
        result("Environment", "ok", t("environment.ok", &[("actual", actual)]), Some(raw(actual)), meta)
    } else {
        let msg = t("environment.failed", &[("actual", actual), ("expected", "production")]);
        result("Environment", "failed", msg, Some(raw(actual)), meta)
    }
}

/// `CacheCheck`: the in-process cache always round-trips.
fn cache(st: &AppState, t: T<'_>) -> CheckResult {
    result("Cache", "ok", t("cache.ok", &[]), None, json!({ "driver": st.config.cache_store }))
}

async fn database(st: &AppState, t: T<'_>) -> CheckResult {
    let meta = json!({ "connection_name": st.config.env.string_or("DB_CONNECTION", "sqlite") });
    match st.db.execute_unprepared("select 1").await {
        Ok(_) => result("Database", "ok", t("database.ok", &[]), None, meta),
        Err(e) => result("Database", "failed", t("database.failed", &[("error", &e.to_string())]), None, meta),
    }
}

/// spatie `QueueCheck` for the `default` queue (5 minutes).
fn queue() -> CheckResult {
    let last = QUEUE_HEARTBEAT.load(Ordering::Relaxed);
    let fail = if last == 0 {
        Some("The `default` queue did not run yet.".to_string())
    } else {
        let minutes = (Utc::now().timestamp() - last) / 60;
        (minutes > 5).then(|| format!("The last run of the `default` queue was more than {minutes} minutes ago."))
    };
    match fail {
        None => result("Queue", "ok", raw(""), None, json!([])),
        Some(f) => result(
            "Queue",
            "failed",
            raw("Queue jobs running failed. Check meta for more information."),
            None,
            json!([f]),
        ),
    }
}

/// `App\Checks\ScheduleCheck` (heartbeat at most a minute old).
fn schedule(t: T<'_>) -> CheckResult {
    let last = SCHEDULE_HEARTBEAT.load(Ordering::Relaxed);
    if last == 0 {
        return result("Schedule", "failed", t("schedule.failed_not_ran", &[]), None, json!([]));
    }
    let minutes = (Utc::now().timestamp() - last) / 60;
    if minutes > 1 {
        return result(
            "Schedule",
            "failed",
            t("schedule.failed_last_ran", &[("time", &minutes.to_string())]),
            None,
            json!([]),
        );
    }
    result("Schedule", "ok", t("schedule.ok", &[]), None, json!([]))
}

/// `App\Checks\UsedDiskSpaceCheck` on `/` (warning above 70 %, failure above 90 %).
fn used_disk_space() -> CheckResult {
    let pct = disk_usage_percentage("/").unwrap_or(0);
    let meta = json!({ "disk_space_used_percentage": pct });
    let summary = Some(raw(format!("{pct}%")));
    let msg = raw(format!("The disk is almost full ({pct}% used)."));
    if pct > 90 {
        result("UsedDiskSpace", "failed", msg, summary, meta)
    } else if pct > 70 {
        result("UsedDiskSpace", "warning", msg, summary, meta)
    } else {
        result("UsedDiskSpace", "ok", raw(""), summary, meta)
    }
}

/// `100 - disk_free_space * 100 / disk_total_space`, cast to int.
fn disk_usage_percentage(path: &str) -> Option<i64> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let free = s.f_bavail as f64 * s.f_frsize as f64;
    let total = s.f_blocks as f64 * s.f_frsize as f64;
    (total > 0.0).then(|| (100.0 - free * 100.0 / total) as i64)
}

async fn panel_version(st: &AppState, t: T<'_>) -> CheckResult {
    let is_latest = crate::versions::is_latest_panel(st).await;
    let current = crate::versions::current_panel_version();
    let latest = crate::versions::latest_panel_version(st).await;
    let meta = json!({ "isLatest": is_latest, "currentVersion": current, "latestVersion": latest });
    let Some(latest) = latest else {
        return result(
            "PanelVersion",
            "warning",
            t("panelversion.unavailable", &[]),
            Some(t("panelversion.unknown", &[])),
            meta,
        );
    };
    if is_latest {
        result("PanelVersion", "ok", t("panelversion.ok", &[]), Some(t("panelversion.up_to_date", &[])), meta)
    } else {
        let msg = t("panelversion.failed", &[("currentVersion", &current), ("latestVersion", &latest)]);
        result("PanelVersion", "failed", msg, Some(t("panelversion.outdated", &[])), meta)
    }
}

/// `App\Checks\NodeVersionsCheck`: unreachable nodes are not counted as outdated.
async fn node_versions(st: &AppState, t: T<'_>) -> CheckResult {
    let all = nodes::Entity::find().all(&st.db).await.unwrap_or_default();
    if all.is_empty() {
        return result(
            "NodeVersions",
            "skipped",
            t("nodeversions.no_nodes_created", &[]),
            Some(t("nodeversions.no_nodes", &[])),
            json!([]),
        );
    }
    let mut outdated = 0;
    for node in &all {
        if let Some(version) = wings_version(st, node).await {
            if !crate::versions::is_latest_wings(st, &version).await {
                outdated += 1;
            }
        }
    }
    let total = all.len().to_string();
    let out = outdated.to_string();
    let latest = crate::versions::latest_wings_version(st).await;
    let meta = json!({ "all": all.len(), "outdated": outdated, "latestVersion": latest });
    if outdated == 0 {
        result("NodeVersions", "ok", t("nodeversions.ok", &[]), Some(t("nodeversions.all_up_to_date", &[])), meta)
    } else {
        let p = [("outdated", out.as_str()), ("all", total.as_str())];
        result("NodeVersions", "failed", t("nodeversions.failed", &p), Some(t("nodeversions.outdated", &p)), meta)
    }
}

/// `Node::systemInformation()['version']`, None when the daemon is unreachable.
async fn wings_version(st: &AppState, node: &nodes::Model) -> Option<String> {
    let conn = st.node_conn(node).ok()?;
    let call = pelican_wings::client::Call::new(pelican_wings::client::Method::GET, "/api/system");
    let v = st.wings.send(&conn, call).await.ok()?.json()?;
    Some(pelican_core::php::string(v.get("version")?))
}
