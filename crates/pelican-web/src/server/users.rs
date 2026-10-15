//! Server panel › Users / subusers (app/Filament/Server/Resources/Subusers):
//! list, the invite action (`SubuserCreationService`), permission editing
//! (`SubuserUpdateService`) and deletion (`SubuserDeletionService`) — reusing
//! the Client API logic of `pelican_api::client::subusers`.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::error::ApiError;
use pelican_api::pipeline::Ctx;
use pelican_api::services::{LogActivity, account_created, subuser_mail};
use pelican_api::{binding::Bound, client::subusers as api, model_events};
use pelican_core::activity::morph;
use pelican_core::events::{FilamentNotification, filament_button};
use pelican_core::i18n::trans;
use pelican_core::state::AppState;
use pelican_db::entities::{servers, subusers, users};
use pelican_db::{Int, Json, Timestamp};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde::Serialize;
use serde_json::{Value, json};

use super::{RecordId, ServerPanel};
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/server/{server}/users", get(index).post(invite))
        .route("/server/{server}/users/{id}", get(edit))
        .route("/server/{server}/users/{id}/update", post(update))
        .route("/server/{server}/users/{id}/delete", post(delete))
}

/// A client-API `Ctx` for the domain functions: ip and actor like this web
/// session, default subject = the server (`ServerSubject` of the client group).
fn api_ctx(p: &ServerPanel) -> Result<Ctx, WebError> {
    let ip: IpAddr = p.ip.parse().map_err(|_| WebError::internal("invalid request ip"))?;
    Ok(Ctx {
        ip,
        user: Some(Arc::new(p.user.clone())),
        bound: Arc::new(Bound { server: Some(p.server.clone()), ..Default::default() }),
        node: None,
        root: p.st.config.app_url.clone(),
        subject: Some((morph::SERVER, p.server.id.0)),
    })
}

// ----- permission catalogue (SubuserPermission groups; `websocket.connect`
// is hidden and never shown in the tabs) -------------------------------

/// `(group, icon, actions)` in enum declaration order —
/// `Subuser::allPermissionData()` with hidden groups filtered out.
const GROUPS: &[(&str, &str, &[&str])] = &[
    ("control", "terminal-2", &["console", "start", "stop", "restart"]),
    ("file", "files", &["read", "read-content", "create", "update", "delete", "archive", "sftp"]),
    ("backup", "file-zip", &["read", "create", "delete", "download", "restore"]),
    ("schedule", "clock", &["read", "create", "update", "delete"]),
    ("user", "users", &["read", "create", "update", "delete"]),
    ("database", "database", &["read", "create", "update", "delete", "view-password"]),
    ("allocation", "network", &["read", "create", "update", "delete"]),
    ("activity", "stack", &["read"]),
    ("mount", "layers-linked", &["read", "update"]),
    ("startup", "player-play", &["read", "update", "docker-image"]),
    ("settings", "settings", &["rename", "description", "reinstall", "change-icon"]),
];

/// PHP `Str::headline` as used for the checkbox options.
fn headline(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.char_indices()
                .map(|(i, c)| if i == 0 { c.to_uppercase().to_string() } else { c.to_string() })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Serialize)]
struct PermOpt {
    value: String,
    label: String,
    desc: String,
    checked: bool,
}

#[derive(Serialize)]
struct PermGroup {
    key: String,
    icon: &'static str,
    label: String,
    desc: String,
    options: Vec<PermOpt>,
}

/// Core groups, then the plugins' (`Subuser::registerCustomPermissions`):
/// `(key, icon, actions)`.
fn all_groups() -> Vec<(String, &'static str, Vec<String>)> {
    let mut out: Vec<(String, &'static str, Vec<String>)> = GROUPS
        .iter()
        .map(|(k, i, perms)| ((*k).to_string(), *i, perms.iter().map(|p| (*p).to_string()).collect()))
        .collect();
    for g in pelican_api::sidecar::all_subuser_permissions() {
        match out.iter_mut().find(|(k, _, _)| *k == g.name) {
            Some((_, _, perms)) => {
                perms.extend(g.permissions.into_iter().filter(|p| !perms.contains(p)).collect::<Vec<_>>())
            }
            None => out.push((g.name, crate::plugin_pages::icon(g.icon.as_deref()), g.permissions)),
        }
    }
    out
}

/// A translation, or `fallback` when the key has none.
fn t_or(p: &ServerPanel, key: &str, fallback: String) -> String {
    let line = p.t(key);
    if line == key { fallback } else { line }
}

/// The tab structure of invite/edit forms: every group with its checkbox list.
fn perm_groups(p: &ServerPanel, selected: &HashSet<String>) -> Vec<PermGroup> {
    all_groups()
        .into_iter()
        .map(|(key, icon, perms)| PermGroup {
            label: t_or(p, &format!("server/user.permissions.{key}_title"), headline(&key)),
            desc: t_or(p, &format!("server/user.permissions.{key}_desc"), String::new()),
            options: perms
                .iter()
                .map(|v| PermOpt {
                    value: v.clone(),
                    label: headline(v),
                    desc: t_or(p, &format!("server/user.permissions.{key}_{}", v.replace('-', "_")), String::new()),
                    // The stored form is `$group.$action` (`mutateRecordDataUsing`).
                    checked: selected.contains(&format!("{key}.{v}")),
                })
                .collect(),
            key,
            icon,
        })
        .collect()
}

/// Permissions posted by a dialog: valid `group.action` pairs only
/// (`SubuserPermission` values), plus `websocket.connect` always pushed
/// (`push(SubuserPermission::WebsocketConnect)->unique()`), first seen order.
fn collect_perms(form: &Form) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (key, _, perms) in all_groups() {
        let valid: HashSet<&str> = perms.iter().map(String::as_str).collect();
        for v in form.list(&format!("perm_{key}")) {
            if valid.contains(v.as_str()) {
                let perm = format!("{key}.{v}");
                if !out.contains(&perm) {
                    out.push(perm);
                }
            }
        }
    }
    if !out.iter().any(|x| x == "websocket.connect") {
        out.push("websocket.connect".into());
    }
    out
}

/// Stored permissions of a subuser row (JSON array or object — historic rows).
fn sub_permissions(sub: &subusers::Model) -> Vec<String> {
    match sub.permissions.as_ref().map(|j| &j.0) {
        Some(Value::Array(a)) => a.iter().map(pelican_core::php::string).collect(),
        Some(Value::Object(m)) => m.values().map(pelican_core::php::string).collect(),
        _ => Vec::new(),
    }
}

/// `permissions_count` column: visible permissions only (`isHidden()` is
/// true just for `websocket.connect`).
fn visible_permission_count(sub: &subusers::Model) -> i64 {
    let perms = sub_permissions(sub);
    let has_ws = perms.iter().any(|p| p == "websocket.connect");
    (perms.len() as i64) - i64::from(has_ws)
}

/// Initials avatar data URI (ADR-0013 — no external avatar provider), as in
/// `crate::admin::users`.
fn avatar_src(name: &str) -> String {
    let esc = |s: &str| {
        s.chars()
            .map(|c| match c {
                '<' => "%3C".to_string(),
                '>' => "%3E".to_string(),
                '"' => "%22".to_string(),
                '&' => "%26".to_string(),
                '#' => "%23".to_string(),
                c => c.to_string(),
            })
            .collect::<String>()
    };
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"40\" viewBox=\"0 0 40 40\">\
         <rect width=\"40\" height=\"40\" rx=\"20\" fill=\"#6574cd\"/>\
         <text x=\"20\" y=\"26\" font-family=\"sans-serif\" font-size=\"16\" fill=\"#fff\" \
         text-anchor=\"middle\">{}</text></svg>",
        crate::activity::initials(name)
    );
    format!("data:image/svg+xml,{}", esc(&svg))
}

// ----- rows ------------------------------------------------------------

async fn find_subuser(p: &ServerPanel, id: i64) -> Result<(subusers::Model, users::Model), WebError> {
    let sub = subusers::Entity::find_by_id(Int(id))
        .filter(subusers::Column::ServerId.eq(p.server.id))
        .one(&p.st.db)
        .await?
        .ok_or(WebError::NotFound)?;
    let user = users::Entity::find_by_id(sub.user_id).one(&p.st.db).await?.ok_or(WebError::NotFound)?;
    Ok((sub, user))
}

#[derive(Clone)]
struct SubRow {
    sub: subusers::Model,
    username: String,
    email: String,
}

/// Every subuser of the server joined with its user (`$server->subusers`,
/// natural order).
async fn sub_rows(p: &ServerPanel) -> Result<Vec<SubRow>, WebError> {
    let subs = subusers::Entity::find()
        .filter(subusers::Column::ServerId.eq(p.server.id))
        .order_by_asc(subusers::Column::Id)
        .all(&p.st.db)
        .await?;
    let ids: Vec<Int> = subs.iter().map(|s| s.user_id).collect();
    let people: Vec<users::Model> = if ids.is_empty() {
        Vec::new()
    } else {
        users::Entity::find().filter(users::Column::Id.is_in(ids)).all(&p.st.db).await?
    };
    let mut out = Vec::new();
    for s in subs {
        if let Some(u) = people.iter().find(|u| u.id == s.user_id) {
            out.push(SubRow { sub: s, username: u.username.clone(), email: u.email.clone() });
        }
    }
    Ok(out)
}

// ----- invite dialog state ---------------------------------------------

#[derive(Clone, Debug, Default)]
struct InviteState {
    email: String,
}

impl InviteState {
    fn from_form(f: &Form) -> InviteState {
        // Laravel trims strings globally (`TrimStrings`) but does not lowercase
        // before validation (`strtolower` happens inside the action).
        InviteState { email: f.opt("email").unwrap_or("").trim().to_string() }
    }

    fn initial() -> InviteState {
        InviteState::default()
    }
}

/// TextInput rules: `email()` + `required()` (+ `between:1,255`, the value
/// range the creation service validates for `User::$validationRules`).
fn validate_invite(p: &ServerPanel, state: &InviteState) -> Errors {
    let input = json!({
        "email": if state.email.is_empty() { Value::Null } else { Value::from(state.email.clone()) },
    });
    let mut v = pelican_api::validation::Validator::for_value(&input);
    v.attribute_name("email", &p.t("server/user.email").to_lowercase());
    v.rules("email", &["required", "email", "between:1,255"]);
    v.run();
    Errors::from_validator(&v)
}

// ----- render ----------------------------------------------------------

/// List page incl. invite dialog state and any open edit dialog
/// (`open_edit` reopens one via GET `/users/{id}`, mirroring EditAction).
#[allow(clippy::too_many_arguments)]
async fn render_list(
    p: ServerPanel,
    lp: ListParams,
    invite: Option<InviteState>,
    errors: Errors,
    open_edit: Option<i64>,
) -> WebResult {
    let base = p.base_url();
    let can_create = p.can("user.create");
    let can_edit = p.can("user.update");
    let can_delete = p.can("user.delete");
    let viewer = p.user.id();

    let t = |k: &str| p.t(&format!("server/user.{k}"));

    // Rows after search + sort (the original paginates nothing and searches
    // `username`/`email`; subuser lists are small, so both happen in memory).
    let query = lp.search.trim().to_lowercase();
    let mut rows: Vec<SubRow> = sub_rows(&p).await?.into_iter().filter(|r| r.matches_search(&query)).collect();
    match lp.sort.as_deref() {
        Some("username") => {
            rows.sort_by_key(|r| r.username.to_lowercase());
            if lp.desc {
                rows.reverse();
            }
        }
        Some("email") => {
            rows.sort_by_key(|r| r.email.to_lowercase());
            if lp.desc {
                rows.reverse();
            }
        }
        _ => {}
    }
    let total = rows.len() as u64;

    let mut table = Table::new("users", &format!("{base}/users"));
    table.columns = vec![
        Column::new("picture", "").center().wide_only(),
        Column::new("username", t("username")).sortable(),
        Column::new("email", t("email")).sortable(),
        Column::new("permissions_count", t("permissions.title")),
    ];
    table.searchable = true;
    let edit_label = t("edit");
    let delete_label = t("delete");
    for r in &rows {
        let own = r.sub.user_id.0 == viewer;
        let mut row = Row::new(r.sub.id.0.to_string())
            .cell("picture", Cell::Image { src: avatar_src(&r.username) })
            .cell("username", Cell::text(r.username.clone()))
            .cell("email", Cell::text(r.email.clone()))
            .cell("permissions_count", Cell::text(visible_permission_count(&r.sub).to_string()));
        // Both row actions are hidden on your own entry (`user()?->id === $subuser->user->id`)
        // and authorized against their permissions at click time.
        if can_edit && !own {
            // EditAction opens its modal; here a scoped URL that renders the page
            // with this subuser's dialog opened.
            row.actions.push(Action::link(edit_label.clone(), "edit", format!("{base}/users/{}", r.sub.id.0)));
        }
        if can_delete && !own {
            row.actions.push(
                Action::post(delete_label.clone(), "trash", format!("{base}/users/{}/delete", r.sub.id.0))
                    .color("danger")
                    .confirm(p.t("filament-actions::modal.confirmation")),
            );
        }
        table.rows.push(row);
    }
    table.empty_icon = "users".into();
    table.empty_heading = p.t_with("filament-tables::table.empty.heading", &[("model", "Subuser")]);
    table.empty_description = p.t_with("filament-tables::table.empty.description", &[("model", "Subuser")]);
    // `paginated(false)` — no per-page control, everything listed.
    table.finish(&lp, total, &[0], &p.locale());

    // Dialog contexts: invite shares the group catalogue, edit dialogs get
    // their checkboxes pre-filled from the stored permissions.
    let blank: HashSet<String> = HashSet::new();
    let invite_groups = Mj::from_serialize(perm_groups(&p, &blank));
    let mut dialogs = Vec::new();
    for r in &rows {
        if !(can_edit && r.sub.user_id.0 != viewer) {
            continue;
        }
        let selected: HashSet<String> = sub_permissions(&r.sub).into_iter().collect();
        dialogs.push(context! {
            id => r.sub.id.0,
            email => r.email.clone(),
            groups => Mj::from_serialize(perm_groups(&p, &selected)),
            open => open_edit == Some(r.sub.id.0),
        });
    }
    let state = invite.unwrap_or_else(InviteState::initial);

    p.page(
        "users",
        "server/users.html",
        context! {
            title => t("title"),
            base => base,
            table => Mj::from_serialize(&table),
            can_create => can_create,
            invite_groups => invite_groups,
            v_email => state.email,
            v_errors => Mj::from_serialize(&errors),
            dialogs => dialogs,
        },
    )
}

impl SubRow {
    fn matches_search(&self, q: &str) -> bool {
        q.is_empty() || self.username.to_lowercase().contains(q) || self.email.to_lowercase().contains(q)
    }
}

/// `GET /server/{server}/users` (ListSubusers → SubuserResource::defaultTable).
async fn index(p: ServerPanel) -> WebResult {
    p.authorize("user.read")?;
    let lp = ListParams::parse(p.query.as_deref(), &[0], 0);
    render_list(p, lp, None, Errors::default(), None).await
}

/// `GET /server/{server}/users/{id}` — the EditAction modal, rendered by
/// loading the list page with this subuser's dialog opened.
async fn edit(p: ServerPanel, RecordId(id): RecordId) -> WebResult {
    p.authorize("user.read")?;
    p.authorize("user.update")?; // EditAction::authorize
    let (sub, _) = find_subuser(&p, id).await?;
    let lp = ListParams::parse(p.query.as_deref(), &[0], 0);
    render_list(p, lp, None, Errors::default(), Some(sub.id.0)).await
}

// ----- actions ---------------------------------------------------------

/// First detail of an API error as the notification body (`$exception->getMessage()`).
fn error_body(e: &ApiError) -> Option<String> {
    e.errors.first().map(|o| o.detail.clone()).filter(|d| !d.is_empty())
}

/// `POST /server/{server}/users` — the toolbar CreateAction: validation, then
/// `SubuserCreationService::handle()` through the Client API logic.
async fn invite(p: ServerPanel, form: Form) -> WebResult<Response> {
    p.authorize("user.create")?;
    let base = p.base_url();
    let list_url = format!("{base}/users");
    let state = InviteState::from_form(&form);

    let errors = validate_invite(&p, &state);
    if !errors.is_empty() {
        let lp = ListParams::parse(p.query.as_deref(), &[0], 0);
        return render_list(p, lp, Some(state), errors, None).await;
    }

    let perms = collect_perms(&form);
    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let server: servers::Model = p.server.clone();
    // Activity logs the submitted address while the service lowercases it.
    let submitted_email = state.email.clone();
    let email = submitted_email.to_lowercase();

    let result: Result<users::Model, ApiError> = async {
        let txn = st.db.begin().await?;
        let existing = users::Entity::find().filter(users::Column::Email.eq(email.clone())).one(&txn).await?;
        let mut created_token = None;
        let user = match existing {
            None => {
                let (u, token) = api::create_user(&st, &txn, &email).await?;
                created_token = Some(token);
                u
            }
            Some(u) => {
                if server.owner_id == u.id {
                    return Err(ApiError::display(
                        "UserIsServerOwnerException",
                        400,
                        p.t("exceptions.subusers.user_is_owner"),
                    ));
                }
                let exists = subusers::Entity::find()
                    .filter(subusers::Column::ServerId.eq(server.id))
                    .filter(subusers::Column::UserId.eq(u.id))
                    .one(&txn)
                    .await?
                    .is_some();
                if exists {
                    return Err(ApiError::display(
                        "ServerSubuserExistsException",
                        400,
                        p.t("exceptions.subusers.subuser_exists"),
                    ));
                }
                u
            }
        };
        // SubuserCreationService: keep what the requester possesses, sorted.
        let stored = api::service_permissions(&st, &ctx, &server, perms.clone()).await?;
        let now = Timestamp(pelican_core::time::now());
        let am = subusers::ActiveModel {
            user_id: Set(user.id),
            server_id: Set(server.id),
            permissions: Set(Some(Json(json!(stored)))),
            created_at: Set(Some(now)),
            updated_at: Set(Some(now)),
            ..Default::default()
        };
        // SubuserCreationService → `Subuser::…updateOrCreate`: the Rust path only
        // reaches the insert (an existing subuser errors above) → created event.
        let new_sub = pelican_db::write::insert(&txn, am).await?;
        FilamentNotification {
            title: trans(&user.language, "notifications.user_added.title", &[]),
            body: Some(trans(&user.language, "notifications.user_added.body", &[("server", &server.name)])),
            status: None,
            actions: vec![filament_button(
                "exclude_view",
                &trans(&user.language, "notifications.open_server", &[]),
                &format!("{}/server/{}/console", ctx.root, server.uuid_short),
            )],
        }
        .send(&txn, user.id.0)
        .await?;
        if let Some(token) = &created_token {
            account_created(&st, &user, Some(token)).await?;
        }
        subuser_mail(&st, &user, &server, true).await?;
        txn.commit().await?;
        // After the transaction: webhook delivery is a synchronous HTTP call.
        // UserCreationService (new account) before Subuser::updateOrCreate.
        if created_token.is_some() {
            model_events::created(&st, &user).await;
        }
        model_events::created(&st, &new_sub).await;
        Ok(user)
    }
    .await;
    match result {
        Ok(user) => {
            ctx.activity("server:subuser.create")
                .subject(morph::USER, user.id.0)
                .property("email", submitted_email.clone())
                .property("permissions", json!(perms))
                .log(&st)
                .await
                .map_err(|e| WebError::internal(e.errors.first().map(|o| o.code.clone()).unwrap_or_default()))?;
            p.flash_success(p.t("server/user.notification_add"));
        }
        Err(e) => {
            tracing::warn!(error=?e, "subuser invitation failed");
            p.flash_danger(p.t("server/user.notification_failed"), error_body(&e));
        }
    }
    Ok(p.redirect(&list_url))
}

/// `POST …/{id}/update` — EditAction action: `SubuserUpdateService::handle`.
async fn update(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("user.update")?;
    let base = p.base_url();
    let list_url = format!("{base}/users");
    let (sub, user) = find_subuser(&p, id).await?;

    let requested = collect_perms(&form);
    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let server: servers::Model = p.server.clone();
    let result: Result<(), ApiError> = async {
        // SubuserUpdateService cleaning: possessed only, sorted.
        let cleaned = api::service_permissions(&st, &ctx, &server, requested).await?;
        let mut current: Vec<String> = sub_permissions(&sub);
        current.sort();
        // Only update the database and revoke daemon access on actual changes.
        if cleaned != current {
            let txn = st.db.begin().await?;
            let now = Timestamp(pelican_core::time::now());
            subusers::Entity::update_many()
                .col_expr(
                    subusers::Column::Permissions,
                    pelican_db::write::json_expr(txn.get_database_backend(), Some(&json!(cleaned))),
                )
                .col_expr(subusers::Column::UpdatedAt, Expr::value(now))
                .filter(subusers::Column::Id.eq(sub.id))
                .exec(&txn)
                .await?;
            api::revoke(&st, &txn, &server, &user.uuid).await?;
            ctx.activity("server:subuser.update")
                .subject(morph::USER, user.id.0)
                .property("email", user.email.clone())
                .property("old", json!(current))
                .property("new", json!(cleaned))
                .property("revoked", true)
                .log_in(&st, &txn)
                .await?;
            txn.commit().await?;
            // `$subuser->update(['permissions' => …])` in the service (updated).
            model_events::fire_id::<subusers::Entity>(&st, model_events::Action::Updated, sub.id.0).await;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(p.t("server/user.notification_edit")),
        Err(e) => {
            tracing::warn!(error=?e, "subuser update failed");
            p.flash_danger(p.t("server/user.notification_failed"), error_body(&e));
        }
    }
    Ok(p.redirect(&list_url))
}

/// `POST …/{id}/delete` — DeleteAction action: `SubuserDeletionService::handle`.
async fn delete(p: ServerPanel, RecordId(id): RecordId, form: Form) -> WebResult<Response> {
    p.authorize("user.delete")?;
    let back = form.back(&format!("{}/users", p.base_url()));
    let (sub, user) = find_subuser(&p, id).await?;
    let ctx = api_ctx(&p)?;
    let st = p.st.clone();
    let result: Result<(), ApiError> = async {
        let txn = st.db.begin().await?;
        api::delete_subuser_in(&st, &ctx, &txn, &p.server, &sub, &user).await?;
        txn.commit().await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => p.flash_success(p.t("server/user.notification_delete")),
        Err(e) => {
            tracing::warn!(error=?e, "subuser deletion failed");
            p.flash_danger(p.t("server/user.notification_delete"), error_body(&e));
        }
    }
    Ok(p.redirect(&back))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_matches_php_str_of() {
        assert_eq!(headline("read"), "Read");
        assert_eq!(headline("read-content"), "Read Content");
        assert_eq!(headline("view-password"), "View Password");
        assert_eq!(headline("docker-image"), "Docker Image");
        assert_eq!(headline("change-icon"), "Change Icon");
    }

    #[test]
    fn websocket_connect_is_always_added_once() {
        let mut f = Form::default();
        f.fields.insert("perm_user[]".into(), vec!["read".into(), "delete".into()]);
        f.fields.insert("perm_file[]".into(), vec!["read-content".into(), "bogus".into()]);
        f.fields.insert("perm_unknown[]".into(), vec!["whatever".into()]);
        assert_eq!(collect_perms(&f), vec!["file.read-content", "user.read", "user.delete", "websocket.connect"]);
    }
}
