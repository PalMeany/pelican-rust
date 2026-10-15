//! `p:server*`: BulkPowerActionCommand (`Server/BulkPowerActionCommand.php`).

use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use clap::{Args, Subcommand};
use pelican_core::state::AppState;
use pelican_db::entities::{nodes, servers};
use sea_orm::{ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::{Value, json};

use super::Ctx;
use crate::console::{Console, t};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Perform bulk power management on large groupings of servers or nodes at once.
    #[command(name = "p:server:bulk-power")]
    BulkPower(BulkPowerArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    let st = ctx.state().await?;
    Ok(match cmd {
        Command::BulkPower(a) => bulk_power(&st, io, a).await,
    })
}

#[derive(Args, Debug)]
pub struct BulkPowerArgs {
    /// The action to perform (start, stop, restart, kill)
    action: String,
    /// A comma separated list of servers.
    #[arg(long)]
    servers: Option<String>,
    /// A comma separated list of nodes.
    #[arg(long)]
    nodes: Option<String>,
}

/// `empty($option) ? [] : explode(',', $option)` as string values (untrimmed).
fn list(option: Option<&String>) -> Vec<Value> {
    match option.filter(|s| !s.is_empty()) {
        Some(s) => s.split(',').map(|p| json!(p)).collect(),
        None => vec![],
    }
}

/// BulkPowerActionCommand::handle().
pub async fn bulk_power(st: &AppState, io: &Console, a: BulkPowerArgs) -> i32 {
    let action = a.action.as_str();
    let nodes_list = list(a.nodes.as_ref());
    let servers_list = list(a.servers.as_ref());

    // The ValidatorFactory pass: action in:start,stop,kill,restart, ids ≥ 1.
    let input = json!({"action": action, "nodes": nodes_list, "servers": servers_list});
    let mut v = pelican_api::validation::Validator::for_value(&input);
    v.rules("action", &["string", "in:start,stop,kill,restart"]);
    v.rules("nodes", &["array"]);
    v.rules("nodes.*", &["integer", "min:1"]);
    v.rules("servers", &["array"]);
    v.rules("servers.*", &["integer", "min:1"]);
    if let Err(e) = v.validate() {
        for o in &e.errors {
            // `$this->output->error($message)` per bag entry, then the uncaught throw.
            io.error(&o.detail);
        }
        return 1;
    }

    let filter = match get_condition(&servers_list, &nodes_list) {
        Ok(c) => c,
        Err(e) => {
            io.error(&e);
            return 1;
        }
    };
    let count = match servers::Entity::find().filter(filter.clone()).count(&st.db).await {
        Ok(c) => c,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    let msg = t("command/messages.server.power.confirm", &[("action", action), ("count", &count.to_string())]);
    // `if (!$this->confirm(...) && $this->input->isInteractive())`: `-n` runs on.
    if !io.confirm(&msg, false) && io.interactive() {
        return 0;
    }

    // `getQueryBuilder(...)->get()` with `with('node')`.
    let rows = match servers::Entity::find().filter(filter).all(&st.db).await {
        Ok(r) => r,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    let mut ids: Vec<pelican_db::Int> = rows.iter().map(|s| s.node_id).collect();
    ids.sort();
    ids.dedup();
    let by_id: HashMap<i64, nodes::Model> =
        match nodes::Entity::find().filter(nodes::Column::Id.is_in(ids)).all(&st.db).await {
            Ok(ns) => ns.into_iter().map(|n| (n.id.0, n)).collect(),
            Err(e) => {
                io.error(&e.to_string());
                return 1;
            }
        };

    let total = rows.len();
    for (i, s) in rows.iter().enumerate() {
        bar_clear(io);
        // DaemonServerRepository::setServer($server)->power($action), caught per server:
        // the exception message goes into `trans(...action_failed)` unchanged.
        let failure: Option<String> = match by_id.get(&s.node_id.0) {
            None => Some("ErrorException".to_string()),
            Some(n) => match st.node_conn(n) {
                Ok(conn) => match st.wings.power(&conn, &s.uuid, action).await {
                    Ok(()) => None,
                    Err(e) => Some(e.to_string()),
                },
                Err(_) => Some("DecryptException".to_string()),
            },
        };
        if let Some(message) = failure {
            let node_name = by_id.get(&s.node_id.0).map(|n| n.name.as_str()).unwrap_or_default();
            io.error(&t(
                "command/messages.server.power.action_failed",
                &[("name", &s.name), ("id", &s.id.0.to_string()), ("node", node_name), ("message", &message)],
            ));
        }
        bar_draw(io, i + 1, total);
    }
    // `$this->line('')`.
    io.line("");
    0
}

/// `(status IS NULL AND id IN …) OR/AND node_id IN …`, preserving the original builder's
/// precedence when both lists are given (`whereNull()->whereIn()->orWhereIn()`).
fn get_condition(server_ids: &[Value], node_ids: &[Value]) -> Result<Condition, String> {
    let parse = |vals: &[Value]| -> Result<Vec<pelican_db::Int>, String> {
        vals.iter()
            .map(|v| {
                v.as_str()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .map(pelican_db::Int)
                    .ok_or_else(|| "invalid id".to_string())
            })
            .collect()
    };
    let sids = parse(server_ids)?;
    let nids = parse(node_ids)?;
    let status_null = servers::Column::Status.is_null();
    // `Condition::add` takes conditions; column expressions are wrapped singly.
    let base = |extra: Condition| Condition::all().add(status_null.clone()).add(extra);
    Ok(match (!sids.is_empty(), !nids.is_empty()) {
        (true, true) => Condition::any()
            .add(base(Condition::all().add(servers::Column::Id.is_in(sids))))
            .add(base(Condition::all().add(servers::Column::NodeId.is_in(nids)))),
        (true, false) => base(Condition::all().add(servers::Column::Id.is_in(sids))),
        (false, true) => base(Condition::all().add(servers::Column::NodeId.is_in(nids))),
        (false, false) => Condition::all().add(status_null),
    })
}

/// The inline redraws of the console progress bar (`createProgressBar`); nothing is
/// written when the output is not decorated (`bar->clear()` leaves the line empty).
fn bar_clear(io: &Console) {
    if io.interactive() && std::io::stdout().is_terminal() {
        let _ = write!(std::io::stdout(), "\r\x1b[2K");
        let _ = std::io::stdout().flush();
    }
}

/// Symfony's default template `{current}/{max} [{bar}] {percent:>3}%` over an 80-column
/// line; single-digit steps only overwrite from `\r`.
fn bar_draw(_io: &Console, done: usize, total: usize) {
    if !(std::io::stdout().is_terminal() && total > 0) {
        return;
    }
    let pct = done * 100 / total;
    let prefix = format!("{done}/{total} [");
    let suffix = format!("] {pct:>3}%");
    let width = 80usize.saturating_sub(prefix.len() + suffix.len()).max(3);
    let filled = width * done / total;
    let mut bar: String = "=".repeat(filled.min(width));
    if filled < width && done < total {
        bar.push('>');
    }
    while bar.chars().count() < width {
        bar.push('-');
    }
    let _ = write!(std::io::stdout(), "\r{prefix}{bar}{suffix}");
    let _ = std::io::stdout().flush();
}
