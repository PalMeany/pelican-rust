//! Write pelican-rs plugins in Rust (ADR-0018). Implement [`PelicanPlugin`] (every hook
//! but `register` has a default) and export it with [`plugin!`]; build with
//! `cargo build --release --target wasm32-wasip2`.
//!
//! ```ignore
//! struct Hello;
//! impl pelican_plugin_sdk::PelicanPlugin for Hello {
//!     fn register() -> pelican_plugin_sdk::Registration { Default::default() }
//! }
//! pelican_plugin_sdk::plugin!(Hello);
//! ```

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../pelican-plugins/wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "__export_world",
        default_bindings_module: "pelican_plugin_sdk::bindings",
    });
}

pub use bindings::pelican::plugin::host;
pub use bindings::pelican::plugin::types::{
    AdminPermission, AvatarProvider, Feature, Field, Header, Level, NavItem, Page, Panel, PermissionGroup,
    Registration, Request, Response, Route, RouteScope, TaskType,
};
pub use serde_json::{self, Value, json};

impl Default for Registration {
    fn default() -> Self {
        Registration {
            events: Vec::new(),
            routes: Vec::new(),
            pages: Vec::new(),
            navigation: Vec::new(),
            tasks: Vec::new(),
            features: Vec::new(),
            avatars: Vec::new(),
            admin_permissions: Vec::new(),
            subuser_permissions: Vec::new(),
            api_resources: Vec::new(),
            settings: false,
        }
    }
}

impl Response {
    /// A response with a body and content type.
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            headers: vec![Header { name: "content-type".into(), value: content_type.into() }],
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: &Value) -> Response {
        Response::new(status, "application/json", value.to_string())
    }

    pub fn text(status: u16, text: &str) -> Response {
        Response::new(status, "text/plain; charset=utf-8", text)
    }

    pub fn not_found() -> Response {
        Response::json(
            404,
            &json!({"errors": [{"code": "NotFoundHttpException", "status": "404", "detail": "The requested resource could not be found on the server."}]}),
        )
    }
}

impl Request {
    /// The request body as JSON (`Null` when empty or invalid).
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The signed-in user (`user` attributes).
    pub fn user(&self) -> Option<Value> {
        self.user.as_deref().and_then(|u| serde_json::from_str(u).ok())
    }

    /// The bound server (`server` attributes).
    pub fn server(&self) -> Option<Value> {
        self.server.as_deref().and_then(|s| serde_json::from_str(s).ok())
    }
}

/// `log` shorthands.
pub fn info(message: &str) {
    host::log(Level::Info, message);
}

pub fn warn(message: &str) {
    host::log(Level::Warn, message);
}

/// Escape text for the HTML fragments returned by `render_page`.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// The hooks of a plugin; only `register` is required.
pub trait PelicanPlugin {
    fn register() -> Registration;
    fn install() -> Result<(), String> {
        Ok(())
    }
    fn uninstall() -> Result<(), String> {
        Ok(())
    }
    fn on_event(_event: String, _payload: Value) {}
    fn handle(_req: Request) -> Response {
        Response::not_found()
    }
    fn render_page(_panel: Panel, _page: String, _req: Request) -> Result<String, String> {
        Err("page not found".into())
    }
    fn settings_form() -> Vec<Field> {
        Vec::new()
    }
    fn settings_data() -> Value {
        json!({})
    }
    fn save_settings(_data: Value) -> Result<(), String> {
        Ok(())
    }
    fn run_task(_task: String, _payload: String, _server: Value) -> Result<(), String> {
        Err("unknown task".into())
    }
    fn avatar_url(_provider: String, _user: Value) -> Option<String> {
        None
    }
}

/// Export a [`PelicanPlugin`] as the component's `plugin` world.
#[macro_export]
macro_rules! plugin {
    ($t:ty) => {
        struct __PelicanAdapter;
        impl $crate::bindings::Guest for __PelicanAdapter {
            fn register() -> $crate::Registration {
                <$t as $crate::PelicanPlugin>::register()
            }
            fn install() -> Result<(), String> {
                <$t as $crate::PelicanPlugin>::install()
            }
            fn uninstall() -> Result<(), String> {
                <$t as $crate::PelicanPlugin>::uninstall()
            }
            fn on_event(event: String, payload: String) {
                <$t as $crate::PelicanPlugin>::on_event(event, $crate::serde_json::from_str(&payload).unwrap_or($crate::Value::Null))
            }
            fn handle(req: $crate::Request) -> $crate::Response {
                <$t as $crate::PelicanPlugin>::handle(req)
            }
            fn render_page(panel: $crate::Panel, page: String, req: $crate::Request) -> Result<String, String> {
                <$t as $crate::PelicanPlugin>::render_page(panel, page, req)
            }
            fn settings_form() -> Vec<$crate::Field> {
                <$t as $crate::PelicanPlugin>::settings_form()
            }
            fn settings_data() -> String {
                <$t as $crate::PelicanPlugin>::settings_data().to_string()
            }
            fn save_settings(data: String) -> Result<(), String> {
                <$t as $crate::PelicanPlugin>::save_settings($crate::serde_json::from_str(&data).unwrap_or($crate::Value::Null))
            }
            fn run_task(task: String, payload: String, server: String) -> Result<(), String> {
                <$t as $crate::PelicanPlugin>::run_task(task, payload, $crate::serde_json::from_str(&server).unwrap_or($crate::Value::Null))
            }
            fn avatar_url(provider: String, user: String) -> Option<String> {
                <$t as $crate::PelicanPlugin>::avatar_url(provider, $crate::serde_json::from_str(&user).unwrap_or($crate::Value::Null))
            }
        }
        $crate::bindings::__export_world!(__PelicanAdapter with_types_in $crate::bindings);
    };
}
