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

pub fn diff_for_humans(locale: &str, t: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let past = now >= t;
    let (n, unit) = carbon_diff(t.min(now), t.max(now));
    if locale.starts_with("ru") {
        // Carbon's ru.php: three plural forms, accusative singulars after "назад"/"через".
        let forms: [&str; 3] = match unit {
            "year" => ["год", "года", "лет"],
            "month" => ["месяц", "месяца", "месяцев"],
            "week" => ["неделю", "недели", "недель"],
            "day" => ["день", "дня", "дней"],
            "hour" => ["час", "часа", "часов"],
            "minute" => ["минуту", "минуты", "минут"],
            _ => ["секунду", "секунды", "секунд"],
        };
        let form = match (n % 10, n % 100) {
            (1, m) if m != 11 => forms[0],
            (2..=4, m) if !(12..=14).contains(&m) => forms[1],
            _ => forms[2],
        };
        return if past { format!("{n} {form} назад") } else { format!("через {n} {form}") };
    }
    let plural = if n == 1 { "" } else { "s" };
    if past { format!("{n} {unit}{plural} ago") } else { format!("{n} {unit}{plural} from now") }
}

/// Carbon's `diffForHumans()` unit: the largest non-zero part of the calendar difference
/// (`DateTime::diff`: years and months by the calendar, weeks from days), at least one second.
fn carbon_diff(from: chrono::DateTime<chrono::Utc>, to: chrono::DateTime<chrono::Utc>) -> (u64, &'static str) {
    use chrono::{Datelike, Months};
    let mut months = (to.year() - from.year()) * 12 + to.month() as i32 - from.month() as i32;
    let (fd, td) = (from.day(), to.day());
    if months > 0 && (td < fd || (td == fd && to.time() < from.time())) {
        months -= 1;
    }
    let anchor = from.checked_add_months(Months::new(months.max(0) as u32)).unwrap_or(from);
    let rest = (to - anchor).num_seconds().max(0) as u64;
    let months = months.max(0) as u64;
    let days = rest / 86_400;
    match () {
        _ if months >= 12 => (months / 12, "year"),
        _ if months > 0 => (months, "month"),
        _ if days >= 7 => (days / 7, "week"),
        _ if days > 0 => (days, "day"),
        _ if rest >= 3600 => (rest / 3600, "hour"),
        _ if rest >= 60 => (rest / 60, "minute"),
        _ => (rest.max(1), "second"),
    }
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

#[cfg(test)]
mod diff_tests {
    use chrono::TimeZone;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.from_utc_datetime(&chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("date"))
    }

    #[test]
    fn calendar_months_like_carbon() {
        let now = at("2026-10-15 14:22:16");
        assert_eq!(super::diff_for_humans("en", at("2026-02-15 13:37:44"), now), "8 months ago");
        assert_eq!(super::diff_for_humans("en", at("2026-09-16 18:42:44"), now), "4 weeks ago");
        assert_eq!(super::diff_for_humans("en", at("2025-10-15 14:22:16"), now), "1 year ago");
        assert_eq!(super::diff_for_humans("ru", at("2026-02-15 13:37:44"), now), "8 месяцев назад");
        assert_eq!(super::diff_for_humans("ru-RU", at("2026-10-14 14:22:16"), now), "1 день назад");
        assert_eq!(super::diff_for_humans("ru", at("2026-10-15 14:20:16"), now), "2 минуты назад");
        assert_eq!(super::diff_for_humans("ru", at("2026-10-15 14:21:16"), now), "1 минуту назад");
    }
}
