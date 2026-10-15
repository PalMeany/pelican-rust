//! `p:node:*`: MakeNodeCommand, NodeListCommand, NodeConfigurationCommand.

use clap::{Args, Subcommand};
use pelican_api::application::nodes as app_nodes;
use pelican_core::state::AppState;
use pelican_db::entities::nodes;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};

use super::{Ctx, api_error};
use crate::console::{Console, t, truthy};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Creates a new node on the system via the CLI.
    #[command(name = "p:node:make")]
    Make(MakeArgs),
    #[command(name = "p:node:list")]
    List(ListArgs),
    /// Displays the configuration for the specified node.
    #[command(name = "p:node:configuration")]
    Configuration(ConfigurationArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    let st = ctx.state().await?;
    Ok(match cmd {
        Command::Make(a) => make(&st, io, a).await,
        Command::List(a) => list(&st, io, a).await,
        Command::Configuration(a) => configuration(&st, io, a).await,
    })
}

#[derive(Args, Debug)]
pub struct MakeArgs {
    /// A name to identify the node.
    #[arg(long)]
    name: Option<String>,
    /// A description to identify the node.
    #[arg(long)]
    description: Option<String>,
    /// A valid locationId.
    #[arg(long = "locationId")]
    location_id: Option<String>,
    /// The domain name to be used for connecting to the daemon.
    #[arg(long)]
    fqdn: Option<String>,
    /// Should the node be public or private? (public=1 / private=0).
    #[arg(long)]
    public: Option<String>,
    /// Which scheme should be used? (Enable SSL=https / Disable SSL=http).
    #[arg(long)]
    scheme: Option<String>,
    /// Is the daemon behind a proxy? (Yes=1 / No=0).
    #[arg(long)]
    proxy: Option<String>,
    /// Should maintenance mode be enabled?
    #[arg(long)]
    maintenance: Option<String>,
    /// Set the max memory amount.
    #[arg(long = "maxMemory")]
    max_memory: Option<String>,
    /// Enter the amount of ram to overallocate (% or -1 to overallocate the maximum).
    #[arg(long = "overallocateMemory")]
    overallocate_memory: Option<String>,
    /// Set the max disk amount.
    #[arg(long = "maxDisk")]
    max_disk: Option<String>,
    /// Enter the amount of disk to overallocate (% or -1 to overallocate the maximum).
    #[arg(long = "overallocateDisk")]
    overallocate_disk: Option<String>,
    /// Set the max cpu amount.
    #[arg(long = "maxCpu")]
    max_cpu: Option<String>,
    /// Enter the amount of cpu to overallocate (% or -1 to overallocate the maximum).
    #[arg(long = "overallocateCpu")]
    overallocate_cpu: Option<String>,
    /// Enter the maximum upload filesize.
    #[arg(long = "uploadSize")]
    upload_size: Option<String>,
    /// Enter the daemon listening port.
    #[arg(long = "daemonListeningPort")]
    daemon_listening_port: Option<String>,
    /// Enter the daemon connecting port.
    #[arg(long = "daemonConnectingPort")]
    daemon_connecting_port: Option<String>,
    /// Enter the daemon SFTP listening port.
    #[arg(long = "daemonSFTPPort")]
    daemon_sftp_port: Option<String>,
    /// Enter the daemon SFTP alias.
    #[arg(long = "daemonSFTPAlias")]
    daemon_sftp_alias: Option<String>,
    /// Enter the base folder.
    #[arg(long = "daemonBase")]
    daemon_base: Option<String>,
}

/// `$this->option('x') ?? $this->ask(...)`: keeps the raw PHP-typed answer
/// (options and asked strings stay strings; an unanswered question stays null).
fn filled(io: &Console, option: &mut Option<String>, question: &str, default: Option<&str>) -> Value {
    match option.take().or_else(|| io.ask(question, default)) {
        Some(s) => json!(s),
        None => Value::Null,
    }
}

/// MakeNodeCommand (`Node::create($data)` with prompts; `--locationId` is accepted but
/// unused, exactly as in the original handle()).
pub async fn make(st: &AppState, io: &Console, a: MakeArgs) -> i32 {
    let mut a = a;
    // Question order follows the original handle() fill order.
    let name = filled(io, &mut a.name, &t("commands.make_node.name", &[]), None);
    let description = filled(io, &mut a.description, &t("commands.make_node.description", &[]), None);
    // anticipate(['https', 'http'], 'https'): free-text ask with the same default.
    let scheme = filled(io, &mut a.scheme, &t("commands.make_node.scheme", &[]), Some("https"));
    let fqdn = filled(io, &mut a.fqdn, &t("commands.make_node.fqdn", &[]), None);
    let public = match a.public.take().map(|s| truthy(&s)) {
        Some(b) => json!(b),
        None => json!(io.confirm(&t("commands.make_node.public", &[]), true)),
    };
    let behind_proxy = match a.proxy.take().map(|s| truthy(&s)) {
        Some(b) => json!(b),
        None => json!(io.confirm(&t("commands.make_node.behind_proxy", &[]), false)),
    };
    let maintenance_mode = match a.maintenance.take().map(|s| truthy(&s)) {
        Some(b) => json!(b),
        None => json!(io.confirm(&t("commands.make_node.maintenance_mode", &[]), false)),
    };
    let memory = filled(io, &mut a.max_memory, &t("commands.make_node.memory", &[]), Some("0"));
    let memory_overallocate =
        filled(io, &mut a.overallocate_memory, &t("commands.make_node.memory_overallocate", &[]), Some("-1"));
    let disk = filled(io, &mut a.max_disk, &t("commands.make_node.disk", &[]), Some("0"));
    let disk_overallocate =
        filled(io, &mut a.overallocate_disk, &t("commands.make_node.disk_overallocate", &[]), Some("-1"));
    let cpu = filled(io, &mut a.max_cpu, &t("commands.make_node.cpu", &[]), Some("0"));
    let cpu_overallocate =
        filled(io, &mut a.overallocate_cpu, &t("commands.make_node.cpu_overallocate", &[]), Some("-1"));
    let upload_size = filled(io, &mut a.upload_size, &t("commands.make_node.upload_size", &[]), Some("256"));
    let daemon_listen =
        filled(io, &mut a.daemon_listening_port, &t("commands.make_node.daemonListen", &[]), Some("8080"));
    let daemon_connect =
        filled(io, &mut a.daemon_connecting_port, &t("commands.make_node.daemonConnect", &[]), Some("8080"));
    let daemon_sftp = filled(io, &mut a.daemon_sftp_port, &t("commands.make_node.daemonSFTP", &[]), Some("2022"));
    let daemon_sftp_alias =
        filled(io, &mut a.daemon_sftp_alias, &t("commands.make_node.daemonSFTPAlias", &[]), Some(""));
    let daemon_base =
        filled(io, &mut a.daemon_base, &t("commands.make_node.daemonBase", &[]), Some("/var/lib/pelican/volumes"));

    let data = vec![
        ("name".into(), name),
        ("description".into(), description),
        ("scheme".into(), scheme),
        ("fqdn".into(), fqdn),
        ("public".into(), public),
        ("behind_proxy".into(), behind_proxy),
        ("maintenance_mode".into(), maintenance_mode),
        ("memory".into(), memory),
        ("memory_overallocate".into(), memory_overallocate),
        ("disk".into(), disk),
        ("disk_overallocate".into(), disk_overallocate),
        ("cpu".into(), cpu),
        ("cpu_overallocate".into(), cpu_overallocate),
        ("upload_size".into(), upload_size),
        ("daemon_listen".into(), daemon_listen),
        ("daemon_connect".into(), daemon_connect),
        ("daemon_sftp".into(), daemon_sftp),
        ("daemon_sftp_alias".into(), daemon_sftp_alias),
        ("daemon_base".into(), daemon_base),
    ];
    let display_name = data[0].1.as_str().unwrap_or_default().to_string();
    let created = match app_nodes::create(st, data).await {
        Ok(c) => c,
        Err(e) => {
            io.error(&api_error(&e));
            return 1;
        }
    };
    io.line(&t("commands.make_node.success", &[("name", &display_name), ("id", &created.node.id.0.to_string())]));
    0
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// The output format: "text" or "json".
    #[arg(long, default_value = "text")]
    format: String,
}

/// `Node::getConnectionAddress()`.
fn connection_address(n: &nodes::Model) -> String {
    format!("{}://{}:{}", n.scheme, n.fqdn, n.daemon_connect.0)
}

/// NodeListCommand.
pub async fn list(st: &AppState, io: &Console, a: ListArgs) -> i32 {
    let rows = match nodes::Entity::find().all(&st.db).await {
        Ok(r) => r,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    if a.format == "json" {
        let items: Vec<Value> = rows
            .iter()
            .map(|n| {
                json!({
                    "id": n.id.0,
                    "uuid": n.uuid.clone().map(|u| json!(u)).unwrap_or(Value::Null),

                    "name": n.name,
                    "host": connection_address(n),
                })
            })
            .collect();
        match app_nodes::pretty_json(&Value::Array(items)) {
            Ok(body) => print_body(&body),
            Err(e) => {
                io.error(&api_error(&e));
                return 1;
            }
        }
    } else {
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|n| {
                vec![n.id.0.to_string(), n.uuid.clone().unwrap_or_default(), n.name.clone(), connection_address(n)]
            })
            .collect();
        io.table(&["ID", "UUID", "Name", "Host"], &table);
    }
    println!();
    0
}

/// `$this->output->write($body)` + `newLine()`: content followed by exactly one newline.
fn print_body(body: &str) {
    print!("{}", body.trim_end_matches('\n'));
    println!();
}

#[derive(Args, Debug)]
pub struct ConfigurationArgs {
    /// The ID or UUID of the node to return the configuration for.
    node: String,
    /// The output format. Options are "yaml" and "json".
    #[arg(long, default_value = "yaml")]
    format: String,
}

/// NodeConfigurationCommand (`firstOr` failure prints the error and exits 1).
pub async fn configuration(st: &AppState, io: &Console, a: ConfigurationArgs) -> i32 {
    // `ctype_digit((string) $node)` decides id vs uuid lookup.
    let node = if a.node.chars().all(|c| c.is_ascii_digit()) && !a.node.is_empty() {
        let id = a.node.parse::<i64>().unwrap_or(-1);
        nodes::Entity::find_by_id(pelican_db::Int(id)).one(&st.db).await
    } else {
        nodes::Entity::find().filter(nodes::Column::Uuid.eq(a.node.clone())).one(&st.db).await
    };
    let n = match node {
        Ok(Some(n)) => n,
        Ok(None) => {
            io.error(&t("commands.node_config.error_not_exist", &[]));
            return 1;
        }
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    if !matches!(a.format.as_str(), "yaml" | "yml" | "json") {
        io.error(&t("commands.node_config.error_invalid_format", &[]));
        return 1;
    }
    let payload = match app_nodes::node_configuration(st, &n).await {
        Ok(v) => v,
        Err(e) => {
            io.error(&api_error(&e));
            return 1;
        }
    };
    // `getJsonConfiguration(true)` / `getYamlConfiguration()` (token never redacted here).
    let body = if a.format == "json" {
        match app_nodes::pretty_json(&payload) {
            Ok(s) => s,
            Err(e) => {
                io.error(&api_error(&e));
                return 1;
            }
        }
    } else {
        app_nodes::yaml_configuration(&payload)
    };
    print_body(&body);
    0
}
