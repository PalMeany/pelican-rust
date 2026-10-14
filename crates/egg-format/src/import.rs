//! `EggImporterService::parse()` (docs/spec/eggs.md §5.2): decode, version
//! upgrade, config/script normalisation, reserved variable names.

use serde_json::{Map, Value, json};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// `InvalidFileUploadException('File parse failed: …')`.
    #[error("File parse failed: {0}")]
    Parse(String),
    /// `InvalidFileUploadException('The file format is not recognized.')`.
    #[error("The file format is not recognized.")]
    UnknownVersion,
    /// A PHP warning/TypeError turned into an exception (500).
    #[error("{0}")]
    Php(&'static str),
}

pub const RESERVED_ENV_NAMES: &[&str] = &[
    "P_SERVER_UUID",
    "P_SERVER_ALLOCATION_LIMIT",
    "SERVER_MEMORY",
    "SERVER_IP",
    "SERVER_PORT",
    "ENV",
    "HOME",
    "USER",
    "STARTUP",
    "MODIFIED_STARTUP",
    "SERVER_UUID",
    "UUID",
    "INTERNAL_IP",
    "HOSTNAME",
    "TERM",
    "LANG",
    "PWD",
    "TZ",
    "TIMEZONE",
];

const UPGRADE_VARIABLES: &[(&str, &str)] = &[
    ("server.build.env.SERVER_IP", "server.allocations.default.ip"),
    ("server.build.default.ip", "server.allocations.default.ip"),
    ("server.build.env.SERVER_PORT", "server.allocations.default.port"),
    ("server.build.default.port", "server.allocations.default.port"),
    ("server.build.env.SERVER_MEMORY", "server.build.memory_limit"),
    ("server.build.memory", "server.build.memory_limit"),
    ("server.build.env.", "server.environment."),
    ("server.build.environment.", "server.environment."),
];

/// YAML → JSON value as `Yaml::parse()` returns it (maps become PHP arrays).
fn yaml_to_json(v: serde_yaml::Value) -> Value {
    use serde_yaml::Value as Y;
    match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                n.as_f64().map(|f| json!(f)).unwrap_or(Value::Null)
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(s) => Value::Array(s.into_iter().map(yaml_to_json).collect()),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                let key = match k {
                    Y::String(s) => s,
                    Y::Bool(b) => {
                        if b {
                            "1".into()
                        } else {
                            String::new()
                        }
                    }
                    Y::Number(n) => n.to_string(),
                    Y::Null => String::new(),
                    other => serde_yaml::to_string(&other).unwrap_or_default().trim().to_string(),
                };
                out.insert(key, yaml_to_json(v));
            }
            crate::export::php_assoc(Value::Object(out))
        }
        Y::Tagged(t) => yaml_to_json(t.value),
    }
}

/// `json_encode((array) $v, JSON_UNESCAPED_SLASHES)`.
fn compact_json(v: &Value) -> Result<String, ImportError> {
    use laravel_compat::json::{JSON_UNESCAPED_SLASHES, to_string_with_flags};
    let arr = match v {
        Value::Object(m) if m.is_empty() => Value::Array(vec![]),
        other => other.clone(),
    };
    to_string_with_flags(&arr, JSON_UNESCAPED_SLASHES).map_err(|_| ImportError::Php("JsonException"))
}

/// Parse an uploaded egg (`Yaml::parse` of the raw content) and normalise it.
pub fn parse(content: &str) -> Result<Value, ImportError> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(content).map_err(|e| ImportError::Parse(e.to_string()))?;
    let mut parsed = yaml_to_json(yaml);
    let Value::Object(_) = parsed else {
        // `$parsed['meta']['version']` on a scalar is null → unknown version.
        return Err(ImportError::UnknownVersion);
    };
    let version = parsed.get("meta").and_then(|m| m.get("version")).cloned().unwrap_or(Value::Null);
    match version.as_str() {
        Some("PTDL_v1") => {
            convert_legacy(&mut parsed);
            convert_to_v3(&mut parsed)?;
        }
        Some("PTDL_v2" | "PLCN_v1" | "PLCN_v2") => convert_to_v3(&mut parsed)?,
        Some("PLCN_v3") => {}
        _ => return Err(ImportError::UnknownVersion),
    }
    let obj = parsed.as_object_mut().ok_or(ImportError::UnknownVersion)?;

    if let Some(config) = obj.get_mut("config") {
        if let Value::Object(cfg) = config {
            let keys: Vec<String> = cfg.keys().cloned().collect();
            for k in keys {
                if let Some(v) = cfg.get(&k).cloned() {
                    if v.is_array() || v.is_object() {
                        cfg.insert(k.clone(), Value::String(compact_json(&v)?));
                    }
                }
                if k == "files" {
                    if let Some(Value::String(s)) = cfg.get(&k).cloned() {
                        let mut s = s;
                        for (from, to) in UPGRADE_VARIABLES {
                            s = s.replace(from, to);
                        }
                        cfg.insert(k.clone(), Value::String(s));
                    }
                }
            }
        } else if let Value::Array(list) = config {
            // A YAML list under `config`: PHP iterates int keys the same way.
            for item in list.iter_mut() {
                if item.is_array() || item.is_object() {
                    *item = Value::String(compact_json(item)?);
                }
            }
        }
    }
    if let Some(Value::Object(inst)) = obj.get_mut("scripts").and_then(|s| s.get_mut("installation")) {
        let keys: Vec<String> = inst.keys().cloned().collect();
        for k in keys {
            if let Some(v) = inst.get(&k).cloned() {
                if v.is_array() || v.is_object() {
                    inst.insert(k, Value::String(compact_json(&v)?));
                }
            }
        }
    }
    if let Some(Value::Array(vars)) = obj.get_mut("variables") {
        for var in vars.iter_mut() {
            if let Some(Value::Bool(b)) = var.get("default_value").cloned() {
                if let Some(m) = var.as_object_mut() {
                    m.insert("default_value".into(), json!(if b { "true" } else { "false" }));
                }
            }
        }
    }
    // Reserved env names.
    let vars: Vec<Value> = match obj.get("variables") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        _ => Vec::new(),
    };
    let upper = |v: &Value| -> Value {
        let mut v = v.clone();
        if let Some(m) = v.as_object_mut() {
            let name = m.get("env_variable").map(php_str).unwrap_or_default().to_uppercase();
            m.insert("env_variable".into(), Value::String(name));
        }
        v
    };
    let mapped: Vec<Value> = vars.iter().map(upper).collect();
    let is_reserved =
        |v: &Value| v.get("env_variable").and_then(Value::as_str).is_some_and(|n| RESERVED_ENV_NAMES.contains(&n));
    let forbidden: Vec<Value> = mapped.iter().filter(|v| is_reserved(v)).cloned().collect();
    if !forbidden.is_empty() {
        let allowed: Vec<Value> = mapped.iter().filter(|v| !is_reserved(v)).cloned().collect();
        let renamed: Vec<Value> = forbidden
            .iter()
            .map(|v| {
                let mut v = v.clone();
                if let Some(m) = v.as_object_mut() {
                    let n = m.get("env_variable").map(php_str).unwrap_or_default();
                    m.insert("env_variable".into(), Value::String(format!("SERVER_{n}")));
                }
                v
            })
            .collect();
        let mut all = allowed;
        all.extend(renamed);
        obj.insert("variables".into(), Value::Array(all));
        let names: Vec<String> =
            forbidden.iter().filter_map(|v| v.get("env_variable").and_then(Value::as_str).map(regex::escape)).collect();
        if let Ok(re) = regex::Regex::new(&format!(r"\b({})\b", names.join("|"))) {
            if let Some(Value::Object(cmds)) = obj.get_mut("startup_commands") {
                for (_, cmd) in cmds.iter_mut() {
                    if let Value::String(s) = cmd {
                        *s = re.replace_all(s, "SERVER_$1").into_owned();
                    }
                }
            }
        }
    }
    if let Some(Value::String(img)) = obj.get("image").cloned() {
        if img.starts_with("data:") {
            obj.shift_remove("image");
            obj.insert("icon".into(), Value::String(img));
        }
    }
    Ok(parsed)
}

fn php_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(true) => "1".into(),
        Value::Bool(false) => String::new(),
        other => other.to_string(),
    }
}

fn convert_legacy(parsed: &mut Value) {
    let Some(obj) = parsed.as_object_mut() else { return };
    let images: Vec<Value> = match obj.get("images") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(m)) => m.values().cloned().collect(),
        Some(other) => vec![other.clone()],
        None => vec![obj.get("image").cloned().filter(|v| !v.is_null()).unwrap_or(json!("nil"))],
    };
    obj.shift_remove("images");
    obj.shift_remove("image");
    obj.shift_remove("field_type");
    let mut map = Map::new();
    for i in images {
        let s = php_str(&i);
        map.insert(s.clone(), Value::String(s));
    }
    obj.insert("docker_images".into(), Value::Object(map));
}

fn convert_to_v3(parsed: &mut Value) -> Result<(), ImportError> {
    let obj = parsed.as_object_mut().ok_or(ImportError::Php("ErrorException"))?;
    let startup = obj.shift_remove("startup").ok_or(ImportError::Php("ErrorException"))?;
    let mut cmds = Map::new();
    cmds.insert("Default".into(), startup);
    obj.shift_remove("startup_commands");
    obj.insert("startup_commands".into(), Value::Object(cmds));
    Ok(())
}
