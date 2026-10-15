//! `p:environment:*` and `p:redis:setup`
//! (`app/Console/Commands/Environment`, `App\Traits\EnvironmentWriterTrait`,
//! `App\Traits\Commands\RequestRedisSettingsTrait`).
//!
//! These commands never open the database connection of [`Ctx::state`]; they ask
//! questions over `.env` values and write back through
//! `pelican_core::env::write_variables`. Laravel-side steps with no Rust counterpart
//! (`config:clear`, `queue:restart`, `storage:link`, `filament:optimize`) are skipped:
//! configuration is re-read at runtime instead (ADR-0014) and cache/session/queue run
//! in-process (ADR-0009).

use clap::{Args, Subcommand};
use pelican_core::env::{Env, EnvWrite, write_variables};

use super::Ctx;
use crate::console::{Console, t};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Configure basic environment settings for the Panel.
    #[command(name = "p:environment:setup")]
    Setup(SetupArgs),
    /// Configure database settings for the Panel.
    #[command(name = "p:environment:database")]
    Database(DatabaseArgs),
    /// Set or update the email sending configuration for the Panel.
    #[command(name = "p:environment:mail")]
    Mail(MailArgs),
    /// Configure cache settings for the Panel.
    #[command(name = "p:environment:cache")]
    Cache(CacheArgs),
    /// Configure session settings for the Panel.
    #[command(name = "p:environment:session")]
    Session(SessionArgs),
    /// Configure queue settings for the Panel.
    #[command(name = "p:environment:queue")]
    Queue(QueueArgs),
    /// Create the service for the queue worker.
    #[command(name = "p:environment:queue-service")]
    QueueService(QueueServiceArgs),
    /// Configure the Panel to use Redis as cache, queue and session driver.
    #[command(name = "p:redis:setup")]
    Redis(RedisArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    Ok(match cmd {
        Command::Setup(a) => setup(ctx, io, a)?,
        Command::Database(a) => database(ctx, io, a).await,
        Command::Mail(a) => mail(ctx, io, a)?,
        Command::Cache(a) => cache(ctx, io, a)?,
        Command::Session(a) => session(ctx, io, a)?,
        Command::Queue(a) => queue(ctx, io, a)?,
        Command::QueueService(_) => queue_service(io),
        Command::Redis(a) => redis(ctx, io, a)?,
    })
}

/// Ordered `KEY => value` map (`protected array $variables`).
#[derive(Default)]
struct Vars(Vec<(String, EnvWrite)>);

impl Vars {
    fn set(&mut self, key: &str, value: EnvWrite) {
        let value = (key.to_string(), value);
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => *slot = value,
            None => self.0.push(value),
        }
    }

    fn get(&self, key: &str) -> Option<&str> {
        match &self.0.iter().find(|(k, _)| k == key)?.1 {
            EnvWrite::Str(s) | EnvWrite::Raw(s) => Some(s.as_str()),
            EnvWrite::Null => Some(""),
        }
    }

    fn write(&self, env_path: &std::path::Path) -> std::io::Result<()> {
        write_variables(env_path, &self.0, true)
    }
}

fn option_or_ask(a: Option<String>, io: &Console, question: &str, default: Option<&str>) -> String {
    a.or_else(|| io.ask(question, default)).unwrap_or_default()
}

fn option_or_secret(a: Option<String>, io: &Console, question: &str) -> String {
    a.or_else(|| io.secret(question)).unwrap_or_default()
}

/// Translation of a `command/messages.environment.mail.*` message.
fn mq(key: &str) -> String {
    t(&format!("command/messages.environment.mail.{key}"), &[])
}

/// `$this->output->note(...)`: SymfonyStyle's yellow `! [NOTE]` block.
fn note(io: &Console, text: &str) {
    io.warn(&format!(" ! [NOTE] {text}"));
}

/// Laravel `choice($question, $assoc_array, $default_key)`: the labels are shown and
/// matched, the keys are returned. An unknown answer (no interaction without a known
/// default key) yields an empty string, as PHP's `null` written to `.env`.
fn keyed_choice(io: &Console, q: &str, options: &[(&str, &str)], default_key: Option<&str>) -> String {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let default =
        known_key(options, default_key).and_then(|k| options.iter().find(|(key, _)| *key == k)).map(|(_, l)| *l);
    let picked = io.choice(q, &labels, default).unwrap_or_default();
    // The answer comes back as a label (directly or via the index); keys are accepted too.
    options
        .iter()
        .find(|(_, l)| *l == picked)
        .or_else(|| options.iter().find(|(k, _)| *k == picked))
        .map_or(picked, |(k, _)| (*k).to_string())
}

/// `$default` of the original only when it is a valid key of the choices.
fn known_key<'a>(options: &[(&'a str, &'a str)], key: Option<&str>) -> Option<&'a str> {
    let key = key?;
    options.iter().find(|(k, _)| *k == key).map(|(k, _)| *k)
}

// ---------------------------------------------------------------------------
// p:environment:setup
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct SetupArgs {
    /// The URL that this Panel is running on.
    #[arg(long)]
    url: Option<String>,
}

/// AppSettingsCommand (`$this->handle()`); the `storage:link` / `filament:optimize`
/// steps have no Rust counterparts and are skipped.
pub fn setup(ctx: &Ctx, io: &Console, a: SetupArgs) -> anyhow::Result<i32> {
    let env_path = ctx.path.join(".env");
    if !env_path.exists() {
        io.warn("Copying example .env file");
        if std::fs::copy(ctx.path.join(".env.example"), &env_path).is_err() {
            return Ok(1);
        }
    }

    // handleAppUrl()
    let mut app_url = a.url;
    if app_url.as_deref().is_none_or(|u| u.trim().is_empty()) && io.interactive() {
        app_url = io.ask("Application URL", Some(&ctx.env.string_or("APP_URL", "http://localhost")));
    }
    let app_url = app_url.unwrap_or_default();
    if app_url.trim().is_empty() {
        io.error("Application URL is required.");
        return Ok(1);
    }
    if !app_url.starts_with("http://") && !app_url.starts_with("https://") {
        io.error("Application URL need to start with either http:// or https://.");
        return Ok(1);
    }

    io.warn("Writing APP_URL to .env file");
    if let Err(e) = write_variables(&env_path, &[("APP_URL".into(), EnvWrite::Str(app_url))], true) {
        io.error(&format!("Could not write .env file: {e}"));
        return Ok(1);
    }

    if ctx.env.string("APP_KEY").is_none_or(|k| k.is_empty()) {
        io.warn("Generating app key");
        if let Err(e) = crate::ensure_app_key(&ctx.env, &ctx.path) {
            io.error(&e.to_string());
            return Ok(1);
        }
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// p:environment:database
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct DatabaseArgs {
    /// The database driver backend to use.
    #[arg(long)]
    driver: Option<String>,
    /// The database to use.
    #[arg(long)]
    database: Option<String>,
    /// The schema to use for the PostgreSQL database.
    #[arg(long)]
    schema: Option<String>,
    /// The connection address for the MySQL/ MariaDB/ PostgreSQL server.
    #[arg(long)]
    host: Option<String>,
    /// The connection port for the MySQL/ MariaDB/ PostgreSQL server.
    #[arg(long)]
    port: Option<String>,
    /// Username to use when connecting to the MySQL/ MariaDB/ PostgreSQL server.
    #[arg(long)]
    username: Option<String>,
    /// Password to use for the MySQL/ MariaDB/ PostgreSQL database.
    #[arg(long)]
    password: Option<String>,
}

const DATABASE_DRIVERS: &[(&str, &str)] =
    &[("sqlite", "SQLite (recommended)"), ("mariadb", "MariaDB"), ("mysql", "MySQL"), ("pgsql", "PostgreSQL")];

/// DatabaseSettingsCommand. `$this->handle()` is a loop here (the original recurses
/// into itself when the user chooses to go back after a failed test connection).
pub async fn database(ctx: &Ctx, io: &Console, a: DatabaseArgs) -> i32 {
    loop {
        io.error("Changing the database driver will NOT move any database data!");
        io.error("Please make sure you made a database backup first!");
        io.error("After changing the driver you will have to manually move the old data to the new database.");
        if !io.confirm("Do you want to continue?", false) {
            return 1;
        }

        let selected = ctx.env.string_or("DB_CONNECTION", "sqlite");
        let mut vars = Vars::default();
        let driver = match a.driver.clone() {
            Some(d) => d,
            None => keyed_choice(io, "Database Driver", DATABASE_DRIVERS, known_key(DATABASE_DRIVERS, Some(&selected))),
        };
        vars.set("DB_CONNECTION", EnvWrite::Str(driver));

        match vars.get("DB_CONNECTION").unwrap_or_default() {
            d @ ("mysql" | "mariadb") => {
                let label = if d == "mysql" { "MySQL" } else { "MariaDB" };
                note(io, &t("commands.database_settings.DB_HOST_note", &[]));
                vars.set(
                    "DB_HOST",
                    EnvWrite::Str(option_or_ask(
                        a.host.clone(),
                        io,
                        "Database Host",
                        Some(&ctx.env.string_or("DB_HOST", "127.0.0.1")),
                    )),
                );
                vars.set(
                    "DB_PORT",
                    EnvWrite::Str(option_or_ask(
                        a.port.clone(),
                        io,
                        "Database Port",
                        Some(&ctx.env.string_or("DB_PORT", "3306")),
                    )),
                );
                vars.set(
                    "DB_DATABASE",
                    EnvWrite::Str(option_or_ask(
                        a.database.clone(),
                        io,
                        "Database Name",
                        Some(&ctx.env.string_or("DB_DATABASE", "panel")),
                    )),
                );
                username_note_and_question(io, &mut vars, &a.username);
                password_flow(&ctx.env, io, &mut vars, &a.password, "Database Password");
                match test_connection(ctx, &vars).await {
                    Ok(()) => (),
                    Err(e) => {
                        io.error(&format!(
                            "Unable to connect to the {label} server using the provided credentials. \
                             The error returned was \"{}\".",
                            e
                        ));
                        io.error(&t("commands.database_settings.DB_error_2", &[]));
                        if !io.confirm(&t("commands.database_settings.go_back", &[]), false) {
                            return 1;
                        }
                        continue;
                    }
                }
            }
            "pgsql" => {
                note(io, &t("commands.database_settings.DB_HOST_note", &[]));
                vars.set(
                    "DB_HOST",
                    EnvWrite::Str(option_or_ask(
                        a.host.clone(),
                        io,
                        "Database Host",
                        Some(&ctx.env.string_or("DB_HOST", "127.0.0.1")),
                    )),
                );
                vars.set(
                    "DB_PORT",
                    EnvWrite::Str(option_or_ask(
                        a.port.clone(),
                        io,
                        "Database Port",
                        Some(&ctx.env.string_or("DB_PORT", "5432")),
                    )),
                );
                vars.set(
                    "DB_DATABASE",
                    EnvWrite::Str(option_or_ask(
                        a.database.clone(),
                        io,
                        "Database Name",
                        Some(&ctx.env.string_or("DB_DATABASE", "panel")),
                    )),
                );
                vars.set(
                    "DB_SCHEMA",
                    EnvWrite::Str(option_or_ask(
                        a.schema.clone(),
                        io,
                        "Database Schema",
                        Some(&ctx.env.string_or("DB_SCHEMA", "public")),
                    )),
                );
                username_note_and_question(io, &mut vars, &a.username);
                password_flow(&ctx.env, io, &mut vars, &a.password, "Database Password");
                match test_connection(ctx, &vars).await {
                    Ok(()) => (),
                    Err(e) => {
                        io.error(&format!(
                            "Unable to connect to the PostgreSQL server using the provided credentials. \
                             The error returned was \"{}\".",
                            e
                        ));
                        io.error(&t("commands.database_settings.DB_error_2", &[]));
                        if !io.confirm(&t("commands.database_settings.go_back", &[]), false) {
                            return 1;
                        }
                        continue;
                    }
                }
            }
            "sqlite" => {
                vars.set(
                    "DB_DATABASE",
                    EnvWrite::Str(option_or_ask(
                        a.database.clone(),
                        io,
                        "Database Path",
                        Some(&ctx.env.string_or("DB_DATABASE", "database.sqlite")),
                    )),
                );
            }
            _ => (), // An unsupported driver given via --driver is written unchanged.
        }

        if let Err(e) = vars.write(&ctx.path.join(".env")) {
            io.error(&format!("Could not write .env file: {e}"));
            return 1;
        }
        // `$this->info($this->console->output())` echoes buffered outputs of artisan's
        // `config:clear`; there is no config cache to clear (ADR-0014).

        return 0;
    }
}

/// `trans('commands.database_settings.DB_USERNAME_note')` + `--username`/question.
fn username_note_and_question(io: &Console, vars: &mut Vars, username: &Option<String>) {
    note(io, &t("commands.database_settings.DB_USERNAME_note", &[]));
    vars.set("DB_USERNAME", EnvWrite::Str(option_or_ask(username.clone(), io, "Database Username", Some("pelican"))));
}

/// The existing password flow (kept when confirmed unchanged) + `--password`/secret.
fn password_flow(env: &Env, io: &Console, vars: &mut Vars, password: &Option<String>, question: &str) {
    let current = env.string("DB_PASSWORD").filter(|p| !p.is_empty());
    let mut ask_for_password = true;
    if current.is_some() && io.interactive() {
        vars.set("DB_PASSWORD", EnvWrite::Str(current.unwrap_or_default()));
        ask_for_password = io.confirm(&t("commands.database_settings.DB_PASSWORD_note", &[]), false);
    }
    if ask_for_password {
        vars.set("DB_PASSWORD", EnvWrite::Str(option_or_secret(password.clone(), io, question)));
    }
}

/// Connect with the entered values, the way `pelican-db` connects at boot.
async fn test_connection(ctx: &Ctx, vars: &Vars) -> Result<(), String> {
    let cfg =
        pelican_db::DbConfig::from_lookup(|k| vars.get(k).filter(|v| !v.is_empty()).map(str::to_string), &ctx.path)
            .map_err(String::from)?;
    pelican_db::connect(&cfg).await.map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// p:environment:mail
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct MailArgs {
    /// The mail driver to use.
    #[arg(long)]
    driver: Option<String>,
    /// Email address that messages from the Panel will originate from.
    #[arg(long)]
    email: Option<String>,
    /// The name emails from the Panel will appear to be from.
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    encryption: Option<String>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    port: Option<String>,
    #[arg(long)]
    endpoint: Option<String>,
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    password: Option<String>,
}

const MAIL_DRIVERS: &[(&str, &str)] = &[
    ("log", "Log"),
    ("smtp", "SMTP Server"),
    ("sendmail", "sendmail Binary"),
    ("mailgun", "Mailgun"),
    ("mandrill", "Mandrill"),
    ("postmark", "Postmark"),
];

const MAIL_ENCRYPTION: &[(&str, &str)] = &[("tls", "TLS"), ("ssl", "SSL"), ("", "None")];

/// EmailSettingsCommand; the final `queue:restart` call has no counterpart (the queue
/// runs in-process, ADR-0009) and is skipped.
pub fn mail(ctx: &Ctx, io: &Console, a: MailArgs) -> anyhow::Result<i32> {
    let mut vars = Vars::default();

    let selected =
        ctx.env.string("MAIL_MAILER").or_else(|| ctx.env.string("MAIL_DRIVER")).unwrap_or_else(|| "smtp".into());
    // `$this->option('driver') ?? $this->choice(...)`: a given driver skips the choice.
    let mailer = match a.driver.clone() {
        Some(d) => d,
        None => keyed_choice(io, &mq("ask_driver"), MAIL_DRIVERS, known_key(MAIL_DRIVERS, Some(&selected))),
    };
    vars.set("MAIL_MAILER", EnvWrite::Str(mailer));

    match vars.get("MAIL_MAILER").unwrap_or_default() {
        "smtp" => {
            vars.set(
                "MAIL_HOST",
                EnvWrite::Str(option_or_ask(
                    a.host.clone(),
                    io,
                    &mq("ask_smtp_host"),
                    Some(&ctx.env.string_or("MAIL_HOST", "127.0.0.1")),
                )),
            );
            vars.set(
                "MAIL_PORT",
                EnvWrite::Str(option_or_ask(
                    a.port.clone(),
                    io,
                    &mq("ask_smtp_port"),
                    Some(&ctx.env.string_or("MAIL_PORT", "2525")),
                )),
            );
            vars.set(
                "MAIL_USERNAME",
                EnvWrite::Str(option_or_ask(
                    a.username.clone(),
                    io,
                    &mq("ask_smtp_username"),
                    ctx.env.raw("MAIL_USERNAME"),
                )),
            );

            vars.set(
                "MAIL_PASSWORD",
                EnvWrite::Str(option_or_secret(a.password.clone(), io, &mq("ask_smtp_password"))),
            );
            // config('mail.mailers.smtp.encryption', 'tls'): Pelican's config/mail.php has
            // no `encryption` key anymore, so the original always falls back to `tls`.
            let scheme = match a.encryption.clone() {
                Some(e) => e,
                None => keyed_choice(io, &mq("ask_encryption"), MAIL_ENCRYPTION, Some("tls")),
            };
            vars.set("MAIL_SCHEME", EnvWrite::Str(scheme));
        }
        "mailgun" => {
            vars.set(
                "MAILGUN_DOMAIN",
                EnvWrite::Str(option_or_ask(
                    a.host.clone(),
                    io,
                    &mq("ask_mailgun_domain"),
                    ctx.env.raw("MAILGUN_DOMAIN"),
                )),
            );
            vars.set(
                "MAILGUN_SECRET",
                EnvWrite::Str(option_or_ask(
                    a.password.clone(),
                    io,
                    &mq("ask_mailgun_secret"),
                    ctx.env.raw("MAILGUN_SECRET"),
                )),
            );
            vars.set(
                "MAILGUN_ENDPOINT",
                EnvWrite::Str(option_or_ask(
                    a.endpoint.clone(),
                    io,
                    &mq("ask_mailgun_endpoint"),
                    Some(&ctx.env.string_or("MAILGUN_ENDPOINT", "api.mailgun.net")),
                )),
            );
        }
        "mandrill" => {
            vars.set(
                "MANDRILL_SECRET",
                EnvWrite::Str(option_or_ask(
                    a.password.clone(),
                    io,
                    &mq("ask_mandrill_secret"),
                    ctx.env.raw("MANDRILL_SECRET"),
                )),
            );
        }
        "postmark" => {
            vars.set("MAIL_DRIVER", EnvWrite::Str("smtp".into()));
            vars.set("MAIL_HOST", EnvWrite::Str("smtp.postmarkapp.com".into()));
            vars.set("MAIL_PORT", EnvWrite::Raw("587".into()));
            let token = option_or_ask(a.username.clone(), io, &mq("ask_postmark_username"), None);
            vars.set("MAIL_USERNAME", EnvWrite::Str(token.clone()));
            vars.set("MAIL_PASSWORD", EnvWrite::Str(token));
        }
        _ => (), // log/sendmail (or an unknown --driver): no extra variables.
    }

    vars.set(
        "MAIL_FROM_ADDRESS",
        EnvWrite::Str(option_or_ask(
            a.email.clone(),
            io,
            &mq("ask_mail_from"),
            Some(&ctx.env.string_or("MAIL_FROM_ADDRESS", "hello@example.com")),
        )),
    );
    vars.set(
        "MAIL_FROM_NAME",
        EnvWrite::Str(option_or_ask(
            a.from.clone(),
            io,
            &mq("ask_mail_name"),
            Some(&ctx.env.string_or("MAIL_FROM_NAME", "Example")),
        )),
    );

    if let Err(e) = vars.write(&ctx.path.join(".env")) {
        io.error(&format!("Could not write .env file: {e}"));
        return Ok(1);
    }

    // `$this->call('queue:restart')` is skipped: nothing in-process to restart.
    io.line("Updating stored environment configuration file.");
    io.line("");
    Ok(0)
}

// ---------------------------------------------------------------------------
// RequestRedisSettingsTrait (shared by cache/ session/ queue/ redis commands)
// ---------------------------------------------------------------------------

#[derive(Args, Default, Debug)]
pub struct RedisOptions {
    /// Redis host to use for connections.
    #[arg(long = "redis-host")]
    redis_host: Option<String>,
    /// User used to connect to redis.
    #[arg(long = "redis-user")]
    redis_user: Option<String>,
    /// Password used to connect to redis.
    #[arg(long = "redis-pass")]
    redis_pass: Option<String>,
    /// Port to connect to redis over.
    #[arg(long = "redis-port")]
    redis_port: Option<String>,
}

/// One line about the pelican-rs drivers, replacing the nested
/// `p:environment:queue-service --overwrite` call at the end of the redis branches.
fn warn_in_process_drivers(io: &Console) {
    io.warn(
        "pelican-rs currently uses in-process drivers: cache, sessions and the queue \
         live inside `pelican serve`.",
    );
}

fn request_redis_settings(env: &Env, io: &Console, r: &RedisOptions, vars: &mut Vars) {
    note(io, &t("commands.appsettings.redis.note", &[]));
    vars.set(
        "REDIS_HOST",
        EnvWrite::Str(option_or_ask(
            r.redis_host.clone(),
            io,
            "Redis Host",
            Some(&env.string_or("REDIS_HOST", "127.0.0.1")),
        )),
    );

    let comment = t("commands.appsettings.redis.comment", &[]);
    let confirm_field = |field: &str| t("commands.appsettings.redis.confirm", &[("field", field)]);

    // Both keep-it questions come before the hidden prompts, as in the original.
    let mut ask_for_user = true;
    let mut ask_for_password = true;
    if let Some(user) = env.string("REDIS_USERNAME").filter(|u| !u.is_empty()) {
        vars.set("REDIS_USERNAME", EnvWrite::Str(user));
        ask_for_user = io.confirm(&confirm_field("user"), false);
    }
    if let Some(password) = env.string("REDIS_PASSWORD").filter(|p| !p.is_empty()) {
        vars.set("REDIS_PASSWORD", EnvWrite::Str(password));
        ask_for_password = io.confirm(&confirm_field("password"), false);
    }
    if ask_for_user {
        io.info(&comment);
        vars.set("REDIS_USERNAME", EnvWrite::Str(option_or_secret(r.redis_user.clone(), io, "Redis User")));
    }
    if ask_for_password {
        io.info(&comment);
        vars.set("REDIS_PASSWORD", EnvWrite::Str(option_or_secret(r.redis_pass.clone(), io, "Redis Password")));
    }

    if vars.get("REDIS_USERNAME").is_none_or(|u| u.is_empty()) {
        vars.set("REDIS_USERNAME", EnvWrite::Str("null".into()));
    }
    if vars.get("REDIS_PASSWORD").is_none_or(|p| p.is_empty()) {
        vars.set("REDIS_PASSWORD", EnvWrite::Str("null".into()));
    }

    vars.set(
        "REDIS_PORT",
        EnvWrite::Str(option_or_ask(
            r.redis_port.clone(),
            io,
            "Redis Port",
            Some(&env.string_or("REDIS_PORT", "6379")),
        )),
    );
}

// ---------------------------------------------------------------------------
// p:environment:cache / p:environment:session / p:environment:queue
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct CacheArgs {
    /// The cache driver backend to use.
    #[arg(long)]
    driver: Option<String>,
    #[command(flatten)]
    redis: RedisOptions,
}

const CACHE_DRIVERS: &[(&str, &str)] =
    &[("file", "Filesystem (default)"), ("database", "Database"), ("redis", "Redis")];

pub fn cache(ctx: &Ctx, io: &Console, a: CacheArgs) -> anyhow::Result<i32> {
    // config('cache.default'): `CACHE_STORE`, legacy `CACHE_DRIVER`, else `file`.
    let selected =
        ctx.env.string("CACHE_STORE").or_else(|| ctx.env.string("CACHE_DRIVER")).unwrap_or_else(|| "file".into());
    let mut vars = Vars::default();
    // `$this->option('driver') ?? $this->choice(...)`: a given driver skips the choice.
    let driver = match a.driver.clone() {
        Some(d) => d,
        None => keyed_choice(io, "Cache Driver", CACHE_DRIVERS, known_key(CACHE_DRIVERS, Some(&selected))),
    };
    vars.set("CACHE_STORE", EnvWrite::Str(driver));
    finish_driver_command(ctx, io, vars.get("CACHE_STORE").unwrap_or_default() == "redis", &a.redis, &mut vars)?;
    vars.write(&ctx.path.join(".env")).map_err(anyhow::Error::from)?;
    Ok(0)
}

#[derive(Args, Debug)]
pub struct SessionArgs {
    /// The session driver backend to use.
    #[arg(long)]
    driver: Option<String>,
    #[command(flatten)]
    redis: RedisOptions,
}

const SESSION_DRIVERS: &[(&str, &str)] =
    &[("file", "Filesystem (default)"), ("redis", "Redis"), ("database", "Database"), ("cookie", "Cookie")];

pub fn session(ctx: &Ctx, io: &Console, a: SessionArgs) -> anyhow::Result<i32> {
    let selected = ctx.env.string_or("SESSION_DRIVER", "file");
    let mut vars = Vars::default();
    // `$this->option('driver') ?? $this->choice(...)`: a given driver skips the choice.
    let driver = match a.driver.clone() {
        Some(d) => d,
        None => keyed_choice(io, "Session Driver", SESSION_DRIVERS, known_key(SESSION_DRIVERS, Some(&selected))),
    };
    vars.set("SESSION_DRIVER", EnvWrite::Str(driver));
    finish_driver_command(ctx, io, vars.get("SESSION_DRIVER").unwrap_or_default() == "redis", &a.redis, &mut vars)?;
    vars.write(&ctx.path.join(".env")).map_err(anyhow::Error::from)?;
    Ok(0)
}

#[derive(Args, Debug)]
pub struct QueueArgs {
    /// The queue driver backend to use.
    #[arg(long)]
    driver: Option<String>,
    #[command(flatten)]
    redis: RedisOptions,
}

const QUEUE_DRIVERS: &[(&str, &str)] =
    &[("database", "Database (default)"), ("redis", "Redis"), ("sync", "Synchronous")];

pub fn queue(ctx: &Ctx, io: &Console, a: QueueArgs) -> anyhow::Result<i32> {
    // Laravel's framework config: `queue.default` = `QUEUE_CONNECTION` or `database`.
    let selected = ctx.env.string_or("QUEUE_CONNECTION", "database");
    let mut vars = Vars::default();
    // `$this->option('driver') ?? $this->choice(...)`: a given driver skips the choice.
    let driver = match a.driver.clone() {
        Some(d) => d,
        None => keyed_choice(io, "Queue Driver", QUEUE_DRIVERS, known_key(QUEUE_DRIVERS, Some(&selected))),
    };
    vars.set("QUEUE_CONNECTION", EnvWrite::Str(driver));
    finish_driver_command(ctx, io, vars.get("QUEUE_CONNECTION").unwrap_or_default() == "redis", &a.redis, &mut vars)?;
    vars.write(&ctx.path.join(".env")).map_err(anyhow::Error::from)?;
    Ok(0)
}

/// The shared tail of the three driver settings commands: redis connection questions
/// plus the warnings replacing the nested artisan calls; then `.env` was NOT written
/// yet (the caller writes `vars`).
fn finish_driver_command(
    ctx: &Ctx,
    io: &Console,
    uses_redis: bool,
    redis: &RedisOptions,
    vars: &mut Vars,
) -> anyhow::Result<()> {
    if uses_redis {
        request_redis_settings(&ctx.env, io, redis, vars);
        // `p:environment:queue-service --overwrite`: not applicable, ADR-0009.
        warn_in_process_drivers(io);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// p:environment:queue-service
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct QueueServiceArgs {
    /// Name of the queue worker service.
    #[arg(long = "service-name")]
    service_name: Option<String>,
    /// The user that PHP runs under.
    #[arg(long)]
    user: Option<String>,
    /// The group that PHP runs under.
    #[arg(long)]
    group: Option<String>,
    /// Force overwrite if the service file already exists.
    #[arg(long)]
    overwrite: bool,
}

/// QueueWorkerServiceCommand: creates a systemd unit for `php ... queue:work`. The
/// pelican-rs queue runs inside `pelican serve` (ADR-0009), so there is no unit to
/// manage; the options are accepted for compatibility only.
pub fn queue_service(io: &Console) -> i32 {
    io.warn(
        "A separate queue worker service is not required for pelican-rs: the queue runs \
         inside `pelican serve`.",
    );
    0
}

// ---------------------------------------------------------------------------
// p:redis:setup
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct RedisArgs {
    #[command(flatten)]
    redis: RedisOptions,
}

/// RedisSetupCommand; like the original it forces all three drivers to `redis`.
pub fn redis(ctx: &Ctx, io: &Console, a: RedisArgs) -> anyhow::Result<i32> {
    let mut vars = Vars::default();
    vars.set("CACHE_STORE", EnvWrite::Str("redis".into()));
    vars.set("QUEUE_CONNECTION", EnvWrite::Str("redis".into()));
    vars.set("SESSION_DRIVER", EnvWrite::Str("redis".into()));

    request_redis_settings(&ctx.env, io, &a.redis, &mut vars);
    // `p:environment:queue-service --overwrite`: not applicable, ADR-0009.
    warn_in_process_drivers(io);

    if let Err(e) = vars.write(&ctx.path.join(".env")) {
        io.error(&format!("Could not write .env file: {e}"));
        return Ok(1);
    }
    // `$this->info($this->console->output())`: nothing buffered to echo.
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console() -> Console {
        // Not a terminal and without `-n`: questions answer their defaults.
        Console::new(false)
    }

    fn env(map: &[(&str, &str)]) -> Env {
        Env::from_map(map.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect())
    }

    /// The choice default maps back to its key; unknown selected values have no default.
    #[test]
    fn keyed_choice_default() {
        let io = console();
        assert_eq!(keyed_choice(&io, "q", CACHE_DRIVERS, known_key(CACHE_DRIVERS, Some("file"))), "file");
        assert_eq!(keyed_choice(&io, "q", CACHE_DRIVERS, known_key(CACHE_DRIVERS, Some("array"))), "");
        assert_eq!(keyed_choice(&io, "q", MAIL_ENCRYPTION, known_key(MAIL_ENCRYPTION, Some("tls")),), "tls");
        // Without a default key there is nothing to answer (`null` in PHP too).
        let labels: Vec<&str> = MAIL_ENCRYPTION.iter().map(|(_, l)| *l).collect();
        let none = io.choice("q", &labels, None);
        assert_eq!(none, None);
    }

    /// Setting the same key again replaces it, like PHP array assignment.
    #[test]
    fn vars_replace() {
        let mut vars = Vars::default();
        vars.set("REDIS_USERNAME", EnvWrite::Str("old".into()));
        vars.set("REDIS_USERNAME", EnvWrite::Str("new".into()));
        vars.set("REDIS_HOST", EnvWrite::Str("h".into()));
        assert_eq!(
            vars.0,
            vec![
                ("REDIS_USERNAME".into(), EnvWrite::Str("new".into())),
                ("REDIS_HOST".into(), EnvWrite::Str("h".into()))
            ]
        );
    }

    /// Non-interactive redis settings with no configured user/password ask neither
    /// question and fall back to `null` like the original's empty answers.
    #[test]
    fn redis_settings_defaults() {
        let mut vars = Vars::default();
        request_redis_settings(&env(&[]), &console(), &RedisOptions::default(), &mut vars);
        let want = [
            ("REDIS_HOST", EnvWrite::Str("127.0.0.1".into())),
            ("REDIS_USERNAME", EnvWrite::Str("null".into())),
            ("REDIS_PASSWORD", EnvWrite::Str("null".into())),
            ("REDIS_PORT", EnvWrite::Str("6379".into())),
        ];
        for (key, value) in want {
            assert_eq!(
                vars.get(key),
                Some(match value {
                    EnvWrite::Str(ref s) => s.as_str(),
                    _ => "",
                })
            );
        }
    }

    /// A configured username kept after declining the change (`--redis-user` unset).
    #[test]
    fn redis_settings_keep_existing() {
        let mut vars = Vars::default();
        let e = env(&[
            ("REDIS_HOST", "r.example"),
            ("REDIS_USERNAME", "default"),
            ("REDIS_PASSWORD", "secret"),
            ("REDIS_PORT", "6381"),
        ]);
        request_redis_settings(&e, &console(), &RedisOptions::default(), &mut vars);
        // confirm() defaults to false without interaction: existing values are kept.
        assert_eq!(vars.0[0].0, "REDIS_HOST");
        assert_eq!(vars.0[1], ("REDIS_USERNAME".into(), EnvWrite::Str("default".into())));
        assert_eq!(vars.0[2], ("REDIS_PASSWORD".into(), EnvWrite::Str("secret".into())));
        assert_eq!(vars.0[3], ("REDIS_PORT".into(), EnvWrite::Str("6381".into())));
    }

    /// Options override the questions at every step of the redis settings.
    #[test]
    fn redis_settings_options() {
        let mut vars = Vars::default();
        let r = RedisOptions {
            redis_host: Some("redis.internal".into()),
            redis_user: Some(String::new()),
            redis_pass: None,
            redis_port: Some("6399".into()),
        };
        request_redis_settings(&env(&[]), &console(), &r, &mut vars);
        assert_eq!(vars.get("REDIS_HOST"), Some("redis.internal"));
        assert_eq!(vars.get("REDIS_USERNAME"), Some("null")); // empty answer -> null
        assert_eq!(vars.get("REDIS_PASSWORD"), Some("null"));
        assert_eq!(vars.get("REDIS_PORT"), Some("6399"));
    }

    /// Without interaction an existing password is silently replaced by an empty one
    /// (`ask_for_password` stays true because the keep-it question is never asked).
    #[test]
    fn password_flow_no_interaction() {
        let mut vars = Vars::default();
        password_flow(&env(&[("DB_PASSWORD", "keep")]), &console(), &mut vars, &None, "Database Password");
        assert_eq!(vars.get("DB_PASSWORD"), Some(""));
    }

    /// SQLite keeps only the database path (per branch).
    #[test]
    fn sqlite_vars() {
        let mut vars = Vars::default();
        vars.set("DB_CONNECTION", EnvWrite::Str("sqlite".into()));
        vars.set("DB_DATABASE", EnvWrite::Str("panel.sqlite".into()));
        assert_eq!(vars.0.len(), 2);
    }
}
