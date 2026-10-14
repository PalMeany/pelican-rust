//! Relation managers of Admin › Servers › Edit: allocations
//! (AllocationsRelationManager) and databases (DatabasesRelationManager).

use axum::Router;
use axum::extract::Path;
use axum::routing::post;
use minijinja::{Value as Mj, context};
use pelican_api::binding::Bound;
use pelican_api::services::LogActivity;
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, database_host_node, database_hosts, databases, nodes, servers};
use pelican_db::{Bool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde_json::Value;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::Form;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/servers/{id}/allocations/create", post(allocation_create))
        .route("/admin/servers/{id}/allocations/associate", post(allocation_associate))
        .route("/admin/servers/{id}/allocations/bulk-dissociate", post(allocation_bulk_dissociate))
        .route("/admin/servers/{id}/allocations/{aid}/{op}", post(allocation_op))
        .route("/admin/servers/{id}/databases/create", post(database_create))
        .route("/admin/servers/{id}/databases/{did}/{op}", post(database_op))
}

async fn find(admin: &Admin, id: i64) -> Result<(servers::Model, nodes::Model), WebError> {
    let s = servers::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    if !admin.user.can_target_node(s.node_id.0) {
        return Err(WebError::NotFound);
    }
    let n = nodes::Entity::find_by_id(s.node_id.0).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    Ok((s, n))
}

fn back(id: i64) -> String {
    format!("/admin/servers/{id}/edit#relations")
}

/// Data for the relation sections on the edit page.
pub async fn context(admin: &Admin, s: &servers::Model, node: &nodes::Model) -> Result<Mj, WebError> {
    let st = &admin.st;
    let allocs = allocations::Entity::find()
        .filter(allocations::Column::ServerId.eq(s.id.0))
        .order_by_asc(allocations::Column::Ip)
        .order_by_asc(allocations::Column::Port)
        .all(&st.db)
        .await?;
    let free = allocations::Entity::find()
        .filter(allocations::Column::NodeId.eq(node.id.0))
        .filter(allocations::Column::ServerId.is_null())
        .order_by_asc(allocations::Column::Ip)
        .order_by_asc(allocations::Column::Port)
        .all(&st.db)
        .await?;
    let dbs = databases::Entity::find()
        .filter(databases::Column::ServerId.eq(s.id.0))
        .order_by_asc(databases::Column::Id)
        .all(&st.db)
        .await?;
    let all_hosts = database_hosts::Entity::find().order_by_asc(database_hosts::Column::Id).all(&st.db).await?;
    let host_nodes =
        database_host_node::Entity::find().filter(database_host_node::Column::NodeId.eq(node.id.0)).all(&st.db).await?;
    let usable_hosts: Vec<Vec<String>> = all_hosts
        .iter()
        .filter(|h| host_nodes.iter().any(|hn| hn.database_host_id == h.id))
        .map(|h| vec![h.id.0.to_string(), h.name.clone()])
        .collect();
    let anywhere = admin.t("admin/databasehost.anywhere");
    let unlimited = admin.t("admin/databasehost.unlimited");
    let db_rows: Vec<Mj> = dbs
        .iter()
        .map(|d| {
            let host = all_hosts.iter().find(|h| h.id == d.database_host_id);
            let password = st.enc.decrypt_string(&d.password).unwrap_or_default();
            let address = host.map(|h| format!("{}:{}", h.name, h.port.0)).unwrap_or_default();
            context! {
                id => d.id.0, database => d.database.clone(), username => d.username.clone(),
                remote => if d.remote == "%" { format!("{anywhere} ( % )") } else { d.remote.clone() },
                max_connections => d.max_connections.map(|m| m.0).filter(|m| *m > 0).map(|m| m.to_string()).unwrap_or_else(|| unlimited.clone()),
                created_at => d.created_at.map(|t| t.0.format("%Y-%m-%d %H:%M:%S").to_string()),
                password => password.clone(),
                jdbc => format!("jdbc:mysql://{}:{}@{}/{}", d.username, form_urlencoded::byte_serialize(password.as_bytes()).collect::<String>(), address, d.database),
            }
        })
        .collect();
    Ok(context! {
        allocations => allocs.iter().map(|a| context! {
            id => a.id.0, ip => a.ip.clone(), port => a.port.0, alias => a.ip_alias.clone().unwrap_or_default(),
            notes => a.notes.clone().unwrap_or_default(), show_port => a.show_port.0, locked => a.is_locked.0,
            primary => Some(a.id) == s.allocation_id, address => crate::models::address(a),
        }).collect::<Vec<_>>(),
        free_allocations => free.iter().map(|a| vec![a.id.0.to_string(), crate::models::address(a)]).collect::<Vec<_>>(),
        node_ips => crate::server_info::node_ips(st, node).await.into_iter().map(|i| vec![i.clone(), i]).collect::<Vec<_>>(),
        databases => db_rows,
        db_hosts => usable_hosts,
        any_db_host => !all_hosts.is_empty(),
        db_prefix => format!("s{}_", s.id.0),
    })
}

/// Row actions of AllocationsRelationManager (and the inline columns).
async fn allocation_op(admin: Admin, Path((id, aid, op)): Path<(i64, i64, String)>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, _) = find(&admin, id).await?;
    let st = &admin.st;
    let a = allocations::Entity::find_by_id(aid).one(&st.db).await?.ok_or(WebError::NotFound)?;
    if a.server_id != Some(s.id) {
        return Err(WebError::NotFound);
    }
    let now = Timestamp(pelican_core::time::now());
    let update = |am: allocations::ActiveModel| async move { pelican_db::write::update(&st.db, &am).await };
    let base = allocations::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(a.id),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    match op.as_str() {
        "primary" => {
            let b = st.db.get_database_backend();
            st.db
                .execute_raw(raw::stmt(
                    b,
                    "update {servers} set {allocation_id} = ?, {updated_at} = ? where {id} = ?",
                    vec![aid.into(), now.into(), id.into()],
                ))
                .await?;
        }
        "lock" | "unlock" => {
            update(allocations::ActiveModel { is_locked: Set(Bool(op == "lock")), ..base }).await?;
        }
        "show-port" => {
            update(allocations::ActiveModel { show_port: Set(Bool(form.bool("show_port"))), ..base }).await?;
        }
        "alias" => {
            let v = form.get("ip_alias").trim().to_string();
            update(allocations::ActiveModel { ip_alias: Set((!v.is_empty()).then_some(v)), ..base }).await?;
        }
        "notes" => {
            let v = form.get("notes").trim().to_string();
            update(allocations::ActiveModel { notes: Set((!v.is_empty()).then_some(v)), ..base }).await?;
        }
        "dissociate" => release(&admin, &s, &[aid]).await?,
        _ => return Err(WebError::Unprocessable),
    }
    Ok(admin.redirect(&back(id)))
}

/// `Server::releaseAllocations()`: unassign, then re-pick the primary if needed.
async fn release(admin: &Admin, s: &servers::Model, ids: &[i64]) -> Result<(), WebError> {
    let st = &admin.st;
    let txn = st.db.begin().await?;
    let b = txn.get_database_backend();
    let now = pelican_core::time::now();
    let marks = vec!["?"; ids.len()].join(", ");
    let mut vals: Vec<sea_orm::Value> = vec![
        sea_orm::Value::BigInt(None),
        sea_orm::Value::String(None),
        false.into(),
        true.into(),
        now.into(),
        s.id.0.into(),
    ];
    vals.extend(ids.iter().map(|i| sea_orm::Value::from(*i)));
    // Allocation::RELEASE_ATTRIBUTES.
    txn.execute_raw(raw::stmt(
        b,
        &format!("update {{allocations}} set {{server_id}} = ?, {{notes}} = ?, {{is_locked}} = ?, {{show_port}} = ?, {{updated_at}} = ? where {{server_id}} = ? and {{id}} in ({marks})"),
        vals,
    ))
    .await?;
    let primary_released = s.allocation_id.is_none_or(|p| ids.contains(&p.0));
    if primary_released {
        let next = allocations::Entity::find()
            .filter(allocations::Column::ServerId.eq(s.id.0))
            .filter(allocations::Column::NodeId.eq(s.node_id.0))
            .order_by_asc(allocations::Column::Id)
            .one(&txn)
            .await?;
        txn.execute_raw(raw::stmt(
            b,
            "update {servers} set {allocation_id} = ?, {updated_at} = ? where {id} = ?",
            vec![next.as_ref().map(|a| a.id.0).into(), now.into(), s.id.0.into()],
        ))
        .await?;
        if let Some(p) = &next {
            let _ = admin
                .activity("server:allocation.primary")
                .subject(morph::SERVER, s.id.0)
                .subject(morph::ALLOCATION, p.id.0)
                .property("allocation", crate::models::address(p))
                .log_in(st, &txn)
                .await;
        }
    }
    txn.commit().await?;
    Ok(())
}

/// `DissociateBulkAction` of AllocationsRelationManager: releases every
/// selected allocation assigned to this server (same `release()` as single).
async fn allocation_bulk_dissociate(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, _) = find(&admin, id).await?;
    let selected = form.list("ids").iter().filter_map(|v| v.parse::<i64>().ok()).collect::<Vec<_>>();
    if !selected.is_empty() {
        // Only allocations actually bound to this server count.
        let st = &admin.st;
        let owned: Vec<i64> = allocations::Entity::find()
            .filter(allocations::Column::Id.is_in(selected))
            .filter(allocations::Column::ServerId.eq(s.id.0))
            .all(&st.db)
            .await?
            .iter()
            .map(|a| a.id.0)
            .collect();
        if !owned.is_empty() {
            release(&admin, &s, &owned).await?;
            admin.flash_success(admin.t("filament-actions::dissociate.multiple.notifications.dissociated.title"));
        }
    }
    Ok(admin.redirect(&back(id)))
}

/// CreateAction: `AssignmentService::handle($node, $data, $server)` with `is_locked`.
async fn allocation_create(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, n) = find(&admin, id).await?;
    let ip = form.get("allocation_ip").trim().to_string();
    let ports: Vec<Value> = form
        .get("allocation_ports")
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| Value::String(p.to_string()))
        .collect();
    let alias = form.opt("allocation_alias").map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    if ip.is_empty() || ports.is_empty() {
        return Ok(admin.redirect(&back(id)));
    }
    match pelican_api::application::nodes::assign_allocations(&admin.st, n.id, &ip, &ports, alias, Some(s.id), true)
        .await
    {
        Ok(ids) => {
            if s.allocation_id.is_none() {
                if let Some(first) = ids.first() {
                    let b = admin.st.db.get_database_backend();
                    admin
                        .st
                        .db
                        .execute_raw(raw::stmt(
                            b,
                            "update {servers} set {allocation_id} = ? where {id} = ?",
                            vec![(*first).into(), id.into()],
                        ))
                        .await?;
                }
            }
            admin.flash_success(admin.t("filament-actions::create.single.notifications.created.title"));
        }
        Err(e) => admin.flash_danger(
            admin.t("admin/server.notifications.create_failed"),
            e.errors.first().map(|o| o.detail.clone()),
        ),
    }
    Ok(admin.redirect(&back(id)))
}

/// AssociateAction (multiple): attach free allocations of the node, lock them.
async fn allocation_associate(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, n) = find(&admin, id).await?;
    let ids: Vec<i64> = form.list("allocations").iter().filter_map(|v| v.parse().ok()).collect();
    let st = &admin.st;
    let now = Timestamp(pelican_core::time::now());
    let mut first: Option<i64> = None;
    for aid in ids {
        let Some(a) = allocations::Entity::find_by_id(aid).one(&st.db).await? else { continue };
        if a.node_id != n.id || a.server_id.is_some() {
            continue;
        }
        pelican_db::write::update(
            &st.db,
            &allocations::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(a.id),
                server_id: Set(Some(s.id)),
                is_locked: Set(Bool(true)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?;
        first.get_or_insert(aid);
    }
    if let (None, Some(f)) = (s.allocation_id, first) {
        let b = st.db.get_database_backend();
        st.db
            .execute_raw(raw::stmt(
                b,
                "update {servers} set {allocation_id} = ? where {id} = ?",
                vec![f.into(), id.into()],
            ))
            .await?;
    }
    Ok(admin.redirect(&back(id)))
}

/// CreateAction of DatabasesRelationManager (limit not enforced for admins).
async fn database_create(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update server")?;
    let (s, n) = find(&admin, id).await?;
    let st = &admin.st;
    let host_id = form.get("database_host_id").parse::<i64>().unwrap_or(0);
    let host = database_hosts::Entity::find_by_id(host_id).one(&st.db).await?;
    let linked = database_host_node::Entity::find()
        .filter(database_host_node::Column::DatabaseHostId.eq(host_id))
        .filter(database_host_node::Column::NodeId.eq(n.id.0))
        .one(&st.db)
        .await?
        .is_some();
    let Some(host) = host.filter(|_| linked) else {
        admin.flash_danger(admin.t("admin/server.failed_to_create"), None);
        return Ok(admin.redirect(&back(id)));
    };
    let remote = form.opt("remote").map(str::trim).filter(|r| !r.is_empty()).unwrap_or("%").to_string();
    let valid_remote = remote.chars().all(|c| c.is_alphanumeric() || "_-/.%:".contains(c));
    let name = form
        .opt("database")
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(String::from)
        .unwrap_or_else(|| format!("{}{}", crate::words::random_word(), rand::random_range(1..=420)));
    let valid_name = name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !valid_remote || !valid_name {
        admin.flash_danger(admin.t("admin/server.failed_to_create"), Some("The given data was invalid.".into()));
        return Ok(admin.redirect(&back(id)));
    }
    let unique = pelican_core::databases::unique_name(&name, s.id.0);
    let mut unlimited = s.clone();
    unlimited.database_limit = None;
    let ctx = admin.api_ctx(Bound::default());
    match pelican_api::client::databases::create_database(st, &ctx, &unlimited, &host, &unique, &remote, false).await {
        Ok(_) => admin.flash_success(admin.t("filament-actions::create.single.notifications.created.title")),
        Err(e) => admin.session.flash(
            "danger",
            admin.t("admin/server.failed_to_create"),
            Some(e.errors.first().map(|o| o.detail.clone()).unwrap_or_default()),
        ),
    }
    Ok(admin.redirect(&back(id)))
}

async fn database_op(admin: Admin, Path((id, did, op)): Path<(i64, i64, String)>) -> WebResult {
    admin.authorize("update server")?;
    let (s, _) = find(&admin, id).await?;
    let st = &admin.st;
    let d = databases::Entity::find_by_id(did).one(&st.db).await?.ok_or(WebError::NotFound)?;
    if d.server_id != s.id {
        return Err(WebError::NotFound);
    }
    match op.as_str() {
        "rotate" => {
            let txn = st.db.begin().await?;
            match pelican_api::client::databases::rotate_password_in(st, &txn, &d).await {
                Ok(()) => {
                    txn.commit().await?;
                    admin.flash_success(admin.t("admin/databasehost.rotated"));
                }
                Err(_) => admin.flash_danger(admin.t("admin/databasehost.rotate_error"), None),
            }
        }
        "delete" => {
            let ctx = admin.api_ctx(Bound::default());
            let txn = st.db.begin().await?;
            match pelican_api::client::databases::delete_database_in(st, &ctx, &txn, &d).await {
                Ok(()) => {
                    txn.commit().await?;
                    admin.flash_success(
                        admin.t_with("server/database.delete_notification", &[("database", &d.database)]),
                    );
                }
                Err(_) => admin.flash_danger(
                    admin.t_with("server/database.delete_notification_fail", &[("database", &d.database)]),
                    None,
                ),
            }
        }
        _ => return Err(WebError::Unprocessable),
    }
    Ok(admin.redirect(&back(id)))
}
