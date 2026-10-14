//! Laravel's `daily`/`single` log channels: our tracing events are appended to
//! `storage/logs/laravel-YYYY-MM-DD.log` (or `laravel.log`) in Monolog's line
//! format, so the admin log viewer shows them next to the original panel's logs.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{NaiveDate, Utc};
use pelican_core::env::Env;
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub struct LaravelLog {
    dir: PathBuf,
    daily: bool,
    days: usize,
    /// `app.env` (Monolog channel name).
    channel: String,
    /// Most verbose level written (`LOG_LEVEL`).
    max: Level,
    tz: chrono_tz::Tz,
    file: Mutex<Option<(NaiveDate, File)>>,
}

impl LaravelLog {
    /// The configured channel (`LOG_CHANNEL`, default `daily` in `config/logging.php`);
    /// None for channels that do not write files.
    pub fn from_env(env: &Env, base: &std::path::Path) -> Option<LaravelLog> {
        let daily = match env.string_or("LOG_CHANNEL", "daily").as_str() {
            "daily" => true,
            "single" => false,
            "stack" => match env.string_or("LOG_STACK", "single").split(',').next().unwrap_or("single").trim() {
                "daily" => true,
                "single" => false,
                _ => return None,
            },
            _ => return None,
        };
        let max = match env.string_or("LOG_LEVEL", "debug").to_lowercase().as_str() {
            "debug" => Level::DEBUG,
            "info" => Level::INFO,
            "notice" | "warning" => Level::WARN,
            _ => Level::ERROR,
        };
        Some(LaravelLog {
            dir: base.join("storage/logs"),
            daily,
            days: env.int_or("LOG_DAILY_DAYS", 14).max(0) as usize,
            channel: env.string_or("APP_ENV", "production"),
            max,
            tz: env.string_or("APP_TIMEZONE", "UTC").parse().unwrap_or(chrono_tz::UTC),
            file: Mutex::new(None),
        })
    }

    fn write(&self, line: &str, today: NaiveDate) {
        let Ok(mut guard) = self.file.lock() else { return };
        if guard.as_ref().is_none_or(|(d, _)| *d != today) {
            let _ = std::fs::create_dir_all(&self.dir);
            let name =
                if self.daily { format!("laravel-{}.log", today.format("%Y-%m-%d")) } else { "laravel.log".into() };
            let Ok(f) = OpenOptions::new().create(true).append(true).open(self.dir.join(name)) else { return };
            *guard = Some((today, f));
            if self.daily {
                self.rotate();
            }
        }
        if let Some((_, f)) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// RotatingFileHandler: keep the newest `days` files.
    fn rotate(&self) {
        if self.days == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.len() == "laravel-0000-00-00.log".len() && n.starts_with("laravel-") && n.ends_with(".log")
                })
            })
            .collect();
        files.sort();
        files.reverse();
        for old in files.into_iter().skip(self.days) {
            let _ = std::fs::remove_file(old);
        }
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    context: Map<String, Value>,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.context.insert(field.name().into(), Value::String(format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.into();
        } else {
            self.context.insert(field.name().into(), Value::String(value.into()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.context.insert(field.name().into(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.context.insert(field.name().into(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.context.insert(field.name().into(), value.into());
    }
}

impl<S: Subscriber> Layer<S> for LaravelLog {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Only the panel's own events (dependencies log at debug a lot).
        if !meta.target().starts_with("pelican") || *meta.level() > self.max {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let level = match *meta.level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARNING",
            Level::INFO => "INFO",
            _ => "DEBUG",
        };
        let now = Utc::now().with_timezone(&self.tz);
        // LineFormatter("[%datetime%] %channel%.%level_name%: %message% %context% %extra%"),
        // empty context/extra omitted (ignoreEmptyContextAndExtra).
        let context = if fields.context.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&Value::Object(fields.context)).unwrap_or_default()
        };
        let line = format!(
            "[{}] {}.{}: {} {} \n",
            now.format("%Y-%m-%d %H:%M:%S"),
            self.channel,
            level,
            fields.message,
            context
        );
        self.write(&line, now.date_naive());
    }
}
