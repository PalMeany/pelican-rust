//! `p:info` (`InfoCommand.php`: calls `about`). Sections follow Laravel's two-column
//! `about` output: a blank line, then a green section header, then `label ..... value`
//! rows aligned to an 80-column line (`components->twoColumnDetail`). The "Pelican"
//! section (Panel/Latest Version, Up-to-Date) and the "Installation Directory" row come
//! from `AppServiceProvider.php:107-115`; the database/mail/backup values are read from
//! the `.env` like the original's config reads them.

use std::io::{IsTerminal, Write};

use clap::Subcommand;

use super::Ctx;
use crate::console::Console;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Displays the application, database, email and backup configurations along with the panel version.
    #[command(name = "p:info")]
    Info,
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    match cmd {
        Command::Info => Ok(info(ctx, io).await),
    }
}

/// `Illuminate\Console\Concerns\InteractsWithIO::twoColumnDetail()` over an 80-column
/// line: two spaces, the label, dots up to the value column, then the value.
fn detail(label: &str, value: &str) {
    const WIDTH: usize = 80;
    let used = label.chars().count() + value.chars().count() + 5;
    let dots = WIDTH.saturating_sub(used).max(5);
    println!("  {label}{}{value}", ".".repeat(dots));
}

/// `<fg=green;options=bold>` etc.; colors only on a terminal.
fn paint(code: &str, text: &str) -> String {
    if std::io::stdout().is_terminal() { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
}

fn section(name: &str) {
    println!();
    println!("  {}", paint("1;32", name));
}

async fn info(ctx: &Ctx, io: &Console) -> i32 {
    let st = match ctx.state().await {
        Ok(st) => st,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    // SoftwareVersionService::latestPanelVersion() — the GitHub release check.
    let latest = pelican_api::versions::latest_panel_version(&st).await;

    section("Pelican");
    detail("Panel Version", &pelican_api::versions::current_panel_version());
    detail("Latest Version", latest.as_deref().unwrap_or("Unknown"));
    let status = match (&latest, pelican_api::versions::current_comparable_version()) {
        (_, None) => paint("33;1", "Unknown"),
        (None, Some(_)) => paint("33;1", "Unknown"),
        (Some(l), Some(c)) => {
            if pelican_core::php::version_compare(c, l).is_ge() {
                paint("32;1", "Yes")
            } else {
                paint("31;1", "No")
            }
        }
    };
    detail("Up-to-Date", &status);

    section("Environment");
    detail("Application Name", &st.config.app_name);
    detail("Debug Mode", &if st.config.app_debug { paint("33;1", "ON") } else { paint("32;1", "OFF") });
    detail("URL", &st.config.app_url);
    // The served panel is never down for maintenance from the CLI's view.
    detail("Maintenance Mode", &paint("32;1", "DISABLED"));
    detail("Installation Directory", &ctx.path.display().to_string());

    section("Database");
    let db = &st.config.db;
    detail("Driver", db.driver.as_str());
    detail("Database", &db.database);
    if db.driver != pelican_db::connect::Driver::Sqlite {
        detail("Host", &db.host);
        detail("Port", &db.port.to_string());
    }
    if db.driver == pelican_db::connect::Driver::Pgsql && !db.schema.is_empty() {
        detail("Schema", &db.schema);
    }

    section("Mail");
    detail("Driver", &st.config.env.string_or("MAIL_MAILER", "log"));
    if let Some(from) = st.config.env.string("MAIL_FROM_ADDRESS").filter(|s| !s.is_empty()) {
        detail("From Address", &from.trim_matches('"').to_string());
    }

    section("Backups");
    detail("Prune Age", &format!("{} minutes", st.config.env.int_or("BACKUP_PRUNE_AGE", 360)));
    detail("Throttle Limit", &st.config.env.int_or("BACKUP_THROTTLE_LIMIT", 2).to_string());
    detail("Throttle Period", &format!("{} seconds", st.config.env.int_or("BACKUP_THROTTLE_PERIOD", 600)));
    println!();
    let _ = std::io::stdout().flush();
    0
}
