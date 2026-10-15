//! Execute a scenario against one panel and render a normalized report.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use laravel_compat::Encrypter;

use crate::db::{self, Change, TestDb};
use crate::mock_wings::{MockWings, WingsCall};
use crate::normalize::Normalizer;
use crate::scenario::{Rows, Scenario};

/// Host header sent to both panels so URL generation is identical.
pub const HOST: &str = "localhost:18000";

pub struct StepResult {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub wings: Vec<WingsCall>,
}

pub struct RunResult {
    pub steps: Vec<StepResult>,
    pub changes: Vec<Change>,
    /// `(dir/plugin.json, content)` after the steps (plugin scenarios only).
    pub plugin_files: Vec<(String, String)>,
    pub start: chrono::NaiveDateTime,
    pub end: chrono::NaiveDateTime,
}

pub struct Ctx<'a> {
    pub db: &'a TestDb,
    pub wings: &'a MockWings,
    pub enc: &'a Encrypter,
    pub http: reqwest::Client,
    pub fixtures_dir: std::path::PathBuf,
}

pub fn merged_rows(ctx: &Ctx<'_>, s: &Scenario) -> Result<Rows> {
    let mut rows = Rows::new();
    for f in &s.fixtures {
        for (t, list) in crate::scenario::load_fixture(&ctx.fixtures_dir, f)? {
            rows.entry(t).or_default().extend(list);
        }
    }
    for (t, list) in &s.rows {
        rows.entry(t.clone()).or_default().extend(list.iter().cloned());
    }
    Ok(rows)
}

fn now() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

/// The panel a scenario runs against.
pub enum Target<'a> {
    /// Original panel: HTTP base URL and its container (for `command` steps).
    Orig { url: &'a str, container: Option<&'a str> },
    /// pelican-rs started in-process.
    Rust { url: &'a str, state: Option<&'a pelican_core::state::AppState> },
}

impl Target<'_> {
    fn url(&self) -> &str {
        match self {
            Target::Orig { url, .. } | Target::Rust { url, .. } => url,
        }
    }

    /// Empty `plugins/` and write the scenario's files.
    async fn plugins_setup(&self, files: &[(String, Vec<u8>)]) -> Result<()> {
        use base64::Engine;
        match self {
            Target::Orig { container: Some(c), .. } => {
                let mut script =
                    String::from("set -e; cd /var/www/html/plugins; find . -mindepth 1 -maxdepth 1 -exec rm -rf {} +;");
                for (path, bytes) in files {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                    script
                        .push_str(&format!(" mkdir -p \"$(dirname '{path}')\"; echo '{b64}' | base64 -d > '{path}';"));
                }
                let code = crate::docker::exec(c, &["sh", "-c", &script]).await?;
                anyhow::ensure!(code == 0, "writing plugin files into panel-orig failed");
                Ok(())
            }
            Target::Rust { state: Some(st), .. } => {
                let root = st.config.base_path.join("plugins");
                let _ = std::fs::remove_dir_all(&root);
                std::fs::create_dir_all(&root)?;
                for (path, bytes) in files {
                    let file = root.join(path);
                    if let Some(parent) = file.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(file, bytes)?;
                }
                Ok(())
            }
            _ => anyhow::bail!("plugin scenarios need PELICAN_ORIG_CONTAINER and an in-process pelican-rs"),
        }
    }

    /// Every `<dir>/plugin.json` (sorted) with its content.
    async fn plugins_dump(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        match self {
            Target::Orig { container: Some(c), .. } => {
                let text = crate::docker::exec_output(
                    c,
                    &["sh", "-c", "cd /var/www/html/plugins && for f in $(ls -d */ 2>/dev/null); do if [ -f \"$f/plugin.json\" ]; then printf '\\n@@@ %splugin.json\\n' \"$f\"; cat \"$f/plugin.json\"; fi; done"],
                )
                .await?;
                for chunk in text.split("\n@@@ ").skip(1) {
                    let (name, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
                    out.push((name.to_string(), body.to_string()));
                }
            }
            Target::Rust { state: Some(st), .. } => {
                let root = st.config.base_path.join("plugins");
                let mut dirs: Vec<String> = std::fs::read_dir(&root)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect();
                dirs.sort();
                for d in dirs {
                    if let Ok(body) = std::fs::read_to_string(root.join(&d).join("plugin.json")) {
                        out.push((format!("{d}/plugin.json"), body));
                    }
                }
            }
            _ => {}
        }
        Ok(out)
    }

    async fn command(&self, name: &str) -> Result<i64> {
        match self {
            Target::Orig { container: Some(c), .. } => {
                let mut argv = vec!["php", "/var/www/html/artisan"];
                argv.extend(name.split_whitespace());
                crate::docker::exec(c, &argv).await
            }
            Target::Rust { state: Some(st), .. } => crate::rust_target::command(st, name).await,
            _ => anyhow::bail!("`command` steps need PELICAN_ORIG_CONTAINER and an in-process pelican-rs"),
        }
    }
}

pub async fn run(ctx: &Ctx<'_>, s: &Scenario, target: &Target<'_>) -> Result<RunResult> {
    let base_url = target.url();
    ctx.wings.reset(s.wings.responses.clone());
    ctx.db.reset().await.context("db reset")?;
    let rows = merged_rows(ctx, s)?;
    ctx.db.load(&rows, ctx.enc, now()).await.context("loading fixtures")?;
    let before = ctx.db.dump().await?;
    let _ = ctx.wings.take_calls();
    if s.plugins.is_some() {
        target.plugins_setup(&crate::scenario::plugin_files(s)?).await.context("plugin files")?;
    }

    let start = now();
    let mut steps = Vec::new();
    for (i, st) in s.steps.iter().enumerate() {
        if s.distinct_seconds {
            let ms = chrono::Utc::now().timestamp_subsec_millis();
            tokio::time::sleep(std::time::Duration::from_millis(u64::from(1000 - ms.min(999)) + 20)).await;
        }
        if let Some(cmd) = &st.command {
            let code = target.command(cmd).await.with_context(|| format!("step {} command {cmd}", i + 1))?;
            steps.push(StepResult {
                status: code as u16,
                headers: Vec::new(),
                body: Vec::new(),
                wings: ctx.wings.take_calls(),
            });
            continue;
        }
        let url = format!("{}{}", base_url.trim_end_matches('/'), st.path);
        let method = reqwest::Method::from_bytes(st.method.as_bytes())?;
        // Both panels trust all proxies (TRUSTED_PROXIES=0.0.0.0/0), so the
        // client IP is taken from X-Forwarded-For unless the step overrides it.
        let mut req = ctx.http.request(method, &url).header("Host", HOST);
        if !st.headers.keys().any(|k| k.eq_ignore_ascii_case("x-forwarded-for")) {
            req = req.header("X-Forwarded-For", "10.0.0.1");
        }
        if st.accept_json {
            req = req.header("Accept", "application/json");
        }
        if let Some(t) = &st.bearer {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        for (k, v) in &st.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(j) = &st.json {
            req = req.header("Content-Type", "application/json").body(serde_json::to_vec(j)?);
        } else if let Some(b) = &st.body {
            req = req.body(b.clone());
        }
        let resp = req.send().await.with_context(|| format!("step {} {}", i + 1, url))?;
        let status = resp.status().as_u16();
        let mut headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_ascii_lowercase(), v.to_str().unwrap_or("<binary>").to_string()))
            .collect();
        headers.sort();
        let body = resp.bytes().await?.to_vec();
        if let Some(want) = st.expect_status {
            anyhow::ensure!(
                status == want,
                "step {}: expected status {want}, got {status}: {}",
                i + 1,
                String::from_utf8_lossy(&body)
            );
        }
        steps.push(StepResult { status, headers, body, wings: ctx.wings.take_calls() });
    }
    let end = now();
    let after = ctx.db.dump().await?;
    let changes = db::diff(&before, &after)
        .into_iter()
        .filter(|c| !s.normalize.ignore_tables.iter().any(|t| t == change_table(c)))
        .collect();
    let plugin_files = if s.plugins.is_some() { target.plugins_dump().await? } else { Vec::new() };
    Ok(RunResult { steps, changes, plugin_files, start, end })
}

fn change_table(c: &Change) -> &str {
    match c {
        Change::Insert { table, .. } | Change::Delete { table, .. } | Change::Update { table, .. } => table,
    }
}

/// Headers compared by default.
const COMPARED_HEADERS: &[&str] = &["content-type", "location", "content-disposition"];

/// JSON text columns are compared byte-for-byte (pelican-rs writes PHP's exact
/// encoding, ADR-0004). Values the driver already decoded (PostgreSQL `json`,
/// MySQL `JSON`) are re-encoded in their decoded key order.
fn canonical_value(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::Value::String(format!("json:{}", laravel_compat::json::to_string(v).unwrap_or_default()))
        }
        _ => v.clone(),
    }
}

fn render_row(table: &str, row: &db::Row, s: &Scenario) -> String {
    let mut m = serde_json::Map::new();
    for (k, v) in row {
        if s.normalize.ignore_columns.iter().any(|c| c == &format!("{table}.{k}")) {
            continue;
        }
        m.insert(k.clone(), canonical_value(v));
    }
    serde_json::Value::Object(m).to_string()
}

/// Render the run as text with all dynamic values replaced by placeholders.
pub fn report(enc: &Encrypter, s: &Scenario, rows_text: &str, r: &RunResult) -> Result<String> {
    let mut raw = String::new();
    for (i, st) in r.steps.iter().enumerate() {
        let spec = &s.steps[i];
        if let Some(cmd) = &spec.command {
            writeln!(raw, "### step {}: COMMAND {cmd}", i + 1)?;
            writeln!(raw, "exit: {}", st.status)?;
        } else {
            writeln!(raw, "### step {}: {} {}", i + 1, spec.method, spec.path)?;
            writeln!(raw, "status: {}", st.status)?;
        }
        for (k, v) in &st.headers {
            if COMPARED_HEADERS.contains(&k.as_str()) || s.normalize.headers.iter().any(|h| h.eq_ignore_ascii_case(k)) {
                writeln!(raw, "{k}: {v}")?;
            }
        }
        match std::str::from_utf8(&st.body) {
            Ok(t) => writeln!(raw, "{t}")?,
            Err(_) => writeln!(raw, "<binary {} bytes>", st.body.len())?,
        }
        for c in &st.wings {
            writeln!(raw, "--> wings {} {}", c.method, c.path_and_query)?;
            writeln!(raw, "    authorization: {}", c.authorization.as_deref().unwrap_or("-"))?;
            writeln!(raw, "    content-type: {}", c.content_type.as_deref().unwrap_or("-"))?;
            if !c.body.is_empty() {
                writeln!(raw, "    {}", String::from_utf8_lossy(&c.body))?;
            }
        }
    }
    writeln!(raw, "### database changes")?;
    let mut lines: Vec<String> = Vec::new();
    for c in &r.changes {
        lines.push(match c {
            Change::Insert { table, row } => format!("+ {table} {}", render_row(table, row, s)),
            Change::Delete { table, row } => format!("- {table} {}", render_row(table, row, s)),
            Change::Update { table, key, cols } => {
                let mut parts = Vec::new();
                for (col, (old, new)) in cols {
                    if s.normalize.ignore_columns.iter().any(|c| c == &format!("{table}.{col}")) {
                        continue;
                    }
                    parts.push(format!("{col}: {} -> {}", canonical_value(old), canonical_value(new)));
                }
                if parts.is_empty() {
                    continue;
                }
                format!("~ {table}[{key}] {}", parts.join(", "))
            }
        });
    }
    raw.push_str(&lines.join("\n"));
    raw.push('\n');
    if s.plugins.is_some() {
        writeln!(raw, "### plugin files")?;
        for (name, body) in &r.plugin_files {
            writeln!(raw, "== {name}\n{body}")?;
        }
    }

    let known = format!(
        "{rows_text}\n{}",
        serde_yaml::to_string(&s.steps.iter().map(|st| (&st.path, &st.json)).collect::<Vec<_>>())?
    );
    let mut n = Normalizer::new(enc, &known, r.start, r.end, &s.normalize.random_patterns)?;
    for c in &r.changes {
        for col in &s.normalize.random_columns {
            let Some((t, cname)) = col.split_once('.') else {
                continue;
            };
            let row = match c {
                Change::Insert { table, row } if table == t => Some(row),
                _ => None,
            };
            if let Some(v) = row.and_then(|r| r.get(cname)) {
                match v {
                    serde_json::Value::String(sv) => n.register_random(sv),
                    other => n.register_random(&other.to_string()),
                }
            }
            if let Change::Update { table, cols, .. } = c {
                if table == t {
                    if let Some((_, serde_json::Value::String(sv))) = cols.get(cname) {
                        n.register_random(sv);
                    }
                }
            }
        }
    }
    n.scan(&raw);
    Ok(n.apply(&raw))
}
