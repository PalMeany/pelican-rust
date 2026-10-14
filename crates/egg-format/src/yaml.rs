//! Port of Symfony YAML 7.4 `Yaml::dump($value, 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`
//! (`vendor/symfony/yaml/{Dumper,Inline,Escaper}.php`).
//!
//! PHP arrays are represented as `serde_json::Value`: JSON arrays are PHP lists,
//! objects are PHP maps (keys that are canonical integers are PHP int keys).

use serde_json::Value;

/// `Yaml::dump($value, 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`.
pub fn dump(value: &Value) -> String {
    let _ = value;
    unimplemented!("port of Symfony\\Component\\Yaml\\Dumper::dump")
}
