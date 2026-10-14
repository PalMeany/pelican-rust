//! Admin › Servers › Transfer (`ServerResource` management tab, `exclude_transfer`
//! action over `TransferServerService`).

use axum::Router;
use axum::extract::Path;
use axum::routing::get;
use minijinja::{Value as Mj, context};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, backup_hosts, backups, nodes, servers};
use pelican_db::raw;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/servers/{id}/transfer", get(show).post(transfer))
}

struct Ctx {
    server: servers::Model,
    node: nodes::Model,
    /// `user()->accessibleNodes()` without the current node.
    targets: Vec<nodes::Model>,
    allocations: Vec<allocations::Model>,
    /// Backups on Wings backup hosts (only those can move with the server).
    wings_backups: Vec<backups::Model>,
}

async fn load(admin: &Admin, id: i64) -> Result<Ctx, WebError> {
    admin.authorize("update server")?;
    let db = &admin.st.db;
    let server = servers::Entity::find_by_id(id).one(db).await?.ok_or(WebError::NotFound)?;
    if !admin.user.can_target_node(server.node_id.0) {
        return Err(WebError::NotFound);
    }
    let node = nodes::Entity::find_by_id(server.node_id.0).one(db).await?.ok_or(WebError::NotFound)?;
    let accessible: Vec<nodes::Model> = nodes::Entity::find()
        .order_by_asc(nodes::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .filter(|n| admin.user.can_target_node(n.id.0))
        .collect();
    let conflict = crate::server_info::in_conflict_state(
        &server,
        &node,
        pelican_core::server_state::active_transfer(db, server.id.0).await?.is_some(),
    );
    // ->disabled(accessibleNodes()->count() <= 1 || isInConflictState())
    if accessible.len() <= 1 || conflict {
        return Err(WebError::Forbidden);
    }
    let allocations = allocations::Entity::find().filter(allocations::Column::ServerId.eq(server.id.0)).all(db).await?;
    let wings_hosts: Vec<i64> = backup_hosts::Entity::find()
        .filter(backup_hosts::Column::Schema.eq("wings"))
        .all(db)
        .await?
        .into_iter()
        .map(|h| h.id.0)
        .collect();
    let wings_backups = backups::Entity::find()
        .filter(backups::Column::ServerId.eq(server.id.0))
        .all(db)
        .await?
        .into_iter()
        .filter(|b| wings_hosts.contains(&b.backup_host_id.0))
        .collect();
    let targets = accessible.into_iter().filter(|n| n.id != node.id).collect();
    Ok(Ctx { server, node, targets, allocations, wings_backups })
}

/// Free allocations of a node as `[id, address]`.
async fn free_allocations(admin: &Admin, node_id: i64, except: Option<i64>) -> Result<Vec<Vec<String>>, WebError> {
    Ok(allocations::Entity::find()
        .filter(allocations::Column::NodeId.eq(node_id))
        .filter(allocations::Column::ServerId.is_null())
        .order_by_asc(allocations::Column::Id)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .filter(|a| Some(a.id.0) != except)
        .map(|a| vec![a.id.0.to_string(), crate::models::address(&a)])
        .collect())
}

async fn render(admin: &Admin, c: &Ctx, form: Option<&Form>, errors: &Errors) -> WebResult {
    let get = |k: &str| form.and_then(|f| f.opt(k)).map(String::from);
    let node_id: i64 =
        get("node_id").and_then(|v| v.parse().ok()).or_else(|| c.targets.first().map(|n| n.id.0)).unwrap_or(0);
    let allocation_id = get("allocation_id").and_then(|v| v.parse::<i64>().ok());
    let additional: Vec<String> = form.map(|f| f.list("allocation_additional")).unwrap_or_default();
    let selected_backups: Vec<String> = form.map(|f| f.list("backups")).unwrap_or_default();
    let has_primary = c.server.allocation_id.is_some();
    let extra = c.allocations.len().saturating_sub(1);
    let ctx = context! {
        title => admin.t("admin/server.transfer"),
        server => context! { id => c.server.id.0, name => c.server.name.clone(), node => c.node.name.clone() },
        nodes => c.targets.iter().map(|n| vec![n.id.0.to_string(), n.name.clone()]).collect::<Vec<_>>(),
        node_id => node_id.to_string(),
        allocations => free_allocations(admin, node_id, None).await?,
        additional_options => free_allocations(admin, node_id, allocation_id).await?,
        allocation_id => allocation_id.map(|a| a.to_string()).unwrap_or_default(),
        additional => additional,
        allocation_enabled => has_primary,
        additional_enabled => extra > 0,
        additional_count => extra,
        backups => c.wings_backups.iter().map(|b| vec![b.uuid.clone(), b.name.clone()]).collect::<Vec<_>>(),
        selected_backups => selected_backups,
        errors => Mj::from_serialize(errors),
    };
    admin.page("servers", "admin/servers/transfer.html", ctx).await
}

async fn show(admin: Admin, Path(id): Path<i64>) -> WebResult {
    let c = load(&admin, id).await?;
    render(&admin, &c, None, &Errors::default()).await
}

async fn transfer(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let c = load(&admin, id).await?;
    if form.opt("_refresh").is_some() {
        return render(&admin, &c, Some(&form), &Errors::default()).await;
    }
    let t = |k: &str| admin.t(k).to_lowercase();
    let mut errors = Errors::default();
    let node_id: Option<i64> = form.get("node_id").parse().ok().filter(|n| c.targets.iter().any(|t| t.id.0 == *n));
    if node_id.is_none() {
        errors.add("node_id", admin.t_with("validation.required", &[("attribute", &t("admin/server.node"))]));
    }
    let allocation_id: Option<i64> = form.get("allocation_id").parse().ok();
    if c.server.allocation_id.is_some() && allocation_id.is_none() {
        errors.add(
            "allocation_id",
            admin.t_with("validation.required", &[("attribute", &t("admin/server.primary_allocation"))]),
        );
    }
    let additional: Vec<i64> = form.list("allocation_additional").iter().filter_map(|a| a.parse().ok()).collect();
    let extra = c.allocations.len().saturating_sub(1);
    if extra > 0 && additional.len() != extra {
        // ->required()->minItems(n)->maxItems(n)
        let attr = t("admin/server.additional_allocations");
        let msg = if additional.is_empty() {
            admin.t_with("validation.required", &[("attribute", &attr)])
        } else if additional.len() < extra {
            admin.t_with("validation.min.array", &[("attribute", &attr), ("min", &extra.to_string())])
        } else {
            admin.t_with("validation.max.array", &[("attribute", &attr), ("max", &extra.to_string())])
        };
        errors.add("allocation_additional", msg);
    }
    if !errors.is_empty() {
        return render(&admin, &c, Some(&form), &errors).await;
    }
    let selected: Vec<String> = form.list("backups");
    let st = &admin.st;
    let result = pelican_api::application::servers_write::transfer_server(
        st,
        &c.server,
        pelican_db::Int(node_id.unwrap_or(0)),
        allocation_id,
        additional,
        &selected,
    )
    .await;
    match result {
        Ok(()) => {
            // Wings backups that are not transferred are only removed from the panel.
            let drop: Vec<i64> =
                c.wings_backups.iter().filter(|b| !selected.contains(&b.uuid)).map(|b| b.id.0).collect();
            if !drop.is_empty() {
                let marks = vec!["?"; drop.len()].join(", ");
                st.db
                    .execute_raw(raw::stmt(
                        st.db.get_database_backend(),
                        &format!("delete from {{backups}} where {{id}} in ({marks})"),
                        drop.iter().map(|i| sea_orm::Value::from(*i)).collect(),
                    ))
                    .await?;
            }
            admin.flash_success(admin.t("admin/server.notifications.transfer_started"));
        }
        Err(e) => admin.flash_danger(
            admin.t("admin/server.notifications.transfer_failed"),
            e.errors.first().map(|o| o.detail.clone()),
        ),
    }
    Ok(admin.redirect(&format!("/admin/servers/{id}/edit?tab=actions")))
}
