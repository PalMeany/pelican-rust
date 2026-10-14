//! Scenario and fixture files (YAML) — see difftest/README.md.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::Deserialize;

/// Rows per table, inserted in file order. Values: scalars, objects/arrays
/// (stored as PHP json_encode text) and string directives:
/// `@encrypt:<plain>`, `@bcrypt:<plain>`, `@now`, `@now+<secs>`, `@now-<secs>`.
pub type Rows = IndexMap<String, Vec<IndexMap<String, serde_yaml::Value>>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Fixture files from `difftest/fixtures/<name>.yaml`, loaded in order.
    #[serde(default)]
    pub fixtures: Vec<String>,
    /// Extra rows on top of the fixtures.
    #[serde(default)]
    pub rows: Rows,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub normalize: NormalizeRules,
    #[serde(default)]
    pub wings: WingsConfig,
    /// Restrict to these backends (default: all).
    #[serde(default)]
    pub backends: Option<Vec<String>>,
    /// Skip with a reason (known divergence, not yet implemented, ...).
    #[serde(default)]
    pub skip: Option<String>,
    #[serde(skip)]
    pub file: PathBuf,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default = "default_method")]
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub headers: IndexMap<String, String>,
    /// Shortcut for `Authorization: Bearer <token>`.
    #[serde(default)]
    pub bearer: Option<String>,
    /// JSON request body.
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    /// Raw request body (sent as-is).
    #[serde(default)]
    pub body: Option<String>,
    /// Send `Accept: application/json` (default true).
    #[serde(default = "default_true")]
    pub accept_json: bool,
    /// Optional sanity check applied to BOTH panels.
    #[serde(default)]
    pub expect_status: Option<u16>,
}

fn default_method() -> String {
    "GET".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizeRules {
    /// `table.column` whose newly written values are random (masked by length).
    #[serde(default)]
    pub random_columns: Vec<String>,
    /// Literal regexes; every match is treated as a random token.
    #[serde(default)]
    pub random_patterns: Vec<String>,
    /// Response headers to compare in addition to the defaults.
    #[serde(default)]
    pub headers: Vec<String>,
    /// Tables whose changes are not compared.
    #[serde(default)]
    pub ignore_tables: Vec<String>,
    /// `table.column` not compared.
    #[serde(default)]
    pub ignore_columns: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WingsConfig {
    #[serde(default)]
    pub responses: Vec<WingsResponse>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WingsResponse {
    #[serde(default)]
    pub method: Option<String>,
    /// Path pattern; `*` matches one path segment, `**` the rest.
    pub path: String,
    #[serde(default = "default_status")]
    pub status: u16,
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: IndexMap<String, String>,
}

fn default_status() -> u16 {
    200
}

pub fn load_scenarios(dir: &Path, filter: Option<&str>) -> Result<Vec<Scenario>> {
    let mut files: Vec<PathBuf> = walk_yaml(dir)?;
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f)?;
        // A file may hold several scenarios separated by `---`.
        for doc in serde_yaml::Deserializer::from_str(&text) {
            let mut s =
                Scenario::deserialize(doc).with_context(|| format!("parsing {}", f.display()))?;
            s.file = f.clone();
            if filter.is_none_or(|flt| s.name.contains(flt)) {
                out.push(s);
            }
        }
    }
    Ok(out)
}

fn walk_yaml(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            out.extend(walk_yaml(&p)?);
        } else if p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
            out.push(p);
        }
    }
    Ok(out)
}

pub fn load_fixture(dir: &Path, name: &str) -> Result<Rows> {
    let p = dir.join(format!("{name}.yaml"));
    let text =
        std::fs::read_to_string(&p).with_context(|| format!("reading fixture {}", p.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parsing fixture {}", p.display()))
}
