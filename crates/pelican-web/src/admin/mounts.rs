//! Admin › Mounts (app/Filament/Admin/Resources/Mounts). Reference resource:
//! list (table), create/edit (form + validation), delete.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::{
    model_events,
    validation::{CustomRule, Validator},
};
use pelican_core::state::AppState;
use pelican_db::entities::{eggs, mountables, mounts, nodes};
use pelican_db::{Int, SmallBool, raw};
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde_json::Value;

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const SLUG: &str = "mounts";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/mounts", get(index))
        .route("/admin/mounts/create", get(create_form).post(create))
        .route("/admin/mounts/{id}", get(view))
        .route("/admin/mounts/{id}/edit", get(edit_form).post(update))
        .route("/admin/mounts/{id}/delete", post(delete))
        .route("/admin/mounts/bulk-delete", post(bulk_delete))
}

/// `MountResource::getEloquentQuery()`: mounts on accessible nodes or on no node.
fn visible(admin: &Admin, node_ids: &[i64]) -> bool {
    node_ids.is_empty() || node_ids.iter().any(|n| admin.user.can_target_node(*n))
}

/// Egg and node ids attached to each mount.
async fn relations(admin: &Admin, ids: &[i64]) -> Result<HashMap<i64, (Vec<i64>, Vec<i64>)>, WebError> {
    let mut out: HashMap<i64, (Vec<i64>, Vec<i64>)> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows =
        mountables::Entity::find().filter(mountables::Column::MountId.is_in(ids.to_vec())).all(&admin.st.db).await?;
    for r in rows {
        let e = out.entry(r.mount_id.0).or_default();
        match r.mountable_type.as_str() {
            "egg" => e.0.push(r.mountable_id.0),
            "node" => e.1.push(r.mountable_id.0),
            _ => {}
        }
    }
    Ok(out)
}

async fn names<E: EntityTrait>(admin: &Admin) -> Result<HashMap<i64, String>, WebError>
where
    E::Model: NamedModel,
{
    Ok(E::find().all(&admin.st.db).await?.into_iter().map(|m| (m.id(), m.name())).collect())
}

pub trait NamedModel {
    fn id(&self) -> i64;
    fn name(&self) -> String;
}

impl NamedModel for eggs::Model {
    fn id(&self) -> i64 {
        self.id.0
    }
    fn name(&self) -> String {
        self.name.clone()
    }
}

impl NamedModel for nodes::Model {
    fn id(&self) -> i64 {
        self.id.0
    }
    fn name(&self) -> String {
        self.name.clone()
    }
}

fn bool_badge(on: bool, on_label: String, off_label: String, on_icon: &str, off_icon: &str) -> Cell {
    Cell::Badge {
        text: if on { on_label } else { off_label },
        color: if on { "success" } else { "warning" }.into(),
        icon: Some(if on { on_icon } else { off_icon }.into()),
        tooltip: None,
        copy: None,
    }
}

/// `GET /admin/mounts` (ListMounts).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList mount")?;
    let opts = [5, 10, 25, 50, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    let mut q = mounts::Entity::find();
    q = match p.sort.as_deref() {
        Some("name") if p.desc => q.order_by_desc(mounts::Column::Name),
        Some("name") => q.order_by_asc(mounts::Column::Name),
        _ => q.order_by_asc(mounts::Column::Id),
    };
    let all = q.all(&admin.st.db).await?;
    let ids: Vec<i64> = all.iter().map(|m| m.id.0).collect();
    let rel = relations(&admin, &ids).await?;
    let visible_rows: Vec<&mounts::Model> =
        all.iter().filter(|m| visible(&admin, rel.get(&m.id.0).map(|r| r.1.as_slice()).unwrap_or(&[]))).collect();
    let total = visible_rows.len() as u64;
    let page: Vec<&&mounts::Model> = match p.limit_offset() {
        Some((limit, offset)) => visible_rows.iter().skip(offset as usize).take(limit as usize).collect(),
        None => visible_rows.iter().collect(),
    };
    let egg_names = names::<eggs::Entity>(&admin).await?;
    let node_names = names::<nodes::Entity>(&admin).await?;
    let t = |k: &str| admin.t(&format!("admin/mount.{k}"));
    let can_edit = admin.can("update mount");
    let mut table = Table::new("mounts", "/admin/mounts");
    table.columns = vec![
        Column::new("name", t("table.name")).sortable(),
        Column::new("eggs", t("eggs")),
        Column::new("nodes", t("nodes")),
        Column::new("read_only", t("table.read_only")),
        Column::new("user_mountable", t("table.user_mountable")),
    ];
    for m in page {
        let (egg_ids, node_ids) = rel.get(&m.id.0).cloned().unwrap_or_default();
        let list = |ids: &[i64], map: &HashMap<i64, String>, placeholder: String| {
            if ids.is_empty() {
                Cell::Text { text: placeholder, desc: None, mono: false, copy: None, tooltip: None }
            } else {
                Cell::Badges {
                    items: ids.iter().filter_map(|i| map.get(i).cloned()).collect(),
                    color: "primary".into(),
                }
            }
        };
        let url = if can_edit { format!("/admin/mounts/{}/edit", m.id.0) } else { format!("/admin/mounts/{}", m.id.0) };
        let mut row = Row::new(m.id.0.to_string())
            .url(url.clone())
            .cell("name", Cell::text_desc(m.name.clone(), Some(format!("{} -> {}", m.source, m.target))))
            .cell("eggs", list(&egg_ids, &egg_names, t("table.all_eggs")))
            .cell("nodes", list(&node_ids, &node_names, t("table.all_nodes")))
            .cell(
                "read_only",
                bool_badge(m.read_only.0, t("toggles.read_only"), t("toggles.writable"), "writing-off", "writing"),
            )
            .cell(
                "user_mountable",
                bool_badge(
                    m.user_mountable.0,
                    t("toggles.user_mountable"),
                    t("toggles.not_user_mountable"),
                    "user",
                    "user-off",
                ),
            );
        row.actions.push(if can_edit {
            Action::link(admin.t("filament-actions::edit.single.label"), "pencil", url)
        } else {
            Action::link(admin.t("filament-actions::view.single.label"), "eye", url)
        });
        table.rows.push(row);
    }
    if admin.can("delete mount") {
        table.bulk.push(
            Action::post(admin.t("filament-actions::delete.multiple.label"), "trash", "/admin/mounts/bulk-delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
        );
    }
    table.empty_icon = "layers-linked".into();
    table.empty_heading = t("no_mounts");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create mount").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => "/admin/mounts/create",
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

/// Form state shared by create and edit.
struct FormData {
    name: String,
    description: String,
    source: String,
    target: String,
    read_only: bool,
    user_mountable: bool,
    eggs: Vec<String>,
    nodes: Vec<String>,
}

impl FormData {
    fn from_model(m: &mounts::Model, eggs: &[i64], nodes: &[i64]) -> FormData {
        FormData {
            name: m.name.clone(),
            description: m.description.clone().unwrap_or_default(),
            source: m.source.clone(),
            target: m.target.clone(),
            read_only: m.read_only.0,
            user_mountable: m.user_mountable.0,
            eggs: eggs.iter().map(i64::to_string).collect(),
            nodes: nodes.iter().map(i64::to_string).collect(),
        }
    }

    fn from_form(f: &Form) -> FormData {
        FormData {
            name: f.get("name").trim().into(),
            description: f.get("description").trim().into(),
            source: f.get("source").trim().into(),
            target: f.get("target").trim().into(),
            read_only: f.bool("read_only"),
            user_mountable: f.bool("user_mountable"),
            eggs: f.list("eggs"),
            nodes: f.list("nodes"),
        }
    }

    fn ctx(&self) -> Mj {
        context! {
            name => self.name, description => self.description, source => self.source, target => self.target,
            read_only => if self.read_only { "1" } else { "0" },
            user_mountable => if self.user_mountable { "1" } else { "0" },
            eggs => self.eggs, nodes => self.nodes,
        }
    }
}

async fn render_form(admin: &Admin, record: Option<&mounts::Model>, data: &FormData, errors: &Errors) -> WebResult {
    let t = |k: &str| admin.t(&format!("admin/mount.{k}"));
    let egg_opts: Vec<(String, String)> = eggs::Entity::find()
        .order_by_asc(eggs::Column::Name)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .map(|e| (e.id.0.to_string(), e.name))
        .collect();
    let node_opts: Vec<(String, String)> = nodes::Entity::find()
        .order_by_asc(nodes::Column::Name)
        .all(&admin.st.db)
        .await?
        .into_iter()
        .filter(|n| admin.user.can_target_node(n.id.0))
        .map(|n| (n.id.0.to_string(), n.name))
        .collect();
    let (title, action) = match record {
        Some(m) => (
            admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &m.name)]),
            format!("/admin/mounts/{}/edit", m.id.0),
        ),
        None => (
            admin.t_with("filament-panels::resources/pages/create-record.title", &[("label", &t("model_label"))]),
            "/admin/mounts/create".into(),
        ),
    };
    let readonly = record.is_some() && !admin.can("update mount");
    admin
        .page(
            SLUG,
            "admin/mounts/form.html",
            context! {
                title => title,
                action => action,
                record => record.map(|m| context! { id => m.id.0, name => m.name.clone() }),
                readonly => readonly,
                can_delete => record.is_some() && admin.can("delete mount"),
                v => data.ctx(),
                errors => Mj::from_serialize(errors),
                egg_options => egg_opts,
                node_options => node_opts,
            },
        )
        .await
}

async fn find(admin: &Admin, id: i64) -> Result<(mounts::Model, Vec<i64>, Vec<i64>), WebError> {
    let m = mounts::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)?;
    let (e, n) = relations(admin, &[id]).await?.remove(&id).unwrap_or_default();
    if !visible(admin, &n) {
        return Err(WebError::NotFound);
    }
    Ok((m, e, n))
}

async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create mount")?;
    let data = FormData {
        name: String::new(),
        description: String::new(),
        source: String::new(),
        target: String::new(),
        read_only: false,
        user_mountable: true,
        eggs: vec![],
        nodes: vec![],
    };
    render_form(&admin, None, &data, &Errors::default()).await
}

async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view mount")?;
    let (m, e, n) = find(&admin, id).await?;
    render_form(&admin, Some(&m), &FormData::from_model(&m, &e, &n), &Errors::default()).await
}

async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("update mount")?;
    let (m, e, n) = find(&admin, id).await?;
    render_form(&admin, Some(&m), &FormData::from_model(&m, &e, &n), &Errors::default()).await
}

/// Form rules (Filament fields) + `Mount::$validationRules`.
async fn validate(admin: &Admin, data: &FormData, ignore: Option<i64>) -> Result<Errors, WebError> {
    let input = serde_json::json!({
        "name": data.name, "description": if data.description.is_empty() { Value::Null } else { Value::from(data.description.clone()) },
        "source": data.source, "target": data.target,
    });
    let mut v = Validator::for_value(&input);
    let t = |k: &str| admin.t(&format!("admin/mount.{k}"));
    for f in ["name", "description", "source", "target"] {
        v.attribute_name(f, &t(f).to_lowercase());
    }
    v.rules("name", &["required", "string", "min:2", "max:64"]);
    let mut q = mounts::Entity::find().filter(mounts::Column::Name.eq(data.name.clone()));
    if let Some(id) = ignore {
        q = q.filter(mounts::Column::Id.ne(id));
    }
    let taken = !data.name.is_empty() && q.one(&admin.st.db).await?.is_some();
    v.custom(
        "name",
        CustomRule::precomputed("unique", taken.then(|| "The :attribute has already been taken.".to_string())),
    );
    v.rules("description", &["nullable", "string", "max:255"]);
    v.rules("source", &["required", "string", "max:255"]);
    v.rules("target", &["required", "string", "max:255"]);
    v.run();
    Ok(Errors::from_validator(&v))
}

fn parse_ids(list: &[String]) -> Vec<i64> {
    let mut ids: Vec<i64> = list.iter().filter_map(|s| s.parse().ok()).collect();
    ids.dedup();
    ids
}

/// Replace the egg/node pivots of a mount (`sync()`).
async fn sync<C: ConnectionTrait>(db: &C, mount: i64, kind: &str, ids: &[i64]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(
        b,
        "delete from {mountables} where {mount_id} = ? and {mountable_type} = ?",
        vec![mount.into(), kind.into()],
    ))
    .await?;
    for id in ids {
        db.execute_raw(raw::stmt(
            b,
            "insert into {mountables} ({mountable_type}, {mountable_id}, {mount_id}) values (?, ?, ?)",
            vec![kind.into(), (*id).into(), mount.into()],
        ))
        .await?;
    }
    Ok(())
}

async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create mount")?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, None).await?;
    if !errors.is_empty() {
        return render_form(&admin, None, &data, &errors).await;
    }
    let txn = admin.st.db.begin().await?;
    let am = mounts::ActiveModel {
        uuid: Set(uuid::Uuid::new_v4().to_string()),
        name: Set(data.name.clone()),
        description: Set((!data.description.is_empty()).then(|| data.description.clone())),
        source: Set(data.source.clone()),
        target: Set(data.target.clone()),
        read_only: Set(SmallBool(data.read_only)),
        user_mountable: Set(SmallBool(data.user_mountable)),
        ..Default::default()
    };
    let id = pelican_db::write::insert_id(&txn, &am).await?.ok_or_else(|| WebError::internal("no id"))?;
    sync(&txn, id, "egg", &parse_ids(&data.eggs)).await?;
    let nodes: Vec<i64> = parse_ids(&data.nodes).into_iter().filter(|n| admin.user.can_target_node(*n)).collect();
    sync(&txn, id, "node", &nodes).await?;
    txn.commit().await?;
    // CreateRecord → `Mount::create($data)`.
    model_events::fire_id::<mounts::Entity>(&admin.st, model_events::Action::Created, id).await;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("/admin/mounts/{id}/edit")))
}

async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    admin.authorize("update mount")?;
    let (m, _, _) = find(&admin, id).await?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, Some(id)).await?;
    if !errors.is_empty() {
        return render_form(&admin, Some(&m), &data, &errors).await;
    }
    let txn = admin.st.db.begin().await?;
    let am = mounts::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(Int(id)),
        name: Set(data.name.clone()),
        description: Set((!data.description.is_empty()).then(|| data.description.clone())),
        source: Set(data.source.clone()),
        target: Set(data.target.clone()),
        read_only: Set(SmallBool(data.read_only)),
        user_mountable: Set(SmallBool(data.user_mountable)),
        ..Default::default()
    };
    // EditRecord → `$record->update($data)`; the event fires only when a
    // column actually changed (Eloquent isDirty — no updated_at bump here).
    let changed = data.name != m.name
        || (!data.description.is_empty()).then(|| data.description.clone()) != m.description
        || data.source != m.source
        || data.target != m.target
        || SmallBool(data.read_only) != m.read_only
        || SmallBool(data.user_mountable) != m.user_mountable;
    pelican_db::write::update(&txn, &am).await?;
    sync(&txn, id, "egg", &parse_ids(&data.eggs)).await?;
    let nodes: Vec<i64> = parse_ids(&data.nodes).into_iter().filter(|n| admin.user.can_target_node(*n)).collect();
    sync(&txn, id, "node", &nodes).await?;
    txn.commit().await?;
    if changed {
        model_events::fire_id::<mounts::Entity>(&admin.st, model_events::Action::Updated, id).await;
    }
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&format!("/admin/mounts/{id}/edit")))
}

async fn delete(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("delete mount")?;
    match delete_one(&admin, id).await? {
        Ok(()) => {
            admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
            Ok(admin.redirect("/admin/mounts"))
        }
        Err(msg) => {
            admin.flash_danger(msg, None);
            Ok(admin.redirect(&format!("/admin/mounts/{id}/edit")))
        }
    }
}

/// `DeleteBulkAction('exclude_bulk_delete')`.
async fn bulk_delete(admin: Admin, form: Form) -> WebResult {
    admin.authorize("delete mount")?;
    let ids = super::bulk_ids(&form);
    let mut failed = 0;
    for id in &ids {
        if !matches!(delete_one(&admin, *id).await, Ok(Ok(()))) {
            failed += 1;
        }
    }
    super::bulk_delete_notify(&admin, ids.len(), failed);
    Ok(admin.redirect(&form.back("/admin/mounts")))
}

/// Mount deletion (the `deleting` hook refuses mounts still attached to servers).
async fn delete_one(admin: &Admin, id: i64) -> Result<Result<(), String>, WebError> {
    let (m, _, _) = find(admin, id).await?;
    let attached = mountables::Entity::find()
        .filter(mountables::Column::MountId.eq(id))
        .filter(mountables::Column::MountableType.eq("server"))
        .one(&admin.st.db)
        .await?
        .is_some();
    if attached {
        return Ok(Err(admin.t("exceptions.mount.servers_attached")));
    }
    let txn = admin.st.db.begin().await?;
    txn.execute_raw(raw::stmt(
        txn.get_database_backend(),
        "delete from {mountables} where {mount_id} = ?",
        vec![id.into()],
    ))
    .await?;
    mounts::Entity::delete_by_id(m.id).exec(&txn).await?;
    txn.commit().await?;
    // DeleteAction / DeleteBulkAction → `$record->delete()` per mount.
    model_events::deleted(&admin.st, &m).await;
    Ok(Ok(()))
}
