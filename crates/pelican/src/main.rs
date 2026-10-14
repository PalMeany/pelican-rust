//! `pelican` — the pelican-rs panel.
//!
//!   pelican serve [--listen 0.0.0.0:8000] [--path <panel dir with .env>]
//!   pelican db:install      create the schema in an empty database
//!   pelican db:status       show the schema state
//!   pelican schedule:process  run due schedules once (`p:schedule:process`)

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
    #[command(name = "schedule:process")]
    ScheduleProcess,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let env = Env::load(&cli.path)?;
    match cli.cmd {
        Cmd::Serve { listen } => {
            let state = pelican::boot(env, &cli.path).await?;
            let listener = tokio::net::TcpListener::bind(&listen).await?;
            tracing::info!("listening on {listen}");
            pelican::serve(state, listener).await
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
