//! `p:maintenance:clean-service-backups`, `p:maintenance:prune-backups`,
//! `p:maintenance:prune-images` (app/Console/Commands/Maintenance).

use std::path::Path;
use std::time::Duration;

use clap::{Args, Subcommand};
use pelican_core::state::AppState;
use pelican_db::entities::{backups, nodes};
use pelican_db::{Int, Timestamp};
use pelican_wings::WingsError;
use pelican_wings::client::{Call, Method};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::Value;

use super::{Ctx, api_error};
use crate::console::{Console, t};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Clean orphaned .bak files created when modifying services.
    #[command(name = "p:maintenance:clean-service-backups")]
    CleanServiceBackups,
    /// Marks all backups older than "n" minutes that have not yet completed as being failed.
    #[command(name = "p:maintenance:prune-backups")]
    PruneBackups(PruneBackupsArgs),
    /// Clean up all dangling docker images to clear up disk space.
    #[command(name = "p:maintenance:prune-images")]
    PruneImages(PruneImagesArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    Ok(match cmd {
        // Files only, no database needed.
        Command::CleanServiceBackups => clean_service_backups(&ctx.path, io),
        Command::PruneBackups(a) => match prune_age(ctx, a) {
            Ok((since, minutes)) => prune_backups(&ctx.state().await?, io, &since, minutes).await,
            Err(message) => {
                io.error(message);
                1
            }
        },
        Command::PruneImages(a) => prune_images(&ctx.state().await?, io, a).await,
    })
}

/// `CleanServiceBackupFilesCommand::BACKUP_THRESHOLD_MINUTES`.
const BACKUP_THRESHOLD_MINUTES: u64 = 5;

/// CleanServiceBackupFilesCommand: files directly in `services/.bak` of the default `local`
/// disk (Laravel 11+ root: `storage/app/private`) modified more than 5 minutes ago.
/// The original calls `getPath()` on the path strings `files()` returns and fails as soon as
/// a file exists; this deletes and reports them as intended.
pub fn clean_service_backups(base: &Path, io: &Console) -> i32 {
    let dir = base.join("storage/app/private/services/.bak");
    let Ok(entries) = std::fs::read_dir(&dir) else { return 0 };
    // `files()`: regular files only, sorted by path.
    let mut files: Vec<_> =
        entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).collect();
    files.sort();
    for path in files {
        let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if age.is_some_and(|d| d > Duration::from_secs(BACKUP_THRESHOLD_MINUTES * 60)) {
            // `$disk->delete()` does not throw (disk `throw` => false).
            let _ = std::fs::remove_file(&path);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            io.info(&t("command/messages.maintenance.deleting_service_backup", &[("file", &name)]));
        }
    }
    0
}

#[derive(Args, Debug)]
pub struct PruneBackupsArgs {
    #[arg(long = "prune-age")]
    prune_age: Option<String>,
}

/// About 1000 years: keeps the cutoff a valid date (`Duration::minutes` / date math panic
/// on overflow); no backup is that old either way.
const MAX_PRUNE_MINUTES: i64 = 1000 * 366 * 24 * 60;

/// `$this->option('prune-age') ?? config('backups.prune_age', 360)`, which must pass
/// `$since && is_digit($since)`. Returns the value as printed and in minutes.
fn prune_age(ctx: &Ctx, a: PruneBackupsArgs) -> Result<(String, i64), &'static str> {
    let since = a.prune_age.unwrap_or_else(|| ctx.env.int_or("BACKUP_PRUNE_AGE", 360).to_string());
    if since.is_empty() || since == "0" || !since.bytes().all(|b| b.is_ascii_digit()) {
        return Err("The \"--prune-age\" argument must be a value greater than 0.");
    }
    let minutes = since.parse::<i64>().unwrap_or(i64::MAX).min(MAX_PRUNE_MINUTES);
    Ok((since, minutes))
}

/// PruneOrphanedBackupsCommand::handle(): the update itself is the scheduler's
/// mass update (no model events, as in the original).
pub async fn prune_backups(st: &AppState, io: &Console, since: &str, minutes: i64) -> i32 {
    let cutoff = Timestamp(pelican_core::time::now() - chrono::Duration::minutes(minutes));
    let count = backups::Entity::find()
        .filter(backups::Column::CompletedAt.is_null())
        .filter(backups::Column::CreatedAt.lte(cutoff))
        .count(&st.db)
        .await;
    let count = match count {
        Ok(c) => c,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    if count == 0 {
        io.info("There are no orphaned backups to be marked as failed.");
        return 0;
    }
    io.warn(&format!("Marking {count} uncompleted backups that are older than {since} minutes as failed."));
    if let Err(e) = pelican_api::jobs::prune_backups(st, minutes).await {
        io.error(&api_error(&e));
        return 1;
    }
    0
}

#[derive(Args, Debug)]
pub struct PruneImagesArgs {
    node: Option<String>,
}

/// PruneImagesCommand::handle(): one node (`findOrFail((int) $node)`) or all of them.
pub async fn prune_images(st: &AppState, io: &Console, a: PruneImagesArgs) -> i32 {
    let arg = a.node.unwrap_or_default();
    let targets = if arg.is_empty() || arg == "0" {
        nodes::Entity::find().all(&st.db).await
    } else {
        let id = pelican_core::php::intval(&arg);
        match nodes::Entity::find_by_id(Int(id)).one(&st.db).await {
            Ok(Some(n)) => Ok(vec![n]),
            Ok(None) => {
                io.error(&format!("No query results for model [App\\Models\\Node] {id}"));
                return 1;
            }
            Err(e) => Err(e),
        }
    };
    let targets = match targets {
        Ok(n) => n,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    for node in &targets {
        match prune_node_images(st, node).await {
            Ok(Some((count, space))) => {
                io.info(&format!("Node {}: Cleaned up {count} dangling docker images. ({space})", node.id.0))
            }
            Ok(None) => io.warn(&format!("Node {}: No images to clean up.", node.id.0)),
            Err(message) => io.error(&message),
        }
    }
    0
}

/// `PruneImagesCommand::cleanupImages()`: `Http::daemon($node)->connectTimeout(5)->timeout(30)
/// ->delete('/api/system/docker/image/prune')->json() ?? []` (non-2xx is not an error).
/// `None` = nothing deleted; `Err` = the exception message the original prints.
async fn prune_node_images(st: &AppState, node: &nodes::Model) -> Result<Option<(usize, String)>, String> {
    let conn = st.node_conn(node).map_err(|e| e.to_string())?;
    let mut call = Call::new(Method::DELETE, "/api/system/docker/image/prune");
    call.throw = false;
    call.connect_timeout = Some(Duration::from_secs(5));
    call.timeout = Some(Duration::from_secs(30));
    let response = st.wings.send(&conn, call).await.map_err(|e| match e {
        WingsError::Connection(message) => message,
        other => other.to_string(),
    })?;
    images_report(response.json().unwrap_or(Value::Null), st.config.use_binary_prefix)
}

/// The prune response as `cleanupImages()` reads it: `(count, "x MiB")`, or `None` when
/// the body is empty or `ImagesDeleted` is null.
fn images_report(body: Value, binary_prefix: bool) -> Result<Option<(usize, String)>, String> {
    if pelican_core::php::empty(&body) {
        return Ok(None);
    }
    let key = |k: &str| body.get(k).ok_or_else(|| format!("Undefined array key \"{k}\""));
    let count = match key("ImagesDeleted")? {
        Value::Null => return Ok(None),
        Value::Array(list) => list.len(),
        Value::Object(map) => map.len(),
        _ => return Err("count(): Argument #1 ($value) must be of type Countable|array".into()),
    };
    let reclaimed = key("SpaceReclaimed")?.as_f64().unwrap_or(0.0);
    let (divisor, unit) = if binary_prefix { (1024.0 * 1024.0, " MiB") } else { (1e6, " MB") };
    // round($bytes / …, 2) . ' MiB'
    let rounded = (reclaimed / divisor * 100.0).round() / 100.0;
    let space = format!("{}{unit}", pelican_core::php::string(&serde_json::json!(rounded)));
    Ok(Some((count, space)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::images_report;

    #[test]
    fn prune_report() {
        let deleted =
            json!({"ImagesDeleted": [{"Deleted": "sha256:a"}, {"Untagged": "b"}], "SpaceReclaimed": 12_949_000});
        assert_eq!(images_report(deleted.clone(), true), Ok(Some((2, "12.35 MiB".into()))));
        assert_eq!(images_report(deleted, false), Ok(Some((2, "12.95 MB".into()))));
        let whole = json!({"ImagesDeleted": [], "SpaceReclaimed": 1_048_576});
        assert_eq!(images_report(whole, true), Ok(Some((0, "1 MiB".into()))));
        assert_eq!(images_report(json!({"ImagesDeleted": null, "SpaceReclaimed": 0}), true), Ok(None));
        assert_eq!(images_report(serde_json::Value::Null, true), Ok(None));
        assert_eq!(images_report(json!({"error": "x"}), true), Err("Undefined array key \"ImagesDeleted\"".into()));
    }
}
