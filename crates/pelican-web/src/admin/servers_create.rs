//! Admin › Servers › Create (Pages/CreateServer.php + ServerCreationService):
//! one page with the wizard's sections; node/egg changes re-render the form.

use std::collections::HashMap;

use axum::Router;
use axum::extract::RawQuery;
use axum::routing::get;
use minijinja::{Value as Mj, context};
use pelican_api::binding::Bound;
use pelican_api::validation::{CustomRule, Validator};
use pelican_core::state::AppState;
use pelican_db::entities::{allocations, eggs, nodes, server_variables, servers, users};
use pelican_db::{Bool, Int, Json, SmallBool, Timestamp, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde_json::{Value, json};

use super::Admin;
use super::servers::{mount_options, variable_fields};
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::variables;

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/servers/create", get(show).post(store))
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
        form.opt(k).filter(|v| !v.trim().is_empty()).map(String::from).unwrap_or_else(|| opts.first().map(|o| o.0.clone()).unwrap_or_default())
    };
    let image = pick("image", &images);
    let startup = pick("startup", &startups);
    let t = |k: &str| admin.t(k);
    admin
        .page(
            "servers",
            "admin/servers/create.html",
            context! {
                title => admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("admin/server.model_label"))]),
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
            },
        )
        .await
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
    for a in &usable {
        pelican_db::write::update(
            &txn,
            &allocations::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(a.id),
                server_id: Set(Some(created.id)),
                is_locked: Set(Bool(true)),
                updated_at: Set(Some(now)),
                ..Default::default()
            },
        )
        .await?;
    }
    let values: HashMap<i64, String> =
        vars.iter().map(|f| (f.id, form.get(&format!("var_{}", f.id)).to_string())).collect();
    for f in &vars {
        pelican_db::write::insert_id(
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
        .await?;
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
