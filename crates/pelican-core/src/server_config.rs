//! Server configuration document sent to Wings (remote-api.md §3):
//! `ServerConfigurationStructureService` (`settings`) and
//! `EggConfigurationService` (`process_configuration`).

use pelican_db::entities::{allocations, egg_variables, eggs, mounts, servers};
use pelican_db::raw;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, FromQueryResult};
use serde_json::{Map, Value, json};

/// A PHP error that the original turns into a 500 (`code` = class basename).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("database: {0}")]
    Db(#[from] DbErr),
    #[error("{0}")]
    Php(&'static str),
}

/// Feature schemas in registration order (FeatureServiceProvider).
pub const FEATURES: &[(&str, &[&str])] = &[
    ("gsl_token", &["(gsl token expired)", "(account not found)"]),
    (
        "java_version",
        &[
            "java.lang.UnsupportedClassVersionError",
            "unsupported major.minor version",
            "has been compiled by a more recent version of the java runtime",
            "minecraft 1.17 requires running the server with java 16 or above",
            "minecraft 1.18 requires running the server with java 17 or above",
            "minecraft 1.19 requires running the server with java 17 or above",
        ],
    ),
    ("eula", &["you need to agree to the eula in order to run the server"]),
    (
        "pid_limit",
        &[
            "pthread_create failed",
            "failed to create thread",
            "unable to create thread",
            "unable to create native thread",
            "unable to create new native thread",
            "exception in thread \"craft async scheduler management thread\"",
        ],
    ),
    ("steam_disk_space", &["steamcmd needs 250mb of free disk space to update", "0x202 after update job"]),
];

/// Everything needed to render a server's configuration.
#[derive(Clone, Debug)]
pub struct ServerBundle {
    pub server: servers::Model,
    pub egg: eggs::Model,
    /// `egg.configFrom` (single level).
    pub config_from: Option<eggs::Model>,
    /// `server.variables`: egg variables with the server's value (LEFT JOIN).
    pub variables: Vec<(egg_variables::Model, Option<String>)>,
    /// `server.allocation` (primary).
    pub allocation: Option<allocations::Model>,
    /// `server.allocations` (all rows with server_id, any node).
    pub allocations: Vec<allocations::Model>,
    pub mounts: Vec<mounts::Model>,
}

impl ServerBundle {
    pub async fn load<C: ConnectionTrait>(db: &C, server: servers::Model) -> Result<ServerBundle, ConfigError> {
        let egg = eggs::Entity::find_by_id(server.egg_id).one(db).await?.ok_or(ConfigError::Php("ErrorException"))?;
        let config_from = match egg.config_from {
            Some(id) => eggs::Entity::find_by_id(id).one(db).await?,
            None => None,
        };
        let backend = db.get_database_backend();
        // Server::variables(): HasMany + leftJoin, no ORDER BY (natural DB order).
        let rows = db
            .query_all_raw(raw::stmt(
                backend,
                "select {egg_variables}.*, {server_variables.variable_value} as {server_value} from {egg_variables} \
                 left join {server_variables} on {server_variables.variable_id} = {egg_variables.id} \
                 and {server_variables.server_id} = ? where {egg_variables.egg_id} = ? and {egg_variables.egg_id} is not null",
                vec![server.id.into(), server.egg_id.into()],
            ))
            .await?;
        let mut variables = Vec::with_capacity(rows.len());
        for r in rows {
            let v = egg_variables::Model::from_query_result(&r, "")?;
            let sv: Option<String> = r.try_get("", "server_value")?;
            variables.push((v, sv));
        }
        let allocation = match server.allocation_id {
            Some(id) => allocations::Entity::find_by_id(id).one(db).await?,
            None => None,
        };
        let allocations = allocations::Model::find_by_statement(raw::stmt(
            backend,
            "select * from {allocations} where {allocations.server_id} = ? and {allocations.server_id} is not null",
            vec![server.id.into()],
        ))
        .all(db)
        .await?;
        // morphToMany: inner join on the pivot, no ORDER BY.
        let mounts = mounts::Model::find_by_statement(raw::stmt(
            backend,
            "select {mounts}.* from {mounts} inner join {mountables} on {mounts.id} = {mountables.mount_id} \
             where {mountables.mountable_id} = ? and {mountables.mountable_type} = ?",
            vec![server.id.into(), "server".into()],
        ))
        .all(db)
        .await?;
        Ok(ServerBundle { server, egg, config_from, variables, allocation, allocations, mounts })
    }

    /// `Egg::inherit_config_*`: own value unless NULL and a parent exists.
    fn inherit(&self, f: impl Fn(&eggs::Model) -> Option<String>) -> Option<String> {
        match (&f(&self.egg), &self.egg.config_from, &self.config_from) {
            (Some(v), _, _) => Some(v.clone()),
            (None, None, _) => None,
            (None, Some(_), Some(parent)) => f(parent),
            (None, Some(_), None) => None,
        }
    }
}

/// `json_decode($v, true)` + later `json_encode`: objects whose keys are
/// exactly 0..n-1 become lists, empty objects become `[]`.
pub fn php_assoc(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            if m.is_empty() {
                return Value::Array(vec![]);
            }
            let is_list = m.keys().enumerate().all(|(i, k)| *k == i.to_string());
            if is_list {
                Value::Array(m.into_iter().map(|(_, v)| php_assoc(v)).collect())
            } else {
                Value::Object(m.into_iter().map(|(k, v)| (k, php_assoc(v))).collect())
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(php_assoc).collect()),
        other => other,
    }
}

fn php_round(x: f64) -> i64 {
    // PHP round(): half away from zero.
    x.round() as i64
}

fn size(v: i64, binary: bool) -> i64 {
    if binary { v } else { php_round(v as f64 / 1.048576) }
}

/// `EnvironmentService::handle`.
pub fn environment(b: &ServerBundle) -> Map<String, Value> {
    let mut env = Map::new();
    for (var, value) in &b.variables {
        let v = value.clone().or_else(|| Some(var.default_value.clone()));
        env.insert(var.env_variable.clone(), v.map(Value::String).unwrap_or(Value::Null));
    }
    env.insert("STARTUP".into(), Value::String(b.server.startup.clone()));
    env.insert("P_SERVER_UUID".into(), Value::String(b.server.uuid.clone()));
    env
}

/// `Server::getAllocationMappings`.
fn mappings(b: &ServerBundle) -> Value {
    let on_node: Vec<&allocations::Model> = b.allocations.iter().filter(|a| a.node_id == b.server.node_id).collect();
    if b.allocation.is_none() || on_node.is_empty() {
        let mut m = Map::new();
        m.insert(String::new(), Value::Array(vec![]));
        return Value::Object(m);
    }
    let mut m: Map<String, Value> = Map::new();
    for a in on_node {
        let entry = m.entry(a.ip.clone()).or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(list) = entry {
            list.push(json!(a.port.0));
        }
    }
    php_assoc(Value::Object(m))
}

/// Features registered at runtime (WASM plugins): `(id, listeners)`, installed by the binary.
static EXTRA_FEATURES: std::sync::OnceLock<fn() -> Vec<(String, Vec<String>)>> = std::sync::OnceLock::new();

pub fn set_extra_features(f: fn() -> Vec<(String, Vec<String>)>) {
    let _ = EXTRA_FEATURES.set(f);
}

/// `FeatureService::getMappings($egg->features)`.
fn features(egg: &eggs::Model) -> Value {
    let wanted: Option<Vec<String>> = egg.features.as_ref().map(|f| match &f.0 {
        Value::Array(a) => a.iter().map(|v| crate::php::string(v)).collect(),
        Value::Object(o) => o.values().map(|v| crate::php::string(v)).collect(),
        Value::Null => Vec::new(),
        other => vec![crate::php::string(other)],
    });
    let mut out = Map::new();
    for (id, listeners) in FEATURES {
        if wanted.as_ref().is_none_or(|w| w.iter().any(|x| x == id)) {
            out.insert((*id).to_string(), json!(listeners));
        }
    }
    for (id, listeners) in EXTRA_FEATURES.get().map(|f| f()).unwrap_or_default() {
        if !out.contains_key(&id) && wanted.as_ref().is_none_or(|w| w.iter().any(|x| *x == id)) {
            out.insert(id, json!(listeners));
        }
    }
    if out.is_empty() { Value::Array(vec![]) } else { Value::Object(out) }
}

/// `ServerConfigurationStructureService::returnFormat` (`settings`).
pub fn structure(b: &ServerBundle, use_binary_prefix: bool) -> Value {
    let s = &b.server;
    let mut root = Map::new();
    root.insert("id".into(), json!(s.id.0));
    root.insert("uuid".into(), json!(s.uuid));
    root.insert("meta".into(), json!({"name": s.name, "description": s.description}));
    root.insert("suspended".into(), json!(s.status.as_deref() == Some("suspended")));
    root.insert("environment".into(), Value::Object(environment(b)));
    root.insert("invocation".into(), json!(s.startup));
    root.insert("skip_egg_scripts".into(), json!(s.skip_scripts.0));
    let mut build = Map::new();
    build.insert("memory_limit".into(), json!(size(s.memory.0, use_binary_prefix)));
    build.insert("swap".into(), json!(size(s.swap.0, use_binary_prefix)));
    build.insert("io_weight".into(), json!(s.io.0));
    build.insert("cpu_limit".into(), json!(s.cpu.0));
    build.insert("threads".into(), json!(s.threads));
    build.insert("disk_space".into(), json!(size(s.disk.0, use_binary_prefix)));
    build.insert("oom_killer".into(), json!(s.oom_killer.0));
    root.insert("build".into(), Value::Object(build));
    root.insert("container".into(), json!({"image": s.image, "requires_rebuild": false}));
    let mut alloc = Map::new();
    alloc.insert("force_outgoing_ip".into(), json!(b.egg.force_outgoing_ip.0));
    let mut default = Map::new();
    default
        .insert("ip".into(), json!(b.allocation.as_ref().map(|a| a.ip.clone()).unwrap_or_else(|| "127.0.0.1".into())));
    default.insert("port".into(), json!(b.allocation.as_ref().map(|a| a.port.0).unwrap_or(0)));
    alloc.insert("default".into(), Value::Object(default));
    alloc.insert("mappings".into(), mappings(b));
    root.insert("allocations".into(), Value::Object(alloc));
    let denylist = match (&b.egg.config_from, &b.config_from) {
        (None, _) => b.egg.file_denylist.as_ref().map(|j| php_assoc(j.0.clone())),
        (Some(_), Some(parent)) => parent.file_denylist.as_ref().map(|j| php_assoc(j.0.clone())),
        (Some(_), None) => None,
    };
    let mut egg = Map::new();
    egg.insert("id".into(), json!(b.egg.uuid));
    egg.insert("file_denylist".into(), denylist.unwrap_or(Value::Null));
    egg.insert("features".into(), features(&b.egg));
    root.insert("egg".into(), Value::Object(egg));
    if let Some(labels) = s.docker_labels.as_ref().map(|j| php_assoc(j.0.clone())) {
        if !crate::php::empty(&labels) {
            root.insert("labels".into(), labels);
        }
    }
    if !b.mounts.is_empty() {
        let list: Vec<Value> = b
            .mounts
            .iter()
            .map(|m| json!({"source": m.source, "target": m.target, "read_only": m.read_only.0}))
            .collect();
        root.insert("mounts".into(), Value::Array(list));
    }
    Value::Object(root)
}

/// `EggConfigurationService::handle` (`process_configuration`).
pub fn process_configuration(b: &ServerBundle, structure: &Value) -> Result<Value, ConfigError> {
    // startup: json_decode(..., true) into an array-typed parameter.
    let startup_raw = b.inherit(|e| e.config_startup.clone()).ok_or(ConfigError::Php("TypeError"))?;
    let startup = match serde_json::from_str::<Value>(&startup_raw) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => php_assoc(v),
        _ => return Err(ConfigError::Php("TypeError")),
    };
    let get = |k: &str| match &startup {
        Value::Object(m) => m.get(k).cloned(),
        _ => None,
    };
    let done = match get("done") {
        Some(Value::String(s)) => json!([s]),
        Some(v) => v,
        None => Value::Null,
    };
    let strip_ansi = match get("strip_ansi") {
        None | Some(Value::Null) => json!(false),
        Some(v) => v,
    };
    let mut st = Map::new();
    st.insert("done".into(), done);
    st.insert("user_interaction".into(), json!([]));
    st.insert("strip_ansi".into(), strip_ansi);

    let stop_raw = b.inherit(|e| e.config_stop.clone()).ok_or(ConfigError::Php("TypeError"))?;
    let stop = match stop_raw.strip_prefix('^') {
        None => json!({"type": "command", "value": stop_raw}),
        Some(sig) => json!({"type": "signal", "value": sig.to_uppercase()}),
    };

    let files_raw = b.inherit(|e| e.config_files.clone()).unwrap_or_else(|| "{}".into());
    let configs = match serde_json::from_str::<Value>(&files_raw) {
        Ok(files @ (Value::Object(_) | Value::Array(_))) => replace_placeholders(&files, structure)?,
        _ => Vec::new(),
    };

    let mut root = Map::new();
    root.insert("startup".into(), Value::Object(st));
    root.insert("stop".into(), stop);
    root.insert("configs".into(), Value::Array(configs));
    Ok(Value::Object(root))
}

fn replace_placeholders(files: &Value, structure: &Value) -> Result<Vec<Value>, ConfigError> {
    let entries: Vec<(Value, &Value)> = match files {
        Value::Object(m) => m.iter().map(|(k, v)| (Value::String(k.clone()), v)).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (json!(i), v)).collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for (file, data) in entries {
        // `!is_object($data) || !isset($data->find)`.
        let Value::Object(obj) = data else { continue };
        let Some(find) = obj.get("find").filter(|f| !f.is_null()) else { continue };
        let mut append = obj.clone();
        append.insert("file".into(), file);
        append.insert("replace".into(), Value::Array(vec![]));
        let mut replace = Vec::new();
        let iterated = iterate(find, structure)?;
        let pairs: Vec<(Value, Value)> = match iterated {
            Value::Object(m) => m.into_iter().map(|(k, v)| (Value::String(k), v)).collect(),
            Value::Array(a) => a.into_iter().enumerate().map(|(i, v)| (json!(i), v)).collect(),
            // foreach over a scalar: PHP warning → ErrorException.
            _ => return Err(ConfigError::Php("ErrorException")),
        };
        for (find_key, rep) in pairs {
            match rep {
                Value::Object(m) => {
                    for (if_value, with) in m {
                        replace.push(json!({"match": find_key, "if_value": if_value, "replace_with": with}));
                    }
                }
                other => replace.push(json!({"match": find_key, "replace_with": other})),
            }
        }
        append.insert("replace".into(), Value::Array(replace));
        append.shift_remove("find");
        out.push(Value::Object(append));
    }
    Ok(out)
}

fn iterate(data: &Value, structure: &Value) -> Result<Value, ConfigError> {
    Ok(match data {
        Value::Object(m) => {
            let mut c = Map::new();
            for (k, v) in m {
                let nv = if v.is_object() || v.is_array() {
                    iterate(v, structure)?
                } else {
                    match_and_replace(v, structure)?
                };
                c.insert(k.clone(), nv);
            }
            Value::Object(c)
        }
        Value::Array(a) => {
            let mut c = Vec::with_capacity(a.len());
            for v in a {
                c.push(if v.is_object() || v.is_array() {
                    iterate(v, structure)?
                } else {
                    match_and_replace(v, structure)?
                });
            }
            Value::Array(c)
        }
        other => other.clone(),
    })
}

/// `Arr::get($structure, $key, '')`.
fn arr_get<'a>(structure: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(v) = structure.as_object().and_then(|m| m.get(key)) {
        return Some(v);
    }
    if !key.contains('.') {
        return None;
    }
    let mut cur = structure;
    for seg in key.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn match_and_replace(value: &Value, structure: &Value) -> Result<Value, ConfigError> {
    let Value::String(s) = value else { return Ok(value.clone()) };
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\{\{(?P<key>[A-Za-z0-9_.\-]*)\}\}").expect("valid regex"));
    let keys: Vec<String> = RE.captures_iter(s).map(|c| c["key"].to_string()).collect();
    let mut out = s.clone();
    for key in keys {
        let path = if let Some(rest) = key.strip_prefix("server.") {
            rest.to_string()
        } else if let Some(rest) = key.strip_prefix("env.") {
            format!("environment.{rest}")
        } else {
            continue;
        };
        let replacement = match arr_get(structure, &path) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(v)) => v.clone(),
            Some(Value::Bool(b)) => {
                if *b {
                    "1".into()
                } else {
                    String::new()
                }
            }
            Some(Value::Number(n)) => n.to_string(),
            Some(_) => return Err(ConfigError::Php("TypeError")),
        };
        out = out.replace(&format!("{{{{{key}}}}}"), &replacement);
    }
    Ok(Value::String(out))
}

/// `JsonResource::filter()` re-indexing used by the list endpoint (§3.9):
/// PHP arrays whose keys are all numeric become lists.
pub fn reindex_numeric(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            if !m.is_empty() && m.keys().all(|k| crate::php::is_numeric_str(k)) {
                Value::Array(m.into_iter().map(|(_, v)| reindex_numeric(v)).collect())
            } else {
                Value::Object(m.into_iter().map(|(k, v)| (k, reindex_numeric(v))).collect())
            }
        }
        Value::Array(a) => Value::Array(a.into_iter().map(reindex_numeric).collect()),
        other => other,
    }
}

/// `{settings, process_configuration}` for one server.
pub async fn render<C: ConnectionTrait>(
    db: &C,
    server: servers::Model,
    use_binary_prefix: bool,
) -> Result<(Value, Value), ConfigError> {
    let b = ServerBundle::load(db, server).await?;
    let settings = structure(&b, use_binary_prefix);
    let pc = process_configuration(&b, &settings)?;
    Ok((settings, pc))
}
