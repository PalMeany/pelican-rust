//! `p:egg:check-updates`, `p:egg:update-index`, `p:egg:normalize` (app/Console/Commands/Egg).

use clap::{Args, Subcommand};
use egg_format::import::ImportError;
use pelican_api::egg_index;
use pelican_core::state::AppState;
use pelican_db::entities::eggs;
use sea_orm::EntityTrait;
use serde_json::{Map, Value};

use super::Ctx;
use crate::console::Console;

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(name = "p:egg:check-updates")]
    CheckUpdates,
    #[command(name = "p:egg:update-index")]
    UpdateIndex,
    /// Upgrades egg json/yaml to latest format as yaml
    #[command(name = "p:egg:normalize")]
    Normalize(NormalizeArgs),
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    Ok(match cmd {
        Command::CheckUpdates => check_updates(&ctx.state().await?, io).await,
        Command::UpdateIndex => update_index(&ctx.state().await?, io).await,
        // Works on files only, no database needed.
        Command::Normalize(a) => normalize(io, a),
    })
}

/// CheckEggUpdatesCommand. The `eggs.<uuid>.update` flags live in this process's memory
/// (ADR-0009), so a running panel does not see them.
pub async fn check_updates(st: &AppState, io: &Console) -> i32 {
    let all = match eggs::Entity::find().all(&st.db).await {
        Ok(e) => e,
        Err(e) => {
            io.error(&e.to_string());
            return 1;
        }
    };
    for egg in all {
        if egg.update_url.is_none() {
            io.warn(&format!("{}: Skipping (no update url set)", egg.name));
            continue;
        }
        match egg_index::check(st, &egg).await {
            Err(e) => io.error(&format!("{}: Error ({e})", egg.name)),
            // `check()` has just stored the comparison result.
            Ok(()) if egg.uuid.as_deref().is_some_and(egg_index::has_update) => {
                io.warn(&format!("{}: Found update", egg.name))
            }
            Ok(()) => io.info(&format!("{}: Up-to-date", egg.name)),
        }
    }
    0
}

/// UpdateEggIndexCommand. The `eggs.index` cache lives in this process's memory (ADR-0009).
pub async fn update_index(st: &AppState, io: &Console) -> i32 {
    let index = match egg_index::update_index(st).await {
        Ok(i) => i,
        Err(e) => {
            io.error(&e);
            return 1;
        }
    };
    for (nest, eggs) in &index {
        io.info(&format!("Nest: {nest}"));
        for name in eggs.values() {
            io.warn(&format!("Egg: {name}"));
        }
        io.info("");
    }
    0
}

#[derive(Args, Debug)]
pub struct NormalizeArgs {
    file: String,
    #[arg(long = "delete-original")]
    delete_original: bool,
}

/// NormalizeEggCommand::handle().
pub fn normalize(io: &Console, a: NormalizeArgs) -> i32 {
    let input = a.file;
    let Ok(bytes) = std::fs::read(&input) else {
        io.error(&format!("Failed to read file: {input}"));
        return 1;
    };
    let extension = php_extension(&input).to_ascii_lowercase();
    io.info(&format!("Importing {input}"));
    if !matches!(extension.as_str(), "yaml" | "yml" | "json") {
        io.error(&format!(" -> unsupported extension {extension} for {input}"));
        return 1;
    }
    // Undecodable bytes fail in the parser like any other malformed file.
    let Ok(unparsed) = String::from_utf8(bytes) else {
        io.error(&format!(" -> unsupported file version, is it actually an egg? ({input})"));
        return 1;
    };
    let mut egg = match upgrade(&input, &unparsed) {
        Ok(egg) => egg,
        Err(message) => {
            io.error(&message);
            return 1;
        }
    };

    let output = replace_extension(&input);
    if output == input && egg_format::export::to_yaml(&Value::Object(egg.clone())) == unparsed {
        io.info(" -> no changes required");
        return 0;
    }
    egg.insert("exported_at".into(), Value::String(pelican_core::time::atom(&pelican_core::time::now())));

    io.info(&format!(" -> exporting to {output}"));
    let yaml = egg_format::export::to_yaml(&Value::Object(egg));
    if std::fs::write(&output, yaml).is_err() {
        io.error(&format!(" -> failed to write output file: {output}"));
        return 1;
    }

    if a.delete_original && output != input {
        io.info(" -> deleting input file as requested");
        if std::fs::remove_file(&input).is_err() {
            io.warn(&format!(" -> failed to delete original file: {input}"));
        }
    }
    0
}

/// `$importer->parse()` plus the checks and upgrades done before export; the error is
/// the line the original prints.
fn upgrade(input: &str, unparsed: &str) -> Result<Map<String, Value>, String> {
    let parsed = match egg_format::import::parse(unparsed) {
        Ok(v) => v,
        // InvalidFileUploadException.
        Err(ImportError::Parse(_) | ImportError::UnknownVersion) => {
            return Err(format!(" -> unsupported file version, is it actually an egg? ({input})"));
        }
        // Any other exception is not caught by the command.
        Err(e) => return Err(e.to_string()),
    };
    let Value::Object(mut egg) = parsed else {
        return Err(format!(" -> unsupported file version, is it actually an egg? ({input})"));
    };
    if !matches!(egg.get("meta"), Some(Value::Object(_) | Value::Array(_))) {
        return Err(format!(
            " -> does not contain existing meta or meta is not array, is it actually an egg? ({input})"
        ));
    }
    if !egg.contains_key("exported_at") {
        return Err(format!(" -> does not contain existing exported_at, is it actually an egg? ({input})"));
    }
    let update_url = match egg.get("meta").and_then(|m| m.get("update_url")) {
        Some(Value::String(url)) => url.clone(),
        _ => {
            return Err(format!(
                " -> does not contain existing meta.update_url or is not a string, is it actually an egg? ({input})"
            ));
        }
    };
    if let Some(Value::Object(meta)) = egg.get_mut("meta") {
        // The import upgraded the array, so it is the latest version now (Egg::EXPORT_VERSION).
        meta.insert("version".into(), Value::String("PLCN_v3".into()));
        meta.insert("update_url".into(), Value::String(replace_extension(&update_url)));
    }
    fix_variable_rules(&mut egg);
    Ok(egg)
}

/// `NormalizeEggCommand::fixVariableRules()`: no `field_type`, `rules` as a list.
fn fix_variable_rules(egg: &mut Map<String, Value>) {
    let vars: Vec<&mut Value> = match egg.get_mut("variables") {
        Some(Value::Array(list)) => list.iter_mut().collect(),
        Some(Value::Object(map)) => map.values_mut().collect(),
        _ => return,
    };
    for var in vars {
        let Value::Object(var) = var else { continue };
        var.shift_remove("field_type");
        if let Some(rules) = var.get_mut("rules")
            && !rules.is_array()
            && !rules.is_object()
        {
            let joined = pelican_core::php::string(rules);
            *rules = Value::Array(joined.split('|').map(|r| Value::String(r.to_string())).collect());
        }
    }
}

/// `preg_replace('/^(.*\.)(?:yml|json|yaml)$/', '$1yaml', $path)` (case-sensitive; `$` also
/// matches before a final newline).
fn replace_extension(path: &str) -> String {
    let (body, newline) = match path.strip_suffix('\n') {
        Some(b) => (b, "\n"),
        None => (path, ""),
    };
    if body.contains('\n') {
        return path.to_string();
    }
    for ext in ["yml", "json", "yaml"] {
        if let Some(stem) = body.strip_suffix(ext).filter(|s| s.ends_with('.')) {
            return format!("{stem}yaml{newline}");
        }
    }
    path.to_string()
}

/// `pathinfo($path, PATHINFO_EXTENSION)`: after the last dot of the base name.
fn php_extension(path: &str) -> &str {
    let base = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    base.rfind('.').map(|i| &base[i + 1..]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests/Unit/Console/Commands/Egg/egg-ptdlv2.json of the original.
    const PTDL_V2: &str = r##"{
    "_comment": "DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL",
    "meta": {
        "version": "PTDL_v2",
        "update_url": "https://raw.githubusercontent.com/pelican/panel/refs/heads/main/tests/Unit/Console/Command/Egg/egg-ptdlv2.json"
    },
    "exported_at": "2026-06-01T01:02:03+00:00",
    "name": "Egg Example",
    "author": "example@example.com",
    "uuid": "21f1c702-953a-45ca-ae58-c205e8a4ea7e",
    "description": "Egg for test purposes",
    "tags": [
        "test"
    ],
    "features": [
        "eula",
        "java_version",
        "pid_limit"
    ],
    "docker_images": {
        "ghcr.io\/pelican-eggs\/yolks:java_8": "ghcr.io\/pelican-eggs\/yolks:java_8",
        "ghcr.io\/pelican-eggs\/yolks:java_11": "ghcr.io\/pelican-eggs\/yolks:java_11",
        "ghcr.io\/pelican-eggs\/yolks:java_16": "ghcr.io\/pelican-eggs\/yolks:java_16",
        "ghcr.io\/pelican-eggs\/yolks:java_17": "ghcr.io\/pelican-eggs\/yolks:java_17"
    },
    "file_denylist": [],
    "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar minecraft.jar",
    "config": {
        "files": "{\r\n    \"server.properties\": {\r\n        \"parser\": \"properties\",\r\n        \"find\": {\r\n            \"server-ip\": \"0.0.0.0\",\r\n            \"server-port\": \"{{server.build.default.port}}\"\r\n        }\r\n    }\r\n}",
        "startup": "{\r\n    \"done\": \")! For help, type \"\r\n}",
        "logs": "{}",
        "stop": "stop"
    },
    "scripts": {
        "installation": {
            "script": "#!\/bin\/ash\r\necho test some escaped backslashes (\/), then proper backslashes (/)",
            "container": "ghcr.io\/pelican-eggs\/installers:alpine",
            "entrypoint": "ash"
        }
    },
    "variables": [
        {
            "name": "Modpack Version",
            "description": "",
            "env_variable": "MODPACK_VERSION",
            "default_value": "1.0.0",
            "user_viewable": true,
            "user_editable": true,
            "rules": "required|string|max:20",
            "sort": null,
            "field_type": "text"
        }
    ]
}"##;

    // tests/Unit/Console/Commands/Egg/egg-plcnv3-example.yaml of the original.
    const PLCN_V3: &str = r#"_comment: 'DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL'
meta:
  version: PLCN_v3
  update_url: 'https://raw.githubusercontent.com/pelican/panel/refs/heads/main/tests/Unit/Console/Command/Egg/egg-ptdlv2.yaml'
exported_at: '2026-06-01T01:02:03+00:00'
name: 'Egg Example'
author: example@example.com
uuid: 21f1c702-953a-45ca-ae58-c205e8a4ea7e
description: 'Egg for test purposes'
tags:
  - test
features:
  - eula
  - java_version
  - pid_limit
docker_images:
  'ghcr.io/pelican-eggs/yolks:java_8': 'ghcr.io/pelican-eggs/yolks:java_8'
  'ghcr.io/pelican-eggs/yolks:java_11': 'ghcr.io/pelican-eggs/yolks:java_11'
  'ghcr.io/pelican-eggs/yolks:java_16': 'ghcr.io/pelican-eggs/yolks:java_16'
  'ghcr.io/pelican-eggs/yolks:java_17': 'ghcr.io/pelican-eggs/yolks:java_17'
file_denylist: {  }
config:
  files:
    server.properties:
      parser: properties
      find:
        server-ip: 0.0.0.0
        server-port: '{{server.allocations.default.port}}'
  startup:
    done: ')! For help, type '
  logs: {  }
  stop: stop
scripts:
  installation:
    script: |-
      #!/bin/ash
      echo test some escaped backslashes (/), then proper backslashes (/)
    container: 'ghcr.io/pelican-eggs/installers:alpine'
    entrypoint: ash
variables:
  -
    name: 'Modpack Version'
    description: ''
    env_variable: MODPACK_VERSION
    default_value: 1.0.0
    user_viewable: true
    user_editable: true
    rules:
      - required
      - string
      - 'max:20'
    sort: null
startup_commands:
  Default: 'java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar minecraft.jar'
"#;

    /// NormalizeEggCommandTest::test_upgrade_ptdlv2_to_plcnv3 (now = 2026-06-01 01:02:03).
    #[test]
    fn upgrades_ptdl_v2_json() {
        let mut egg = upgrade("/tmp/egg-ptdlv2.json", PTDL_V2).expect("upgrade");
        egg.insert("exported_at".into(), Value::String("2026-06-01T01:02:03+00:00".into()));
        assert_eq!(egg_format::export::to_yaml(&Value::Object(egg)), PLCN_V3);
    }

    /// NormalizeEggCommandTest::test_idempotency: the normalized file needs no changes.
    #[test]
    fn normalized_yaml_is_unchanged() {
        let egg = upgrade("/tmp/egg-ptdlv2.yaml", PLCN_V3).expect("upgrade");
        assert_eq!(egg_format::export::to_yaml(&Value::Object(egg)), PLCN_V3);
    }

    #[test]
    fn rejects_non_eggs() {
        let err = upgrade("a.yaml", "foo: bar").expect_err("not an egg");
        assert_eq!(err, " -> unsupported file version, is it actually an egg? (a.yaml)");
        let err = upgrade("a.yaml", "meta:\n  version: PLCN_v3\n").expect_err("no exported_at");
        assert_eq!(err, " -> does not contain existing exported_at, is it actually an egg? (a.yaml)");
        let err = upgrade("a.yaml", "meta:\n  version: PLCN_v3\n  update_url: null\nexported_at: x\n")
            .expect_err("null update_url");
        assert!(err.starts_with(" -> does not contain existing meta.update_url or is not a string"));
    }

    #[test]
    fn paths() {
        assert_eq!(replace_extension("dir/egg.json"), "dir/egg.yaml");
        assert_eq!(replace_extension("egg.yml"), "egg.yaml");
        assert_eq!(replace_extension("egg.yaml"), "egg.yaml");
        assert_eq!(replace_extension("egg.JSON"), "egg.JSON");
        assert_eq!(replace_extension("eggjson"), "eggjson");
        assert_eq!(php_extension("a/b.c/egg.YML"), "YML");
        assert_eq!(php_extension("a.d/egg"), "");
        assert_eq!(php_extension(".json"), "json");
    }
}
