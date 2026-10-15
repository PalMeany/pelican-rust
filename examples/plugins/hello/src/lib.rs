//! Example pelican-rs plugin: counts `server:power.start` events, shows the count on an
//! admin page and a server page, offers `GET /api/client/plugins/hello/count`, a
//! settings form (greeting) and a `hello-log` schedule task.

use pelican_plugin_sdk::{
    AdminPermission, AvatarProvider, Field, NavItem, Page, Panel, PelicanPlugin, PermissionGroup, Registration,
    Request, Response, Route, RouteScope, TaskType, Value, escape, host, json,
};

struct Hello;

fn count() -> u64 {
    host::kv_get("starts").and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn greeting() -> String {
    host::kv_get("greeting").unwrap_or_else(|| "Hello".into())
}

impl PelicanPlugin for Hello {
    fn register() -> Registration {
        Registration {
            events: vec!["server:power.start".into()],
            routes: vec![
                Route { scope: RouteScope::Client, method: "GET".into(), path: "/count".into() },
                Route { scope: RouteScope::Web, method: "GET".into(), path: "/status".into() },
            ],
            pages: vec![
                Page {
                    panel: Panel::Admin,
                    id: "dashboard".into(),
                    title: "hello::hello.title".into(),
                    permission: None,
                },
                Page {
                    panel: Panel::Server,
                    id: "info".into(),
                    title: "hello::hello.title".into(),
                    permission: Some("hello.view".into()),
                },
            ],
            navigation: vec![
                NavItem {
                    panel: Panel::Admin,
                    page: "dashboard".into(),
                    label: "hello::hello.nav".into(),
                    icon: Some("puzzle".into()),
                    group: Some("Advanced".into()),
                    sort: Some(100),
                },
                NavItem {
                    panel: Panel::Server,
                    page: "info".into(),
                    label: "hello::hello.nav".into(),
                    icon: Some("puzzle".into()),
                    group: None,
                    sort: None,
                },
            ],
            tasks: vec![TaskType {
                id: "hello-log".into(),
                name: "Log a greeting".into(),
                payload_label: Some("Message".into()),
                default_payload: None,
            }],
            avatars: vec![AvatarProvider { id: "robohash".into(), name: "RoboHash".into() }],
            admin_permissions: vec![AdminPermission {
                model: "helloGreeting".into(),
                prefixes: vec!["view".into(), "update".into()],
            }],
            subuser_permissions: vec![PermissionGroup {
                name: "hello".into(),
                permissions: vec!["view".into(), "count".into()],
                icon: Some("packages".into()),
            }],
            api_resources: vec!["hello".into()],
            settings: true,
            ..Default::default()
        }
    }

    fn install() -> Result<(), String> {
        host::kv_set("starts", Some("0"));
        Ok(())
    }

    fn on_event(event: String, payload: Value) {
        let n = count() + 1;
        host::kv_set("starts", Some(&n.to_string()));
        pelican_plugin_sdk::info(&format!("{event} #{n}: {}", payload["properties"]));
    }

    fn handle(req: Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/count") => {
                Response::json(200, &json!({"starts": count(), "server": req.server().map(|s| s["uuid"].clone())}))
            }
            ("GET", "/status") => Response::text(
                200,
                &format!(
                    "{}, {}!",
                    greeting(),
                    req.user().map(|u| u["username"].as_str().unwrap_or("").to_string()).unwrap_or_default()
                ),
            ),
            _ => Response::not_found(),
        }
    }

    fn render_page(panel: Panel, page: String, req: Request) -> Result<String, String> {
        let text = host::trans("hello::hello.starts", &[("count".into(), count().to_string())]);
        let who = req.user().and_then(|u| u["username"].as_str().map(str::to_string)).unwrap_or_default();
        match (panel, page.as_str()) {
            (Panel::Admin, "dashboard") => Ok(format!(
                "<section class=\"card card-body stack\"><p>{}, {}!</p><p class=\"muted\">{}</p></section>",
                escape(&greeting()),
                escape(&who),
                escape(&text)
            )),
            (Panel::Server, "info") => {
                let name = req.server().and_then(|s| s["name"].as_str().map(str::to_string)).unwrap_or_default();
                Ok(format!(
                    "<section class=\"card card-body\"><p>{}</p><p class=\"muted\">{}</p></section>",
                    escape(&name),
                    escape(&text)
                ))
            }
            _ => Err("not found".into()),
        }
    }

    fn settings_form() -> Vec<Field> {
        vec![Field {
            name: "greeting".into(),
            label: "Greeting".into(),
            kind: "text".into(),
            options: vec![],
            required: true,
            help: Some("Shown on the plugin page.".into()),
        }]
    }

    fn settings_data() -> Value {
        json!({"greeting": greeting()})
    }

    fn save_settings(data: Value) -> Result<(), String> {
        let g = data["greeting"].as_str().unwrap_or("").trim().to_string();
        if g.is_empty() {
            return Err("The greeting may not be empty.".into());
        }
        host::kv_set("greeting", Some(&g));
        Ok(())
    }

    fn avatar_url(provider: String, user: Value) -> Option<String> {
        (provider == "robohash").then(|| Hello::robohash(&user)).flatten()
    }

    fn run_task(task: String, payload: String, server: Value) -> Result<(), String> {
        if task != "hello-log" {
            return Err("unknown task".into());
        }
        pelican_plugin_sdk::info(&format!("{} ({}): {payload}", greeting(), server["name"].as_str().unwrap_or("?")));
        Ok(())
    }
}

impl Hello {
    fn robohash(user: &Value) -> Option<String> {
        user["uuid"].as_str().map(|id| format!("https://robohash.org/{id}?size=128x128"))
    }
}

pelican_plugin_sdk::plugin!(Hello);
