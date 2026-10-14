//! Server create/build/startup/transfer (`ServerCreationService`,
//! `BuildModificationService`, `StartupModificationService`,
//! `TransferServerService`; spec §3.3.4, §3.3.6, §3.3.7, §3.3.10).

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, egg_variables, eggs, nodes, server_transfers, server_variables, servers};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use rand::seq::IndexedRandom;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, TransactionTrait,
};
use serde_json::{Map, Value, json};

use super::WRITE;
use super::data::server_item;
use super::servers::{bound, count_where, exists_rule, node_conn, unique_rule};
use crate::envelope::Includes;
use crate::error::{ApiError, ApiResult, Style};
use crate::input::{In, Input};
use crate::json::{PhpJson, no_content};
use crate::pipeline::Ctx;
use crate::services::server_saved;
use crate::validation::{CustomRule, Validator};

fn wings_err(e: pelican_wings::WingsError) -> ApiError {
    ApiError::internal(e.code())
}

fn php_int(v: &Value) -> i64 {
    match v {
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => s.trim().parse::<i64>().unwrap_or_else(|_| pelican_core::php::intval(s.trim())),
        _ => 0,
    }
}

fn opt_int(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| !v.is_null()).map(php_int)
}

fn first_value(v: Option<&Json>) -> Option<String> {
    match v.map(|j| &j.0) {
        Some(Value::Object(m)) => m.values().next().map(pelican_core::php::string),
        Some(Value::Array(a)) => a.first().map(pelican_core::php::string),
        _ => None,
    }
}

/// `VariableValidatorService::handle()` at admin level: every egg variable is
/// validated with its own rules against `environment[ENV] ?? default`.
/// Returns `(variable, provided value)` pairs.
pub(super) async fn validate_variables<C: ConnectionTrait>(
    db: &C,
    egg_id: Int,
    fields: &Value,
) -> ApiResult<Vec<(egg_variables::Model, Option<Value>)>> {
    let vars = egg_variables::Model::find_by_statement(raw::stmt(
        db.get_database_backend(),
        "select * from {egg_variables} where {egg_id} = ?",
        vec![egg_id.into()],
    ))
    .all(db)
    .await?;
    let get = |env: &str| fields.get(env).filter(|v| !v.is_null()).cloned();
    let mut env = Map::new();
    for v in &vars {
        env.insert(v.env_variable.clone(), get(&v.env_variable).unwrap_or_else(|| json!(v.default_value)));
    }
    let data = Input::from_value(json!({"environment": Value::Object(env)}));
    let mut validator = Validator::new(&data);
    let rules: Vec<Vec<String>> = vars
        .iter()
        .map(|v| match &v.rules.0 {
            Value::Array(a) => a.iter().map(pelican_core::php::string).collect(),
            Value::String(s) => s.split('|').map(str::to_string).collect(),
            _ => Vec::new(),
        })
        .collect();
    for (v, r) in vars.iter().zip(&rules) {
        let key = format!("environment.{}", v.env_variable);
        let refs: Vec<&str> = r.iter().map(String::as_str).collect();
        validator.rules(&key, &refs);
        validator.attribute_name(&key, &format!("{} variable", v.name));
    }
    validator.validate()?;
    Ok(vars
        .into_iter()
        .map(|v| {
            let val = get(&v.env_variable);
            (v, val)
        })
        .collect())
}

/// `ServerVariable` value as stored (`$value ?? ''`, PHP string conversion).
fn stored_value(v: &Option<Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(false)) => String::new(),
        Some(other) => pelican_core::php::string(other),
    }
}

/// `FindViableNodesService` → ids of public nodes with room (natural order).
async fn viable_nodes(
    st: &AppState,
    memory: i64,
    disk: i64,
    cpu: i64,
    tags: &[String],
) -> ApiResult<Vec<nodes::Model>> {
    let all = nodes::Model::find_by_statement(raw::stmt(
        st.db.get_database_backend(),
        "select * from {nodes} where {public} = ?",
        vec![1.into()],
    ))
    .all(&st.db)
    .await?;
    let mut out = Vec::new();
    for n in all {
        let node_tags: Vec<String> = match n.tags.as_ref().map(|t| &t.0) {
            Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
            Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
            _ => Vec::new(),
        };
        if !tags.is_empty() && !node_tags.iter().any(|t| tags.contains(t)) {
            continue;
        }
        if is_viable(&st.db, &n, memory, disk, cpu).await? {
            out.push(n);
        }
    }
    Ok(out)
}

/// `Node::isViable()`.
async fn is_viable<C: ConnectionTrait>(db: &C, n: &nodes::Model, memory: i64, disk: i64, cpu: i64) -> ApiResult<bool> {
    let ss = servers::Entity::find().filter(servers::Column::NodeId.eq(n.id)).all(db).await?;
    let sum = |f: fn(&servers::Model) -> i64| ss.iter().map(f).sum::<i64>();
    let ok = |limit: i64, over: i64, used: i64, req: i64| {
        !(limit > 0 && over >= 0) || (used + req) as f64 <= limit as f64 * (1.0 + over as f64 / 100.0)
    };
    Ok(ok(n.memory.0, n.memory_overallocate.0, sum(|s| s.memory.0), memory)
        && ok(n.disk.0, n.disk_overallocate.0, sum(|s| s.disk.0), disk)
        && ok(n.cpu.0, n.cpu_overallocate.0, sum(|s| s.cpu.0), cpu))
}

/// `AllocationSelectionService::handle()`: a random free allocation on the nodes.
async fn select_allocation(
    st: &AppState,
    node_ids: &[i64],
    ports: &[Value],
    dedicated: bool,
) -> ApiResult<allocations::Model> {
    let range_re = regex::Regex::new(r"^(\d{4,5})-(\d{4,5})$").map_err(|_| ApiError::internal("Error"))?;
    let mut singles: Vec<i64> = Vec::new();
    let mut ranges: Vec<(i64, i64)> = Vec::new();
    for p in ports {
        let s = pelican_core::php::string(p);
        if !matches!(p, Value::Bool(_)) && !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
            singles.push(s.parse().unwrap_or(0));
        }
        if let Some(c) = range_re.captures(&s) {
            let (a, b): (i64, i64) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
            if b - a + 1 > 1000 {
                return Err(ApiError::display(
                    "DisplayException",
                    400,
                    "Adding more than 1000 ports in a single range at once is not supported.",
                ));
            }
            ranges.push((a, b));
        }
    }
    let all = allocations::Entity::find().filter(allocations::Column::ServerId.is_null()).all(&st.db).await?;
    let taken_ips: Vec<String> = if dedicated {
        allocations::Entity::find()
            .filter(allocations::Column::ServerId.is_not_null())
            .filter(allocations::Column::NodeId.is_in(node_ids.iter().map(|i| Int(*i))))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|a| a.ip)
            .collect()
    } else {
        Vec::new()
    };
    let candidates: Vec<allocations::Model> = all
        .into_iter()
        .filter(|a| node_ids.contains(&a.node_id.0))
        .filter(|a| {
            (singles.is_empty() && ranges.is_empty())
                || singles.contains(&a.port.0)
                || ranges.iter().any(|(x, y)| a.port.0 >= *x && a.port.0 <= *y)
        })
        .filter(|a| !taken_ips.contains(&a.ip))
        .collect();
    candidates.choose(&mut rand::rng()).cloned().ok_or_else(|| {
        ApiError::display(
            "NoViableAllocationException",
            400,
            "No allocations satisfying the requirements for automatic deployment were found.",
        )
    })
}

/// `POST /servers` (`StoreServerRequest` + `ServerCreationService`).
pub async fn store(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let deploy = input.get("deploy").filter(|d| !pelican_core::php::empty(d)).is_some();
    let mut v = Validator::new(&input);
    v.rules("external_id", &["sometimes", "nullable", "string", "between:1,255"]);
    v.custom("external_id", unique_rule(&st, &input, "external_id", "servers", "external_id", None).await?);
    v.rules("name", &["required", "string", "min:1", "max:255"]);
    v.rules("description", &["nullable", "string"]);
    v.rules("user", &["required", "integer"]);
    v.custom("user", exists_rule(&st, input.get("user"), "users", "id").await?);
    v.rules("egg", &["required"]);
    v.custom("egg", exists_rule(&st, input.get("egg"), "eggs", "id").await?);
    v.rules("docker_image", &["sometimes", "string"]);
    v.rules("startup", &["sometimes", "string"]);
    v.rules("environment", &["present", "array"]);
    v.rules("skip_scripts", &["sometimes", "boolean"]);
    v.rules("oom_killer", &["sometimes", "boolean"]);
    v.rules("limits", &["required", "array"]);
    v.rules("limits.memory", &["required", "numeric", "min:0"]);
    v.rules("limits.swap", &["required", "numeric", "min:-1"]);
    v.rules("limits.disk", &["required", "numeric", "min:0"]);
    v.rules("limits.io", &["required", "numeric", "between:0,1000"]);
    v.rules("limits.threads", &["nullable", r"regex:/^[0-9-,]+$/"]);
    v.rules("limits.cpu", &["required", "numeric", "min:0"]);
    v.rules("feature_limits", &["required", "array"]);
    v.rules("feature_limits.databases", &["present", "nullable", "integer", "min:0"]);
    v.rules("feature_limits.allocations", &["sometimes", "nullable", "integer", "min:0"]);
    v.rules("feature_limits.backups", &["present", "nullable", "integer", "min:0"]);
    if !deploy {
        v.rules("allocation.default", &["required", "integer", "bail"]);
        let free = |val: Option<&Value>| {
            let st = st.clone();
            let val = val.cloned();
            async move {
                let missing = match val {
                    None | Some(Value::Null) => false,
                    Some(v) => match pelican_core::php::string(&v).trim().parse::<i64>() {
                        Ok(id) => allocations::Entity::find_by_id(Int(id))
                            .filter(allocations::Column::ServerId.is_null())
                            .one(&st.db)
                            .await?
                            .is_none(),
                        Err(_) => true,
                    },
                };
                Ok::<_, ApiError>(CustomRule::precomputed(
                    "exists",
                    missing.then(|| "The selected :attribute is invalid.".to_string()),
                ))
            }
        };
        v.custom("allocation.default", free(input.get("allocation.default")).await?);
        v.rules("allocation.additional.*", &["integer"]);
        for path in crate::validation::expand_wildcard(&input.data, "allocation.additional.*") {
            v.custom(&path, free(input.get(&path)).await?);
        }
    }
    v.rules("deploy", &["sometimes", "required", "array"]);
    v.rules("deploy.tags", &if deploy { vec!["array", "present"] } else { vec!["array"] });
    v.rules("deploy.tags.*", &["required_with:deploy.tags", "string", "min:1"]);
    v.rules("deploy.dedicated_ip", &["required_with:deploy", "boolean"]);
    v.rules("deploy.port_range", &if deploy { vec!["array", "present"] } else { vec!["array"] });
    v.rules("deploy.port_range.*", &["string"]);
    v.rules("start_on_completion", &["sometimes", "boolean"]);
    v.validate()?;

    let g = |k: &str| input.get(k).cloned().filter(|v| !v.is_null());
    let egg_id = Int(g("egg").map(|v| php_int(&v)).unwrap_or(0));
    let egg = eggs::Entity::find_by_id(egg_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    let image =
        g("docker_image").map(|v| pelican_core::php::string(&v)).or_else(|| first_value(egg.docker_images.as_ref()));
    let startup =
        g("startup").map(|v| pelican_core::php::string(&v)).or_else(|| first_value(egg.startup_commands.as_ref()));
    let memory = g("limits.memory").map(|v| php_int(&v)).unwrap_or(0);
    let disk = g("limits.disk").map(|v| php_int(&v)).unwrap_or(0);
    let cpu = g("limits.cpu").map(|v| php_int(&v)).unwrap_or(0);
    let mut allocation_id = opt_int(input.get("allocation.default"));
    let node_id: i64;
    if deploy {
        let list_of = |k: &str| match input.get(k) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(m)) => m.values().cloned().collect(),
            _ => Vec::new(),
        };
        let tags: Vec<String> = list_of("deploy.tags").iter().map(pelican_core::php::string).collect();
        let nodes = viable_nodes(&st, memory, disk, cpu, &tags).await?;
        if nodes.is_empty() {
            return Err(ApiError::display(
                "NoViableNodeException",
                400,
                "No nodes satisfying the requirements specified for automatic deployment could be found.",
            ));
        }
        let ports = list_of("deploy.port_range");
        let ids: Vec<i64> = nodes.iter().map(|n| n.id.0).collect();
        if !ports.is_empty() {
            let dedicated = input.get("deploy.dedicated_ip").is_some_and(|v| !pelican_core::php::empty(v));
            let a = select_allocation(&st, &ids, &ports, dedicated).await?;
            allocation_id = Some(a.id.0);
            node_id = a.node_id.0;
        } else {
            node_id = ids[0];
        }
    } else {
        let a = match allocation_id {
            Some(id) => allocations::Entity::find_by_id(Int(id)).one(&st.db).await?,
            None => None,
        };
        node_id = a.map(|a| a.node_id.0).ok_or_else(|| ApiError::internal("InvalidArgumentException"))?;
    }
    let env_fields = input.get("environment").cloned().unwrap_or(Value::Null);
    let vars = validate_variables(&st.db, egg_id, &env_fields).await?;

    let (Some(image), Some(startup)) = (image, startup) else {
        return Err(ApiError::internal("DataValidationException"));
    };
    let txn = st.db.begin().await?;
    let uuid = loop {
        let u = uuid::Uuid::new_v4().to_string();
        let short = u[..8].to_string();
        let clash = servers::Entity::find()
            .filter(
                sea_orm::Condition::any()
                    .add(servers::Column::Uuid.eq(u.clone()))
                    .add(servers::Column::UuidShort.eq(short)),
            )
            .one(&txn)
            .await?;
        if clash.is_none() {
            break u;
        }
    };
    let now = Timestamp(pelican_core::time::now());
    let created = pelican_db::write::insert(
        &txn,
        servers::ActiveModel {
            external_id: Set(g("external_id").map(|v| pelican_core::php::string(&v))),
            uuid_short: Set(uuid[..8].to_string()),
            uuid: Set(uuid.clone()),
            node_id: Set(Int(node_id)),
            name: Set(g("name").map(|v| pelican_core::php::string(&v)).unwrap_or_default()),
            description: Set(Some(g("description").map(|v| pelican_core::php::string(&v)).unwrap_or_default())),
            status: Set(Some("installing".into())),
            skip_scripts: Set(Bool(input.get("skip_scripts").is_some_and(|v| !pelican_core::php::empty(v)))),
            owner_id: Set(Int(g("user").map(|v| php_int(&v)).unwrap_or(0))),
            memory: Set(Int(memory)),
            swap: Set(Int(g("limits.swap").map(|v| php_int(&v)).unwrap_or(0))),
            disk: Set(Int(disk)),
            io: Set(Int(g("limits.io").map(|v| php_int(&v)).unwrap_or(0))),
            cpu: Set(Int(cpu)),
            threads: Set(g("limits.threads").map(|v| pelican_core::php::string(&v))),
            oom_killer: Set(SmallBool(g("oom_killer").is_some_and(|v| !pelican_core::php::empty(&v)))),
            allocation_id: Set(allocation_id.map(Int)),
            egg_id: Set(egg_id),
            startup: Set(startup),
            image: Set(image),
            database_limit: Set(Some(Int(opt_int(input.get("feature_limits.databases")).unwrap_or(0)))),
            allocation_limit: Set(Some(Int(opt_int(input.get("feature_limits.allocations")).unwrap_or(0)))),
            backup_limit: Set(Int(opt_int(input.get("feature_limits.backups")).unwrap_or(0))),
            docker_labels: Set(None),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    server_saved(&st, &ctx, &txn, &created).await?;
    if let Some(primary) = allocation_id {
        let mut ids = vec![primary];
        let extra: Vec<Value> = match input.get("allocation.additional") {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Object(m)) => m.values().cloned().collect(),
            _ => Vec::new(),
        };
        for e in &extra {
            let id = php_int(e);
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let free = allocations::Entity::find()
            .filter(allocations::Column::Id.is_in(ids.iter().map(|i| Int(*i))))
            .filter(allocations::Column::ServerId.is_null())
            .all(&txn)
            .await?;
        for a in free {
            pelican_db::write::update(
                &txn,
                &allocations::ActiveModel {
                    id: sea_orm::ActiveValue::Unchanged(a.id),
                    server_id: Set(Some(created.id)),
                    is_locked: Set(Bool(true)),
                    updated_at: Set(Some(now)),
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    for (var, val) in &vars {
        pelican_db::write::insert(
            &txn,
            server_variables::ActiveModel {
                server_id: Set(created.id),
                variable_id: Set(var.id),
                variable_value: Set(stored_value(val)),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?;
    }
    txn.commit().await?;

    let conn = node_conn(&st, &st.db, created.node_id).await?;
    let start = input.get("start_on_completion").is_some_and(|v| !pelican_core::php::empty(v));
    match st.wings.create_server(&conn, &created.uuid, start).await {
        Ok(()) => {}
        Err(pelican_wings::WingsError::Connection(_)) => {
            super::servers::force_delete(&st, &ctx, &created).await?;
            return Err(ApiError::internal("ConnectionException"));
        }
        Err(e) => return Err(wings_err(e)),
    }
    let fresh = servers::Entity::find_by_id(created.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::with_status(StatusCode::CREATED, server_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?)
        .into_response())
}

/// `PATCH /servers/{server}/build` (`BuildModificationService`).
pub async fn build(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(&ctx)?;
    let mut v = Validator::new(&input);
    v.rules("allocation", &["sometimes", "nullable"]);
    v.custom("allocation", unique_rule(&st, &input, "allocation", "servers", "allocation_id", Some(s.id.0)).await?);
    v.custom("allocation", exists_rule(&st, input.get("allocation"), "allocations", "id").await?);
    v.rules("oom_killer", &["sometimes", "boolean"]);
    v.rules("limits", &["sometimes", "array"]);
    v.rules("limits.memory", &["required_with:limits", "numeric", "min:0"]);
    v.rules("limits.swap", &["required_with:limits", "numeric", "min:-1"]);
    v.rules("limits.io", &["required_with:limits", "numeric", "between:0,1000"]);
    v.rules("limits.cpu", &["required_with:limits", "numeric", "min:0"]);
    v.rules("limits.threads", &["nullable", r"regex:/^[0-9-,]+$/"]);
    v.rules("limits.disk", &["required_with:limits", "numeric", "min:0"]);
    v.rules("add_allocations", &["bail", "array"]);
    v.rules("add_allocations.*", &["integer"]);
    v.rules("remove_allocations", &["bail", "array"]);
    v.rules("remove_allocations.*", &["integer"]);
    v.rules("feature_limits", &["required", "array"]);
    v.rules("feature_limits.databases", &["present", "nullable", "integer", "min:0"]);
    v.rules("feature_limits.allocations", &["sometimes", "nullable", "integer", "min:0"]);
    v.rules("feature_limits.backups", &["present", "nullable", "integer", "min:0"]);
    v.attribute_name("add_allocations", "allocations to add");
    v.attribute_name("remove_allocations", "allocations to remove");
    v.attribute_name("add_allocations.*", "allocation to add");
    v.attribute_name("remove_allocations.*", "allocation to remove");
    v.attribute_name("feature_limits.databases", "Database Limit");
    v.attribute_name("feature_limits.allocations", "Allocation Limit");
    v.attribute_name("feature_limits.backups", "Backup Limit");
    v.validate()?;
    // `$data['allocation']` on a missing key: "Undefined array key" → ErrorException.
    let Some(allocation_raw) = input.get("allocation") else {
        return Err(ApiError::internal("ErrorException"));
    };
    let mut allocation_id = opt_int(Some(allocation_raw));
    let list_ids = |k: &str| -> Vec<i64> {
        match input.get(k) {
            Some(Value::Array(a)) => a.iter().map(php_int).collect(),
            Some(Value::Object(m)) => m.values().map(php_int).collect(),
            _ => Vec::new(),
        }
    };
    let add = list_ids("add_allocations");
    let remove = list_ids("remove_allocations");
    let b = st.db.get_database_backend();
    let now = Timestamp(pelican_core::time::now());

    let txn = st.db.begin().await?;
    if !add.is_empty() {
        let marks = vec!["?"; add.len()].join(", ");
        let mut vals: Vec<sea_orm::Value> = vec![s.id.into(), now.into(), s.node_id.into()];
        vals.extend(add.iter().map(|i| sea_orm::Value::from(*i)));
        txn.execute_raw(raw::stmt(
            b,
            &format!("update {{allocations}} set {{server_id}} = ?, {{updated_at}} = ? where {{node_id}} = ? and {{id}} in ({marks}) and {{server_id}} is null"),
            vals,
        ))
        .await?;
    }
    if !remove.is_empty() {
        let targets: Vec<i64> = remove.iter().copied().filter(|r| !add.contains(r)).collect();
        if !targets.is_empty() {
            let mine = allocations::Entity::find()
                .filter(allocations::Column::ServerId.eq(s.id))
                .filter(allocations::Column::Id.is_in(targets.iter().map(|i| Int(*i))))
                .all(&txn)
                .await?;
            if s.allocation_id.is_some_and(|p| mine.iter().any(|a| a.id == p)) {
                let all = allocations::Entity::find().filter(allocations::Column::ServerId.eq(s.id)).all(&txn).await?;
                allocation_id =
                    all.iter().find(|a| !remove.contains(&a.id.0)).map(|a| a.id.0).or_else(|| add.first().copied());
            }
            let marks = vec!["?"; targets.len()].join(", ");
            let mut vals: Vec<sea_orm::Value> = vec![
                sea_orm::Value::BigInt(None),
                sea_orm::Value::String(None),
                false.into(),
                true.into(),
                now.into(),
                s.id.into(),
            ];
            vals.extend(targets.iter().map(|i| sea_orm::Value::from(*i)));
            txn.execute_raw(raw::stmt(
                b,
                &format!("update {{allocations}} set {{server_id}} = ?, {{notes}} = ?, {{is_locked}} = ?, {{show_port}} = ?, {{updated_at}} = ? where {{server_id}} = ? and {{id}} in ({marks})"),
                vals,
            ))
            .await?;
        }
    }
    if let Some(id) = allocation_id {
        if Some(Int(id)) != s.allocation_id {
            let owned = allocations::Entity::find_by_id(Int(id))
                .filter(allocations::Column::ServerId.eq(s.id))
                .one(&txn)
                .await?;
            if owned.is_none() {
                return Err(ApiError::model_not_found());
            }
        }
    }
    let mut am = servers::ActiveModel { id: sea_orm::ActiveValue::Unchanged(s.id), ..Default::default() };
    let mut dirty = false;
    macro_rules! set_if {
        ($field:ident, $new:expr) => {{
            let new = $new;
            if s.$field != new {
                am.$field = Set(new);
                dirty = true;
            }
        }};
    }
    if let Some(v) = input.get("oom_killer") {
        set_if!(oom_killer, SmallBool(!pelican_core::php::empty(v)));
    }
    if input.get("limits").is_some_and(|l| !pelican_core::php::empty(l)) {
        for (k, f) in [("memory", 0), ("swap", 1), ("io", 2), ("cpu", 3), ("disk", 4)] {
            if let Some(v) = input.get(&format!("limits.{k}")) {
                let n = Int(php_int(v));
                match f {
                    0 => set_if!(memory, n),
                    1 => set_if!(swap, n),
                    2 => set_if!(io, n),
                    3 => set_if!(cpu, n),
                    _ => set_if!(disk, n),
                }
            }
        }
        if let Some(v) = input.get("limits.threads") {
            set_if!(threads, if v.is_null() { None } else { Some(pelican_core::php::string(v)) });
        }
    }
    set_if!(allocation_id, allocation_id.map(Int));
    set_if!(database_limit, opt_int(input.get("feature_limits.databases")).map(Int));
    set_if!(allocation_limit, opt_int(input.get("feature_limits.allocations")).map(Int));
    set_if!(backup_limit, Int(opt_int(input.get("feature_limits.backups")).unwrap_or(0)));
    if dirty {
        am.updated_at = Set(Some(now));
        pelican_db::write::update(&txn, &am).await?;
    }
    let fresh = servers::Entity::find_by_id(s.id).one(&txn).await?.ok_or_else(ApiError::model_not_found)?;
    server_saved(&st, &ctx, &txn, &fresh).await?;
    txn.commit().await?;

    let conn = node_conn(&st, &st.db, fresh.node_id).await?;
    match st.wings.sync_server(&conn, &fresh.uuid).await {
        Ok(()) | Err(pelican_wings::WingsError::Connection(_)) => {}
        Err(e) => return Err(wings_err(e)),
    }
    let fresh = servers::Entity::find_by_id(s.id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    Ok(PhpJson::ok(server_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?).into_response())
}

/// `PATCH /servers/{server}/startup` (`StartupModificationService`, admin level).
pub async fn startup(State(st): State<AppState>, Extension(ctx): Extension<Ctx>, In(input): In) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(&ctx)?;
    let mut v = Validator::new(&input);
    v.rules("startup", &["sometimes", "string"]);
    v.rules("environment", &["present", "array"]);
    v.rules("egg", &["required"]);
    v.custom("egg", exists_rule(&st, input.get("egg"), "eggs", "id").await?);
    v.rules("image", &["sometimes", "string"]);
    v.rules("skip_scripts", &["present", "boolean"]);
    v.validate()?;
    let egg_raw = input.get("egg").cloned().unwrap_or(Value::Null);
    let egg_input = php_int(&egg_raw);
    let environment = input.get("environment").cloned().unwrap_or(Value::Null);
    let now = Timestamp(pelican_core::time::now());

    let txn = st.db.begin().await?;
    if !pelican_core::php::empty(&environment) {
        let egg = if egg_raw.is_null() { s.egg_id } else { Int(egg_input) };
        let results = validate_variables(&txn, egg, &environment).await?;
        for (var, val) in &results {
            let value = stored_value(val);
            let existing = server_variables::Entity::find()
                .filter(server_variables::Column::ServerId.eq(s.id))
                .filter(server_variables::Column::VariableId.eq(var.id))
                .one(&txn)
                .await?;
            match existing {
                Some(row) if row.variable_value != value => {
                    pelican_db::write::update(
                        &txn,
                        &server_variables::ActiveModel {
                            id: sea_orm::ActiveValue::Unchanged(row.id),
                            variable_value: Set(value),
                            updated_at: Set(Some(now)),
                            ..Default::default()
                        },
                    )
                    .await?;
                }
                Some(_) => {}
                None => {
                    pelican_db::write::insert(
                        &txn,
                        server_variables::ActiveModel {
                            server_id: Set(s.id),
                            variable_id: Set(var.id),
                            variable_value: Set(value),
                            created_at: Set(Some(now)),
                            updated_at: Set(Some(now)),
                            ..Default::default()
                        },
                    )
                    .await?;
                }
            }
        }
    }
    // updateAdministrativeSettings()
    let mut startup = input.get("startup").filter(|v| !v.is_null()).map(pelican_core::php::string);
    let mut image = input.get("image").filter(|v| !v.is_null()).map(pelican_core::php::string);
    let mut am = servers::ActiveModel { id: sea_orm::ActiveValue::Unchanged(s.id), ..Default::default() };
    let mut dirty = false;
    let digit = !matches!(egg_raw, Value::Bool(_))
        && pelican_core::php::string(&egg_raw).chars().all(|c| c.is_ascii_digit())
        && !pelican_core::php::string(&egg_raw).is_empty();
    if digit && s.egg_id.0 != egg_input {
        let egg = eggs::Entity::find_by_id(Int(egg_input)).one(&txn).await?.ok_or_else(ApiError::model_not_found)?;
        am.egg_id = Set(egg.id);
        dirty = true;
        image = image.or_else(|| first_value(egg.docker_images.as_ref()));
        startup = startup.or_else(|| first_value(egg.startup_commands.as_ref()));
    }
    let new_startup = startup.unwrap_or_else(|| s.startup.clone());
    if new_startup != s.startup {
        am.startup = Set(new_startup);
        dirty = true;
    }
    let skip = input.get("skip_scripts").map(|v| !pelican_core::php::empty(v)).unwrap_or(false);
    if skip != s.skip_scripts.0 {
        am.skip_scripts = Set(Bool(skip));
        dirty = true;
    }
    let new_image = image.unwrap_or_else(|| s.image.clone());
    if new_image != s.image {
        am.image = Set(new_image);
        dirty = true;
    }
    if dirty {
        am.updated_at = Set(Some(now));
        pelican_db::write::update(&txn, &am).await?;
    }
    let fresh = servers::Entity::find_by_id(s.id).one(&txn).await?.ok_or_else(ApiError::model_not_found)?;
    server_saved(&st, &ctx, &txn, &fresh).await?;
    txn.commit().await?;
    Ok(PhpJson::ok(server_item(&st, &ctx, &fresh, &Includes::parse(&input)).await?).into_response())
}

/// `POST /servers/{server}/transfer` (`TransferServerService`).
pub async fn transfer(
    State(st): State<AppState>,
    Extension(ctx): Extension<Ctx>,
    In(input): In,
) -> ApiResult<Response> {
    ctx.acl("server", WRITE)?;
    let s = bound(&ctx)?;
    let mut v = Validator::new(&input);
    v.rules("node_id", &["required"]);
    v.custom("node_id", exists_rule(&st, input.get("node_id"), "nodes", "id").await?);
    v.rules("allocation_id", &["required", "bail"]);
    let taken = match input.get("allocation_id") {
        None | Some(Value::Null) => false,
        Some(val) => count_where(&st, "servers", "allocation_id", val, None).await? > 0,
    };
    v.custom(
        "allocation_id",
        CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
    );
    v.custom("allocation_id", exists_rule(&st, input.get("allocation_id"), "allocations", "id").await?);
    v.rules("allocation_additional", &["nullable", "array"]);
    v.rules("allocation_additional.*", &["integer"]);
    for path in crate::validation::expand_wildcard(&input.data, "allocation_additional.*") {
        v.custom(&path, exists_rule(&st, input.get(&path), "allocations", "id").await?);
    }
    v.validate()?;
    let node_id = Int(input.get("node_id").map(php_int).unwrap_or(0));
    let allocation_id = input.get("allocation_id").map(php_int).unwrap_or(0);
    let additional: Vec<i64> = match input.get("allocation_additional") {
        Some(Value::Array(a)) => a.iter().map(php_int).collect(),
        Some(Value::Object(m)) => m.values().map(php_int).collect(),
        _ => Vec::new(),
    };
    let node = nodes::Entity::find_by_id(node_id).one(&st.db).await?.ok_or_else(ApiError::model_not_found)?;
    if !is_viable(&st.db, &node, s.memory.0, s.disk.0, s.cpu.0).await? {
        // The viability query selects no `name`, so the message has an empty node name.
        return Err(ApiError::new(
            406,
            "NodeNotViableException",
            "The node  does not have enough free resources to accept this server.",
            Style::Pretty,
        ));
    }
    let blocked = !pelican_core::server_state::is_installed(&s)
        || s.status.as_deref() == Some("restoring_backup")
        || pelican_core::server_state::active_transfer(&st.db, s.id.0).await?.is_some();
    if blocked {
        let detail = pelican_core::server_state::conflict(&st.db, &s).await?.unwrap_or("");
        return Err(ApiError::new(409, "ServerStateConflictException", detail, Style::Pretty));
    }
    let now = Timestamp(pelican_core::time::now());
    let txn = st.db.begin().await?;
    let mut am = server_transfers::ActiveModel {
        server_id: Set(s.id),
        old_node: Set(s.node_id),
        new_node: Set(node_id),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    if let Some(primary) = s.allocation_id {
        let others: Vec<i64> = allocations::Entity::find()
            .filter(allocations::Column::ServerId.eq(s.id))
            .all(&txn)
            .await?
            .into_iter()
            .map(|a| a.id.0)
            .filter(|id| *id != primary.0)
            .collect();
        am.old_allocation = Set(Some(primary));
        am.new_allocation = Set(Some(Int(allocation_id)));
        am.old_additional_allocations = Set(Some(Json(json!(others))));
        am.new_additional_allocations = Set(Some(Json(json!(additional))));
        let unassigned: Vec<i64> = allocations::Entity::find()
            .filter(allocations::Column::NodeId.eq(node_id))
            .filter(allocations::Column::ServerId.is_null())
            .all(&txn)
            .await?
            .into_iter()
            .map(|a| a.id.0)
            .collect();
        let mut wanted = additional.clone();
        wanted.push(allocation_id);
        let update: Vec<i64> = wanted.into_iter().filter(|a| unassigned.contains(a)).collect();
        if !update.is_empty() {
            let marks = vec!["?"; update.len()].join(", ");
            let mut vals: Vec<sea_orm::Value> = vec![s.id.into(), now.into()];
            vals.extend(update.iter().map(|i| sea_orm::Value::from(*i)));
            txn.execute_raw(raw::stmt(
                st.db.get_database_backend(),
                &format!("update {{allocations}} set {{server_id}} = ?, {{updated_at}} = ? where {{id}} in ({marks})"),
                vals,
            ))
            .await?;
        }
    }
    pelican_db::write::insert(&txn, am).await?;
    txn.commit().await?;

    // JWT for the destination node; the source node is told via the bare client (no throwIf).
    let new_conn = node_conn(&st, &st.db, node_id).await?;
    let unix = chrono::Utc::now().timestamp();
    let token = pelican_wings::jwt::JwtBuilder {
        issuer: st.config.app_url.clone(),
        audience: new_conn.connection_address(),
        identified_by: s.uuid.clone(),
        issued_at: unix,
        expires_at: unix + 15 * 60,
        subject: Some(s.uuid.clone()),
        claims: vec![],
        user_uuid: None,
        scope: "transfer".into(),
        unique_id: pelican_wings::jwt::str_random(16),
    }
    .sign(&new_conn.token)
    .map_err(|_| ApiError::internal("InvalidKeyProvided"))?;
    let old_conn = node_conn(&st, &st.db, s.node_id).await?;
    let body = json!({
        "url": format!("{}/api/transfers", new_conn.connection_address()),
        "token": format!("Bearer {token}"),
        "backups": [],
        "server": {"uuid": s.uuid, "start_on_completion": false},
    });
    let path = format!("/api/servers/{}/transfer", s.uuid);
    let mut call = pelican_wings::client::Call::new(pelican_wings::client::Method::POST, &path).json(body);
    call.throw = false;
    st.wings.send(&old_conn, call).await.map_err(wings_err)?;
    Ok(no_content())
}
