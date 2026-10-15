//! `p:node*` commands (to be ported).

use clap::Subcommand;

use super::Ctx;
use crate::console::Console;

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(name = "p:node:__pending", hide = true)]
    Pending,
}

pub async fn run(_ctx: &Ctx, io: &Console, _cmd: Command) -> anyhow::Result<i32> {
    io.error("Not implemented yet.");
    Ok(1)
}
