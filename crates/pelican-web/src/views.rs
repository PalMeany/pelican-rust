//! minijinja environment: embedded templates, optional `PELICAN_VIEWS_PATH`
//! override directory, translation/asset helpers (ADR-0013).

use std::path::PathBuf;
use std::sync::OnceLock;

use minijinja::value::{Kwargs, Rest};
use minijinja::{Environment, Error, ErrorKind, State, Value};

use crate::{assets, i18n};

static ENV: OnceLock<Environment<'static>> = OnceLock::new();

fn embedded(name: &str) -> Option<String> {
    crate::TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, b)| String::from_utf8_lossy(b).into_owned())
}

fn kwargs_pairs(kw: &Kwargs) -> Vec<(String, String)> {
    kw.args().filter_map(|k| kw.get::<Value>(k).ok().map(|v| (k.to_string(), v.to_string()))).collect()
}

fn locale(state: &State) -> String {
    state.lookup("locale").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "en".into())
}

/// `trans('group.key', name=value, ...)`.
fn trans(state: &State, key: &str, kw: Kwargs) -> Result<String, Error> {
    let out = i18n::trans(&locale(state), key, &kwargs_pairs(&kw));
    Ok(out)
}

/// `trans_choice('group.key', n, name=value, ...)`.
fn trans_choice(state: &State, key: &str, n: f64, kw: Kwargs) -> Result<String, Error> {
    Ok(i18n::trans_choice(&locale(state), key, n, &kwargs_pairs(&kw)))
}

/// `convert_bytes_to_readable()` (app/helpers.php).
pub fn bytes(n: i64, binary: bool) -> String {
    let suffixes: [&str; 9] = if binary {
        ["Bytes", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"]
    } else {
        ["Bytes", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"]
    };
    if n <= 0 {
        return format!("0 {}", suffixes[0]);
    }
    let unit: f64 = if binary { 1024.0 } else { 1000.0 };
    let from = (n as f64).ln() / unit.ln();
    let base = (from.floor() as usize).min(suffixes.len() - 1);
    format!("{:.2} {}", unit.powf(from - base as f64), suffixes[base])
}

fn bytes_filter(state: &State, v: Value) -> String {
    let binary = state.lookup("binary_prefix").is_none_or(|b| b.is_true());
    bytes(i64::try_from(v).unwrap_or(0), binary)
}

/// Format a UTC timestamp (`Y-m-d H:i:s` / RFC 3339) in the viewer's timezone.
fn datetime_filter(state: &State, v: Value, format: Option<&str>) -> String {
    let Some(s) = v.as_str() else { return String::new() };
    let parsed = chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").map(|n| n.and_utc()));
    let Ok(utc) = parsed else { return s.to_string() };
    let tz: chrono_tz::Tz =
        state.lookup("timezone").and_then(|t| t.as_str().and_then(|t| t.parse().ok())).unwrap_or(chrono_tz::UTC);
    utc.with_timezone(&tz).format(format.unwrap_or("%b %-d, %Y %H:%M:%S")).to_string()
}

/// Carbon `diffForHumans()` relative to now (English short form).
fn ago_filter(state: &State, v: Value) -> String {
    let Some(s) = v.as_str() else { return String::new() };
    let parsed = chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").map(|n| n.and_utc()));
    let Ok(t) = parsed else { return s.to_string() };
    diff_for_humans(&locale(state), t, chrono::Utc::now())
}

pub fn diff_for_humans(_locale: &str, t: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (now - t).num_seconds();
    let (past, s) = (secs >= 0, secs.unsigned_abs());
    let (n, unit) = match s {
        0..=59 => (s.max(1), "second"),
        60..=3599 => (s / 60, "minute"),
        3600..=86399 => (s / 3600, "hour"),
        86400..=604799 => (s / 86400, "day"),
        604800..=2_629_799 => (s / 604800, "week"),
        2_629_800..=31_557_599 => (s / 2_629_800, "month"),
        _ => (s / 31_557_600, "year"),
    };
    let plural = if n == 1 { "" } else { "s" };
    if past { format!("{n} {unit}{plural} ago") } else { format!("{n} {unit}{plural} from now") }
}

/// `{{ csrf_field() }}`.
fn csrf_field(state: &State) -> Value {
    let token = state.lookup("csrf").map(|v| v.to_string()).unwrap_or_default();
    Value::from_safe_string(format!(r#"<input type="hidden" name="_token" value="{token}">"#))
}

/// `{{ icon('tabler-name') }}`: inline SVG from `assets/icons/*.svg`.
/// Escape text for HTML built in Rust (`Cell::Html`).
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#039;")
}

/// Inline icon markup, as the `icon()` template function renders it.
pub fn icon_html(name: &str, class: &str) -> String {
    assets::icon_svg(name).unwrap_or_default().replacen(
        "<svg",
        &format!(r#"<svg class="{class}" aria-hidden="true""#),
        1,
    )
}

fn icon(name: &str, rest: Rest<Value>) -> Value {
    let class = rest.first().map(|v| v.to_string()).unwrap_or_else(|| "icon".into());
    Value::from_safe_string(icon_html(name, &class))
}

fn build(views_path: Option<PathBuf>) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(move |name| {
        if name.split('/').any(|seg| seg == "..") {
            return Ok(None);
        }
        if let Some(dir) = &views_path {
            let p = dir.join(name);
            if p.is_file() {
                return std::fs::read_to_string(&p)
                    .map(Some)
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, "reading template").with_source(e));
            }
        }
        Ok(embedded(name))
    });
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
    env.add_function("trans", trans);
    env.add_function("trans_choice", trans_choice);
    env.add_function("asset", |p: &str| assets::url(p));
    env.add_function("csrf_field", csrf_field);
    env.add_function("icon", icon);
    env.add_filter("bytes", bytes_filter);
    env.add_filter("datetime", datetime_filter);
    env.add_filter("ago", ago_filter);
    env
}

pub fn init(views_path: Option<PathBuf>) {
    if ENV.get().is_none() {
        let _ = ENV.set(build(views_path));
    }
}

fn env() -> &'static Environment<'static> {
    ENV.get_or_init(|| build(None))
}

pub fn render(name: &str, ctx: Value) -> Result<String, Error> {
    env().get_template(name)?.render(ctx)
}

/// Render one named block (htmx partial responses). `eval_to_state` avoids rendering
/// the whole template first, unlike its `render_captured` replacement.
#[allow(deprecated)]
pub fn render_block(name: &str, block: &str, ctx: Value) -> Result<String, Error> {
    env().get_template(name)?.eval_to_state(ctx)?.render_block(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_bytes() {
        assert_eq!(bytes(0, true), "0 Bytes");
        assert_eq!(bytes(1536, true), "1.50 KiB");
        assert_eq!(bytes(1_000_000, false), "1.00 MB");
    }

    #[test]
    fn humans() {
        let now = chrono::Utc::now();
        assert_eq!(diff_for_humans("en", now - chrono::Duration::minutes(5), now), "5 minutes ago");
        assert_eq!(diff_for_humans("en", now + chrono::Duration::hours(1), now), "1 hour from now");
    }
}
