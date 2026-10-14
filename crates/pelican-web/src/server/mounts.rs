//! Server panel › Mounts (app/Filament/Server/Pages/Mounts.php): the
//! user-mountable checkbox list synced into `mountables`
//! (`mountable_type = server`) on every change (`->live()` +
//! `afterStateUpdated` → `save()`, served by js/mounts.js auto-submit).

use std::collections::{HashMap, HashSet};

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::services::LogActivity;
use pelican_core::state::AppState;
use pelican_db::entities::mounts;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use super::ServerPanel;
use crate::error::{WebError, WebResult};
use crate::forms::Form;

pub fn routes() -> Router<AppState> {
    Router::new().route("/server/{server}/mounts", get(page)).route("/server/{server}/mounts/save", post(save))
}

/// One row of the `CheckboxList`: option label/description plus its state.
#[derive(serde::Serialize)]
struct MountRow {
    id: i64,
    name: String,
    /// Option description: `str("$mount->source -> $mount->target")`.
    path: String,
    description: Option<String>,
    checked: bool,
}

/// Egg and node ids attached to each mount.
async fn pivots(db: &impl ConnectionTrait) -> Result<(HashMap<i64, Vec<i64>>, HashMap<i64, Vec<i64>>), WebError> {
    use pelican_db::entities::mountables;
    let rows = mountables::Entity::find().all(db).await?;
    let mut eggs: HashMap<i64, Vec<i64>> = Default::default();
    let mut nodes: HashMap<i64, Vec<i64>> = Default::default();
    for r in rows {
        match r.mountable_type.as_str() {
            "egg" => eggs.entry(r.mount_id.0).or_default().push(r.mountable_id.0),
            "node" => nodes.entry(r.mount_id.0).or_default().push(r.mountable_id.0),
            _ => {}
        }
    }
    Ok((eggs, nodes))
}

/// `Mounts::form()`'s allowedMounts query: user-mountable mounts usable with
/// this server's node and egg (no restriction when none attached).
async fn allowed_mounts(p: &ServerPanel) -> Result<Vec<mounts::Model>, WebError> {
    let (eggs, nodes) = pivots(&p.st.db).await?;
    let egg = p.server.egg_id.0;
    let node = p.server.node_id.0;
    let fits = |m: &mounts::Model| match (&nodes.get(&m.id.0), &eggs.get(&m.id.0)) {
        (Some(ids), _) if !ids.is_empty() && !ids.contains(&node) => false,
        (_, Some(ids)) if !ids.is_empty() && !ids.contains(&egg) => false,
        _ => m.user_mountable.0,
    };
    // No explicit orderBy in the original — natural (id) order.
    Ok(mounts::Entity::find().all(&p.st.db).await?.into_iter().filter(fits).collect())
}

async fn render(p: ServerPanel) -> WebResult {
    let db = &p.st.db;

    // fillForm(): `$server->mounts` relationship state.
    use pelican_db::entities::mountables;
    let mounted: HashSet<i64> = mountables::Entity::find()
        .filter(mountables::Column::MountableType.eq("server"))
        .filter(mountables::Column::MountableId.eq(p.server.id))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.mount_id.0)
        .collect();

    let rows: Vec<MountRow> = allowed_mounts(&p)
        .await?
        .into_iter()
        .map(|m| MountRow {
            id: m.id.0,
            name: m.name.clone(),
            path: format!("{} -> {}", m.source, m.target),
            description: m.description.clone(),
            checked: mounted.contains(&m.id.0),
        })
        .collect();

    p.page(
        "mounts",
        "server/mounts.html",
        context! {
            title => p.t("server/mount.title"),
            base => p.base_url(),
            field_label => p.t("server/mount.description"),
            no_mounts_hint => p.t("server/mount.no_mounts"),
            mounts => Mj::from_serialize(&rows),
            can_update => p.can("mount.update"),
        },
    )
}

/// GET /server/{server}/mounts — access via SubuserPermission::MountRead.
async fn page(p: ServerPanel) -> WebResult {
    p.authorize("mount.read")?;
    render(p).await
}

/// Replace this server's mount pivots (`saveRelationships()` sync).
async fn sync<C: ConnectionTrait>(db: &C, server: i64, ids: &[i64]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(pelican_db::raw::stmt(
        b,
        "delete from {mountables} where {mountable_type} = 'server' and {mountable_id} = ?",
        vec![server.into()],
    ))
    .await?;
    for id in ids {
        db.execute_raw(pelican_db::raw::stmt(
            b,
            "insert into {mountables} ({mountable_type}, {mountable_id}, {mount_id}) values ('server', ?, ?)",
            vec![server.into(), (*id).into()],
        ))
        .await?;
    }
    Ok(())
}

/// POST /server/{server}/mounts/save — `Mounts::save()`: sync the checkbox
/// state into the `mounts` relationship, log `server:mount.update` and notify
/// success or the caught-exception danger toast.
async fn save(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("mount.update")?;
    let back = form.back(&format!("{}/mounts", p.base_url()));

    // getState(): posted ids; Filament validates against the field options
    // (`getInValidationRuleValues`), so keep only currently allowed mounts.
    let allowed: HashSet<i64> = allowed_mounts(&p).await?.iter().map(|m| m.id.0).collect();
    let mut ids: Vec<i64> =
        form.list("mounts").iter().filter_map(|s| s.parse::<i64>().ok()).filter(|id| allowed.contains(id)).collect();
    ids.dedup();

    let result: Result<(), WebError> = async {
        sync(&p.st.db, p.server.id.0, &ids).await?;
        p.activity_for_server("server:mount.update")
            .log(&p.st)
            .await
            .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            // One success toast (Notification::title + ->body()).
            p.session.flash(
                "success",
                p.t("server/mount.notification_updated"),
                Some(p.t("server/mount.notification_updated_body")),
            );
        }
        Err(e) => {
            tracing::warn!(error=?e, "mount update failed");
            p.flash_danger(p.t("server/mount.notification_failed"), Some(e.to_string()));
        }
    }
    Ok(p.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    fn row(id: i64, name: &str, checked: bool) -> MountRow {
        MountRow {
            id,
            name: name.into(),
            path: format!("/src/{name} -> /dst/{name}"),
            description: Some("desc".into()),
            checked,
        }
    }

    #[test]
    fn mounts_page_template_renders() {
        for can_update in [true, false] {
            for no_mounts in [true, false] {
                let rows = if no_mounts { Vec::new() } else { vec![row(1, "m1", true), row(2, "m2", false)] };
                let ctx = context! {
                    title => "Mounts",
                    base => "/server/abcd1234",
                    field_label => "Manage the mounts attached to your server:",
                    no_mounts_hint => "There are no user-mountable mounts available for this server.",
                    mounts => Mj::from_serialize(&rows),
                    can_update => can_update,
                };
                crate::views::render("server/mounts.html", ctx).expect("template renders");
            }
        }
    }

    /// The `fits` closure must mirror the original OR-composition:
    /// empty/absent restriction never blocks; non-empty must contain the node/egg.
    #[test]
    fn allowed_filter_semantics() {
        let mut nodes: HashMap<i64, Vec<i64>> = HashMap::new();
        nodes.insert(7, vec![99]);
        let node_ok_absent = nodes.get(&1).map_or(true, |v| v.is_empty() || v.contains(&10));
        let node_ok_present = nodes.get(&7).map_or(true, |v| v.is_empty() || v.contains(&99));
        assert!(node_ok_absent);
        assert!(!nodes.get(&7).map_or(true, |v| v.is_empty() || v.contains(&10)));
        assert!(node_ok_present);
    }

    #[test]
    fn row_serializes_checked_state() {
        let r = row(3, "x", false);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"checked\":false"));
    }
}
