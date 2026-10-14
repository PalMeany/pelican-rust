//! Admin › Panel Logs (app/Filament/Admin/Pages/{ListLogs,ViewLogs}.php over
//! gboquizosanchez/filament-log-viewer): daily `storage/logs/laravel-YYYY-MM-DD.log`
//! files with per-level counts, entries with stack traces and context, download,
//! delete and upload to logs.pelican.dev.

use std::path::PathBuf;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_core::state::AppState;
use serde_json::Value;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::Form;
use crate::table::{Action, Cell, Column, ListParams, Row, Tab, Table};

const LEVELS: [&str; 8] = ["emergency", "alert", "critical", "error", "warning", "notice", "info", "debug"];
const PER_PAGE: [u64; 4] = [5, 10, 25, 30];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/logs", get(index))
        .route("/admin/logs/{date}", get(view))
        .route("/admin/logs/{date}/download", get(download))
        .route("/admin/logs/{date}/delete", post(delete))
        .route("/admin/logs/{date}/upload", post(upload))
}

fn logs_dir(admin: &Admin) -> PathBuf {
    admin.st.config.base_path.join("storage/logs")
}

/// `laravel-<date>.log` for a valid `YYYY-MM-DD` date only (no path traversal).
fn log_path(admin: &Admin, date: &str) -> Option<PathBuf> {
    let valid = date.len() == 10
        && date.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    valid.then(|| logs_dir(admin).join(format!("laravel-{date}.log")))
}

/// Dates of the daily log files, newest first.
fn dates(admin: &Admin) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(logs_dir(admin))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let date = name.strip_prefix("laravel-")?.strip_suffix(".log")?.to_string();
                    log_path(admin, &date).map(|_| date)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.reverse();
    out
}

#[derive(Clone, Debug)]
struct Entry {
    env: String,
    level: &'static str,
    datetime: String,
    header: String,
    stack: String,
    context: Option<Value>,
}

/// `Parser::parse` + `Entry`: headings `[Y-m-d H:i:s] env.LEVEL: message {context}`,
/// the lines up to the next heading are the stack; newest first.
fn parse(raw: &str) -> Vec<Entry> {
    let mut headings: Vec<(usize, usize)> = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' && is_datetime_bracket(&raw[i..]) {
            let end = raw[i..].find('\n').map_or(raw.len(), |n| i + n);
            headings.push((i, end));
            i = end;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &(start, end)) in headings.iter().enumerate() {
        let heading = &raw[start..end];
        let next = headings.get(k + 1).map_or(raw.len(), |h| h.0);
        let stack = &raw[end..next];
        for level in LEVELS {
            if heading.contains(&format!(".{}:", level.to_uppercase())) {
                out.push(entry(level, heading, stack));
            }
        }
    }
    out.reverse();
    out
}

fn is_datetime_bracket(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 21 || b[20] != b']' {
        return false;
    }
    let pattern = b"[0000-00-00 00:00:00";
    pattern.iter().zip(b).all(|(p, c)| if *p == b'0' { c.is_ascii_digit() } else { p == c })
}

fn entry(level: &'static str, heading: &str, stack: &str) -> Entry {
    let datetime = heading[1..20].to_string();
    let mut header = heading[21..].trim_start().to_string();
    let mut env = String::new();
    // `/^[a-z]+.[A-Z]+:/`
    let lower = header.chars().take_while(|c| c.is_ascii_lowercase()).count();
    if lower > 0 {
        let rest: Vec<char> = header.chars().skip(lower).collect();
        let upper = rest.iter().skip(1).take_while(|c| c.is_ascii_uppercase()).count();
        if rest.len() > upper + 1 && upper > 0 && rest[upper + 1] == ':' {
            env = header[..lower].to_string();
            header = header[lower + upper + 2..].trim().to_string();
        }
    }
    let mut context = None;
    if let Some((s, e)) = first_balanced_braces(&header) {
        if let Ok(v @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str::<Value>(&header[s..e]) {
            header = header.replacen(&header[s..e].to_string(), "", 1);
            context = Some(v);
        }
    }
    Entry { env, level, datetime, header: header.trim().to_string(), stack: stack.trim().to_string(), context }
}

/// The first match of `/{(?:[^{}]|(?R))*}/`.
fn first_balanced_braces(s: &str) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    for start in (0..b.len()).filter(|&i| b[i] == b'{') {
        let mut depth = 0usize;
        for (j, &c) in b.iter().enumerate().skip(start) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, j + 1));
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn counts(entries: &[Entry]) -> Vec<(&'static str, usize)> {
    let mut out = vec![("all", entries.len())];
    out.extend(LEVELS.iter().map(|l| (*l, entries.iter().filter(|e| e.level == *l).count())));
    out
}

fn level_icon(level: &str) -> (&'static str, &'static str) {
    match level {
        "emergency" => ("bug", "danger"),
        "alert" => ("speakerphone", "danger"),
        "critical" => ("flame", "danger"),
        "error" => ("circle-x", "danger"),
        "warning" => ("alert-triangle", "warning"),
        "notice" => ("exclamation-circle", "success"),
        "info" => ("info-circle", "info"),
        "debug" => ("terminal", "gray"),
        _ => ("list", "gray"),
    }
}

/// `ParseDateUseCase`: Carbon `isoFormat('LL')`.
fn pretty_date(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or(date.into())
}

fn read(admin: &Admin, date: &str) -> Result<(PathBuf, String), WebError> {
    let path = log_path(admin, date).ok_or(WebError::NotFound)?;
    let raw = std::fs::read(&path).map_err(|_| WebError::NotFound)?;
    Ok((path, String::from_utf8_lossy(&raw).into_owned()))
}

/// `GET /admin/logs` (ListLogs).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("view panelLog")?;
    let p = ListParams::parse(admin.query.as_deref(), &PER_PAGE, 5);
    let mut rows: Vec<(String, Vec<(&'static str, usize)>)> = dates(&admin)
        .into_iter()
        .filter(|d| p.search.is_empty() || d.to_lowercase().contains(&p.search.to_lowercase()))
        .map(|d| {
            let c = log_path(&admin, &d)
                .and_then(|path| std::fs::read(path).ok())
                .map(|raw| counts(&parse(&String::from_utf8_lossy(&raw))))
                .unwrap_or_default();
            (d, c)
        })
        .collect();
    let shown = ["all", "error", "warning", "notice", "info", "debug"];
    if let Some(col) = p.sort.as_deref() {
        let key = |r: &(String, Vec<(&str, usize)>)| r.1.iter().find(|(l, _)| *l == col).map_or(0, |(_, n)| *n);
        if col == "date" {
            rows.sort_by(|a, b| a.0.cmp(&b.0));
        } else {
            rows.sort_by_key(key);
        }
        if p.desc {
            rows.reverse();
        }
    }
    let total = rows.len() as u64;
    let t = |k: &str| admin.t(k);
    let mut table = Table::new("logs", "/admin/logs");
    table.columns = vec![Column::new("date", t("filament-log-viewer::log.table.columns.date.label")).sortable()];
    for l in shown {
        let label = if l == "all" { t("admin/log.total_logs") } else { t(&format!("admin/log.{l}")) };
        table.columns.push(Column::new(l, label).center().sortable());
    }
    let page: Vec<_> = match p.limit_offset() {
        Some((l, o)) => rows.into_iter().skip(o as usize).take(l as usize).collect(),
        None => rows,
    };
    for (date, c) in page {
        let url = format!("/admin/logs/{date}");
        let mut row = Row::new(date.clone()).url(url.clone()).cell("date", Cell::mono(date.clone()));
        for l in shown {
            let n = c.iter().find(|(x, _)| *x == l).map_or(0, |(_, n)| *n);
            let (_, color) = level_icon(l);
            row = row.cell(l, if n == 0 { Cell::text("0") } else { Cell::badge(n.to_string(), color) });
        }
        row.actions.push(Action::link(
            t("filament-log-viewer::log.table.actions.view.label"),
            "file-description",
            url.clone(),
        ));
        row.actions.push(Action::link(
            admin.t_with("filament-log-viewer::log.table.actions.download.label", &[("log", &pretty_date(&date))]),
            "file-download",
            format!("{url}/download"),
        ));
        row.actions.push(
            Action::post(
                admin.t_with("admin/log.actions.upload_tooltip", &[("url", "logs.pelican.dev")]),
                "world-upload",
                format!("{url}/upload"),
            )
            .confirm(admin.t_with(
                "admin/log.actions.upload_logs_description",
                &[("file", &date), ("url", "https://logs.pelican.dev")],
            )),
        );
        row.actions.push(
            Action::post(
                admin.t_with("filament-log-viewer::log.table.actions.delete.label", &[("log", &pretty_date(&date))]),
                "trash",
                format!("{url}/delete"),
            )
            .color("danger")
            .confirm(admin.t("filament-actions::modal.confirmation")),
        );
        table.rows.push(row);
    }
    table.searchable = true;
    table.empty_icon = "check".into();
    table.empty_heading = t("admin/log.empty_table");
    table.finish(&p, total, &PER_PAGE, &admin.locale());
    admin
        .page(
            "logs",
            "admin/list.html",
            context! { title => t("admin/log.navigation.panel_logs"), table => Mj::from_serialize(&table) },
        )
        .await
}

/// `GET /admin/logs/{date}` (ViewLogs): file details, level tabs, entries.
async fn view(admin: Admin, Path(date): Path<String>) -> WebResult {
    admin.authorize("view panelLog")?;
    let (path, raw) = read(&admin, &date)?;
    let entries = parse(&raw);
    let c = counts(&entries);
    let mut p = ListParams::parse(admin.query.as_deref(), &PER_PAGE, 5);
    p.tab.get_or_insert_with(|| "all".into());
    let level = p.tab.clone().filter(|l| LEVELS.contains(&l.as_str()));
    let q = p.search.to_lowercase();
    let mut list: Vec<&Entry> = entries
        .iter()
        .filter(|e| level.as_deref().is_none_or(|l| e.level == l))
        .filter(|e| {
            q.is_empty()
                || e.datetime.to_lowercase().contains(&q)
                || e.header.to_lowercase().contains(&q)
                || e.stack.to_lowercase().contains(&q)
                || e.context.as_ref().is_some_and(|c| c.to_string().to_lowercase().contains(&q))
        })
        .collect();
    match p.sort.as_deref() {
        Some("datetime") => list.sort_by(|a, b| a.datetime.cmp(&b.datetime)),
        Some("level") => list.sort_by(|a, b| a.level.cmp(b.level)),
        Some("header") => list.sort_by(|a, b| a.header.cmp(&b.header)),
        _ => {}
    }
    if p.sort.is_some() && p.desc {
        list.reverse();
    }
    let total = list.len() as u64;
    let t = |k: &str| admin.t(k);
    let base = format!("/admin/logs/{date}");
    let mut table = Table::new("entries", &base);
    table.columns = vec![
        Column::new("env", ""),
        Column::new("datetime", t("filament-log-viewer::log.table.columns.date.label")).sortable(),
        Column::new("level", t("filament-log-viewer::log.table.columns.level.label")).center().sortable(),
        Column::new("header", t("filament-log-viewer::log.table.columns.message.label")).sortable(),
    ];
    // Tabs are hidden when a single level holds every entry.
    let all = c[0].1;
    if !c[1..].iter().any(|(_, n)| *n == all) {
        table.tabs.push(tab("all", &admin.t("filament-log-viewer::log.levels.all"), all));
        for (l, n) in &c[1..] {
            if *n > 0 {
                table.tabs.push(tab(l, &admin.t(&format!("filament-log-viewer::log.levels.{l}")), *n));
            }
        }
    }
    let tz: chrono_tz::Tz = admin.st.config.env.string_or("APP_TIMEZONE", "UTC").parse().unwrap_or(chrono_tz::UTC);
    let user_tz: chrono_tz::Tz = admin.timezone().parse().unwrap_or(chrono_tz::UTC);
    let page: Vec<&Entry> = match p.limit_offset() {
        Some((l, o)) => list.into_iter().skip(o as usize).take(l as usize).collect(),
        None => list,
    };
    for (i, e) in page.iter().enumerate() {
        let (icon, color) = level_icon(e.level);
        let shown_at = chrono::NaiveDateTime::parse_from_str(&e.datetime, "%Y-%m-%d %H:%M:%S")
            .ok()
            .and_then(|n| n.and_local_timezone(tz).single())
            .map(|d| d.with_timezone(&user_tz).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| e.datetime.clone());
        let env_color = match e.env.as_str() {
            "production" => "danger",
            "staging" => "warning",
            _ => "success",
        };
        let mut html = crate::views::escape(&e.header);
        if !e.stack.is_empty() {
            html.push_str(&format!(
                r#"<details class="log-extra"><summary class="btn-link small">{}</summary><pre class="code-block">{}</pre></details>"#,
                crate::views::escape(&admin.t("filament-log-viewer::log.table.modal.stack_trace")),
                crate::views::escape(&e.stack)
            ));
        }
        if let Some(ctx) = &e.context {
            html.push_str(&format!(
                r#"<details class="log-extra"><summary class="btn-link small">{}</summary><pre class="code-block">{}</pre></details>"#,
                crate::views::escape(&admin.t("filament-log-viewer::log.table.modal.context")),
                crate::views::escape(&serde_json::to_string_pretty(ctx).unwrap_or_default())
            ));
        }
        table.rows.push(
            Row::new(i.to_string())
                .cell("env", if e.env.is_empty() { Cell::Empty } else { Cell::badge(e.env.clone(), env_color) })
                .cell("datetime", Cell::mono(shown_at))
                .cell(
                    "level",
                    Cell::Icon {
                        icon: icon.into(),
                        color: color.into(),
                        tooltip: Some(admin.t(&format!("filament-log-viewer::log.levels.{}", e.level))),
                    },
                )
                .cell("header", Cell::Html { html }),
        );
    }
    table.searchable = true;
    table.search_placeholder = t("filament-log-viewer::log.table.modal.search_placeholder");
    table.empty_icon = "check".into();
    table.empty_heading = t("filament-log-viewer::log.table.modal.no_matching_entries");
    table.finish(&p, total, &PER_PAGE, &admin.locale());
    let meta = std::fs::metadata(&path).ok();
    let stamp = |t: Option<std::time::SystemTime>| {
        t.map(|t| chrono::DateTime::<chrono::Utc>::from(t).with_timezone(&tz).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default()
    };
    admin
        .page(
            "logs",
            "admin/logs/view.html",
            context! {
                title => admin.t_with("filament-log-viewer::log.show.title", &[("log", &pretty_date(&date))]),
                date => date,
                pretty => pretty_date(&date),
                file_path => path.to_string_lossy().into_owned(),
                entries => entries.len(),
                size => meta.as_ref().map(|m| m.len() as i64).unwrap_or(0),
                created_at => stamp(meta.as_ref().and_then(|m| m.created().or_else(|_| m.modified()).ok())),
                updated_at => stamp(meta.as_ref().and_then(|m| m.modified().ok())),
                app_tz => tz.to_string(),
                table => Mj::from_serialize(&table),
            },
        )
        .await
}

fn tab(key: &str, label: &str, count: usize) -> Tab {
    Tab { key: key.into(), label: label.into(), badge: Some(count as i64), url: String::new(), active: false }
}

/// `DownloadLogUseCase`: `laravel-<date>.log` attachment.
async fn download(admin: Admin, Path(date): Path<String>) -> Result<Response, WebError> {
    admin.authorize("view panelLog")?;
    let path = log_path(&admin, &date).ok_or(WebError::NotFound)?;
    let body = std::fs::read(&path).map_err(|_| WebError::NotFound)?;
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=laravel-{date}.log")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

async fn delete(admin: Admin, Path(date): Path<String>) -> WebResult {
    admin.authorize("view panelLog")?;
    let path = log_path(&admin, &date).ok_or(WebError::NotFound)?;
    match std::fs::remove_file(&path) {
        Ok(()) => admin.flash_success(admin.t("filament-log-viewer::log.table.actions.delete.success")),
        Err(_) => admin.flash_danger(admin.t("filament-log-viewer::log.table.actions.delete.error"), None),
    }
    Ok(admin.redirect("/admin/logs"))
}

/// `uploadLogs`: the last 1000 non-empty lines to logs.pelican.dev (expires in 14 days).
async fn upload(admin: Admin, Path(date): Path<String>, form: Form) -> WebResult {
    admin.authorize("view panelLog")?;
    let back = form.back("/admin/logs");
    let Some(raw) = log_path(&admin, &date).and_then(|p| std::fs::read(p).ok()) else {
        admin.flash_danger(
            admin.t("admin/log.actions.log_not_found"),
            Some(admin.t_with("admin/log.actions.log_not_found_description", &[("filename", &date)])),
        );
        return Ok(admin.redirect(&back));
    };
    let text = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let content = lines[lines.len().saturating_sub(1000)..].join("\n");
    let result = async {
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build()?;
        let form = reqwest::multipart::Form::new().text("c", content).text("e", "14d");
        client.post("https://logs.pelican.dev").multipart(form).send().await
    }
    .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            let url =
                resp.json::<Value>().await.ok().and_then(|v| v.get("url").and_then(|u| u.as_str()).map(String::from));
            admin.session.flash("success", admin.t("admin/log.actions.log_upload"), url);
        }
        Ok(resp) => admin.flash_danger(
            admin.t("admin/log.actions.failed_to_upload"),
            Some(admin.t_with(
                "admin/log.actions.failed_to_upload_description",
                &[("status", &resp.status().as_u16().to_string())],
            )),
        ),
        Err(e) => admin.flash_danger(admin.t("admin/log.actions.failed_to_upload"), Some(e.to_string())),
    }
    Ok(admin.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_laravel_and_our_lines() {
        let raw = "[2026-10-15 01:00:00] production.ERROR: Boom {\"exception\":\"[object] (Exception(code: 0): Boom at /a.php:1)\"} \n[stacktrace]\n#0 {main}\n\
                   [2026-10-15 01:01:00] local.INFO: configuration reloaded  \n";
        let e = parse(raw);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].level, "info");
        assert_eq!(e[0].env, "local");
        assert_eq!(e[0].header, "configuration reloaded");
        assert_eq!(e[1].level, "error");
        assert_eq!(e[1].header, "Boom");
        assert_eq!(e[1].stack, "[stacktrace]\n#0 {main}");
        assert!(e[1].context.as_ref().is_some_and(|c| c.get("exception").is_some()));
    }
}
