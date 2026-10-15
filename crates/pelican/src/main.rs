//! `pelican` — the pelican-rs panel.
//!
//!   pelican serve [--listen 0.0.0.0:8000] [--path <panel dir with .env>]
//!   pelican db:install      create the schema in an empty database
//!   pelican db:status       show the schema state
//!   pelican schedule:process  run due schedules once (`p:schedule:process`)
//!   pelican p:<group>:<command> …  the original's `php artisan p:*` commands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pelican_core::config::Config;
use pelican_core::env::Env;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Directory containing the panel's `.env` (and `database/` for SQLite).
    #[arg(long, global = true, default_value = ".")]
    path: PathBuf,
    /// Do not ask any interactive question (Laravel's `-n`).
    #[arg(short = 'n', long = "no-interaction", global = true)]
    no_interaction: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Serve {
        #[arg(long, default_value = "0.0.0.0:8000")]
        listen: String,
    },
    #[command(name = "db:install")]
    DbInstall,
    #[command(name = "db:status")]
    DbStatus,
    #[command(name = "schedule:process", alias = "p:schedule:process")]
    ScheduleProcess,
    #[command(flatten)]
    User(pelican::cli::user::Command),
    #[command(flatten)]
    Node(pelican::cli::node::Command),
    #[command(flatten)]
    Server(pelican::cli::server::Command),
    #[command(flatten)]
    Info(pelican::cli::info::Command),
    #[command(flatten)]
    Environment(pelican::cli::environment::Command),
    #[command(flatten)]
    Egg(pelican::cli::egg::Command),
    #[command(flatten)]
    Maintenance(pelican::cli::maintenance::Command),
    #[command(flatten)]
    Plugin(pelican::cli::plugin::Command),
}

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::prelude::*;
    let cli = Cli::parse();
    let env = Env::load(&cli.path)?;
    // Console output follows RUST_LOG; the served panel also writes Laravel-style log files.
    let console = tracing_subscriber::fmt::layer()
        .with_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()));
    let file = match cli.cmd {
        Cmd::Serve { .. } => pelican::logfile::LaravelLog::from_env(&env, &cli.path),
        _ => None,
    };
    tracing_subscriber::registry().with(console).with(file).init();
    let io = pelican::console::Console::new(cli.no_interaction);
    let (path, cmd_env) = (cli.path.clone(), env.clone());
    let ctx = move || pelican::cli::Ctx { path: path.clone(), env: cmd_env.clone() };
    match cli.cmd {
        Cmd::User(c) => exit(pelican::cli::user::run(&ctx(), &io, c).await?),
        Cmd::Node(c) => exit(pelican::cli::node::run(&ctx(), &io, c).await?),
        Cmd::Server(c) => exit(pelican::cli::server::run(&ctx(), &io, c).await?),
        Cmd::Info(c) => exit(pelican::cli::info::run(&ctx(), &io, c).await?),
        Cmd::Environment(c) => exit(pelican::cli::environment::run(&ctx(), &io, c).await?),
        Cmd::Egg(c) => exit(pelican::cli::egg::run(&ctx(), &io, c).await?),
        Cmd::Maintenance(c) => exit(pelican::cli::maintenance::run(&ctx(), &io, c).await?),
        Cmd::Plugin(c) => exit(pelican::cli::plugin::run(&ctx(), &io, c).await?),
        Cmd::Serve { listen } => {
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!("listening on {listen}");
            pelican::serve_or_install(env, &cli.path, listener).await
        }
        Cmd::ScheduleProcess => {
            let state = pelican::boot(env, &cli.path).await?;
            let ran = pelican_api::jobs::process_runnable(&state)
                .await
                .map_err(|e| anyhow::anyhow!("schedule processing failed: {:?}", e.errors.first().map(|o| &o.code)))?;
            println!("{ran} schedule(s) processed");
            Ok(())
        }
        Cmd::DbInstall => {
            let (config, _) = Config::from_env(env, &cli.path)?;
            let db = pelican_db::connect(&config.db).await?;
            pelican_db::install::install_fresh(&db, config.db.driver).await?;
            println!("schema installed ({} migrations recorded)", pelican_db::install::migration_names().len());
            Ok(())
        }
        Cmd::DbStatus => {
            let (config, _) = Config::from_env(env, &cli.path)?;
            let db = pelican_db::connect(&config.db).await?;
            println!("{:?}", pelican_db::install::schema_state(&db).await?);
            Ok(())
        }
    }
}

/// A command's exit status (Artisan's `return 1`).
fn exit(code: i32) -> Result<()> {
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}
