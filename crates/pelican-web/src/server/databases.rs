//! Server panel › Databases (app/Filament/Server/Resources/Databases):
//! list with a create action (`CreateAction`), view fields of `defaultForm`,
//! password rotation and deletion — reusing the Client API logic of
//! `pelican_api::client::databases` (`DatabaseManagementService`).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::Path;
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::binding::Bound;
use pelican_api::client::databases as api;
use pelican_api::error::ApiError;
use pelican_api::pipeline::Ctx;
use pelican_api::services::LogActivity;
use pelican_core::activity::morph;
use pelican_core::databases::{str_random, unique_name};
use pelican_core::state::AppState;
use pelican_db::entities::{database_hosts, databases};
use rand::seq::IndexedRandom;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait};
use serde_json::{Value, json};

use super::ServerPanel;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/databases", get(index).post(create))
        .route("/server/{server}/databases/{id}", get(view))
        .route("/server/{server}/databases/{id}/rotate-password", post(rotate_password))
        .route("/server/{server}/databases/{id}/delete", post(delete))
}

#[derive(FromQueryResult)]
struct Aggregate {
    aggregate: pelican_db::Int,
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

/// A database row scoped to the panel's server (`scopeBindings`).
async fn find(p: &ServerPanel, id: i64) -> Result<databases::Model, WebError> {
    databases::Entity::find_by_id(pelican_db::Int(id))
        .filter(databases::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)
}

/// The host behind a database; missing rows are internal errors
/// (`ErrorException` in the original).
async fn host_of<C: ConnectionTrait>(db: &C, id: pelican_db::Int) -> Result<database_hosts::Model, WebError> {
    database_hosts::Entity::find_by_id(id).one(db).await?.ok_or_else(|| WebError::internal("ErrorException"))
}

/// Notification strings of the resource carry `<b>` tags; flashes are plain text.
fn plain(s: &str) -> String {
    s.replace("<b>", "").replace("</b>", "")
}

/// PHP `urlencode()` (RFC 1738): space to `+`, everything outside
/// `[A-Za-z0-9-_.]` percent-encoded — used by `Database::jdbc()`.
fn php_urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn node_hosts(p: &ServerPanel) -> Result<Vec<database_hosts::Model>, WebError> {
    let b = p.st.db.get_database_backend();
    let sql = "select {database_hosts}.* from {database_hosts} inner join {database_host_node} on \
               {database_hosts.id} = {database_host_node.database_host_id} where {database_host_node.node_id} = ?";
    let rows = p.st.db.query_all_raw(pelican_db::raw::stmt(b, sql, vec![p.server.node_id.into()])).await?;
    Ok(rows.iter().map(|r| database_hosts::Model::from_query_result(r, "")).collect::<Result<Vec<_>, _>>()?)
}

/// Options of the create form's host select: `$server->node->databaseHosts`.
/// When the node has no hosts and random hosts are allowed, every host is
/// offered instead (the Client API picks randomly in that case).
async fn host_options(p: &ServerPanel) -> Result<Vec<(String, String)>, WebError> {
    let pool = match node_hosts(p).await? {
        hosts if !hosts.is_empty() => hosts,
        _ if p.st.config.client_databases_allow_random => database_hosts::Entity::find().all(&p.st.db).await?,
        _ => Vec::new(),
    };
    Ok(pool.into_iter().map(|h| (h.id.0.to_string(), h.name)).collect())
}

/// Databases of the server + their total count.
async fn rows(p: &ServerPanel, lp: &ListParams) -> Result<(Vec<databases::Model>, u64), WebError> {
    let b = p.st.db.get_database_backend();
    let order = match lp.sort.as_deref() {
        Some("created_at") => format!("{{databases.created_at}} {}", if lp.desc { "desc" } else { "asc" }),
        _ => "{databases.id} asc".to_string(),
    };
    let mut sql =
        format!("select {{databases}}.* from {{databases}} where {{databases.server_id}} = ? order by {order}");
    if let Some((limit, offset)) = lp.limit_offset() {
        sql.push_str(&format!(" limit {limit} offset {offset}"));
    }
    let db_rows = databases::Model::find_by_statement(pelican_db::raw::stmt(b, &sql, vec![p.server.id.into()]))
        .all(&p.st.db)
        .await?;
    let total = Aggregate::find_by_statement(pelican_db::raw::stmt(
        b,
        "select count(*) as {aggregate} from {databases} where {databases.server_id} = ?",
        vec![p.server.id.into()],
    ))
    .one(&p.st.db)
    .await?
    .map(|a| a.aggregate.0.max(0) as u64)
    .unwrap_or(0);
    Ok((db_rows, total))
}

/// Values submitted through the create modal (empty remote shows `%`).
#[derive(Clone, Debug)]
struct CreateState {
    host_id: String,
    database: String,
    remote: String,
}

impl CreateState {
    fn initial() -> CreateState {
        CreateState { host_id: String::new(), database: String::new(), remote: "%".into() }
    }

    fn from_form(f: &Form) -> CreateState {
        CreateState {
            // Filament required select posts an empty string when unset.
            host_id: f.opt("database_host_id").unwrap_or("").trim().to_string(),
            // Laravel does not trim; empty values become null (`??=` case).
            database: f.opt("database").unwrap_or("").to_string(),
            remote: f.opt("remote").unwrap_or("").to_string(),
        }
    }
}

/// Validation of the create form: the select is required and `remote` follows
/// the rules the Client API applies (`StoreDatabaseRequest`).
async fn validate(p: &ServerPanel, state: &CreateState) -> Result<Errors, WebError> {
    let input = json!({
        "database_host_id": if state.host_id.is_empty() { Value::Null } else { Value::from(state.host_id.clone()) },
        "remote": if state.remote.is_empty() { Value::Null } else { Value::from(state.remote.clone()) },
    });
    let mut v = pelican_api::validation::Validator::for_value(&input);
    let label = |k: &str| p.t(&format!("server/database.{k}")).to_lowercase();
    v.attribute_name("database_host_id", &label("database_host"));
    v.attribute_name("remote", &label("connections_from"));
    v.rules("database_host_id", &["required"]);
    v.rules("remote", &["required", "string", r"regex:/^[\w\-\/.%:]+$/"]);
    v.run();
    Ok(Errors::from_validator(&v))
}

async fn render_list(p: ServerPanel, lp: ListParams, total: u64, state: CreateState, errors: Errors) -> WebResult {
    let base = p.base_url();
    let opts = [10, 25, 50, 0];
    let (rows, _) = rows(&p, &lp).await?;

    // Hosts behind each row (`Database::address()`).
    let ids: Vec<i64> = rows.iter().map(|d| d.database_host_id.0).collect();
    let hosts: HashMap<i64, database_hosts::Model> = if ids.is_empty() {
        HashMap::new()
    } else {
        database_hosts::Entity::find()
            .filter(database_hosts::Column::Id.is_in(ids))
            .all(&p.st.db)
            .await?
            .into_iter()
            .map(|h| (h.id.0, h))
            .collect()
    };

    let t = |k: &str| p.t(&format!("server/database.{k}"));
    let mut table = Table::new("databases", &format!("{base}/databases"));
    table.columns = vec![
        Column::new("host", t("host")),
        Column::new("database", t("database")),
        Column::new("username", t("username")),
        Column::new("remote", t("remote")),
        Column::new("created_at", t("created_at")).sortable(),
    ];
    let can_delete = p.can("database.delete");
    let view_label = p.t("filament-actions::view.single.label");
    let delete_label = p.t("filament-actions::delete.single.label");
    for d in rows {
        let address = hosts.get(&d.database_host_id.0).map(|h| format!("{}:{}", h.name, h.port.0)).unwrap_or_default();
        let created = d.created_at.map(|t| t.0.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default();
        let mut row = Row::new(d.id.0.to_string())
            .cell("host", Cell::badge(address, "primary"))
            .cell("database", Cell::text(d.database.clone()))
            .cell("username", Cell::text(d.username.clone()))
            .cell("remote", Cell::text(d.remote.clone()))
            .cell("created_at", Cell::text(created));
        row.actions.push(Action::link(view_label.clone(), "eye", format!("{base}/databases/{}", d.id.0)));
        if can_delete {
            row.actions.push(
                Action::post(delete_label.clone(), "trash", format!("{base}/databases/{}/delete", d.id.0))
                    .color("danger")
                    .confirm(p.t("filament-actions::modal.confirmation")),
            );
        }
        table.rows.push(row);
    }
    table.empty_icon = "database".into();
    table.empty_heading = t("empty");
    table.finish(&lp, total, &opts, &p.locale());

    // Toolbar button (CreateAction): disabled at the limit with its tooltip.
    let limit = p.server.database_limit.map(|l| l.0);
    let at_limit = matches!(limit, Some(l) if total as i64 >= l);
    let host_options = host_options(&p).await?;

    p.page(
        "databases",
        "server/databases.html",
        context! {
            title => p.t("server/database.title"),
            base => base,
            table => Mj::from_serialize(&table),
            can_create => p.can("database.create"),
            at_limit => at_limit,
            host_options => host_options,
            v_host => state.host_id,
            v_database => state.database,
            v_remote => state.remote,
            v_errors => Mj::from_serialize(&errors),
        },
    )
}

/// `GET /server/{server}/databases` (ListDatabases).
async fn index(p: ServerPanel) -> WebResult {
    p.authorize("database.read")?;
    let lp = ListParams::parse(p.query.as_deref(), &[10, 25, 50, 0], 10);
    render_list(p, lp, 0, CreateState::initial(), Errors::default()).await
}

/// Outcome of resolving the create form's host field.
enum HostChoice {
    Chosen(database_hosts::Model),
    /// A posted id that does not exist: the insert would violate the foreign
    /// key, so the action reports its failure notification.
    Failed,
    /// Nothing posted and no pool to draw from: the required select fails.
    Required,
}

/// The `CreateAction` host field: a host attached to the server's node first,
/// falling back to a random one from all hosts when the node has none and
/// random assignment is allowed (`DeployServerDatabaseService`).
async fn choose_host(p: &ServerPanel, host_id: &str) -> Result<HostChoice, WebError> {
    if !host_id.is_empty() {
        let found = match host_id.parse::<i64>() {
            Ok(id) => database_hosts::Entity::find_by_id(pelican_db::Int(id)).one(&p.st.db).await?,
            Err(_) => None,
        };
        return Ok(match found {
            Some(h) => HostChoice::Chosen(h),
            None if p.st.config.client_databases_allow_random && node_hosts(p).await?.is_empty() => {
                let pool = database_hosts::Entity::find().all(&p.st.db).await?;
                match pool.choose(&mut rand::rng()).cloned() {
                    Some(h) => HostChoice::Chosen(h),
                    None => HostChoice::Failed,
                }
            }
            None => HostChoice::Failed,
        });
    }
    if !node_hosts(p).await?.is_empty() {
        return Ok(HostChoice::Required);
    }
    if p.st.config.client_databases_allow_random {
        let pool = database_hosts::Entity::find().all(&p.st.db).await?;
        return Ok(match pool.choose(&mut rand::rng()).cloned() {
            Some(h) => HostChoice::Chosen(h),
            // NoSuitableDatabaseHostException would abort the action.
            None => HostChoice::Failed,
        });
    }
    Ok(HostChoice::Required)
}

/// `POST /server/{server}/databases` — the `CreateAction` of the toolbar:
/// random name fallback, unique prefix, then
/// `DatabaseManagementService::create()` via the Client API logic.
async fn create(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("database.create")?;
    let base = p.base_url();
    let list_url = format!("{base}/databases");
    let state = CreateState::from_form(&form);

    // `$data['database'] ??= Str::random(12)` + generateUniqueDatabaseName().
    let raw_name = state.database.clone();
    let name = if raw_name.is_empty() { str_random(12) } else { raw_name };
    let name = unique_name(&name, p.server.id.0);

    let host = match choose_host(&p, &state.host_id).await? {
        HostChoice::Chosen(h) => h,
        HostChoice::Failed => {
            // Foreign-key style failure of the posted id: same notification
            // path as an exception inside the service.
            p.flash_danger(plain(&p.t_with("server/database.create_notification_fail", &[("database", &name)])), None);
            return Ok(p.redirect(&list_url));
        }
        HostChoice::Required => {
            let errors = validate(&p, &state).await?;
            let lp = ListParams::parse(p.query.as_deref(), &[10, 25, 50, 0], 10);
            return render_list(p, lp, 0, state, errors).await;
        }
    };

    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let server = p.server.clone();
    let result: Result<(), ApiError> = async {
        let txn = st.db.begin().await?;
        api::create_database_in(&st, &ctx, &txn, &server, &host, &name, &state.remote).await?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(plain(&p.t_with("server/database.create_notification", &[("database", &name)]))),
        Err(e) => {
            tracing::warn!(error=?e, "database creation failed");
            p.flash_danger(plain(&p.t_with("server/database.create_notification_fail", &[("database", &name)])), None);
        }
    }
    Ok(p.redirect(&list_url))
}

/// `GET /server/{server}/databases/{id}` — the fields of `defaultForm`
/// (the ViewAction content): password/JDBC only with `database.view-password`.
async fn view(p: ServerPanel, Path(id): Path<i64>) -> WebResult {
    p.authorize("database.read")?;
    let d = find(&p, id).await?;
    let host = host_of(&p.st.db, d.database_host_id).await?;
    let address = format!("{}:{}", host.name, host.port.0);
    let max_connections = match d.max_connections.map(|m| m.0) {
        Some(n) if n != 0 => n.to_string(),
        _ => p.t("server/database.unlimited"),
    };
    let can_password = p.can("database.view-password");
    let (password, jdbc) = if can_password {
        let pw = p.st.enc.decrypt_string_lenient(&d.password).map_err(|_| WebError::internal("DecryptException"))?;
        let jdbc = format!("jdbc:mysql://{}:{}@{}/{}", d.username, php_urlencode(&pw), address, d.database);
        (Some(pw), jdbc)
    } else {
        (None, String::new())
    };
    p.page(
        "databases",
        "server/databases_view.html",
        context! {
            title => p.t_with("server/database.viewing", &[("database", &d.database)]),
            base => p.base_url(),
            d => context! {
                id => d.id.0,
                name => d.database.clone(),
                username => d.username.clone(),
                remote => d.remote.clone(),
            },
            address => address,
            max_connections => max_connections,
            can_update => p.can("database.update"),
            can_delete => p.can("database.delete"),
            password => password,
            jdbc => jdbc,
        },
    )
}

/// `POST …/rotate-password` — `RotateDatabasePasswordAction`: service call plus
/// the same activity event as the Client API.
async fn rotate_password(p: ServerPanel, Path(id): Path<i64>, form: Form) -> WebResult<Response> {
    p.authorize("database.update")?;
    let d = find(&p, id).await?;
    let back = form.back(&format!("{}/databases/{id}", p.base_url()));
    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let result: Result<(), ApiError> = async {
        let txn = st.db.begin().await?;
        api::rotate_password_in(&st, &txn, &d).await?;
        ctx.activity("server:database.rotate-password")
            .subject(morph::DATABASE, d.id.0)
            .property("name", d.database.clone())
            .log_in(&st, &txn)
            .await?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(p.t("admin/databasehost.rotated")),
        Err(e) => {
            tracing::warn!(error=?e, "database password rotation failed");
            // Body of the exception message only for panel administrators.
            let body = p.user.is_admin().then(|| e.errors.first().map(|o| o.detail.clone()).unwrap_or_default());
            p.flash_danger(p.t("admin/databasehost.rotate_error"), body);
        }
    }
    Ok(p.redirect(&back))
}

/// `POST …/delete` — `DeleteAction` (`DatabaseManagementService::delete()`).
async fn delete(p: ServerPanel, Path(id): Path<i64>, form: Form) -> WebResult<Response> {
    p.authorize("database.delete")?;
    let d = find(&p, id).await?;
    let back = form.back(&format!("{}/databases", p.base_url()));
    let name = d.database.clone();
    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let result: Result<(), ApiError> = async {
        let txn = st.db.begin().await?;
        api::delete_database_in(&st, &ctx, &txn, &d).await?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(plain(&p.t_with("server/database.delete_notification", &[("database", &name)]))),
        Err(e) => {
            tracing::warn!(error=?e, "database deletion failed");
            p.flash_danger(plain(&p.t_with("server/database.delete_notification_fail", &[("database", &name)])), None);
        }
    }
    Ok(p.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_matches_php() {
        assert_eq!(php_urlencode("abc-_.12"), "abc-_.12");
        assert_eq!(php_urlencode("a b"), "a+b");
        assert_eq!(php_urlencode("~!@%^"), "%7E%21%40%25%5E");
    }

    #[test]
    fn notification_text_strips_markup() {
        assert_eq!(plain("Created new <b>s1_x</b>"), "Created new s1_x");
    }
}
