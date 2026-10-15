//! `php artisan p:*` commands of the original (`app/Console/Commands`), same names,
//! arguments and options. One module per command group, each with its own clap
//! `Command` enum (flattened into the binary's subcommands) and `run()`.

pub mod egg;
pub mod environment;
pub mod info;
pub mod maintenance;
pub mod node;
pub mod plugin;
pub mod server;
pub mod user;

use std::path::PathBuf;

use pelican_api::error::ApiError;
use pelican_core::env::Env;
use pelican_core::state::AppState;

/// What a command runs against: the panel directory and its `.env`. Commands that need
/// the database call [`Ctx::state`] (the `p:environment:*` ones must work without it).
pub struct Ctx {
    pub path: PathBuf,
    pub env: Env,
}

impl Ctx {
    pub async fn state(&self) -> anyhow::Result<AppState> {
        crate::boot(self.env.clone(), &self.path).await
    }
}

/// The messages of an API/service error, as the original prints an exception.
pub fn api_error(e: &ApiError) -> String {
    let details: Vec<&str> = e.errors.iter().map(|o| o.detail.as_str()).filter(|d| !d.is_empty()).collect();
    if details.is_empty() {
        e.errors.first().map(|o| o.code.clone()).unwrap_or_else(|| "Error".into())
    } else {
        details.join("\n")
    }
}
