//! `plugins/<id>/plugin.json` as `App\Models\Plugin` (a Sushi model) reads it, and
//! `PluginService::setMetaData()` writes it back.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    Disabled,
    Enabled,
    Errored,
    Incompatible,
    Crashed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::NotInstalled => "not_installed",
            Status::Disabled => "disabled",
            Status::Enabled => "enabled",
            Status::Errored => "errored",
            Status::Incompatible => "incompatible",
            Status::Crashed => "crashed",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "not_installed" => Status::NotInstalled,
            "disabled" => Status::Disabled,
            "enabled" => Status::Enabled,
            "errored" => Status::Errored,
            "incompatible" => Status::Incompatible,
            "crashed" => Status::Crashed,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Plugin,
    Theme,
    Language,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Plugin => "plugin",
            Category::Theme => "theme",
            Category::Language => "language",
        }
    }

    fn parse(s: &str) -> Option<Category> {
        Some(match s {
            "plugin" => Category::Plugin,
            "theme" => Category::Theme,
            "language" => Category::Language,
            _ => return None,
        })
    }
}

/// One row of the Sushi model (`Plugin::getRows()`).
#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub author: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub category: Category,
    pub url: Option<String>,
    pub update_url: Option<String>,
    pub namespace: String,
    pub class: String,
    /// Comma-joined panel ids (`null` = every panel).
    pub panels: Option<String>,
    pub panel_version: Option<String>,
    pub api_version: Option<i64>,
    /// JSON string of `{"vendor/package": "constraint"}`.
    pub composer_packages: Option<String>,
    pub status: Status,
    pub status_message: Option<String>,
    pub load_order: i64,
    /// pelican-rs: the WASM component section (`"wasm": {...}`), ignored by the original.
    pub wasm: Option<Value>,
    /// The plugin directory name.
    pub dir: String,
}

impl Plugin {
    /// `fullClass()`.
    pub fn full_class(&self) -> String {
        format!("\\{}\\{}", self.namespace, self.class)
    }

    pub fn is_error_row(&self) -> bool {
        self.namespace == "Error" && self.class == "Error"
    }

    /// `shouldLoad(?panelId)`.
    pub fn should_load(&self, panel: Option<&str>) -> bool {
        !self.is_error_row()
            && matches!(self.status, Status::Enabled | Status::Errored)
            && match (panel, self.panels.as_deref()) {
                (None, _) | (_, None) | (_, Some("")) => true,
                (Some(p), Some(list)) => list.split(',').any(|x| x == p),
            }
    }

    pub fn is_theme(&self) -> bool {
        self.category == Category::Theme
    }

    pub fn is_language(&self) -> bool {
        self.category == Category::Language
    }

    /// `effectiveApiVersion()`.
    pub fn effective_api_version(&self) -> i64 {
        self.api_version.unwrap_or(1)
    }

    /// `isPanelVersionStrict()`.
    pub fn is_panel_version_strict(&self) -> bool {
        !self.panel_version.as_deref().unwrap_or("").starts_with('^')
    }

    /// `panels ? explode(',', panels) : null` (PluginData).
    pub fn panel_list(&self) -> Option<Vec<String>> {
        self.panels.as_deref().filter(|p| !p.is_empty()).map(|p| p.split(',').map(str::to_string).collect())
    }
}

/// `Str::headline()` of a folder name.
fn headline(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for part in s.split(|c: char| c == '-' || c == '_' || c.is_whitespace()).filter(|p| !p.is_empty()) {
        let mut cur = String::new();
        for c in part.chars() {
            if c.is_uppercase() && !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            words.push(cur);
        }
    }
    words
        .iter()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(v) => Some(pelican_core::php::string(v)),
    }
}

/// The checked part of `getRows()`; `Err` carries the message that ends up in the error row.
fn parse_row(dir: &str, data: &Map<String, Value>) -> std::result::Result<Plugin, (String, bool)> {
    let undefined = |k: &str| (format!("Undefined array key \"{k}\""), false);
    let id = data.get("id").ok_or_else(|| undefined("id"))?;
    let id = pelican_core::php::string(id).to_lowercase();
    if id != dir.to_lowercase() {
        return Err((format!("Plugin id mismatch for folder name ({dir}) and id in plugin.json ({id})!"), true));
    }
    let panels = match data.get("panels") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().map(pelican_core::php::string).collect::<Vec<_>>().join(",")),
        Some(v) => Some(pelican_core::php::string(v)),
    };
    let composer_packages = match data.get("composer_packages") {
        None | Some(Value::Null) => None,
        Some(v @ (Value::Array(_) | Value::Object(_))) => {
            Some(laravel_compat::json::to_string_with_flags(v, 0).map_err(|e| (e.to_string(), false))?)
        }
        Some(v) => Some(pelican_core::php::string(v)),
    };
    let required = |k: &str| data.get(k).map(pelican_core::php::string).ok_or_else(|| undefined(k));
    let name = required("name")?;
    let author = required("author")?;
    let category_raw = required("category")?;
    let namespace = required("namespace")?;
    let class = required("class")?;
    let meta = data.get("meta");
    let load_order = match meta.and_then(|m| m.get("load_order")) {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(0),
        Some(_) => {
            return Err(("Array value for key [meta.load_order] must be an integer, string found.".into(), false));
        }
    };
    let category = Category::parse(&category_raw).ok_or_else(|| {
        (format!("\"{category_raw}\" is not a valid backing value for enum App\\Enums\\PluginCategory"), false)
    })?;
    let status_raw = meta
        .and_then(|m| m.get("status"))
        .map(pelican_core::php::string)
        .unwrap_or_else(|| "not_installed".into())
        .to_lowercase();
    let status = Status::parse(&status_raw).ok_or_else(|| {
        (format!("\"{status_raw}\" is not a valid backing value for enum App\\Enums\\PluginStatus"), false)
    })?;
    Ok(Plugin {
        id,
        name,
        author,
        version: match data.get("version") {
            None => Some("1.0.0".into()),
            v => opt_string(v),
        },
        description: opt_string(data.get("description")),
        category,
        url: opt_string(data.get("url")),
        update_url: opt_string(data.get("update_url")),
        namespace,
        class,
        panels,
        panel_version: opt_string(data.get("panel_version")),
        api_version: data.get("api_version").and_then(Value::as_i64),
        composer_packages,
        status,
        status_message: opt_string(meta.and_then(|m| m.get("status_message"))),
        load_order,
        wasm: data.get("wasm").filter(|w| w.is_object()).cloned(),
        dir: dir.to_string(),
    })
}

/// `Plugin::getRows()`: every directory (by name, dot-directories skipped) with a
/// `plugin.json`; unreadable JSON drops the plugin, other errors become error rows.
pub fn scan(root: &Path) -> Vec<Plugin> {
    let mut dirs: Vec<String> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    let mut rows = Vec::new();
    for dir in dirs {
        let path = root.join(&dir).join("plugin.json");
        let Ok(raw) = std::fs::read(&path) else { continue };
        let data = match serde_json::from_slice::<Value>(&raw) {
            Ok(Value::Object(m)) => m,
            Ok(_) | Err(_) => {
                tracing::warn!(plugin = %dir, "plugin.json is not valid JSON");
                continue;
            }
        };
        match parse_row(&dir, &data) {
            Ok(p) => rows.push(p),
            Err((message, mismatch)) => {
                tracing::warn!(plugin = %dir, error = %message, "invalid plugin");
                let lowered_id = data.get("id").map(|v| pelican_core::php::string(v).to_lowercase());
                rows.push(Plugin {
                    id: if mismatch {
                        dir.clone()
                    } else {
                        lowered_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
                    },
                    name: data.get("name").map(pelican_core::php::string).unwrap_or_else(|| headline(&dir)),
                    author: data.get("author").map(pelican_core::php::string).unwrap_or_else(|| "Unknown".into()),
                    version: Some(data.get("version").map(pelican_core::php::string).unwrap_or_else(|| "0.0.0".into())),
                    description: Some(if mismatch { message.clone() } else { "Plugin.json is invalid!".into() }),
                    category: Category::Plugin,
                    url: opt_string(data.get("url")),
                    update_url: None,
                    namespace: "Error".into(),
                    class: "Error".into(),
                    panels: None,
                    panel_version: None,
                    api_version: None,
                    composer_packages: None,
                    status: Status::Errored,
                    status_message: Some(message),
                    load_order: 0,
                    wasm: None,
                    dir: dir.clone(),
                });
            }
        }
    }
    rows
}

/// `json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)` after a PHP
/// associative-array decode (empty objects become `[]`).
pub fn encode_manifest(data: &Value) -> Result<String> {
    let v = pelican_core::server_config::php_assoc(data.clone());
    laravel_compat::json::to_string_with_flags(
        &v,
        laravel_compat::json::JSON_PRETTY_PRINT | laravel_compat::json::JSON_UNESCAPED_SLASHES,
    )
    .map_err(|e| Error::Message(e.to_string()))
}

pub fn read_manifest(path: &Path) -> Result<Map<String, Value>> {
    let raw = std::fs::read(path).map_err(Error::io)?;
    match serde_json::from_slice::<Value>(&raw) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(Error::Message("Syntax error".into())),
        Err(e) => Err(Error::Message(e.to_string())),
    }
}

/// `setMetaData()`: merge into `meta` and rewrite the file (missing file = no-op).
pub fn set_meta(root: &Path, dir: &str, data: &[(&str, Value)]) -> Result<()> {
    let path: PathBuf = root.join(dir).join("plugin.json");
    if !path.exists() {
        return Ok(());
    }
    let mut manifest = read_manifest(&path)?;
    let mut meta = match manifest.get("meta").cloned().map(pelican_core::server_config::php_assoc) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (k, v) in data {
        meta.insert((*k).to_string(), v.clone());
    }
    manifest.insert("meta".into(), Value::Object(meta));
    std::fs::write(&path, encode_manifest(&Value::Object(manifest))?).map_err(Error::io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, dir: &str, json: &str) {
        std::fs::create_dir_all(root.join(dir)).expect("mkdir");
        std::fs::write(root.join(dir).join("plugin.json"), json).expect("write");
    }

    #[test]
    fn rows_defaults_and_errors() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        write(
            root,
            "good",
            r#"{"id":"Good","name":"Good","author":"Me","category":"plugin","namespace":"Me\\Good","class":"GoodPlugin","panels":["admin","server"],"composer_packages":{"a/b":"^1"}}"#,
        );
        write(
            root,
            "mismatch",
            r#"{"id":"other","name":"X","author":"Y","category":"plugin","namespace":"N","class":"C"}"#,
        );
        write(root, "no-name", r#"{"id":"no-name","author":"Y","category":"plugin","namespace":"N","class":"C"}"#);
        write(root, "broken", "{nope");
        std::fs::create_dir_all(root.join(".import-abc")).expect("mkdir");
        let rows = scan(root);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["good", "mismatch", "no-name"]);
        let good = &rows[0];
        assert_eq!(good.version.as_deref(), Some("1.0.0"));
        assert_eq!(good.panels.as_deref(), Some("admin,server"));
        assert_eq!(good.composer_packages.as_deref(), Some(r#"{"a\/b":"^1"}"#));
        assert_eq!(good.status, Status::NotInstalled);
        assert!(rows[1].is_error_row() && rows[1].description.as_deref().is_some_and(|d| d.contains("mismatch")));
        assert_eq!(rows[2].name, "No Name");
        assert_eq!(rows[2].status_message.as_deref(), Some("Undefined array key \"name\""));
    }

    #[test]
    fn meta_write_keeps_unknown_keys_php_style() {
        let tmp = tempfile::tempdir().expect("tmp");
        write(tmp.path(), "p", r#"{"id":"p","extra":{},"url":"https://x/y","meta":{"load_order":2}}"#);
        set_meta(tmp.path(), "p", &[("status", Value::from("enabled")), ("status_message", Value::Null)])
            .expect("meta");
        let out = std::fs::read_to_string(tmp.path().join("p/plugin.json")).expect("read");
        assert_eq!(
            out,
            "{\n    \"id\": \"p\",\n    \"extra\": [],\n    \"url\": \"https://x/y\",\n    \"meta\": {\n        \"load_order\": 2,\n        \"status\": \"enabled\",\n        \"status_message\": null\n    }\n}"
        );
    }
}
