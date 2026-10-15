//! Admin › Roles (app/Filament/Admin/Resources/Roles): list, create, view,
//! edit (name, spatie permission checklist, node access) and delete.
//! Root Admin cannot be renamed/deleted (`Role::ROOT_ADMIN` guards).

use std::collections::{HashMap, HashSet};

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::validation::{CustomRule, Validator};
use pelican_core::auth::ROOT_ADMIN_ROLE as ROOT_ADMIN;
use pelican_core::state::AppState;
use pelican_db::entities::{model_has_roles, node_role, nodes, permissions, role_has_permissions, roles};
use pelican_db::{Int, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    TransactionTrait,
};
use serde::Serialize;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const SLUG: &str = "roles";
/// `Role::DEFAULT_GUARD_NAME`; panel roles are managed through this guard.
const GUARD: &str = "web";
/// `RolePermissionPrefixes`.
const PREFIXES: [&str; 5] = ["viewList", "view", "create", "update", "delete"];
/// `RolePermissionModels::cases()` in declaration order.
const MODELS: [&str; 12] = [
    "apiKey",
    "allocation",
    "backupHost",
    "databaseHost",
    "database",
    "egg",
    "mount",
    "node",
    "role",
    "server",
    "user",
    "webhook",
];
/// `Role::MODEL_SPECIFIC_PERMISSIONS`.
const EGG_EXTRA: [&str; 2] = ["import", "export"];
/// `Role::SPECIAL_PERMISSIONS` in declaration order.
fn special_perms(model: &str) -> Option<Vec<&'static str>> {
    match model {
        "settings" => Some(vec!["view", "update"]),
        "health" => Some(vec!["view"]),
        "activityLog" => Some(vec!["seeIps"]),
        "panelLog" => Some(vec!["view"]),
        "plugin" => Some(vec!["viewList", "create", "update", "delete"]),
        _ => None,
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/roles", get(index))
        .route("/admin/roles/create", get(create_form).post(create))
        .route("/admin/roles/{id}", get(view))
        .route("/admin/roles/{id}/edit", get(edit_form).post(update))
        .route("/admin/roles/{id}/delete", post(delete))
        .route("/admin/roles/bulk-delete", post(bulk_delete))
}

/// `(model, form field)` pairs of every permission section, in
/// `Role::getPermissionList()` order; the field follows the Filament
/// `strtolower($model) . '_list'` convention.
fn special_models() -> [&'static str; 5] {
    ["settings", "health", "activityLog", "panelLog", "plugin"]
}

/// Core models, special ones, then models registered by plugins
/// (`Role::registerCustomPermissions`).
fn all_models() -> Vec<String> {
    let mut out: Vec<String> = MODELS.iter().copied().chain(special_models()).map(str::to_string).collect();
    for (model, _) in pelican_plugins::runtime::admin_permissions() {
        if !out.contains(&model) {
            out.push(model);
        }
    }
    out
}

fn all_sections() -> Vec<(String, String)> {
    all_models()
        .into_iter()
        .map(|m| {
            let field = format!("{}_list", m.to_lowercase());
            (m, field)
        })
        .collect()
}

/// Permission names offered by a section (`{prefix} {model}`).
fn section_perms(model: &str) -> Vec<String> {
    let list: Vec<&str> = match special_perms(model) {
        Some(p) => p,
        // Models only plugins register get just their own prefixes.
        None if !MODELS.contains(&model) => Vec::new(),
        None => {
            let mut v = PREFIXES.to_vec();
            if model == "egg" {
                v.extend(EGG_EXTRA);
            }
            v
        }
    };
    let mut perms: Vec<String> = list.into_iter().map(|p| format!("{p} {model}")).collect();
    for (m, prefixes) in pelican_plugins::runtime::admin_permissions() {
        for p in prefixes.iter().filter(|_| m == model) {
            let name = format!("{p} {model}");
            if !perms.contains(&name) {
                perms.push(name);
            }
        }
    }
    perms
}

/// Section icons: `Role::getModelIcon()` (MODEL_ICONS overrides plus the
/// navigation icon of the resource/page backing the model).
fn model_icon(model: &str) -> &'static str {
    match model {
        "health" => "heart",
        "activityLog" => "stack",
        "panelLog" => "file-info",
        "apiKey" => "key",
        "allocation" => "network",
        "backupHost" => "file-zip",
        "databaseHost" | "database" => "database",
        "egg" => "eggs",
        "mount" => "layers-linked",
        "node" => "server-2",
        "role" => "users-group",
        "server" => "brand-docker",
        "user" => "users",
        "webhook" => "webhook",
        "settings" => "settings",
        "plugin" => "packages",
        _ => "users-group",
    }
}

/// Deduplicate while keeping first-seen order (`collect()->unique()`).
fn unique(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for v in values {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Per-role tables: permission/user counts and attached node names.
struct Counts {
    perms: HashMap<i64, i64>,
    users: HashMap<i64, i64>,
    node_names: HashMap<i64, Vec<String>>,
}

async fn counts(st: &AppState) -> Result<Counts, WebError> {
    let perms = role_has_permissions::Entity::find().all(&st.db).await?.into_iter().fold(
        HashMap::<i64, i64>::new(),
        |mut m, r| {
            *m.entry(r.role_id.0).or_default() += 1;
            m
        },
    );
    let users = model_has_roles::Entity::find()
        .filter(model_has_roles::Column::ModelType.eq("user"))
        .all(&st.db)
        .await?
        .into_iter()
        .fold(HashMap::<i64, i64>::new(), |mut m, r| {
            *m.entry(r.role_id.0).or_default() += 1;
            m
        });
    let node_map: HashMap<i64, String> =
        nodes::Entity::find().all(&st.db).await?.into_iter().map(|n| (n.id.0, n.name)).collect();
    let mut node_names: HashMap<i64, Vec<String>> = HashMap::new();
    for r in node_role::Entity::find().all(&st.db).await? {
        if let Some(name) = node_map.get(&r.node_id.0) {
            node_names.entry(r.role_id.0).or_default().push(name.clone());
        }
    }
    Ok(Counts { perms, users, node_names })
}

/// Spatie permission names bound to a role (`checkPermissionTo` sees guard web).
async fn permission_names<C: ConnectionTrait>(db: &C, role_id: i64) -> Result<HashSet<String>, WebError> {
    let pivots = role_has_permissions::Entity::find()
        .filter(role_has_permissions::Column::RoleId.eq(Int(role_id)))
        .all(db)
        .await?;
    let ids: Vec<i64> = pivots.iter().map(|p| p.permission_id.0).collect();
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(permissions::Entity::find()
        .filter(permissions::Column::Id.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .filter(|p| p.guard_name == GUARD)
        .map(|p| p.name)
        .collect())
}

/// `GET /admin/roles` (ListRoles): name, permission count, nodes, user count.
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList role")?;
    let opts = [10, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let q = match p.sort.as_deref() {
        Some("name") if p.desc => roles::Entity::find().order_by_desc(roles::Column::Name),
        Some("name") => roles::Entity::find().order_by_asc(roles::Column::Name),
        _ => roles::Entity::find(),
    };
    let all = q.all(&admin.st.db).await?;
    let c = counts(&admin.st).await?;
    let total = all.len() as u64;
    let page: Vec<&roles::Model> = match p.limit_offset() {
        Some((limit, offset)) => all.iter().skip(offset as usize).take(limit as usize).collect(),
        None => all.iter().collect(),
    };
    let t = |k: &str| admin.t(&format!("admin/role.{k}"));
    let can_edit = admin.can("update role");
    let can_view = admin.can("view role");
    let mut table = Table::new("roles", "/admin/roles");
    table.columns = vec![
        Column::new("name", t("name")).sortable(),
        Column::new("permissions", t("permissions")),
        Column::new("nodes", t("nodes")).wide_only(),
        Column::new("users", t("users")),
    ];
    for r in page {
        let is_root = r.name == ROOT_ADMIN;
        let url = if can_edit { format!("/admin/roles/{}/edit", r.id.0) } else { format!("/admin/roles/{}", r.id.0) };
        let mut row = Row::new(r.id.0.to_string()).url(url.clone()).cell("name", Cell::text(r.name.clone()));
        row = row
            .cell(
                "permissions",
                if is_root {
                    Cell::badge(t("all"), "primary")
                } else {
                    Cell::badge(c.perms.get(&r.id.0).copied().unwrap_or(0).to_string(), "primary")
                },
            )
            .cell(
                "nodes",
                match c.node_names.get(&r.id.0) {
                    Some(names) if !names.is_empty() || is_root => {
                        Cell::Badges { items: names.clone(), color: "primary".into() }
                    }
                    _ => Cell::text(t("all")),
                },
            )
            .cell("users", Cell::text(c.users.get(&r.id.0).copied().unwrap_or(0).to_string()));
        if can_edit {
            row.actions.push(Action::link(admin.t("filament-actions::edit.single.label"), "pencil", url));
        } else if can_view {
            row.actions.push(Action::link(admin.t("filament-actions::view.single.label"), "eye", url));
        }
        // `checkIfRecordIsSelectableUsing`: Root Admin and roles held by users.
        let users = c.users.get(&r.id.0).copied().unwrap_or(0);
        if admin.can("delete role") && (users >= 1 || r.name == ROOT_ADMIN) {
            row.unselectable = true;
        }
        table.rows.push(row);
    }
    if admin.can("delete role") {
        table.bulk.push(
            Action::post(admin.t("filament-actions::delete.multiple.label"), "trash", "/admin/roles/bulk-delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
        );
    }
    table.empty_icon = "users-group".into();
    table.empty_heading = t("no_roles");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create role").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => "/admin/roles/create",
        }
    });
    admin
        .page(
            SLUG,
            "admin/list.html",
            context! { title => t("model_label_plural"), table => Mj::from_serialize(&table), create => create },
        )
        .await
}

/// Form state shared by create/edit/view.
struct FormData {
    name: String,
    guard_name: String,
    nodes: Vec<String>,
    /// Checked option values per section field (`{model}_list`), in
    /// `all_sections()` order.
    perms: Vec<Vec<String>>,
}

impl FormData {
    fn blank() -> FormData {
        FormData {
            name: String::new(),
            guard_name: GUARD.into(),
            nodes: Vec::new(),
            perms: all_sections().iter().map(|_| Vec::new()).collect(),
        }
    }

    fn from_form(f: &Form) -> FormData {
        let sections = all_sections();
        FormData {
            name: f.get("name").trim().into(),
            guard_name: f.opt("guard_name").unwrap_or(GUARD).to_string(),
            nodes: f.list("nodes"),
            perms: sections.iter().map(|(_, field)| f.list(field)).collect(),
        }
    }

    fn from_record(m: &roles::Model, granted: &HashSet<String>) -> FormData {
        FormData {
            name: m.name.clone(),
            guard_name: m.guard_name.clone(),
            nodes: Vec::new(),
            perms: all_models()
                .iter()
                .map(|model| section_perms(model).into_iter().filter(|v| granted.contains(v)).collect())
                .collect(),
        }
    }

    fn with_nodes(mut self, ids: &[i64]) -> FormData {
        self.nodes = ids.iter().map(i64::to_string).collect();
        self
    }

    fn ctx(&self) -> Mj {
        let fields: Vec<String> = all_sections().into_iter().map(|(_, f)| f).collect();
        context! {
            name => self.name,
            guard_name => self.guard_name,
            nodes => self.nodes,
            perms => Mj::from_serialize(fields.iter().zip(self.perms.iter()).collect::<Vec<_>>()),
        }
    }
}

#[derive(Serialize)]
struct SectionView {
    model: String,
    field: String,
    label: String,
    icon: &'static str,
    options: Vec<(String, String)>,
    selected: Vec<String>,
    count: usize,
    open: bool,
}

fn section_views(admin: &Admin, data: &FormData) -> Vec<SectionView> {
    all_sections()
        .into_iter()
        .enumerate()
        .map(|(i, (model, field))| {
            // `trans()->has("admin/role.models.$model") ?: Str::headline($model)`
            // — every model has a key except `backupHost`.
            let label = if model == "backupHost" {
                "Backup Host".to_string()
            } else {
                admin.t(&format!("admin/role.models.{model}"))
            };
            let selected: Vec<String> =
                data.perms[i].iter().filter(|v| section_perms(&model).contains(v)).cloned().collect();
            let count = selected.len();
            SectionView {
                model: model.clone(),
                field,
                label,
                icon: model_icon(&model),
                options: section_perms(&model)
                    .into_iter()
                    .map(|v| {
                        let key = v.split_once(' ').map(|(p, _)| p).unwrap_or("").to_string();
                        (v.clone(), admin.t(&format!("admin/role.permissions_list.{key}")))
                    })
                    .collect(),
                selected,
                count,
                open: count > 0,
            }
        })
        .collect()
}

/// Node options for the access-restriction select.
async fn node_options(admin: &Admin) -> Result<Vec<(String, String)>, WebError> {
    Ok(nodes::Entity::find()
        .order_by_asc(nodes::Column::Id)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .map(|n| (n.id.0.to_string(), n.name))
        .collect())
}

async fn find_role(admin: &Admin, id: i64) -> Result<roles::Model, WebError> {
    roles::Entity::find_by_id(Int(id)).one(&admin.st.db).await?.ok_or(WebError::NotFound)
}

async fn render_form(admin: &Admin, record: Option<&roles::Model>, data: &FormData, errors: &Errors) -> WebResult {
    let t = |k: &str| admin.t(&format!("admin/role.{k}"));
    let is_root = record.is_some_and(|r| r.name == ROOT_ADMIN);
    let (title, action) = match record {
        Some(r) => (
            admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &r.name)]),
            format!("/admin/roles/{}/edit", r.id.0),
        ),
        None => (
            admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
            "/admin/roles/create".into(),
        ),
    };
    let readonly = record.is_some() && !admin.can("update role");
    // DeleteAction (EditRole): disabled for Root Admin or when users hold it.
    let delete_state = match record {
        Some(r) if admin.can("delete role") => {
            let users = {
                let c = counts(&admin.st).await?;
                c.users.get(&r.id.0).copied().unwrap_or(0)
            };
            let tooltip = if r.name == ROOT_ADMIN {
                t("root_admin_delete")
            } else if users >= 1 {
                t("in_use")
            } else {
                admin.t("filament-actions::delete.single.label")
            };
            Some((tooltip, users == 0 && r.name != ROOT_ADMIN))
        }
        _ => None,
    };
    admin
        .page(
            SLUG,
            "admin/roles/form.html",
            context! {
                title => title,
                action => action,
                record => record.map(|r| context! { id => r.id.0, name => r.name.clone() }),
                readonly => readonly,
                is_root_admin => is_root,
                root_admin_label =>
                    admin.t_with("admin/role.root_admin", &[("role", ROOT_ADMIN)]),
                can_delete => delete_state.is_some(),
                delete_tooltip => delete_state.as_ref().map(|(tt, _)| tt.clone()).unwrap_or_default(),
                delete_enabled => delete_state.as_ref().is_some_and(|(_, e)| *e),
                delete_url => record.map(|r| format!("/admin/roles/{}/delete", r.id.0)).unwrap_or_default(),
                v => data.ctx(),
                errors => Mj::from_serialize(errors),
                sections => Mj::from_serialize(&section_views(admin, data)),
                node_options => node_options(admin).await?,
            },
        )
        .await
}

async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create role")?;
    render_form(&admin, None, &FormData::blank(), &Errors::default()).await
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view role")?;
    let r = find_role(&admin, id).await?;
    let granted = permission_names(&admin.st.db, id).await?;
    render_form(&admin, Some(&r), &FormData::from_record(&r, &granted), &Errors::default()).await
}

async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update role")?;
    let r = find_role(&admin, id).await?;
    let granted = permission_names(&admin.st.db, id).await?;
    let nodes: Vec<i64> = node_role::Entity::find()
        .filter(node_role::Column::RoleId.eq(Int(id)))
        .all(&admin.st.db)
        .await?
        .iter()
        .map(|n| n.node_id.0)
        .collect();
    render_form(&admin, Some(&r), &FormData::from_record(&r, &granted).with_nodes(&nodes), &Errors::default()).await
}

/// Form rules (TextInput `required`) + spatie uniqueness per guard
/// (`RoleAlreadyExists`, equivalent to a `unique` error here).
async fn validate(admin: &Admin, data: &FormData, ignore: Option<i64>) -> Result<Errors, WebError> {
    let input = serde_json::json!({ "name": data.name });
    let mut v = Validator::for_value(&input);
    v.attribute_name("name", &admin.t("admin/role.name").to_lowercase());
    v.rules("name", &["required", "string"]);
    let mut q = roles::Entity::find()
        .filter(roles::Column::Name.eq(data.name.clone()))
        .filter(roles::Column::GuardName.eq(GUARD));
    if let Some(id) = ignore {
        q = q.filter(roles::Column::Id.ne(Int(id)));
    }
    let taken = !data.name.is_empty() && q.one(&admin.st.db).await?.is_some();
    v.custom(
        "name",
        CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())),
    );
    v.run();
    Ok(Errors::from_validator(&v))
}

fn parse_ids(list: &[String]) -> Vec<i64> {
    let mut ids: Vec<i64> = list.iter().filter_map(|s| s.parse().ok()).collect();
    ids.dedup();
    ids
}

/// Selected permission names, flattened across sections in submission order.
fn collected(data: &FormData) -> Vec<String> {
    unique(data.perms.iter().flatten().cloned().collect())
}

/// `syncPermissions`: replace the role's permission pivots, creating missing
/// permissions (`Permission::firstOrCreate`) for this guard first.
async fn sync_permissions<C: ConnectionTrait>(db: &C, role_id: i64, names: &[String]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(b, "delete from {role_has_permissions} where {role_id} = ?", vec![role_id.into()]))
        .await?;
    let now = Timestamp(pelican_core::time::now());
    for name in names {
        let pid = match permissions::Entity::find()
            .filter(permissions::Column::Name.eq(name.clone()))
            .filter(permissions::Column::GuardName.eq(GUARD))
            .one(db)
            .await?
        {
            Some(p) => p.id.0,
            None => {
                let created = pelican_db::write::insert(
                    db,
                    permissions::ActiveModel {
                        name: Set(name.clone()),
                        guard_name: Set(GUARD.into()),
                        created_at: Set(Some(now)),
                        updated_at: Set(Some(now)),
                        ..Default::default()
                    },
                )
                .await?;
                created.id.0
            }
        };
        db.execute_raw(raw::stmt(
            b,
            "insert into {role_has_permissions} ({permission_id}, {role_id}) values (?, ?)",
            vec![pid.into(), role_id.into()],
        ))
        .await?;
    }
    Ok(())
}

/// Replace the node pivots of a role (the `nodes` relationship `sync()`).
async fn sync_nodes<C: ConnectionTrait>(db: &C, role_id: i64, ids: &[i64]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(b, "delete from {node_role} where {role_id} = ?", vec![role_id.into()])).await?;
    for id in ids {
        db.execute_raw(raw::stmt(
            b,
            "insert into {node_role} ({node_id}, {role_id}) values (?, ?)",
            vec![(*id).into(), role_id.into()],
        ))
        .await?;
    }
    Ok(())
}

/// Redirect after create (Filament: view › edit › index, first authorized).
fn post_create_target(admin: &Admin, id: i64) -> String {
    if admin.can("view role") {
        format!("/admin/roles/{id}")
    } else if admin.can("update role") {
        format!("/admin/roles/{id}/edit")
    } else {
        "/admin/roles".into()
    }
}

async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create role")?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, None).await?;
    if !errors.is_empty() {
        return render_form(&admin, None, &data, &errors).await;
    }
    let txn = admin.st.db.begin().await?;
    let now = Timestamp(pelican_core::time::now());
    let role = pelican_db::write::insert(
        &txn,
        roles::ActiveModel {
            name: Set(data.name.clone()),
            guard_name: Set(data.guard_name.clone()),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        },
    )
    .await?;
    let id = role.id.0;
    sync_permissions(&txn, id, &collected(&data)).await?;
    sync_nodes(&txn, id, &parse_ids(&data.nodes)).await?;
    txn.commit().await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&post_create_target(&admin, id)))
}

/// `POST …/edit` (EditRole::save). For the Root Admin every editable field is
/// disabled, so nothing changes besides `updated_at` — skip permission/node
/// sync to keep its full grants intact.
async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update role")?;
    let role = find_role(&admin, id).await?;
    let is_root = role.name == ROOT_ADMIN;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, Some(id)).await?;
    if !errors.is_empty() {
        return render_form(&admin, Some(&role), &data, &errors).await;
    }
    let txn = admin.st.db.begin().await?;
    if is_root {
        pelican_db::write::update(
            &txn,
            &roles::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(role.id),
                updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
                ..Default::default()
            },
        )
        .await?;
    } else {
        pelican_db::write::update(
            &txn,
            &roles::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(role.id),
                name: Set(data.name.clone()),
                guard_name: Set(data.guard_name.clone()),
                updated_at: Set(Some(Timestamp(pelican_core::time::now()))),
                ..Default::default()
            },
        )
        .await?;
        sync_permissions(&txn, id, &collected(&data)).await?;
        sync_nodes(&txn, id, &parse_ids(&data.nodes)).await?;
    }
    txn.commit().await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&format!("/admin/roles/{id}/edit")))
}

/// Header DeleteAction: blocked for Root Admin or roles in use, pivots
/// detached like spatie's deleting hook before removing the row.
async fn delete(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("delete role")?;
    match delete_one(&admin, id).await? {
        Ok(()) => {
            admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
            Ok(admin.redirect("/admin/roles"))
        }
        Err(msg) => {
            admin.flash_danger(msg, None);
            Ok(admin.redirect(&format!("/admin/roles/{id}/edit")))
        }
    }
}

/// `DeleteBulkAction('exclude_bulk_delete')`.
async fn bulk_delete(admin: Admin, form: Form) -> WebResult {
    admin.authorize("delete role")?;
    let ids = super::bulk_ids(&form);
    let mut failed = 0;
    for id in &ids {
        if !matches!(delete_one(&admin, *id).await, Ok(Ok(()))) {
            failed += 1;
        }
    }
    super::bulk_delete_notify(&admin, ids.len(), failed);
    Ok(admin.redirect(&form.back("/admin/roles")))
}

/// Role deletion (spatie `deleting` hooks): refused for Root Admin or while
/// users hold the role.
async fn delete_one(admin: &Admin, id: i64) -> Result<Result<(), String>, WebError> {
    let role = find_role(admin, id).await?;
    let users =
        model_has_roles::Entity::find().filter(model_has_roles::Column::RoleId.eq(Int(id))).count(&admin.st.db).await?;
    if role.name == ROOT_ADMIN {
        return Ok(Err(admin.t("admin/role.root_admin_delete")));
    }
    if users >= 1 {
        return Ok(Err(admin.t("admin/role.in_use")));
    }
    let txn = admin.st.db.begin().await?;
    let b = txn.get_database_backend();
    txn.execute_raw(raw::stmt(b, "delete from {role_has_permissions} where {role_id} = ?", vec![id.into()])).await?;
    txn.execute_raw(raw::stmt(b, "delete from {model_has_roles} where {role_id} = ?", vec![id.into()])).await?;
    txn.execute_raw(raw::stmt(b, "delete from {node_role} where {role_id} = ?", vec![id.into()])).await?;
    roles::Entity::delete_by_id(role.id).exec(&txn).await?;
    txn.commit().await?;
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_cover_permission_list() {
        // 12 standard models + 5 special groups.
        assert_eq!(all_sections().len(), 17);
        let fields: Vec<String> = all_sections().into_iter().map(|(_, f)| f).collect();
        for f in ["apikey_list", "databasehost_list", "activitylog_list", "backuphost_list"] {
            assert!(fields.contains(&f.to_string()));
        }
    }

    #[test]
    fn egg_gets_specific_permissions() {
        let egg = section_perms("egg");
        assert_eq!(egg.last().map(String::as_str), Some("export egg"));
        assert_eq!(egg.first().map(String::as_str), Some("viewList egg"));
        // Special groups replace the standard prefixes.
        assert_eq!(section_perms("activityLog"), vec!["seeIps activityLog"]);
        let settings = section_perms("settings");
        assert_eq!(settings, vec!["view settings", "update settings"]);
    }

    #[test]
    fn unique_keeps_first_seen_order() {
        let v = unique(vec!["b".into(), "a".into(), "b".into()]);
        assert_eq!(v, ["b", "a"]);
    }
}
