//! The example plugin (`scripts/build-plugin examples/plugins/hello`) through the runtime.

use std::path::PathBuf;
use std::sync::Arc;

use pelican_plugins::runtime::{BoxFuture, Panel, Request, Runtime, Services};
use pelican_plugins::{Config, Status};
use serde_json::{Map, Value};

struct Fake;

impl Services for Fake {
    fn trans(&self, _plugin: &str, _locale: &str, key: &str, replace: &[(String, String)]) -> String {
        format!("{key}{replace:?}")
    }
    fn activity<'a>(&'a self, _plugin: &'a str, _event: &'a str, _properties: Value) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn api<'a>(
        &'a self,
        _plugin: &'a str,
        _grants: &'a Map<String, Value>,
        _method: &'a str,
        _path: &'a str,
        _body: Option<String>,
    ) -> BoxFuture<'a, Result<(u16, String), String>> {
        Box::pin(async { Err("no".into()) })
    }
}

fn request(path: &str) -> Request {
    Request {
        method: "GET".into(),
        path: path.into(),
        query: String::new(),
        headers: vec![],
        body: vec![],
        locale: "en".into(),
        user: Some(r#"{"username":"admin"}"#.into()),
        server: Some(r#"{"uuid":"abc","name":"Box"}"#.into()),
    }
}

#[tokio::test]
async fn hello_plugin_hooks() {
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/hello");
    if !example.join("hello.wasm").exists() {
        eprintln!("skipped: run scripts/build-plugin examples/plugins/hello first");
        return;
    }
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("plugins");
    std::fs::create_dir_all(root.join("hello")).expect("mkdir");
    for f in ["plugin.json", "hello.wasm"] {
        std::fs::copy(example.join(f), root.join("hello").join(f)).expect("copy");
    }
    let cfg = Config {
        root,
        dev_mode: false,
        safe_mode: false,
        max_import_size: 1 << 20,
        hub_url: "https://hub.pelican.dev".into(),
        hub_api_key: None,
        panel_version: None,
        panel_version_display: "canary".into(),
        production: true,
        translate: Arc::new(pelican_plugins::english),
    };
    let rt = Runtime::new(Arc::new(Fake), tmp.path().join("storage")).expect("runtime");

    // Not installed: nothing loads; install runs the plugin's install hook.
    rt.load_all(&cfg).await;
    assert!(rt.get("hello").is_none());
    let p = cfg.find("hello").expect("row");
    rt.lifecycle_hook(&cfg, &p, true).await.expect("install hook");
    pelican_plugins::lifecycle::install(&cfg, &p, true).expect("install");
    rt.load_all(&cfg).await;
    let loaded = rt.get("hello").expect("loaded");
    assert_eq!(loaded.registration.events, vec!["server:power.start".to_string()]);
    assert!(loaded.registration.settings);

    rt.dispatch("server:power.start", &serde_json::json!({"properties": {}}));
    rt.dispatch("server:power.stop", &serde_json::json!({}));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let r = rt.handle("hello", request("/count")).await.expect("call");
    assert_eq!(r.status, 200);
    assert_eq!(String::from_utf8_lossy(&r.body), r#"{"starts":1,"server":"abc"}"#);
    let r = rt.handle("hello", request("/nope")).await.expect("call");
    assert_eq!(r.status, 404);

    let html = rt.render_page("hello", Panel::Admin, "dashboard", request("/")).await.expect("call").expect("page");
    assert!(html.contains("Hello, admin!") && html.contains("hello::hello.starts"), "{html}");

    assert_eq!(rt.settings_form("hello").await.expect("form").len(), 1);
    assert_eq!(
        rt.save_settings("hello", r#"{"greeting":""}"#).await.expect("call"),
        Err("The greeting may not be empty.".into())
    );
    rt.save_settings("hello", r#"{"greeting":"Hi"}"#).await.expect("call").expect("saved");
    assert_eq!(rt.settings_data("hello").await.expect("data"), r#"{"greeting":"Hi"}"#);
    rt.run_task("hello", "hello-log", "x", r#"{"name":"Box"}"#).await.expect("call").expect("task");

    // The store survives a reload.
    rt.load_all(&cfg).await;
    let r = rt.handle("hello", request("/status")).await.expect("call");
    assert_eq!(String::from_utf8_lossy(&r.body), "Hi, admin!");
    assert_eq!(cfg.find("hello").expect("row").status, Status::Enabled);
}
