//! Admin › Servers › Create (Pages/CreateServer.php + ServerCreationService):
//! one page with the wizard's sections; node/egg changes re-render the form.

use std::collections::HashMap;

use axum::Router;
use axum::extract::RawQuery;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::{
    binding::Bound,
    model_events,
    validation::{CustomRule, Validator},
};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, eggs, nodes, server_variables, servers, users};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde_json::{Value, json};

use super::Admin;
use super::servers::{mount_options, variable_fields};
use super::users::FormData;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::server_info;
use crate::variables;

const TPL: &str = "admin/servers/create.html";
const TAKEN: &str = "The :attribute has already been taken.";
const CREATE_OPTION_LABEL: &str = "filament-forms::components.select.actions.create_option.label";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/servers/create", get(show).post(store))
        .route("/admin/servers/create/owner", post(store_owner))
        .route("/admin/servers/create/allocation", post(store_allocation))
}

fn options_of(v: Option<&Json>) -> Vec<(String, String)> {
    match v.map(|j| &j.0) {
        Some(Value::Object(m)) => {
            m.iter().map(|(k, v)| (v.as_str().unwrap_or_default().to_string(), k.clone())).collect()
        }
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(|s| (s.to_string(), s.to_string()))).collect(),
        _ => Vec::new(),
    }
}

async fn render(admin: &Admin, form: &Form, errors: &Errors) -> WebResult {
    let ctx = form_context(admin, form, errors).await?;
    admin
        .page(
            "servers",
            TPL,
            context! {
                title => admin.t_with(
                    "filament-panels::resources/pages/create-record.title",
                    &[("label", &admin.t("admin/server.model_label"))],
                ),
                ..ctx
            },
        )
        .await
}

/// Page/form state for the wizard and its htmx partials (GET refresh and the
/// create-option success responses all re-render `#create-form` from here).
async fn form_context(admin: &Admin, form: &Form, errors: &Errors) -> WebResult<Mj> {
    let st = &admin.st;
    let nodes: Vec<nodes::Model> = nodes::Entity::find()
        .order_by_desc(nodes::Column::Id)
        .all(&st.db)
        .await?
        .into_iter()
        .filter(|n| admin.user.can_target_node(n.id.0))
        .collect();
    // Default node: the last one used in this session, else the latest accessible.
    let last_used = admin.session.data().extra.get("last_utilized_node").and_then(Value::as_i64);
    let node_id = form
        .opt("node_id")
        .and_then(|v| v.parse::<i64>().ok())
        .or(last_used.filter(|id| nodes.iter().any(|n| n.id.0 == *id)))
        .or(nodes.first().map(|n| n.id.0));
    let eggs_all = eggs::Entity::find().order_by_asc(eggs::Column::Name).all(&st.db).await?;
    let egg_id = form.opt("egg_id").and_then(|v| v.parse::<i64>().ok());
    let egg = egg_id.and_then(|id| eggs_all.iter().find(|e| e.id.0 == id).cloned());
    let owners = users::Entity::find().order_by_asc(users::Column::Username).all(&st.db).await?;
    let free: Vec<allocations::Model> = match node_id {
        Some(n) => {
            allocations::Entity::find()
                .filter(allocations::Column::NodeId.eq(n))
                .filter(allocations::Column::ServerId.is_null())
                .order_by_asc(allocations::Column::Ip)
                .order_by_asc(allocations::Column::Port)
                .all(&st.db)
                .await?
        }
        None => Vec::new(),
    };
    let mut vars = match &egg {
        Some(e) => variable_fields(admin, None, e.id.0).await?,
        None => Vec::new(),
    };
    for v in &mut vars {
        if let Some(val) = form.opt(&format!("var_{}", v.id)) {
            v.value = val.to_string();
        }
    }
    let mounts = match (node_id, &egg) {
        (Some(n), Some(e)) => mount_options(admin, n, e.id.0).await?,
        _ => Vec::new(),
    };
    let chosen_mounts = form.list("mounts");
    let images = egg.as_ref().map(|e| options_of(e.docker_images.as_ref())).unwrap_or_default();
    let startups = egg.as_ref().map(|e| options_of(e.startup_commands.as_ref())).unwrap_or_default();
    let get = |k: &str, d: &str| form.opt(k).map(String::from).unwrap_or_else(|| d.to_string());
    // A blank image/startup (fresh egg choice) falls back to the egg's first entry.
    let pick = |k: &str, opts: &[(String, String)]| {
        form.opt(k)
            .filter(|v| !v.trim().is_empty())
            .map(String::from)
            .unwrap_or_else(|| opts.first().map(|o| o.0.clone()).unwrap_or_default())
    };
    let image = pick("image", &images);
    let startup = pick("startup", &startups);
    let node = node_id.and_then(|n| nodes.iter().find(|m| m.id.0 == n));
    // `Node::ipAddresses()` for the allocation create-option + alias datalist
    // suggestions (node and egg names).
    let ips = match node {
        Some(n) => server_info::node_ips(st, n).await,
        None => Vec::new(),
    };
    let mut alias_suggestions = node.map(|n| n.name.clone()).into_iter().collect::<Vec<String>>();
    if let Some(e) = &egg {
        alias_suggestions.push(e.name.clone());
    }
    alias_suggestions.retain(|s| !s.is_empty());
    Ok(context! {
        nodes => nodes.iter().map(|n| vec![n.id.0.to_string(), n.name.clone()]).collect::<Vec<_>>(),
        eggs => eggs_all.iter().map(|e| vec![e.id.0.to_string(), e.name.clone()]).collect::<Vec<_>>(),
        owners => owners.iter().map(|u| vec![u.id.0.to_string(), format!("{} ({})", u.username, u.email)]).collect::<Vec<_>>(),
        allocations => free.iter().map(|a| vec![a.id.0.to_string(), crate::models::address(a)]).collect::<Vec<_>>(),
        images => images.iter().map(|(v, l)| vec![v.clone(), l.clone()]).collect::<Vec<_>>(),
        startups => startups.iter().map(|(v, l)| vec![v.clone(), l.clone()]).collect::<Vec<_>>(),
        variables => Mj::from_serialize(&vars),
        mounts => mounts.iter().map(|m| context! { id => m.id.0, name => m.name.clone(), desc => format!("{} -> {}", m.source, m.target), checked => chosen_mounts.contains(&m.id.0.to_string()) }).collect::<Vec<_>>(),
        v => context! {
            name => get("name", ""), external_id => get("external_id", ""), description => get("description", ""),
            node_id => node_id.map(|n| n.to_string()).unwrap_or_default(),
            owner_id => get("owner_id", &admin.user.id().to_string()),
            allocation_id => get("allocation_id", ""),
            allocation_additional => form.list("allocation_additional"),
            egg_id => egg_id.map(|e| e.to_string()).unwrap_or_default(),
            skip_scripts => get("skip_scripts", "0"), start_on_completion => get("start_on_completion", "1"),
            image => image, startup => startup,
            cpu => get("cpu", "0"), memory => get("memory", "0"), disk => get("disk", "0"), swap => get("swap", "0"),
            threads => get("threads", ""), oom_killer => get("oom_killer", "0"),
            allocation_limit => get("allocation_limit", "0"), database_limit => get("database_limit", "0"),
            backup_limit => get("backup_limit", "0"),
            swap_support => get("swap_support", "disabled"), cpu_pinning => get("cpu_pinning", "0"),
        },
        labels => form.list("label_key").into_iter().zip(form.list("label_value")).filter(|(k, _)| !k.is_empty()).map(|(k, v)| vec![k, v]).collect::<Vec<_>>(),
        binary => st.config.use_binary_prefix,
        errors => Mj::from_serialize(errors),
        can_create_user => admin.can("create user"),
        can_create_allocation => admin.can("create allocation"),
        node_ips => ips.iter().map(|i| vec![i.clone(), i.clone()]).collect::<Vec<_>>(),
        alias_suggestions => alias_suggestions,
        create_option_label => admin.t(CREATE_OPTION_LABEL),
        // Defaults so the create-option modals also render within the page.
        m => context! { username => "", email => "", password => "" },
        o_errors => Mj::from_serialize(&Errors::default()),
        a_ip => String::new(),
        a_alias => String::new(),
        a_ports => Vec::<String>::new(),
        a_service_error => Mj::from_serialize(&None::<String>),
        a_errors => Mj::from_serialize(&Errors::default()),
    })
}

/// `GET /admin/servers/create` (also the htmx refresh when node/egg change).
async fn show(admin: Admin, RawQuery(q): RawQuery) -> WebResult {
    admin.authorize("create server")?;
    let form = Form::parse(q.unwrap_or_default().as_bytes());
    render(&admin, &form, &Errors::default()).await
}

fn num(form: &Form, unlimited: &str, key: &str) -> String {
    if form.get(unlimited) == "1" { "0".into() } else { form.get(key).trim().to_string() }
}

/// `POST /admin/servers/create`: `ServerCreationService::handle()`.
async fn store(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create server")?;
    let st = &admin.st;
    let swap = match form.get("swap_support") {
        "unlimited" => "-1".to_string(),
        "limited" => form.get("swap").trim().to_string(),
        _ => "0".to_string(),
    };
    let threads = if form.get("cpu_pinning") == "1" { form.get("threads").trim().to_string() } else { String::new() };
    let data = json!({
        "name": form.get("name").trim(), "external_id": form.opt("external_id").map(str::trim).filter(|v| !v.is_empty()),
        "node_id": form.get("node_id"), "owner_id": form.get("owner_id"), "egg_id": form.get("egg_id"),
        "allocation_id": form.opt("allocation_id").filter(|v| !v.is_empty()),
        "cpu": num(&form, "unlimited_cpu", "cpu"), "memory": num(&form, "unlimited_memory", "memory"),
        "disk": num(&form, "unlimited_disk", "disk"), "swap": swap,
        "threads": (!threads.is_empty()).then(|| threads.clone()),
        "allocation_limit": form.get("allocation_limit").trim(), "database_limit": form.get("database_limit").trim(),
        "backup_limit": form.get("backup_limit").trim(),
        "image": form.get("image").trim(), "startup": form.get("startup").trim(),
    });
    let mut v = Validator::for_value(&data);
    for (k, label) in [
        ("name", "admin/server.name"),
        ("node_id", "admin/server.node"),
        ("owner_id", "admin/server.owner"),
        ("egg_id", "admin/server.egg"),
        ("image", "admin/server.image"),
        ("startup", "admin/server.startup_cmd"),
    ] {
        v.attribute_name(k, &admin.t(label).to_lowercase());
    }
    v.rules("name", &["required", "string", "min:1", "max:255"]);
    v.rules("external_id", &["sometimes", "nullable", "string", "between:1,255"]);
    v.rules("node_id", &["required", "integer"]);
    v.rules("owner_id", &["required", "integer"]);
    v.rules("egg_id", &["required", "integer"]);
    v.rules("cpu", &["required", "numeric", "min:0"]);
    v.rules("memory", &["required", "numeric", "min:0"]);
    v.rules("disk", &["required", "numeric", "min:0"]);
    v.rules("swap", &["required", "numeric", "min:-1"]);
    v.rules("threads", &["nullable", "regex:/^[0-9-,]+$/"]);
    v.rules("allocation_limit", &["required", "integer", "min:0"]);
    v.rules("database_limit", &["required", "integer", "min:0"]);
    v.rules("backup_limit", &["required", "integer", "min:0"]);
    v.rules("image", &["required", "string", "max:255"]);
    v.rules("startup", &["required", "string"]);
    let node_id = form.get("node_id").parse::<i64>().ok();
    let node = match node_id {
        Some(n) if admin.user.can_target_node(n) => nodes::Entity::find_by_id(n).one(&st.db).await?,
        _ => None,
    };
    v.custom(
        "node_id",
        CustomRule::precomputed("exists", node.is_none().then(|| "The selected :attribute is invalid.".into())),
    );
    let owner_ok = match form.get("owner_id").parse::<i64>() {
        Ok(o) => users::Entity::find_by_id(o).one(&st.db).await?.is_some(),
        Err(_) => false,
    };
    v.custom(
        "owner_id",
        CustomRule::precomputed("exists", (!owner_ok).then(|| "The selected :attribute is invalid.".into())),
    );
    let egg = match form.get("egg_id").parse::<i64>() {
        Ok(e) => eggs::Entity::find_by_id(e).one(&st.db).await?,
        Err(_) => None,
    };
    v.custom(
        "egg_id",
        CustomRule::precomputed("exists", egg.is_none().then(|| "The selected :attribute is invalid.".into())),
    );
    if let Some(ext) = data["external_id"].as_str() {
        let taken = servers::Entity::find().filter(servers::Column::ExternalId.eq(ext)).one(&st.db).await?.is_some();
        v.custom(
            "external_id",
            CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".into())),
        );
    }
    // The primary/additional allocations must be free and on the chosen node.
    let mut alloc_ids: Vec<i64> = Vec::new();
    if let Some(a) = data["allocation_id"].as_str().and_then(|a| a.parse::<i64>().ok()) {
        alloc_ids.push(a);
    }
    for a in form.list("allocation_additional").iter().filter_map(|a| a.parse::<i64>().ok()) {
        if !alloc_ids.contains(&a) {
            alloc_ids.push(a);
        }
    }
    let usable = if alloc_ids.is_empty() {
        Vec::new()
    } else {
        allocations::Entity::find()
            .filter(allocations::Column::Id.is_in(alloc_ids.clone()))
            .filter(allocations::Column::ServerId.is_null())
            .all(&st.db)
            .await?
            .into_iter()
            .filter(|a| Some(a.node_id.0) == node_id)
            .collect::<Vec<_>>()
    };
    if usable.len() != alloc_ids.len() {
        v.add_error("allocation_id", "The selected primary allocation is invalid.");
    }
    v.run();
    let mut errors = Errors::from_validator(&v);
    let vars = match &egg {
        Some(e) => variable_fields(&admin, None, e.id.0).await?,
        None => Vec::new(),
    };
    for (field, msg) in variables::validate(&vars, |f| form.get(&format!("var_{}", f.id)).to_string()) {
        errors.add(&field, msg);
    }
    let (Some(node), Some(egg)) = (node, egg) else {
        return render(&admin, &form, &errors).await;
    };
    if !errors.is_empty() {
        return render(&admin, &form, &errors).await;
    }
    admin.session.update(|d| {
        d.extra.insert("last_utilized_node".into(), json!(node.id.0));
    });

    let int = |k: &str| data[k].as_str().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as i64;
    let labels: serde_json::Map<String, Value> = form
        .list("label_key")
        .into_iter()
        .zip(form.list("label_value"))
        .filter(|(k, _)| !k.trim().is_empty())
        .map(|(k, v)| (k.trim().to_string(), Value::String(v)))
        .collect();
    let primary = data["allocation_id"].as_str().and_then(|a| a.parse::<i64>().ok());
    let txn = st.db.begin().await?;
    let uuid = loop {
        let u = uuid::Uuid::new_v4().to_string();
        let clash = servers::Entity::find()
            .filter(
                sea_orm::Condition::any()
                    .add(servers::Column::Uuid.eq(u.clone()))
                    .add(servers::Column::UuidShort.eq(u[..8].to_string())),
            )
            .one(&txn)
            .await?;
        if clash.is_none() {
            break u;
        }
    };
    let now = Timestamp(pelican_core::time::now());
    let am = servers::ActiveModel {
        external_id: Set(data["external_id"].as_str().map(String::from)),
        uuid_short: Set(uuid[..8].to_string()),
        uuid: Set(uuid.clone()),
        node_id: Set(node.id),
        name: Set(data["name"].as_str().unwrap_or_default().to_string()),
        description: Set(Some(form.get("description").to_string())),
        status: Set(Some("installing".into())),
        skip_scripts: Set(Bool(form.get("skip_scripts") == "1")),
        owner_id: Set(Int(int("owner_id"))),
        memory: Set(Int(int("memory"))),
        swap: Set(Int(int("swap"))),
        disk: Set(Int(int("disk"))),
        io: Set(Int(500)),
        cpu: Set(Int(int("cpu"))),
        threads: Set((!threads.is_empty()).then_some(threads)),
        oom_killer: Set(SmallBool(form.get("oom_killer") == "1")),
        allocation_id: Set(primary.map(Int)),
        egg_id: Set(egg.id),
        startup: Set(data["startup"].as_str().unwrap_or_default().to_string()),
        image: Set(data["image"].as_str().unwrap_or_default().to_string()),
        database_limit: Set(Some(Int(int("database_limit")))),
        allocation_limit: Set(Some(Int(int("allocation_limit")))),
        backup_limit: Set(Int(int("backup_limit"))),
        docker_labels: Set((!labels.is_empty()).then(|| Json(Value::Object(labels)))),
        created_at: Set(Some(now)),
        updated_at: Set(Some(now)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&txn, &am).await?.ok_or_else(|| WebError::internal("no id"))?;
    let created = servers::Entity::find_by_id(id).one(&txn).await?.ok_or(WebError::NotFound)?;
    let ctx = admin.api_ctx(Bound::default());
    pelican_api::services::server_saved(st, &ctx, &txn, &created)
        .await
        .map_err(|e| WebError::internal(format!("{e:?}")))?;
    let mut alloc_updated: Vec<i64> = Vec::new();
    for a in &usable {
        // ServerCreationService: `Allocation::query()->…->get()->each(fn ($a) =>
        // $a->save())` — per-instance saves (updated events).
        if pelican_db::write::update(
            &txn,
            &allocations::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(a.id),
                server_id: Set(Some(created.id)),
                is_locked: Set(Bool(true)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?
        {
            alloc_updated.push(a.id.0);
        }
    }
    let values: HashMap<i64, String> =
        vars.iter().map(|f| (f.id, form.get(&format!("var_{}", f.id)).to_string())).collect();
    let mut var_created: Vec<i64> = Vec::new();
    for f in &vars {
        // `storeEggVariables`: `$server->serverVariables()->forceCreate([...])`.
        if let Some(new_id) = pelican_db::write::insert_id(
            &txn,
            &server_variables::ActiveModel {
                server_id: Set(created.id),
                variable_id: Set(Int(f.id)),
                variable_value: Set(values.get(&f.id).cloned().unwrap_or_default()),
                created_at: Set(Some(now)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?
        {
            var_created.push(new_id);
        }
    }
    let allowed: Vec<i64> = mount_options(&admin, node.id.0, egg.id.0).await?.into_iter().map(|m| m.id.0).collect();
    let b = txn.get_database_backend();
    for m in form.list("mounts").iter().filter_map(|m| m.parse::<i64>().ok()).filter(|m| allowed.contains(m)) {
        txn.execute_raw(raw::stmt(
            b,
            "insert into {mountables} ({mountable_type}, {mountable_id}, {mount_id}) values (?, ?, ?)",
            vec!["server".into(), id.into(), m.into()],
        ))
        .await?;
    }
    txn.commit().await?;
    // ServerCreationService events (order: `Server::create`, allocation saves,
    // variable forceCreates; the mounts pivot fires nothing).
    model_events::fire_id::<servers::Entity>(st, model_events::Action::Created, id).await;
    for aid in alloc_updated {
        model_events::fire_id::<allocations::Entity>(st, model_events::Action::Updated, aid).await;
    }
    for vid in var_created {
        model_events::fire_id::<server_variables::Entity>(st, model_events::Action::Created, vid).await;
    }

    // Create on Wings; a connection failure rolls the server back (force delete).
    let start = form.get("start_on_completion") == "1";
    let created_on_wings = match st.node_conn(&node) {
        Ok(conn) => st.wings.create_server(&conn, &created.uuid, start).await,
        Err(e) => Err(pelican_wings::WingsError::Connection(e.to_string())),
    };
    if let Err(e) = created_on_wings {
        if matches!(e, pelican_wings::WingsError::Connection(_)) {
            let _ = pelican_api::application::servers::delete_server(st, &ctx, &created, true).await;
            admin.flash_danger(
                admin.t("admin/server.notifications.create_failed"),
                Some(admin.t_with("admin/server.notifications.error_connecting", &[("node", &node.name)])),
            );
            return render(&admin, &form, &Errors::default()).await;
        }
        admin.flash_danger(admin.t("admin/server.notifications.create_failed"), Some(e.to_string()));
    } else {
        admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    }
    Ok(admin.redirect(&format!("/admin/servers/{id}/edit")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-render the wizard page and its htmx partials (form + modal fields).
    #[test]
    fn create_template_renders() {
        crate::views::init(None);
        let ctx = context! {
            title => "Create Server",
            nodes => Vec::<Vec<String>>::new(),
            eggs => vec![vec!["1".to_string(), "Egg".to_string()]],
            owners => vec![vec!["1".to_string(), "jane (jane@example.com)".to_string()]],
            allocations => vec![vec!["2".to_string(), "127.0.0.1:27015".to_string()]],
            images => Vec::<Vec<String>>::new(),
            startups => Vec::<Vec<String>>::new(),
            variables => Mj::from_serialize(Vec::<Value>::new()),
            mounts => Mj::from_serialize(Vec::<Value>::new()),
            labels => Mj::from_serialize(Vec::<Value>::new()),
            binary => false,
            errors => Mj::from_serialize(Errors::default()),
            can_create_user => true,
            can_create_allocation => true,
            node_ips => vec![vec!["127.0.0.1".to_string(), "127.0.0.1".to_string()]],
            alias_suggestions => vec!["Node A".to_string()],
            create_option_label => "Create",
            m => context! { username => "", email => "", password => "" },
            o_errors => Mj::from_serialize(Errors::default()),
            a_ip => "", a_alias => "", a_ports => Vec::<String>::new(),
            a_service_error => Mj::from_serialize(&None::<String>),
            a_errors => Mj::from_serialize(Errors::default()),
            v => context! {
                name => "demo", external_id => "", description => "",
                node_id => "1", owner_id => "1", allocation_id => "2",
                allocation_additional => Vec::<String>::new(), egg_id => "1",
                skip_scripts => "0", start_on_completion => "1",
                image => "ghcr.io/pty/code-1.0", startup => "code-server",
                cpu => "0", memory => "0", disk => "0", swap => "0",
                threads => "", oom_killer => "0",
                allocation_limit => "0", database_limit => "0", backup_limit => "0",
                swap_support => "disabled", cpu_pinning => "0",
            },
        };
        let html = crate::views::render(TPL, ctx.clone()).expect("page renders");
        assert!(html.contains("create-form"));
        assert!(html.contains("owner-modal"));
        assert!(html.contains("allocation-modal"));
        crate::views::render_block(TPL, "form", ctx.clone()).expect("form block renders");
        let fields = crate::views::render_block(TPL, "owner_fields", ctx.clone()).expect("owner fields render");
        assert!(fields.contains("username") && fields.contains("f-password"));
        let alloc = crate::views::render_block(TPL, "allocation_fields", ctx).expect("allocation fields render");
        assert!(alloc.contains("allocation_ip") && alloc.contains("allocation_ports"));
    }
}

// ----- inline create-option actions (owner_id / allocation_id) ---------------

/// Successful create-option: swap the re-rendered `#create-form` in and close
/// the originating modal.
fn hx_swap_form(response: Response, modal: &str) -> Response {
    let mut r = response;
    let h = r.headers_mut();
    h.insert("hx-retarget", HeaderValue::from_static("#create-form"));
    h.insert("hx-reswap", HeaderValue::from_static("outerHTML"));
    if let Ok(v) = HeaderValue::from_str(&format!(r#"{{"close-modal": {{"id": "{modal}"}}}}"#)) {
        h.insert("hx-trigger", v);
    }
    r
}

fn page_fragment(html: String) -> Response {
    let mut resp = axum::response::Html(html).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, private"));
    resp
}

/// Re-rendered form block for a successful create-option (`refreshForm()`).
async fn form_block(admin: &Admin, form: &Form) -> WebResult<Response> {
    let ctx = form_context(admin, form, &Errors::default()).await?;
    Ok(admin.render_block(TPL, "form", ctx)?)
}

/// Owner create-option modal: re-render its fields with the entered values
/// and validation errors (the modal stays open).
fn owner_fields_block(
    admin: &Admin,
    username: &str,
    email: &str,
    password: &str,
    errors: &Errors,
) -> WebResult<Response> {
    Ok(page_fragment(crate::views::render_block(
        TPL,
        "owner_fields",
        context! {
            m => context! { username => username, email => email, password => password },
            o_errors => Mj::from_serialize(errors),
            create_option_label => admin.t(CREATE_OPTION_LABEL),
        },
    )?))
}

/// `POST /admin/servers/create/owner`: the `createOptionForm` +
/// `createOptionUsing` of the owner_id field (username/email/password,
/// then `UserCreationService::handle`).
async fn store_owner(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create server")?;
    admin.authorize("create user")?;
    let st = &admin.st;
    // The saving hook lowercases username/email before persisting.
    let username = form.get("username").trim().to_lowercase();
    let email = form.get("email").trim().to_lowercase();
    let password = form.get("password").to_string();
    // Rules of the createOptionForm: username alphaNum()/minLength(3), unique
    // email; the saving hook keeps uniqueness checks against stored values.
    let user_data = json!({
        "username": username.clone(), "email": email.clone(),
        "password": (!password.is_empty()).then(|| password.clone()),
    });
    let mut v = Validator::for_value(&user_data);
    v.attribute_name("username", &admin.t("admin/user.username").to_lowercase());
    v.attribute_name("email", &admin.t("admin/user.email").to_lowercase());
    v.rules("username", &["required", "string", "alpha_num", "min:3", "max:255"]);
    v.rules("email", &["required", "email", "max:255"]);
    let dup_username =
        users::Entity::find().filter(users::Column::Username.eq(username.clone())).one(&st.db).await?.is_some();
    v.custom("username", CustomRule::precomputed("unique", dup_username.then(|| TAKEN.into())));
    let dup_email = users::Entity::find().filter(users::Column::Email.eq(email.clone())).one(&st.db).await?.is_some();
    v.custom("email", CustomRule::precomputed("unique", dup_email.then(|| TAKEN.into())));
    v.run();
    let errors = Errors::from_validator(&v);
    if !errors.is_empty() {
        return owner_fields_block(&admin, &username, &email, &password, &errors);
    }
    let user = super::users::store_new_user(st, &FormData::owner_option(username, email, password)).await?;
    let mut form = form;
    form.fields.insert("owner_id".into(), vec![user.id.0.to_string()]);
    Ok(hx_swap_form(form_block(&admin, &form).await?, "owner-modal"))
}

/// Allocation create-option modal on validation/service errors.
fn allocation_fields_block(
    admin: &Admin,
    ips: &[String],
    data: &AllocData,
    service_error: Option<String>,
    errors: &Errors,
) -> WebResult<Response> {
    Ok(page_fragment(crate::views::render_block(
        TPL,
        "allocation_fields",
        context! {
            node_ips => ips.iter().map(|i| vec![i.clone(), i.clone()]).collect::<Vec<_>>(),
            alias_suggestions => Mj::from_serialize(&data.suggestions),
            a_ip => data.ip, a_alias => data.alias, a_ports => data.ports,
            a_service_error => service_error,
            a_errors => Mj::from_serialize(errors),
            create_option_label => admin.t(CREATE_OPTION_LABEL),
        },
    )?))
}

/// Entered values of the allocation create-option form.
#[derive(Default)]
struct AllocData {
    ip: String,
    alias: String,
    ports: Vec<String>,
    suggestions: Vec<String>,
}

impl AllocData {
    fn from_form(form: &Form) -> AllocData {
        AllocData {
            ip: form.get("allocation_ip").trim().to_string(),
            alias: form
                .opt("allocation_alias")
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
                .unwrap_or_default(),
            ports: crate::forms::parse_tags(form.get("allocation_ports")),
            suggestions: Vec::new(),
        }
    }
}

/// `POST /admin/servers/create/allocation`: the `createOptionForm` +
/// `createOptionUsing` of the allocation_id field —
/// `AssignmentService::handle($node, $data)` and the first created id.
async fn store_allocation(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create server")?;
    admin.authorize("create allocation")?;
    let st = &admin.st;
    let nid = form.opt("node_id").and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
    // The button is hidden until a node is chosen; still resolve it defensively.
    let node = nodes::Entity::find_by_id(nid).one(&st.db).await?.filter(|_| admin.user.can_target_node(nid));
    let mut data = AllocData::from_form(&form);
    data.suggestions.push(form.get("name").trim().to_string());
    if let Some(egg_id) = form.opt("egg_id").and_then(|e| e.parse::<i64>().ok()) {
        if let Some(e) = eggs::Entity::find_by_id(egg_id).one(&st.db).await? {
            data.suggestions.push(e.name);
        }
    }
    data.suggestions.retain(|s| !s.is_empty());
    let ips = match &node {
        Some(n) => server_info::node_ips(st, n).await,
        None => Vec::new(),
    };
    let alloc_data = json!({
        "allocation_ip": data.ip,
        "allocation_ports": (!data.ports.is_empty()).then(|| json!(data.ports)),
    });
    let mut v = Validator::for_value(&alloc_data);
    v.attribute_name("allocation_ip", &admin.t("admin/server.ip_address").to_lowercase());
    v.attribute_name("allocation_ports", &admin.t("admin/server.port").to_lowercase());
    v.rules("allocation_ip", &["required", "ip"]);
    v.custom(
        "allocation_ip",
        CustomRule::precomputed("exists", node.is_none().then(|| "The selected :attribute is invalid.".into())),
    );
    v.custom(
        "allocation_ports",
        CustomRule::precomputed("required", data.ports.is_empty().then(|| "The :attribute field is required.".into())),
    );
    v.run();
    let errors = Errors::from_validator(&v);
    let Some(node) = node.filter(|_| errors.is_empty()) else {
        return allocation_fields_block(&admin, &ips, &data, None, &errors);
    };
    // AssignmentService validation/range rules raise service errors → they are
    // shown inside the open modal.
    let ports: Vec<Value> = data.ports.iter().cloned().map(Value::String).collect();
    match pelican_api::application::nodes::assign_allocations(
        st,
        node.id,
        &data.ip,
        &ports,
        (!data.alias.is_empty()).then(|| data.alias.clone()),
        None,
        false,
    )
    .await
    {
        Ok(ids) => {
            let first = *ids.first().ok_or_else(|| WebError::internal("no allocations created"))?;
            let mut form = form;
            form.fields.insert("allocation_id".into(), vec![first.to_string()]);
            Ok(hx_swap_form(form_block(&admin, &form).await?, "allocation-modal"))
        }
        Err(e) => {
            allocation_fields_block(&admin, &ips, &data, e.errors.first().map(|o| o.detail.clone()), &Errors::default())
        }
    }
}
