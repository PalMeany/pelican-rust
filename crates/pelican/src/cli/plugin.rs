//! `p:plugin:*` (`app/Console/Commands/Plugin`): the plugin operations of the Application
//! API (`pelican_api::application::plugins`) from the command line. PHP plugins are handed
//! to the sidecar (ADR-0020); WASM plugins' install/uninstall hooks run in a runtime of this
//! process. A running panel picks the change up within seconds (`plugins::watch`), as the
//! original re-reads its plugins on every request.

use clap::{Args, Subcommand};
use pelican_api::application::plugins as app_plugins;
use pelican_core::state::AppState;
use pelican_plugins::{Config, Plugin, Status, lifecycle, updates};
use serde_json::{Value, json};

use super::Ctx;
use crate::console::{Console, t};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List all installed plugins
    #[command(name = "p:plugin:list")]
    List,
    /// Installs a plugin
    #[command(name = "p:plugin:install")]
    Install(IdArgs),
    /// Updates a plugin
    #[command(name = "p:plugin:update")]
    Update(IdArgs),
    /// Uninstalls a plugin
    #[command(name = "p:plugin:uninstall")]
    Uninstall(UninstallArgs),
    /// Disables a plugin
    #[command(name = "p:plugin:disable")]
    Disable(IdArgs),
    /// Makes sure the needed composer packages for all installed plugins are available.
    #[command(name = "p:plugin:composer")]
    Composer,
    /// Create a new plugin
    #[command(name = "p:plugin:make")]
    Make(MakeArgs),
}

#[derive(Args, Debug)]
pub struct IdArgs {
    id: Option<String>,
}

#[derive(Args, Debug)]
pub struct UninstallArgs {
    id: Option<String>,
    /// Delete the plugin files
    #[arg(long)]
    delete: bool,
}

#[derive(Args, Debug)]
pub struct MakeArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    author: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(long)]
    category: Option<String>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long = "updateUrl")]
    update_url: Option<String>,
    #[arg(long)]
    panels: Option<String>,
    #[arg(long = "panelVersion")]
    panel_version: Option<String>,
}

pub async fn run(ctx: &Ctx, io: &Console, cmd: Command) -> anyhow::Result<i32> {
    if let Command::Make(a) = cmd {
        let cfg = Config::from_env(
            &ctx.env,
            &ctx.path,
            pelican_api::versions::current_comparable_version(),
            pelican_api::versions::current_panel_version(),
        );
        return make(&cfg, io, a);
    }
    let st = ctx.state().await?;
    let cfg = app_plugins::config(&st);
    // `PluginService::loadPlugins()` runs on every artisan boot.
    cfg.refresh_statuses();
    match cmd {
        Command::List => list(&cfg, io),
        Command::Install(a) => install(&st, &cfg, io, a.id).await,
        Command::Update(a) => update(&st, &cfg, io, a.id).await,
        Command::Uninstall(a) => uninstall(&st, &cfg, io, a).await,
        Command::Disable(a) => disable(&st, &cfg, io, a.id).await,
        Command::Composer => composer(&st, io).await,
        Command::Make(_) => {}
    }
    // The original's handle() methods return void: exit code 0 even after an error line.
    Ok(0)
}

/// ListPluginsCommand: `Plugin::get([name, author, status, version, panels, category])`.
fn list(cfg: &Config, io: &Console) {
    let plugins = cfg.plugins();
    if plugins.is_empty() {
        io.warn("No plugins installed");
        return;
    }
    let rows: Vec<Vec<String>> = plugins
        .iter()
        .map(|p| {
            vec![
                p.name.clone(),
                p.author.clone(),
                p.status.as_str().to_string(),
                p.version.clone().unwrap_or_default(),
                p.panels.clone().unwrap_or_default(),
                p.category.as_str().to_string(),
            ]
        })
        .collect();
    io.table(&["Name", "Author", "Status", "Version", "Panels", "Category"], &rows);
    io.line("");
}

/// `$this->argument('id') ?? $this->choice('Plugin', Plugin::pluck('name', 'id'))`, then
/// `Plugin::find(lower($id))`.
fn pick(cfg: &Config, io: &Console, id: Option<String>) -> Option<Plugin> {
    let plugins = cfg.plugins();
    let id = id.or_else(|| {
        let choices: Vec<(&str, &str)> = plugins.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
        io.choice_assoc("Plugin", &choices, None)
    });
    let found = id.and_then(|id| cfg.find(&id).ok());
    if found.is_none() {
        io.error("Plugin does not exist!");
    }
    found
}

/// The WASM plugin's install/uninstall hook in this process (the runtime is created on
/// first use; PHP plugins run theirs in the sidecar).
async fn hook(st: &AppState, cfg: &Config, p: &Plugin, install: bool) -> Result<(), String> {
    if p.wasm.is_some() && pelican_plugins::runtime::global().is_none() {
        let rt = crate::plugins::runtime(&pelican_core::state::Live::new(st.clone()))?;
        pelican_plugins::runtime::set_global(rt);
    }
    app_plugins::lifecycle_hook(cfg, p, install).await
}

/// InstallPluginCommand (`PluginService::installPlugin($plugin)`).
async fn install(st: &AppState, cfg: &Config, io: &Console, id: Option<String>) {
    let Some(p) = pick(cfg, io, id) else { return };
    if p.status != Status::NotInstalled {
        io.error("Plugin is already installed!");
        return;
    }
    let result = match pelican_api::sidecar::delegate(st, &p, "install", json!({ "enable": true })).await {
        Some(r) => r,
        None => match hook(st, cfg, &p, true).await {
            Ok(()) => lifecycle::install(cfg, &p, true).map_err(|e| e.to_string()),
            Err(e) => Err(e),
        },
    };
    match result {
        Ok(()) => io.info("Plugin installed and enabled."),
        Err(e) => io.error(&format!("Could not install plugin: {e}")),
    }
}

/// UpdatePluginCommand (`PluginService::updatePlugin($plugin)`).
async fn update(st: &AppState, cfg: &Config, io: &Console, id: Option<String>) {
    let Some(p) = pick(cfg, io, id) else { return };
    if !updates::update_available(cfg, &p).await {
        io.error("Plugin doesn't need updating!");
        return;
    }
    let result = match pelican_api::sidecar::delegate(st, &p, "update", json!({})).await {
        Some(r) => r.map(|()| updates::forget(&p.id)),
        None => match lifecycle::update(cfg, &p).await {
            Ok(fresh) if fresh.status != Status::NotInstalled => hook(st, cfg, &fresh, true).await,
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    };
    match result {
        Ok(()) => io.info("Plugin updated."),
        Err(e) => io.error(&format!("Could not update plugin: {e}")),
    }
}

/// UninstallPluginCommand (`PluginService::uninstallPlugin($plugin, $deleteFiles)`).
async fn uninstall(st: &AppState, cfg: &Config, io: &Console, a: UninstallArgs) {
    let Some(p) = pick(cfg, io, a.id) else { return };
    if p.status == Status::NotInstalled {
        io.error("Plugin is not installed!");
        return;
    }
    let mut delete = a.delete;
    if io.interactive() && !delete {
        delete = io.confirm("Do you also want to delete the plugin files?", false);
    }
    let result = match pelican_api::sidecar::delegate(st, &p, "uninstall", json!({ "delete": false })).await {
        // The sidecar sees the plugin through a link: files are deleted here.
        Some(Ok(())) if delete => lifecycle::delete(cfg, &p).map_err(|e| e.to_string()),
        Some(r) => r,
        None => match hook(st, cfg, &p, false).await {
            Ok(()) => lifecycle::uninstall(cfg, &p, delete).map_err(|e| e.to_string()),
            Err(e) => Err(e),
        },
    };
    match result {
        Ok(()) => io.info(&format!("Plugin uninstalled{}.", if delete { " and files deleted" } else { "" })),
        Err(e) => io.error(&format!("Could not uninstall plugin: {e}")),
    }
}

/// DisablePluginCommand (`canDisable()`, `PluginService::disablePlugin($plugin)`).
async fn disable(st: &AppState, cfg: &Config, io: &Console, id: Option<String>) {
    let Some(p) = pick(cfg, io, id) else { return };
    if !matches!(p.status, Status::Enabled | Status::Incompatible) {
        io.error("Plugin can't be disabled!");
        return;
    }
    let result = match pelican_api::sidecar::delegate(st, &p, "disable", json!({})).await {
        Some(r) => r,
        None => lifecycle::disable(cfg, &p).map_err(|e| e.to_string()),
    };
    match result {
        Ok(()) => io.info("Plugin disabled."),
        // The original lets the exception escape.
        Err(e) => io.error(&e),
    }
}

/// ComposerPluginsCommand (`PluginService::manageComposerPackages()`): composer packages
/// only exist for PHP plugins, so this runs in the sidecar.
async fn composer(st: &AppState, io: &Console) {
    match pelican_api::sidecar::composer(st).await {
        Some(Ok(())) => {}
        Some(Err(e)) => io.error(&e),
        None => {
            io.warn("Composer packages are only used by PHP plugins, which need the PHP sidecar (PANEL_PHP_SIDECAR).")
        }
    }
}

/// `Str::ascii()` then `preg_replace('/[^A-Za-z0-9 ]/', '', ...)`.
fn clean(s: &str) -> String {
    deunicode::deunicode(s).chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect()
}

/// `Str::slug()` of an already cleaned name (`[A-Za-z0-9 ]`).
fn slug(s: &str) -> String {
    s.split(' ').filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>().join("-")
}

/// `Str::studly()`: words split on spaces, `-` and `_`, each with an upper-case first letter.
fn studly(s: &str) -> String {
    s.split([' ', '-', '_'])
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect()
}

/// MakePluginCommand: a PHP plugin skeleton (runs in the sidecar once linked). The
/// original remembers the last author in its cache; pelican-rs has no shared cache for that.
fn make(cfg: &Config, io: &Console, a: MakeArgs) -> anyhow::Result<i32> {
    let name = clean(&a.name.or_else(|| io.ask("Name", None)).unwrap_or_default());
    let id = slug(&name);
    if cfg.root.join(&id).exists() {
        io.error("Plugin with that name already exists!");
        return Ok(0);
    }

    let author = clean(&a.author.or_else(|| io.ask("Author", None)).unwrap_or_default());
    let namespace = format!("{}\\{}", studly(&author), studly(&name));
    let class = studly(&format!("{name}Plugin"));
    if cfg.plugins().iter().any(|p| p.namespace == namespace && p.class == class) {
        io.error("Plugin class with that name already exists!");
        return Ok(0);
    }

    io.info(&format!("Creating Plugin \"{name}\" ({id}) by {author}"));

    let description = a.description.or_else(|| io.ask("Description (can be empty)", None));

    let labels: Vec<(&str, String)> = ["plugin", "theme", "language"]
        .into_iter()
        .map(|c| (c, t(&format!("admin/plugin.category_enum.{c}"), &[])))
        .collect();
    let choices: Vec<(&str, &str)> = labels.iter().map(|(k, l)| (*k, l.as_str())).collect();
    let category = a.category.or_else(|| io.choice_assoc("Category", &choices, Some("plugin"))).unwrap_or_default();
    if !["plugin", "theme", "language"].contains(&category.as_str()) {
        io.error("Unknown plugin category!");
        return Ok(0);
    }

    let url = a.url.or_else(|| io.ask("URL (can be empty)", None));
    let update_url = a.update_url.or_else(|| io.ask("Update URL (can be empty)", None));

    let panels: Option<Vec<String>> = match a.panels.filter(|p| !p.is_empty() && p != "0") {
        Some(p) => Some(p.split(',').map(str::to_string).collect()),
        None if io.confirm("Should the plugin be available on all panels?", true) => None,
        None => io.choice_assoc_multiple(
            "Panels (comma separated list)",
            &[("admin", "Admin Area"), ("server", "Client Area"), ("app", "Server List")],
            None,
        ),
    };

    let mut panel_version = a.panel_version.filter(|v| !v.is_empty() && v != "0");
    if panel_version.is_none() {
        panel_version = io
            .ask(
                "Required panel version (leave empty for no constraint)",
                pelican_api::versions::current_comparable_version(),
            )
            .filter(|v| !v.is_empty() && v != "0");
        if let Some(v) = panel_version.clone() {
            let q = format!(
                "Should the version constraint be minimal instead of strict? ({v} or higher instead of only {v})"
            );
            if io.confirm(&q, false) {
                panel_version = Some(format!("^{v}"));
            }
        }
    }

    let manifest = json!({
        "id": id,
        "name": name,
        "author": author,
        "version": "1.0.0",
        "description": description,
        "category": category,
        "url": url,
        "update_url": update_url,
        "namespace": namespace,
        "class": class,
        "panels": panels,
        "panel_version": panel_version,
        "api_version": pelican_plugins::SUPPORTED_API_VERSION,
        "composer_packages": Value::Null,
        "meta": { "status": Status::Enabled.as_str(), "status_message": Value::Null },
    });
    let body =
        pelican_api::application::nodes::pretty_json(&manifest).map_err(|e| anyhow::anyhow!(super::api_error(&e)))?;

    let dir = cfg.root.join(&id);
    std::fs::create_dir_all(dir.join("src").join("Providers"))?;
    std::fs::create_dir_all(dir.join("config"))?;
    std::fs::write(dir.join("plugin.json"), body)?;
    let fill = |stub: &str| {
        stub.replace("$namespace$", &namespace).replace("$class$", &class).replace("$id$", &id).replace("$name$", &name)
    };
    std::fs::write(dir.join("src").join(format!("{class}.php")), fill(include_str!("stubs/Plugin.stub")))?;
    std::fs::write(
        dir.join("src").join("Providers").join(format!("{class}Provider.php")),
        fill(include_str!("stubs/PluginProvider.stub")),
    )?;
    std::fs::write(dir.join("config").join(format!("{id}.php")), fill(include_str!("stubs/PluginConfig.stub")))?;

    io.info("Plugin created.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn laravel_str_helpers() {
        assert_eq!(super::clean("Héllo, World!"), "Hello World");
        assert_eq!(super::slug("My  Cool Plugin"), "my-cool-plugin");
        assert_eq!(super::studly("my cool_plugin-x"), "MyCoolPluginX");
        assert_eq!(super::studly("Hello WorldPlugin"), "HelloWorldPlugin");
    }
}
