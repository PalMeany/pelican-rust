//! `ClientController`, `ServerController`, `WebsocketController`,
//! `ResourceUtilizationController`, `CommandController`, `PowerController`.

use axum::Extension;
use axum::extract::{OriginalUri, State};
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{nodes, servers};
use pelican_db::raw;
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, FromQueryResult, Value as DbValue};
use serde_json::{Map, Value, json};

use super::data::server_item;
use crate::envelope::{Includes, Pagination, list, per_page, query_appends, with_meta};
use crate::error::{ApiError, ApiResult, Style};
use crate::input::In;
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::query;
use crate::services::LogActivity;
use crate::validation::Validator;

/// `Subuser::allPermissionKeys()` (app/Enums/SubuserPermission.php order).
pub const PERMISSIONS: &[&str] = &[
    "websocket.connect",
    "control.console",
    "control.start",
    "control.stop",
    "control.restart",
    "file.read",
    "file.read-content",
    "file.create",
    "file.update",
    "file.delete",
    "file.archive",
    "file.sftp",
    "backup.read",
    "backup.create",
    "backup.delete",
    "backup.download",
    "backup.restore",
    "schedule.read",
    "schedule.create",
    "schedule.update",
    "schedule.delete",
    "user.read",
    "user.create",
    "user.update",
    "user.delete",
    "database.read",
    "database.create",
    "database.update",
    "database.delete",
    "database.view-password",
    "allocation.read",
    "allocation.create",
    "allocation.update",
    "allocation.delete",
    "activity.read",
    "mount.read",
    "mount.update",
    "startup.read",
    "startup.update",
    "startup.docker-image",
    "settings.rename",
    "settings.description",
    "settings.reinstall",
    "settings.change-icon",
];

/// `GET /permissions`.
pub async fn permissions() -> ApiResult<Response> {
    Ok(PhpJson::ok(json!({"object": "system_permissions", "attributes": {"permissions": PERMISSIONS}})).into_response())
}

/// `User::directAccessibleServers()->pluck('id')`.
async fn direct_accessible_ids<C: ConnectionTrait>(db: &C, user_id: i64) -> Result<Vec<i64>, sea_orm::DbErr> {
    let b = db.get_database_backend();
    let rows = db
        .query_all_raw(raw::stmt(
            b,
            "select distinct {servers}.* from {servers} left join {subusers} on {subusers.server_id} = {servers.id} \
             where ({servers.owner_id} = ? or {subusers.user_id} = ?)",
            vec![user_id.into(), user_id.into()],
        ))
        .await?;
    Ok(rows.iter().filter_map(|r| r.try_get::<pelican_db::Int>("", "id").ok().map(|i| i.0)).collect())
}

fn in_list(ids: &[i64]) -> (String, Vec<DbValue>) {
    if ids.is_empty() {
        // Laravel compiles `whereIn(col, [])` to `0 = 1`.
        return ("0 = 1".into(), vec![]);
    }
    (
        format!("{{servers.id}} in ({})", vec!["?"; ids.len()].join(", ")),
        ids.iter().map(|i| DbValue::from(*i)).collect(),
    )
}

/// `GET /api/client` — servers visible to the user.
pub async fn index(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    uri: OriginalUri,
    In(input): In,
) -> ApiResult<Response> {
    let user = ctx.user()?;
    let db = &st.db;
    let backend = db.get_database_backend();
    let includes = Includes::parse(&input);
    let filters = query::filters(&input, &["uuid", "name", "description", "external_id", "*"])?;

    let mut from = "{servers}".to_string();
    let mut select = "*".to_string();
    let mut wheres: Vec<String> = Vec::new();
    let mut values: Vec<DbValue> = Vec::new();
    let mut group_by = false;
    for (name, value) in &filters {
        match name.as_str() {
            "*" => {
                let v = value.as_php_string();
                if matches!(value, query::FilterValue::List(_)) {
                    return Err(ApiError::internal("TypeError"));
                }
                if matches!(value, query::FilterValue::Null) {
                    continue;
                }
                let ipv4 = regex_is_match(r"^(?:[0-9]{1,3}\.){3}[0-9]{1,3}(\:\d{1,5})?$", &v);
                if ipv4 || regex_is_match(r"^:\d{1,5}$", &v) {
                    select = "{servers}.*".into();
                    from = "{servers} inner join {allocations} on {allocations.server_id} = {servers.id}".into();
                    group_by = true;
                    if let Some(port) = v.strip_prefix(':') {
                        wheres.push(format!("({} LIKE ?)", like_col(backend, "allocations.port")));
                        values.push(format!("{port}%").into());
                    } else {
                        let mut parts = v.splitn(2, ':');
                        let ip = parts.next().unwrap_or("");
                        match parts.next() {
                            Some(port) => {
                                wheres.push(format!(
                                    "({{allocations.ip}} = ? and {} LIKE ?)",
                                    like_col(backend, "allocations.port")
                                ));
                                values.push(ip.into());
                                values.push(format!("{port}%").into());
                            }
                            None => {
                                wheres.push("({allocations.ip} = ?)".into());
                                values.push(ip.into());
                            }
                        }
                    }
                } else {
                    wheres.push(format!(
                        "({{servers.uuid}} = ? or {} LIKE ? or {{servers.uuid_short}} = ? or {{servers.external_id}} = ? or LOWER(servers.name) LIKE ?)",
                        like_col(backend, "servers.uuid")
                    ));
                    values.extend([
                        v.clone().into(),
                        format!("{v}%").into(),
                        v.clone().into(),
                        v.clone().into(),
                        format!("%{v}%").into(),
                    ]);
                }
            }
            col => {
                if let Some((sql, vals)) = query::partial(backend, &format!("servers.{col}"), value) {
                    wheres.push(sql);
                    values.extend(vals);
                }
            }
        }
    }
    match input.str("type") {
        Some("admin" | "admin-all") if !user.is_root_admin() => wheres.push("1 = 2".into()),
        Some("admin-all") => {}
        Some("admin") => {
            let ids = direct_accessible_ids(db, user.id()).await?;
            if !ids.is_empty() {
                wheres.push(format!("{{servers.id}} not in ({})", vec!["?"; ids.len()].join(", ")));
                values.extend(ids.iter().map(|i| DbValue::from(*i)));
            }
        }
        Some("owner") => {
            wheres.push("{servers.owner_id} = ?".into());
            values.push(user.id().into());
        }
        _ => {
            let ids = direct_accessible_ids(db, user.id()).await?;
            let (sql, vals) = in_list(&ids);
            wheres.push(sql);
            values.extend(vals);
        }
    }
    let where_sql = if wheres.is_empty() { String::new() } else { format!(" where {}", wheres.join(" and ")) };
    let group_sql = if group_by { " group by {servers.id}" } else { "" };
    // per_page comes from the query string only (`$request->query('per_page', '50')`).
    let query_input = crate::input::Input::from_value(crate::input::parse_query(uri.query().unwrap_or("")));
    let per = per_page(&query_input, 50, 100);
    let page = crate::paginate::resolve_page(input.get("page"));
    let total =
        count(db, backend, &format!("select {select} from {from}{where_sql}{group_sql}"), values.clone()).await?;
    let mut page_sql = format!("select {select} from {from}{where_sql}{group_sql}");
    let mut page_vals = values.clone();
    if per > 0 {
        page_sql.push_str(" limit ? offset ?");
        page_vals.push(per.into());
        page_vals.push(((page - 1) * per).into());
    }
    let rows = if total > 0 {
        servers::Model::find_by_statement(raw::stmt(backend, &page_sql, page_vals)).all(db).await?
    } else {
        Vec::new()
    };
    let data = crate::client::data::server_items(&st, &ctx, &rows, &includes).await?;
    let p = Pagination {
        total,
        count: data.len() as i64,
        per_page: per,
        current_page: page,
        url: format!("{}{}", ctx.root, uri.path().trim_end_matches('/')),
        appends: query_appends(uri.query()),
    };
    let mut meta = Map::new();
    meta.insert("pagination".into(), p.meta());
    Ok(PhpJson::ok(with_meta(list(data), meta)).into_response())
}

/// Laravel's PostgresGrammar casts the column to text for builder `LIKE` wheres.
pub fn like_col(backend: DbBackend, col: &str) -> String {
    if backend == DbBackend::Postgres { format!("{{{col}}}::text") } else { format!("{{{col}}}") }
}

fn regex_is_match(pattern: &str, s: &str) -> bool {
    regex::Regex::new(pattern).map(|r| r.is_match(s)).unwrap_or(false)
}

/// `select count(*) as aggregate from (<query>) as temp` semantics.
pub async fn count<C: ConnectionTrait>(db: &C, backend: DbBackend, sql: &str, values: Vec<DbValue>) -> ApiResult<i64> {
    let wrapped = format!("select count(*) as {{aggregate}} from ({sql}) as {{temp_table}}");
    let row = db.query_one_raw(raw::stmt(backend, &wrapped, values)).await?;
    Ok(row.and_then(|r| r.try_get::<pelican_db::Int>("", "aggregate").ok()).map(|i| i.0).unwrap_or(0))
}

/// `GET /servers/{server}`.
pub async fn view(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let server = ctx.bound.server()?.clone();
    let user = ctx.user()?;
    let includes = Includes::parse(&input);
    let body = server_item(&st, &ctx, &server, &includes).await?;
    let perms = pelican_core::permissions::user_permissions(&st.db, &server, user).await?;
    let mut meta = Map::new();
    meta.insert("is_server_owner".into(), json!(user.id() == server.owner_id.0));
    meta.insert("user_permissions".into(), perms);
    Ok(PhpJson::ok(with_meta(body, meta)).into_response())
}

fn forbidden(detail: &str) -> ApiError {
    ApiError::new(403, "HttpForbiddenException", detail, Style::Pretty)
}

/// `GET /servers/{server}/websocket`.
pub async fn websocket(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let server = ctx.bound.server()?.clone();
    let user = ctx.user()?;
    if !ctx.can(&st, "websocket.connect").await? {
        return Err(forbidden("You do not have permission to connect to this server's websocket."));
    }
    let permissions = pelican_core::permissions::user_permissions(&st.db, &server, user).await?;
    let mut node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    if let Some(t) = pelican_core::server_state::active_transfer(&st.db, server.id.0).await? {
        let has = match &permissions {
            Value::Array(a) => a.iter().any(|p| p.as_str() == Some("admin.websocket.transfer")),
            Value::Object(m) => m.values().any(|p| p.as_str() == Some("admin.websocket.transfer")),
            _ => false,
        };
        if !has {
            return Err(forbidden("You do not have permission to view server transfer logs."));
        }
        if t.archived.0 {
            node = nodes::Entity::find_by_id(t.new_node)
                .one(&st.db)
                .await?
                .ok_or_else(|| ApiError::internal("ErrorException"))?;
        }
    }
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    let now = chrono::Utc::now().timestamp();
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: conn.connection_address(),
        identified_by: format!("{}{}", user.id(), server.uuid),
        issued_at: now,
        expires_at: now + 600,
        subject: None,
        claims: vec![("server_uuid".into(), json!(server.uuid)), ("permissions".into(), permissions)],
        user_uuid: Some(user.user.uuid.clone()),
        scope: "websocket".into(),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&conn.token)
    .map_err(|_| ApiError::internal("InvalidKeyProvided"))?;
    let socket = conn.connection_address().replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let body = json!({"data": {"token": token, "socket": format!("{socket}/api/servers/{}/ws", server.uuid)}});
    Ok(PhpJson::ok(body).into_response())
}

/// `GET /servers/{server}/resources` — Wings details, `{"state":"missing"}` on failure.
pub async fn resources(State(st): State<AppState>, Extension(ctx): Extension<Ctx>) -> ApiResult<Response> {
    let server = ctx.bound.server()?.clone();
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    let path = format!("/api/servers/{}", server.uuid);
    let mut call = pelican_wings::client::Call::new(reqwest_method_get(), &path);
    call.timeout = Some(std::time::Duration::from_secs(1));
    call.connect_timeout = Some(std::time::Duration::from_secs(1));
    let details = match st.wings.send(&conn, call).await {
        Ok(r) => r.json().ok_or_else(|| ApiError::internal("TypeError"))?,
        Err(_) => json!({"state": "missing"}),
    };
    let get = |path: &str, default: Value| -> Value {
        let mut cur = &details;
        for seg in path.split('.') {
            match cur.get(seg) {
                Some(v) => cur = v,
                None => return default,
            }
        }
        cur.clone()
    };
    let state = get("state", json!("stopped"));
    if state.is_null() {
        return Err(ApiError::internal("TypeError"));
    }
    let mut res = Map::new();
    res.insert("memory_bytes".into(), get("utilization.memory_bytes", json!(0)));
    res.insert("cpu_absolute".into(), get("utilization.cpu_absolute", json!(0)));
    res.insert("disk_bytes".into(), get("utilization.disk_bytes", json!(0)));
    res.insert("network_rx_bytes".into(), get("utilization.network.rx_bytes", json!(0)));
    res.insert("network_tx_bytes".into(), get("utilization.network.tx_bytes", json!(0)));
    res.insert("uptime".into(), get("utilization.uptime", json!(0)));
    let mut attrs = Map::new();
    attrs.insert("current_state".into(), state);
    attrs.insert("is_suspended".into(), get("is_suspended", json!(false)));
    attrs.insert("resources".into(), Value::Object(res));
    Ok(PhpJson::ok(crate::envelope::item("stats", Value::Object(attrs), vec![])).into_response())
}

fn reqwest_method_get() -> pelican_wings::client::Method {
    pelican_wings::client::Method::GET
}

/// `POST /servers/{server}/command`.
pub async fn command(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.authorize(&st, "control.console").await?;
    let mut v = Validator::new(&input);
    v.rules("command", &["required", "string", "min:1"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    let command = input.str("command").unwrap_or_default().to_string();
    st.wings
        .send_commands(&conn, &server.uuid, std::slice::from_ref(&command))
        .await
        .map_err(|e| ApiError::internal(e.code()))?;
    ctx.activity("server:console.command").property("command", command).log(&st).await?;
    Ok(no_content())
}

/// `POST /servers/{server}/power`.
pub async fn power(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    let signal = input.get("signal").map(pelican_core::php::string).unwrap_or_default();
    let permission = match signal.as_str() {
        "start" => "control.start",
        "stop" | "kill" => "control.stop",
        "restart" => "control.restart",
        _ => "websocket.connect",
    };
    ctx.authorize(&st, permission).await?;
    let mut v = Validator::new(&input);
    v.rules("signal", &["required", "string", "in:start,stop,restart,kill"]);
    v.validate()?;
    let server = ctx.bound.server()?.clone();
    let node = nodes::Entity::find_by_id(server.node_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| ApiError::internal("ErrorException"))?;
    let conn = st.node_conn(&node).map_err(|_| ApiError::internal("DecryptException"))?;
    st.wings.power(&conn, &server.uuid, &signal).await.map_err(|e| ApiError::internal(e.code()))?;
    ctx.activity(&format!("server:power.{}", signal.to_lowercase())).log(&st).await?;
    Ok(no_content())
}
