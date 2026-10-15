//! Server panel › Network (app/Filament/Server/Resources/Allocations):
//! list of the server's allocations with inline notes editing
//! (`TextInputColumn`), the primary-star toggle (`IconColumn` action),
//! detaching rows (`DetachAction` → `Server::releaseAllocations`) and the
//! toolbar add-allocation action (`FindAssignableAllocationService`),
//! reusing `pelican_api::client::network`.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::error::ApiError;
use pelican_api::pipeline::Ctx;
use pelican_api::services::{LogActivity, server_saved};
use pelican_api::{binding::Bound, client::network as api, model_events};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::Timestamp;
use pelican_db::entities::allocations;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
    Value as DbValue,
};
use serde_json::json;

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/network", get(index))
        .route("/server/{server}/network/add", post(add))
        .route("/server/{server}/network/{id}/primary", post(set_primary))
        .route("/server/{server}/network/{id}/notes", post(notes))
        .route("/server/{server}/network/{id}/delete", post(delete))
}

/// A client-API `Ctx` for the domain functions: ip and actor like this web
/// session, default subject = the server (`ServerSubject` of the client group).
fn api_ctx(p: &ServerPanel) -> Result<Ctx, WebError> {
    let ip: IpAddr = p.ip.parse().map_err(|_| WebError::internal("invalid request ip"))?;
    Ok(Ctx {
        ip,
        user: Some(Arc::new(p.user.clone())),
        bound: Arc::new(Bound { server: Some(p.server.clone()), ..Default::default() }),
        node: None,
        root: p.st.config.app_url.clone(),
        subject: Some((morph::SERVER, p.server.id.0)),
    })
}

/// An allocation row scoped to the panel's server (Filament tenant scoping).
async fn find(p: &ServerPanel, id: i64) -> Result<allocations::Model, WebError> {
    allocations::Entity::find_by_id(pelican_db::Int(id))
        .filter(allocations::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

fn ts_now() -> Timestamp {
    Timestamp(pelican_core::time::now())
}

/// Display messages of the auto-allocation service exceptions; Filament shows
/// them as danger notifications inside the toolbar action.
fn no_space_message() -> String {
    "Cannot assign additional allocation: no more space available on node.".into()
}

fn not_enabled_message() -> String {
    "Server auto-allocation is not enabled for this instance.".into()
}

/// `$user->can('update', $node)` with the `DefaultAdminPolicies` update rule:
/// spatie `update node` plus node targeting.
fn can_update_node(p: &ServerPanel) -> bool {
    p.user.gate_before("update node") && p.user.can_target_node(p.node.id.0)
}

async fn render_list(
    p: ServerPanel,
    lp: ListParams,
    all: Vec<allocations::Model>,
    total: u64,
) -> Result<Response, WebError> {
    let base = p.base_url();
    let opts = [10, 25, 50, 0];
    let primary_id = p.server.allocation_id.map(|x| x.0);
    let admin_update = can_update_node(&p);
    let can_delete = p.can("allocation.delete");
    let can_update = p.can("allocation.update");
    let t = |k: &str| p.t(&format!("server/network.{k}"));

    // Toolbar state (`add_allocation`): disabled without a primary allocation or
    // at the limit; PHP `count >= null` is always true, so unlimited servers are
    // blocked from adding more.
    let enabled = p.st.config.env.bool_or("PANEL_CLIENT_ALLOCATIONS_ENABLED", false);
    let no_primary = p.allocation().await?.is_none();
    let at_limit = match p.server.allocation_limit {
        None => true,
        Some(l) => total as i64 >= l.0,
    };
    let unavailable = no_primary || at_limit;

    // Page window over the unordered allocation list.
    let mut lp = lp;
    lp.clamp(total);
    let skip = lp.limit_offset().map_or(0usize, |(_, off)| off.min(all.len() as u64) as usize);
    let take = lp.limit_offset().map_or(usize::MAX, |(lim, _)| lim as usize);
    let page_rows: Vec<&allocations::Model> = all.iter().skip(skip).take(take).collect();

    let mut table = Table::new("network", &format!("{base}/network"));
    table.columns = vec![
        Column::new("address", t("address")),
        Column::new("port", t("port")),
        // `visibleFrom('sm')`: hidden below the small breakpoint.
        Column::new("notes", t("notes")).wide_only(),
        Column::new("primary", t("primary")).center(),
        Column::new("is_locked", t("locked")).center(),
    ];
    let delete_label = t("delete");
    for a in &page_rows {
        // TextColumn 'ip' → `$allocation->alias` (ip_alias else ip).
        let alias = a.ip_alias.clone().unwrap_or_else(|| a.ip.clone());
        let is_primary = Some(a.id.0) == primary_id;
        let mut row = Row::new(a.id.0.to_string())
            .cell(
                "address",
                Cell::mono(if alias.parse::<Ipv6Addr>().is_ok() { format!("[{alias}]") } else { alias.clone() }),
            )
            .cell("port", if a.show_port.0 { Cell::text(a.port.0.to_string()) } else { Cell::text(t("port_hidden")) })
            .cell(
                "primary",
                Cell::Icon {
                    icon: (if is_primary { "star-filled" } else { "star" }).into(),
                    color: (if is_primary { "warning" } else { "gray" }).into(),
                    tooltip: Some(if is_primary { t("primary") } else { t("make_primary") }),
                },
            )
            .cell(
                "is_locked",
                Cell::Icon {
                    icon: (if a.is_locked.0 { "lock" } else { "lock-open" }).into(),
                    color: "gray".into(),
                    tooltip: Some(t("locked_helper")),
                },
            );
        // Notes cell; for empty notes the `no_notes` tooltip doubles as the
        // TextInputColumn placeholder that js/network.js reads.
        row = row.cell(
            "notes",
            match &a.notes {
                Some(n) => Cell::text(n.clone()),
                None => Cell::Text {
                    text: String::new(),
                    desc: None,
                    mono: false,
                    copy: None,
                    tooltip: can_update.then(|| t("no_notes")),
                },
            },
        );
        // IconColumn 'primary' action (`$server->update(['allocation_id' => …])`
        // guarded by AllocationUpdate).
        if can_update {
            row.actions.push(Action::post(t("make_primary"), "star", format!("{base}/network/{}/primary", a.id.0)));
        }
        // DetachAction: visible when unlocked (or node-updating admins) and more
        // than one allocation remains, authorized with allocation.delete.
        if can_delete && (!a.is_locked.0 || admin_update) && all.len() > 1 {
            row.actions.push(
                Action::post(delete_label.clone(), "x", format!("{base}/network/{}/delete", a.id.0))
                    .color("danger")
                    .confirm(p.t("filament-actions::modal.confirmation")),
            );
        }
        table.rows.push(row);
    }
    table.empty_icon = "network".into();
    table.empty_heading = p.t_with("filament-tables::table.empty.heading", &[("model", "Allocation")]);
    table.empty_description = p.t_with("filament-tables::table.empty.description", &[("model", "Allocation")]);
    table.finish(&lp, total, &opts, &p.locale());

    p.page(
        "network",
        "server/network.html",
        context! {
            title => t("title"),
            base => base,
            table => Mj::from_serialize(&table),
            can_add => enabled && p.can("allocation.create") && !unavailable,
            add_unavailable => unavailable,
            add_tooltip => if no_primary { t("no_primary") } else if at_limit { t("limit") } else { t("add") },
            add_icon => if unavailable { "network-off" } else { "network" },
        },
    )
}

/// GET /server/{server}/network — ListAllocations (`AllocationResource::defaultTable`).
async fn index(p: ServerPanel) -> WebResult {
    p.authorize("allocation.read")?;
    let lp = ListParams::parse(p.query.as_deref(), &[10, 25, 50, 0], 10);
    let all = api::server_allocations(&p.st.db, p.server.id.0)
        .await
        .map_err(|e| WebError::internal(format!("server allocations: {e:?}")))?;
    let total = all.len() as u64;
    render_list(p, lp, all, total).await
}

/// POST …/network/add — the toolbar `add_allocation` Action:
/// FindAssignableAllocationService::handle inside the
/// `server:allocation.create` activity transaction.
async fn add(p: ServerPanel, form: crate::forms::Form) -> WebResult<Response> {
    p.authorize("allocation.create")?;
    let back = form.back(&format!("{}/network", p.base_url()));
    let server = p.server.clone();

    // Service outcome: Ok(Done) or the DisplayException message for the danger
    // notification; DB/internal errors surface through WebError.
    enum Out {
        Done,
        Message(String),
    }
    let result: Result<Out, WebError> = async {
        let txn = p.st.db.begin().await?;

        // throw_unless(config(...enabled), AutoAllocationNotEnabledException)
        if !p.st.config.env.bool_or("PANEL_CLIENT_ALLOCATIONS_ENABLED", false) {
            return Ok(Out::Message(not_enabled_message()));
        }
        // throw_unless($server->allocation, DisplayException(no_primary))
        let Some(primary) = p.allocation_in(&txn).await? else {
            return Ok(Out::Message(p.t("server/network.no_primary")));
        };

        let create_new = p.st.config.env.bool_or("PANEL_CLIENT_ALLOCATIONS_CREATE_NEW", true);
        // Assert::integerish runs before any allocation search; a bad
        // configuration raises an uncaught InvalidArgumentException (generic 500).
        let (start, end) = match (
            api::integerish(p.st.config.env.string("PANEL_CLIENT_ALLOCATIONS_RANGE_START").as_deref()),
            api::integerish(p.st.config.env.string("PANEL_CLIENT_ALLOCATIONS_RANGE_END").as_deref()),
        ) {
            (Ok(s), Ok(e)) => (s, e),
            _ => return Err(WebError::internal("InvalidArgumentException")),
        };
        // lockForUpdate + where node/ip/between port + whereNull server_id +
        // inRandomOrder → first.
        let backend = txn.get_database_backend();
        let cands = allocations::Model::find_by_statement(pelican_db::raw::stmt(
            backend,
            "select * from {allocations} where {allocations.node_id} = ? and {allocations.ip} = ? \
             and {allocations.port} between ? and ? and {allocations.server_id} is null",
            vec![
                DbValue::from(server.node_id.0),
                DbValue::from(primary.ip.clone()),
                DbValue::from(start),
                DbValue::from(end),
            ],
        ))
        .all(&txn)
        .await?;

        // If create_new is disabled, only pick from existing allocations.
        if cands.is_empty() && !create_new {
            return Ok(Out::Message(no_space_message()));
        }

        // $allocation ??= createNewAllocation(...): a DisplayException out of the
        // service keeps its message (port range), everything else reads as no
        // space left.
        let chosen = match use_rand_pick(&cands) {
            Some(a) => a.clone(),
            None => match api::create_new_allocation(&txn, &server, &primary, start, end).await {
                Ok(a) => a,
                Err(e) if e.errors.first().is_some_and(|o| o.code == "PortOutOfRangeException") => {
                    return Ok(Out::Message(p.t("exceptions.allocations.port_out_of_range")));
                }
                Err(_) => return Ok(Out::Message(no_space_message())),
            },
        };

        // `$allocation->update(['server_id' => $server->id])`.
        pelican_db::write::update(
            &txn,
            &allocations::ActiveModel {
                id: Set(chosen.id),
                server_id: Set(Some(server.id)),
                updated_at: Set(Some(ts_now())),
                ..Default::default()
            },
        )
        .await?;
        let fresh =
            allocations::Entity::find_by_id(chosen.id).one(&txn).await?.ok_or(WebError::internal("ErrorException"))?;

        if p.server.allocation_id.is_none() {
            // if (!$server->allocation_id) $server->update([...]) (unreachable in
            // practice, kept like the resource).
            pelican_db::write::update(
                &txn,
                &pelican_db::entities::servers::ActiveModel {
                    id: Set(server.id),
                    allocation_id: Set(Some(fresh.id)),
                    updated_at: Set(Some(ts_now())),
                    ..Default::default()
                },
            )
            .await?;
        }

        let ctx = api_ctx(&p)?;
        ctx.activity("server:allocation.create")
            .subject(morph::ALLOCATION, fresh.id.0)
            .property("allocation", api::alloc_address(&fresh))
            .log_in(&p.st, &txn)
            .await
            .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
        txn.commit().await?;
        // After the transaction: webhook delivery is a synchronous HTTP call.
        // FindAssignableAllocationService → `$allocation->update(['server_id' => …])`
        // (the candidate row has `server_id is null`, so always dirty).
        model_events::fire_id::<allocations::Entity>(&p.st, model_events::Action::Updated, fresh.id.0).await;
        if p.server.allocation_id.is_none() {
            // `if (!$server->allocation_id) $server->update([...])`.
            model_events::fire_id::<pelican_db::entities::servers::Entity>(
                &p.st,
                model_events::Action::Updated,
                server.id.0,
            )
            .await;
        }
        Ok(Out::Done)
    }
    .await;

    match result {
        Ok(Out::Done) => {}
        Ok(Out::Message(msg)) => p.flash_danger(msg, None),
        Err(e) => {
            // Uncaught service exception: Filament renders the generic 500 message.
            tracing::warn!(error=?e, "auto-allocation failed");
            p.flash_danger(pelican_api::error::GENERIC_500, None);
        }
    }
    Ok(p.redirect(&back))
}

/// Stand-in for SQL `inRandomOrder()` / PHP `array_rand`.
fn use_rand_pick(v: &[allocations::Model]) -> Option<&allocations::Model> {
    use rand::seq::IndexedRandom;
    v.choose(&mut rand::rng())
}

impl ServerPanel {
    /// `Server::allocation` re-read inside a transaction/connection.
    async fn allocation_in<C: ConnectionTrait>(&self, db: &C) -> Result<Option<allocations::Model>, WebError> {
        Ok(match self.server.allocation_id {
            Some(id) => allocations::Entity::find_by_id(id.0).one(db).await?,
            None => None,
        })
    }
}

/// POST …/network/{id}/primary — the `IconColumn` 'primary' action:
/// `$user->can(AllocationUpdate) && $server->update(['allocation_id' => $id])`
/// (no activity event in the resource; the `saved` hook still runs, matching
/// the Client API's set-primary).
async fn set_primary(p: ServerPanel, RecordId(id): RecordId, form: crate::forms::Form) -> WebResult<Response> {
    p.authorize("allocation.update")?;
    let alloc = find(&p, id).await?;
    let back = form.back(&format!("{}/network", p.base_url()));

    // associate()+save(): clean models perform no write at all.
    if p.server.allocation_id != Some(alloc.id) {
        let ctx = api_ctx(&p)?;
        pelican_db::write::update(
            &p.st.db,
            &pelican_db::entities::servers::ActiveModel {
                id: Set(p.server.id),
                allocation_id: Set(Some(alloc.id)),
                updated_at: Set(Some(ts_now())),
                ..Default::default()
            },
        )
        .await?;
        // IconColumn action → `$server->update(['allocation_id' => …])` (updated).
        model_events::fire_id::<pelican_db::entities::servers::Entity>(
            &p.st,
            model_events::Action::Updated,
            p.server.id.0,
        )
        .await;
        // Server::saved hook; failures surface as an internal error like an
        // uncaught exception in the original.
        if let Err(e) = server_saved(&p.st, &ctx, &p.st.db, &p.server).await {
            tracing::warn!(error=?e, "server saved hook failed");
            return Err(WebError::internal("Server save hook failed"));
        }
    }
    Ok(p.redirect(&back))
}

/// POST …/network/{id}/notes — the `TextInputColumn` saves (via network.js):
/// Client API rules (`UpdateAllocationRequest`: present, max:256), writing only
/// when the value changed and logging `server:allocation.notes`.
async fn notes(p: ServerPanel, RecordId(id): RecordId, form: crate::forms::Form) -> WebResult<Response> {
    p.authorize("allocation.update")?;
    let alloc = find(&p, id).await?;

    // TextInputColumn: Allocation::$validationRules, no activity event.
    let input = form.to_value();
    let mut v = pelican_api::validation::Validator::for_value(&input);
    v.rules("notes", &["nullable", "string", "max:256", "present"]);
    v.attribute_name("notes", &p.t("server/network.notes").to_lowercase());
    v.run();
    if v.has_errors() {
        let msg = v.error_messages().first().map(|(_, m)| m.clone()).unwrap_or_default();
        let mut resp = axum::Json(json!({ "message": msg })).into_response();
        *resp.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return Ok(resp);
    }

    let original = alloc.notes.clone();
    // ConvertEmptyStringsToNull turned "" into null before validation.
    let new_notes = match form.opt("notes") {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    if new_notes != original {
        // Client API `update` → `$allocation->forceFill(['notes' => …])->save()`
        // (updated; the change check above matches Eloquent isDirty).
        pelican_db::write::update(
            &p.st.db,
            &allocations::ActiveModel {
                id: Set(alloc.id),
                notes: Set(new_notes.clone()),
                updated_at: Set(Some(ts_now())),
                ..Default::default()
            },
        )
        .await?;
        model_events::fire_id::<allocations::Entity>(&p.st, model_events::Action::Updated, id).await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// POST …/network/{id}/delete — `DetachAction` through
/// `Server::releaseAllocations([id], keepOne: true)` (without the Client API's
/// extra allocation-limit guard the resource does not apply), then the
/// `server:allocation.delete` event of the action closure.
async fn delete(p: ServerPanel, RecordId(id): RecordId, form: crate::forms::Form) -> WebResult<Response> {
    p.authorize("allocation.delete")?;
    let alloc = find(&p, id).await?;
    let back = form.back(&format!("{}/network", p.base_url()));
    let server = p.server.clone();
    let ctx = api_ctx(&p)?;

    let result: Result<(), ApiError> = async {
        let txn = p.st.db.begin().await?;
        // releaseAllocations re-reads the locked primary pointer first.
        let current = pelican_db::entities::servers::Entity::find_by_id(server.id)
            .one(&txn)
            .await?
            .ok_or_else(|| ApiError::internal("ErrorException"))?
            .allocation_id
            .map(|x| x.0);

        // throw_if($keepOne && !$this->allocations()->…whereNotIn('id', $ids)->doesntExist())
        if api::other_node_allocations(&txn, server.id.0, server.node_id.0, alloc.id.0).await?.is_empty() {
            return Err(ApiError::display(
                "DisplayException",
                400,
                "You cannot delete the last allocation for this server.",
            ));
        }

        // RELEASE_ATTRIBUTES via query-builder update (updated_at added by Eloquent Builder).
        txn.execute_raw(pelican_db::raw::stmt(
            txn.get_database_backend(),
            // Postgres forbids table-qualified column names in the SET list.
            "update {allocations} set server_id = null, notes = null, is_locked = ?, show_port = ?, updated_at = ? \
             where {allocations.server_id} = ? and {allocations.id} = ?",
            vec![
                DbValue::Bool(Some(false)),
                DbValue::Bool(Some(true)),
                DbValue::ChronoDateTime(Some(pelican_core::time::now())),
                DbValue::from(server.id.0),
                DbValue::from(alloc.id.0),
            ],
        ))
        .await?;

        // Re-associate the primary when it was just released.
        let mut primary_saved = false;
        if current.is_none() || current == Some(alloc.id.0) {
            let next = api::other_node_allocations(&txn, server.id.0, server.node_id.0, alloc.id.0)
                .await?
                .into_iter()
                .min_by_key(|a| a.id.0);
            let new_id = next.as_ref().map(|a| a.id.0);
            if new_id != current {
                pelican_db::write::update(
                    &txn,
                    &pelican_db::entities::servers::ActiveModel {
                        id: Set(server.id),
                        allocation_id: Set(new_id.map(pelican_db::Int)),
                        updated_at: Set(Some(ts_now())),
                        ..Default::default()
                    },
                )
                .await?;
                primary_saved = true;
                server_saved(&p.st, &ctx, &txn, &server)
                    .await
                    .map_err(|_| ApiError::internal("Server save hook failed"))?;
            }
            if let Some(primary) = next {
                ctx.activity("server:allocation.primary")
                    .subject(morph::ALLOCATION, primary.id.0)
                    .property("allocation", api::alloc_address(&primary))
                    .log_in(&p.st, &txn)
                    .await?;
            }
        }
        txn.commit().await?;
        // `releaseAllocations`: the RELEASE_ATTRIBUTES reset is a mass relation
        // update (no events); the re-picked primary is `$this->save()` (updated).
        if primary_saved {
            model_events::fire_id::<pelican_db::entities::servers::Entity>(
                &p.st,
                model_events::Action::Updated,
                server.id.0,
            )
            .await;
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            ctx.activity("server:allocation.delete")
                .subject(morph::ALLOCATION, alloc.id.0)
                .property("allocation", api::alloc_address(&alloc))
                .log(&p.st)
                .await
                .map_err(|e| WebError::internal(format!("activity log: {e:?}")))?;
            p.flash_success(p.t("filament-actions::detach.single.notifications.detached.title"));
        }
        Err(e) => {
            tracing::warn!(error=?e, "allocation detach failed");
            p.flash_danger(e.errors.first().map(|o| o.detail.clone()).unwrap_or_default(), None);
        }
    }
    Ok(p.redirect(&back))
}

/// Same allocations-on-node-excluding-one check of `releaseAllocations`
/// (the `DetachAction` visibility uses the full count instead).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_messages_match_original() {
        assert_eq!(no_space_message(), "Cannot assign additional allocation: no more space available on node.");
        assert_eq!(not_enabled_message(), "Server auto-allocation is not enabled for this instance.");
    }
}
