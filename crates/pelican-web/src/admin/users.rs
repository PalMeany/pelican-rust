//! Admin › Users (app/Filament/Admin/Resources/Users): list, create/view/edit
//! (account/roles/keys/activity tabs), delete, plus the Servers relation
//! manager rendered under the record form (`ServersRelationManager`).

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use minijinja::{Value as Mj, context};
use pelican_api::validation::{CustomRule, Validator};
use pelican_api::{application::servers as app_servers, binding::Bound, model_events};
use pelican_core::activity::morph;
use pelican_core::state::AppState;
use pelican_db::entities::{
    activity_logs, allocations, api_keys, eggs, model_has_roles, nodes, password_resets, roles, servers, user_ssh_keys,
    users,
};
use pelican_db::{Bool, Int, Json, Timestamp, raw};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    TransactionTrait,
};
use serde_json::{Value, json};

use super::Admin;
use crate::error::{WebError, WebResult};
use crate::forms::{Errors, Form};
use crate::table::{Action, Cell, Column, ListParams, Row, Table};

const SLUG: &str = "users";
const TABS: [&str; 4] = ["account", "roles", "keys", "activity"];
/// `Role::ROOT_ADMIN`.
const ROOT_ADMIN: &str = pelican_core::auth::ROOT_ADMIN_ROLE;

static PHP_LISTS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../pelican-api/data/php-lists.json")).unwrap_or(Value::Null)
});

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(index))
        .route("/admin/users/create", get(create_form).post(create))
        .route("/admin/users/{id}", get(view))
        .route("/admin/users/{id}/edit", get(edit_form).post(update))
        .route("/admin/users/{id}/delete", post(delete))
        .route("/admin/users/bulk-delete", post(bulk_delete))
        .route("/admin/users/{id}/avatar/delete", post(avatar_delete))
        .route("/admin/users/{uid}/api-keys/{key_id}/delete", post(api_key_delete))
        .route("/admin/users/{uid}/ssh-keys/{key_id}/delete", post(ssh_key_delete))
        .route("/admin/users/{uid}/oauth/unlink", post(oauth_unlink))
        .route("/admin/users/{id}/suspend-all", post(suspend_all))
        .route("/admin/users/{id}/unsuspend-all", post(unsuspend_all))
}

fn ts() -> Timestamp {
    Timestamp(pelican_core::time::now())
}

/// Laravel lists for the `in:` rules (`User::getRules()` appends them).
fn in_rule(key: &str) -> String {
    let items: Vec<String> = PHP_LISTS[key]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    format!("in:{}", items.join(","))
}

#[derive(FromQueryResult)]
struct CountRow {
    aggregate: Int,
}

async fn total_of(admin: &Admin, where_sql: &str, vals: Vec<sea_orm::Value>) -> Result<u64, WebError> {
    let row = CountRow::find_by_statement(raw::stmt(
        admin.st.db.get_database_backend(),
        &format!("select count(*) as {{aggregate}} from {{users}}{where_sql}"),
        vals,
    ))
    .one(&admin.st.db)
    .await?;
    Ok(row.map(|r| r.aggregate.0.max(0) as u64).unwrap_or(0))
}

/// Per-user aggregate counts (`TextColumn` with `counts(...)`).
async fn load_counts(admin: &Admin, table: &str, id_col: &str, ids: &[i64]) -> Result<HashMap<i64, i64>, WebError> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let marks = vec!["?"; ids.len()].join(", ");
    // Output aliases kept unquoted (`as uid`, `as cnt`) — plain names.
    let sql = format!(
        "select {{{id_col}}} as uid, count(*) as cnt from {{{table}}} where {{{id_col}}} in ({marks}) group by {{{id_col}}}"
    );
    let vals: Vec<sea_orm::Value> = ids.iter().map(|i| (*i).into()).collect();
    for r in admin.st.db.query_all_raw(raw::stmt(admin.st.db.get_database_backend(), &sql, vals)).await? {
        let uid = r.try_get::<Int>("", "uid")?.0;
        let cnt = r.try_get::<Int>("", "cnt")?.0;
        out.insert(uid, cnt);
    }
    Ok(out)
}

/// Role names of every listed user (`roles.name` badge column).
async fn roles_map<C: ConnectionTrait>(db: &C, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>, WebError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let pivots = model_has_roles::Entity::find()
        .filter(model_has_roles::Column::ModelType.eq("user"))
        .filter(model_has_roles::Column::ModelId.is_in(ids.to_vec()))
        .all(db)
        .await?;
    let mut per_user: HashMap<i64, Vec<i64>> = HashMap::new();
    for p in pivots {
        per_user.entry(p.model_id.0).or_default().push(p.role_id.0);
    }
    let role_ids: Vec<i64> = per_user.values().flatten().copied().collect();
    let names: HashMap<i64, String> = if role_ids.is_empty() {
        HashMap::new()
    } else {
        roles::Entity::find()
            .filter(roles::Column::Id.is_in(role_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|r| (r.id.0, r.name))
            .collect()
    };
    Ok(per_user
        .into_iter()
        .map(|(u, rs)| (u, rs.into_iter().filter_map(|r| names.get(&r).cloned()).collect()))
        .collect())
}

/// Activity logs where the user is the subject (`User::activity()` morphToMany),
/// newest first, capped at 50 like the profile page.
async fn subject_activity(admin: &Admin, uid: i64) -> Result<Vec<activity_logs::Model>, WebError> {
    let sql = "select al.* from {activity_logs} al inner join {activity_log_subjects} als \
               on als.{activity_log_id} = al.{id} \
               where als.{subject_type} = ? and als.{subject_id} = ? \
               order by al.{timestamp} desc limit 50";
    activity_logs::Model::find_by_statement(raw::stmt(
        admin.st.db.get_database_backend(),
        sql,
        vec![morph::USER.into(), uid.into()],
    ))
    .all(&admin.st.db)
    .await
    .map_err(WebError::from)
}

// ----- permissions / records -------------------------------------------------

async fn find(admin: &Admin, id: i64) -> Result<users::Model, WebError> {
    users::Entity::find_by_id(id).one(&admin.st.db).await?.ok_or(WebError::NotFound)
}

/// `UserPolicy::update/delete` add `User::canTarget($model)`: non-root admins
/// cannot act on root admins.
async fn authorize_for(admin: &Admin, ability: &str, u: &users::Model) -> Result<(), WebError> {
    admin.authorize(ability)?;
    if !admin.user.is_root_admin() && is_root_target(&admin.st, u.id.0).await? {
        return Err(WebError::Forbidden);
    }
    Ok(())
}

/// `User::isRootAdmin()` of the record (not the current actor).
async fn is_root_target(st: &AppState, id: i64) -> Result<bool, WebError> {
    pelican_core::auth::is_root_admin(&st.db, id).await.map_err(Into::into)
}

/// `servers()->count()` for the delete-action gating.
async fn count_owned_servers(st: &AppState, uid: i64) -> Result<i64, WebError> {
    let rows = servers::Entity::find().filter(servers::Column::OwnerId.eq(uid)).all(&st.db).await?;
    Ok(rows.len() as i64)
}

async fn role_ids_of(admin: &Admin, u: &users::Model) -> Result<Vec<i64>, WebError> {
    Ok(model_has_roles::Entity::find()
        .filter(model_has_roles::Column::ModelType.eq("user"))
        .filter(model_has_roles::Column::ModelId.eq(u.id.0))
        .all(&admin.st.db)
        .await?
        .iter()
        .map(|p| p.role_id.0)
        .collect())
}

/// Root Admin role id (`Role::getRootAdmin()`), if it exists yet.
async fn root_admin_role_id(admin: &Admin) -> Result<Option<i64>, WebError> {
    Ok(roles::Entity::find()
        .filter(roles::Column::Name.eq(ROOT_ADMIN))
        .filter(roles::Column::GuardName.eq("web"))
        .one(&admin.st.db)
        .await?
        .map(|r| r.id.0))
}

/// spatie `syncRoles()`: replace the user's role pivots.
async fn sync_roles<C: ConnectionTrait>(db: &C, user_id: i64, role_ids: &[i64]) -> Result<(), WebError> {
    let b = db.get_database_backend();
    db.execute_raw(raw::stmt(
        b,
        "delete from {model_has_roles} where {model_type} = ? and {model_id} = ?",
        vec!["user".into(), user_id.into()],
    ))
    .await?;
    for r in role_ids {
        db.execute_raw(raw::stmt(
            b,
            "insert into {model_has_roles} ({role_id}, {model_type}, {model_id}) values (?, ?, ?)",
            vec![(*r).into(), "user".into(), user_id.into()],
        ))
        .await?;
    }
    Ok(())
}

// ----- list ------------------------------------------------------------------

/// Initials avatar (`ui-avatars` style, inlined SVG data URI; ADR-0013 — no
/// external avatar provider).
fn avatar_url(name: &str) -> String {
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

/// `GET /admin/users` (ListUsers).
async fn index(admin: Admin) -> WebResult {
    admin.authorize("viewList user")?;
    let st = &admin.st;
    let opts = [10, 25, 50, 100, 0];
    let p = ListParams::parse(admin.query.as_deref(), &opts, 10);
    // Searchable columns: username, email; default order = natural (id asc).
    let mut wheres: Vec<String> = Vec::new();
    let mut vals: Vec<sea_orm::Value> = Vec::new();
    if !p.search.is_empty() {
        wheres.push("(LOWER({username}) LIKE ? OR LOWER({email}) LIKE ?)".into());
        let like = format!("%{}%", p.search.to_lowercase());
        vals.push(like.clone().into());
        vals.push(like.into());
    }
    let dir = if p.desc { "desc" } else { "asc" };
    let order = match p.sort.as_deref() {
        Some("username") => format!("{{username}} {dir}"),
        Some("email") => format!("{{email}} {dir}"),
        _ => "{id} asc".into(),
    };
    let where_sql = if wheres.is_empty() { String::new() } else { format!(" where {}", wheres.join(" and ")) };
    let total = total_of(&admin, &where_sql, vals.clone()).await?;
    let mut sql = format!("select * from {{users}}{where_sql} order by {order}");
    if let Some((l, o)) = p.limit_offset() {
        sql.push_str(&format!(" limit {l} offset {o}"));
    }
    let rows = users::Model::find_by_statement(raw::stmt(st.db.get_database_backend(), &sql, vals)).all(&st.db).await?;

    let ids: Vec<i64> = rows.iter().map(|u| u.id.0).collect();
    let role_list = roles_map(&st.db, &ids).await?;
    let servers_count = load_counts(&admin, "servers", "owner_id", &ids).await?;
    let subusers_count = load_counts(&admin, "subusers", "user_id", &ids).await?;

    let t = |k: &str| admin.t(&format!("admin/user.{k}"));
    let mut table = Table::new("users", "/admin/users");
    table.columns = vec![
        Column::new("picture", "").center().wide_only(), // visibleFrom('lg')
        Column::new("username", t("username")).sortable(),
        Column::new("email", t("email")).sortable(),
        Column::new("mfa_email_enabled", admin.t("profile.tabs.2fa")).center().wide_only(),
        Column::new("roles", t("roles")),
        Column::new("servers_count", t("servers")),
        Column::new("subusers_count", t("subusers")).wide_only(), // visibleFrom('sm')
    ];
    let can_view_record = admin.can("view user");
    for u in &rows {
        let role_names: Vec<String> = role_list.get(&u.id.0).cloned().unwrap_or_default();
        let root_target = role_names.iter().any(|n| n == ROOT_ADMIN);
        let (mfa_icon, color, tooltip) = if u.mfa_app_secret.as_deref().is_some_and(|s| !s.is_empty()) {
            ("qrcode", "success", admin.t("profile.mfa.app"))
        } else if u.mfa_email_enabled.0 {
            ("mail", "info", admin.t("profile.mfa.email"))
        } else {
            ("lock-open-off", "gray", admin.t("profile.mfa.none"))
        };
        let url = if admin.can("update user") && (admin.user.is_root_admin() || !root_target) {
            format!("/admin/users/{}/edit", u.id.0)
        } else if can_view_record {
            format!("/admin/users/{}", u.id.0)
        } else {
            String::new()
        };
        let mut row = Row::new(u.id.0.to_string())
            .cell("picture", Cell::Image { src: avatar_url(&u.username) })
            .cell("username", Cell::text(u.username.clone()))
            .cell("email", Cell::text(u.email.clone()))
            .cell(
                "mfa_email_enabled",
                Cell::Icon { icon: mfa_icon.into(), color: color.into(), tooltip: Some(tooltip) },
            )
            .cell(
                "roles",
                if role_names.is_empty() {
                    Cell::badge(t("no_roles"), "gray")
                } else {
                    Cell::Badges { items: role_names, color: "primary".into() }
                },
            )
            .cell(
                "servers_count",
                Cell::text(servers_count.get(&u.id.0).map(i64::to_string).unwrap_or_else(|| "0".into())),
            )
            .cell(
                "subusers_count",
                Cell::text(subusers_count.get(&u.id.0).map(i64::to_string).unwrap_or_else(|| "0".into())),
            );
        if !url.is_empty() {
            row.url = Some(url.clone());
            // EditAction shown when updatable, otherwise ViewAction
            // (`ViewAction->hidden(edit allowed)`).
            row.actions.push(if url.ends_with("/edit") {
                Action::link(admin.t("filament-actions::edit.single.label"), "pencil", url)
            } else {
                Action::link(admin.t("filament-actions::view.single.label"), "eye", url)
            });
        }
        // `checkIfRecordIsSelectableUsing`: not the current user, no servers.
        let owned = servers_count.get(&u.id.0).copied().unwrap_or(0);
        if admin.can("delete user") && (owned > 0 || admin.user.id() == u.id.0) {
            row.unselectable = true;
        }
        table.rows.push(row);
    }
    if admin.can("delete user") {
        table.bulk.push(
            Action::post(admin.t("filament-actions::delete.multiple.label"), "trash", "/admin/users/bulk-delete")
                .color("danger")
                .confirm(admin.t("filament-actions::modal.confirmation")),
        );
    }
    table.empty_icon = "users".into();
    table.empty_heading = t("model_label_plural");
    table.finish(&p, total, &opts, &admin.locale());
    let create = admin.can("create user").then(|| {
        context! {
            label => admin.t_with("filament-actions::create.single.label", &[("label", &t("model_label"))]),
            url => "/admin/users/create",
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

// ----- form data -------------------------------------------------------------

pub(crate) struct FormData {
    username: String,
    email: String,
    /// Plain password as typed; hashed on save when non-empty.
    password: String,
    external_id: String,
    is_managed_externally: bool,
    timezone: String,
    language: String,
    roles: Vec<String>,
}

impl FormData {
    fn from_model(m: &users::Model, role_ids: &[i64]) -> FormData {
        FormData {
            username: m.username.clone(),
            email: m.email.clone(),
            password: String::new(),
            external_id: m.external_id.clone().unwrap_or_default(),
            is_managed_externally: m.is_managed_externally.0,
            timezone: m.timezone.clone(),
            language: m.language.clone(),
            roles: role_ids.iter().map(i64::to_string).collect(),
        }
    }

    fn from_form(f: &Form) -> FormData {
        FormData {
            username: f.get("username").trim().into(),
            email: f.get("email").trim().into(),
            password: f.get("password").to_string(),
            external_id: f.get("external_id").trim().into(),
            is_managed_externally: f.bool("is_managed_externally"),
            timezone: f.get("timezone").trim().into(),
            language: f.get("language").trim().into(),
            roles: f.list("roles"),
        }
    }

    /// Owner create-option of the server wizard (`CreateServer`): only
    /// username/email/password are submitted, the rest uses the defaults above.
    pub(crate) fn owner_option(username: String, email: String, password: String) -> FormData {
        FormData {
            username,
            email,
            password,
            external_id: String::new(),
            is_managed_externally: false,
            // Model default attribute (the creating hook only fills a null).
            timezone: "UTC".into(),
            language: "en".into(),
            roles: Vec::new(),
        }
    }

    fn ctx(&self) -> Mj {
        context! {
            username => self.username, email => self.email, external_id => self.external_id,
            is_managed_externally => if self.is_managed_externally { "1" } else { "0" },
            timezone => self.timezone, language => self.language, roles => self.roles,
        }
    }

    /// Checked roles as role ids (validated earlier).
    fn role_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.roles.iter().filter_map(|s| s.parse().ok()).collect();
        ids.dedup();
        ids
    }
}

/// Filament field constraints + `User::$validationRules` (+ `getRules()` `in:` lists).
async fn validate(admin: &Admin, data: &FormData, ignore: Option<i64>) -> Result<Errors, WebError> {
    let input = json!({
        "username": data.username,
        "email": data.email,
        "password": if data.password.is_empty() { Value::Null } else { Value::from(data.password.clone()) },
        "external_id": if data.external_id.is_empty() { Value::Null } else { Value::from(data.external_id.clone()) },
        "is_managed_externally": data.is_managed_externally,
        "timezone": data.timezone,
        "language": data.language,
    });
    let mut v = Validator::for_value(&input);
    for (field, key) in [
        ("username", "admin/user.username"),
        ("email", "admin/user.email"),
        ("password", "admin/user.password"),
        ("external_id", "admin/user.external_id"),
        ("timezone", "profile.timezone"),
        ("language", "profile.language"),
    ] {
        v.attribute_name(field, &admin.t(key).to_lowercase());
    }
    // The saving hook lowercases username/email before persisting; uniqueness
    // (`unique:users,<col>`) compares against the stored (lowercase) value.
    #[allow(clippy::redundant_closure_for_method_calls)]
    let unique_taken = |column: users::Column, value: String| async move {
        if value.is_empty() {
            return Ok::<bool, sea_orm::DbErr>(false);
        }
        let other = users::Entity::find()
            .filter(column.eq(value))
            .filter(users::Column::Id.ne(ignore.unwrap_or(-1)))
            .one(&admin.st.db)
            .await?;
        Ok(other.is_some())
    };
    v.rules("username", &["required", "string", "max:255"]);
    v.custom(
        "username",
        CustomRule::precomputed(
            "unique",
            unique_taken(users::Column::Username, data.username.to_lowercase())
                .await?
                .then(|| "The :attribute has already been taken.".into()),
        ),
    );
    v.rules("email", &["required", "email", "max:255"]);
    v.custom(
        "email",
        CustomRule::precomputed(
            "unique",
            unique_taken(users::Column::Email, data.email.to_lowercase())
                .await?
                .then(|| "The :attribute has already been taken.".into()),
        ),
    );
    v.rules("external_id", &["sometimes", "nullable", "string", "max:255"]);
    if !data.external_id.is_empty() {
        let dup = users::Entity::find()
            .filter(users::Column::ExternalId.eq(data.external_id.clone()))
            .filter(users::Column::Id.ne(ignore.unwrap_or(-1)))
            .one(&admin.st.db)
            .await?
            .is_some();
        v.custom(
            "external_id",
            CustomRule::precomputed("unique", dup.then(|| "The :attribute has already been taken.".into())),
        );
    }
    v.rules("password", &["sometimes", "nullable", "string"]);
    v.rules("is_managed_externally", &["boolean"]);
    v.rules("timezone", &["required", "string", &in_rule("timezones")]);
    v.rules("language", &["required", "string", &in_rule("locales")]);
    v.run();
    let mut errors = Errors::from_validator(&v);

    // Roles tab: every selected id must be an assignable spatie web role. The
    // Root Admin role itself is never offered here.
    let root_id = root_admin_role_id(admin).await?;
    for raw_id in &data.roles {
        let ok = match raw_id.trim().parse::<i64>() {
            Ok(id) => {
                Some(id) != root_id
                    && roles::Entity::find_by_id(Int(id))
                        .filter(roles::Column::GuardName.eq("web"))
                        .one(&admin.st.db)
                        .await?
                        .is_some()
            }
            Err(_) => false,
        };
        if !ok {
            errors.add("roles", admin.t("validation.exists"));
            break;
        }
    }
    Ok(errors)
}

// ----- rendering --------------------------------------------------------------

struct FormPage<'a> {
    record: Option<&'a users::Model>,
    data: &'a FormData,
    errors: &'a Errors,
}

/// Languages dropdown (`LanguageService::getAvailableLanguages()`); mirrors
/// `crate::app::profile::languages`.
fn languages() -> Vec<(String, String)> {
    #[allow(clippy::too_many_lines)]
    let native = |code: &str| -> String {
        match code {
            "ar" => "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064A}\u{0629}",
            "be" => "\u{0411}\u{0435}\u{043B}\u{0430}\u{0440}\u{0443}\u{0441}\u{043A}\u{0430}\u{044F}",
            "bg" => "\u{0411}\u{044A}\u{043B}\u{0433}\u{0430}\u{0440}\u{0441}\u{043A}\u{0438}",
            "cs" => "\u{010C}e\u{0161}tina",
            "da" => "Dansk",
            "de" => "Deutsch",
            "en" => "English",
            "es" => "Espa\u{00F1}ol",
            "fi" => "Suomi",
            "fr" => "Fran\u{00E7}ais",
            "hu" => "Magyar",
            "it" => "Italiano",
            "ja" => "\u{65E5}\u{672C}\u{8A9E}",
            "ko" => "\u{D55C}\u{AD6D}\u{C5B4}",
            "nl" => "Nederlands",
            "pl" => "Polski",
            "pt" => "Portugu\u{00EA}s",
            "pt_BR" => "Portugu\u{00EA}s (Brasil)",
            "ro" => "Rom\u{00E2}n\u{0103}",
            "ru" => "\u{0420}\u{0443}\u{0441}\u{0441}\u{043A}\u{0438}\u{0439}",
            "sv" => "Svenska",
            "tr" => "T\u{00FC}rk\u{00E7}e",
            "uk" => "\u{0423}\u{043A}\u{0440}\u{0430}\u{0457}\u{043D}\u{0441}\u{044C}\u{043A}\u{0430}",
            other => other,
        }
        .to_string()
    };
    crate::i18n::locales()
        .into_iter()
        .map(|code| {
            let name = crate::i18n::trans(&code, "profile.current_language_name", &[]);
            let name = if name == "profile.current_language_name" { native(&code) } else { name };
            (code, name)
        })
        .collect()
}

/// `ServersRelationManager`: the user's owned servers as an embedded table.
/// Returns `(table, can_suspend_all, can_unsuspend_all)`.
async fn servers_table(admin: &Admin, u: &users::Model) -> Result<(Mj, bool, bool), WebError> {
    let st = &admin.st;
    let owned = servers::Entity::find()
        .filter(servers::Column::OwnerId.eq(u.id.0))
        .order_by_asc(servers::Column::Id)
        .all(&st.db)
        .await?;
    let node_ids: Vec<i64> = owned.iter().map(|s| s.node_id.0).collect();
    let egg_ids: Vec<i64> = owned.iter().map(|s| s.egg_id.0).collect();
    let nodes_map: HashMap<i64, nodes::Model> = nodes::Entity::find()
        .filter(nodes::Column::Id.is_in(node_ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|n| (n.id.0, n))
        .collect();
    let eggs_map: HashMap<i64, eggs::Model> = eggs::Entity::find()
        .filter(eggs::Column::Id.is_in(egg_ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|e| (e.id.0, e))
        .collect();
    let server_ids: Vec<i64> = owned.iter().map(|s| s.id.0).collect();
    let mut allocs: HashMap<i64, Vec<allocations::Model>> = HashMap::new();
    for a in allocations::Entity::find()
        .filter(allocations::Column::ServerId.is_in(server_ids))
        .order_by_asc(allocations::Column::Id)
        .all(&st.db)
        .await?
    {
        if let Some(sid) = a.server_id {
            allocs.entry(sid.0).or_default().push(a);
        }
    }
    let p = ListParams::parse(admin.query.as_deref(), &[10, 25, 50, 0], 25);
    let search = p.search.to_lowercase();
    let filtered: Vec<&servers::Model> = owned
        .iter()
        .filter(|s| {
            search.is_empty()
                || s.name.to_lowercase().contains(&search)
                || nodes_map.get(&s.node_id.0).map(|n| n.name.to_lowercase().contains(&search)).unwrap_or(false)
                || eggs_map.get(&s.egg_id.0).map(|e| e.name.to_lowercase().contains(&search)).unwrap_or(false)
        })
        .collect();
    let mut sorted: Vec<&servers::Model> = filtered;
    sorted.sort_by(|a, b| {
        let na = nodes_map.get(&a.node_id.0).map(|n| n.name.clone()).unwrap_or_default();
        let nb = nodes_map.get(&b.node_id.0).map(|n| n.name.clone()).unwrap_or_default();
        let ea = eggs_map.get(&a.egg_id.0).map(|e| e.name.clone()).unwrap_or_default();
        let eb = eggs_map.get(&b.egg_id.0).map(|e| e.name.clone()).unwrap_or_default();
        match p.sort.as_deref() {
            Some("name") => a.name.cmp(&b.name),
            Some("node") => na.cmp(&nb),
            Some("egg") => ea.cmp(&eb),
            Some("allocation") => a.allocation_id.map(|x| x.0).cmp(&b.allocation_id.map(|x| x.0)),
            _ => std::cmp::Ordering::Equal,
        }
    });
    if p.desc {
        sorted.reverse();
    }
    let base = format!("/admin/users/{}/edit", u.id.0);
    let can_update_server = admin.can("update server");
    let t = |k: &str| admin.t(k);
    let mut table = Table::new("user-servers", &base);
    table.columns = vec![
        Column::new("name", t("admin/server.name")).sortable(),
        Column::new("node", t("admin/server.node")).sortable(),
        Column::new("egg", t("admin/server.egg")).sortable(),
        Column::new("allocation", t("admin/server.primary_allocation")).sortable(),
    ];
    for s in &sorted {
        let allocs_list = allocs.get(&s.id.0).cloned().unwrap_or_default();
        // SelectColumn editable when `update server` + the server's node is
        // targetable (`user()->can('update', $server)`).
        let selectable = can_update_server && admin.user.can_target_node(s.node_id.0);
        let allocation_cell = if selectable {
            Cell::Select {
                url: format!("/admin/servers/{}/primary", s.id.0),
                field: "allocation_id".into(),
                options: allocs_list.iter().map(|a| (a.id.0.to_string(), crate::models::address(a))).collect(),
                value: s.allocation_id.map(|a| a.0.to_string()).unwrap_or_default(),
                disabled: allocs_list.len() <= 1,
                placeholder: (allocs_list.len() <= 1 && s.allocation_id.is_none()).then(|| t("admin/server.none")),
            }
        } else {
            Cell::text(
                allocs_list
                    .iter()
                    .find(|a| Some(a.id) == s.allocation_id)
                    .map(crate::models::address)
                    .unwrap_or_else(|| t("admin/server.none")),
            )
        };
        // Name links to EditServer when updatable; node/egg columns stay plain
        // text (their resources have no pages in this build yet).
        let url =
            if selectable { format!("/admin/servers/{}/edit", s.id.0) } else { format!("/admin/servers/{}", s.id.0) };
        let mut row = Row::new(s.id.0.to_string())
            .url(url.clone())
            .cell("name", Cell::text(s.name.clone()))
            .cell("node", Cell::text(nodes_map.get(&s.node_id.0).map(|n| n.name.clone()).unwrap_or_default()))
            .cell("egg", Cell::text(eggs_map.get(&s.egg_id.0).map(|e| e.name.clone()).unwrap_or_default()))
            .cell("allocation", allocation_cell);
        // ViewConsoleAction + EditAction.
        row.actions.push(Action::link(
            t("admin/server.view_console"),
            "terminal",
            format!("/server/{}/console", s.uuid_short),
        ));
        row.actions.push(if selectable {
            Action::link(t("filament-actions::edit.single.label"), "pencil", url)
        } else {
            Action::link(t("filament-actions::view.single.label"), "eye", url)
        });
        table.rows.push(row);
    }
    table.searchable = true;
    table.empty_heading = t("admin/server.no_servers");
    table.empty_icon = "brand-docker".into();
    table.finish(&p, sorted.len() as u64, &[10, 25, 50, 0], &admin.locale());
    let any_active = owned.iter().any(|s| s.status.as_deref() != Some("suspended"));
    let any_suspended = owned.iter().any(|s| s.status.as_deref() == Some("suspended"));
    Ok((Mj::from_serialize(&table), any_active && can_update_server, any_suspended && can_update_server))
}

async fn render_form(admin: &Admin, page: FormPage<'_>) -> WebResult {
    let st = &admin.st;
    let record = page.record;
    let tab = {
        let q = admin.query.as_deref().unwrap_or("");
        form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "tab")
            .map(|(_, v)| v.into_owned())
            .filter(|tt| TABS.contains(&tt.as_str()))
            .unwrap_or_else(|| "account".into())
    };

    let root_target = match record {
        Some(u) => is_root_target(st, u.id.0).await?,
        None => false,
    };
    // Create: `create user`; edit: `update user` + canTarget; view: read-only.
    let can_write = match record {
        Some(u) => admin.can("update user") && (admin.user.is_root_admin() || !is_root_target(st, u.id.0).await?),
        None => admin.can("create user"),
    };
    let readonly = record.is_some() && !can_write;

    // Assignable roles (excludes Root Admin, per the relationship query).
    let role_opts: Vec<(String, String)> = {
        let root_id = root_admin_role_id(admin).await?;
        roles::Entity::find()
            .filter(roles::Column::GuardName.eq("web"))
            .order_by_asc(roles::Column::Name)
            .all(&st.db)
            .await?
            .into_iter()
            .filter(|r| Some(r.id.0) != root_id)
            .map(|r| (r.id.0.to_string(), r.name))
            .collect()
    };

    // Existing avatar preview (public disk, `avatars/<id>.png`).
    let avatar_file = record.and_then(|u| {
        let path = crate::models::public_disk(st).join("avatars").join(format!("{}.png", u.id.0));
        path.exists().then(|| format!("/storage/avatars/{}.png", u.id.0))
    });

    let oauth: serde_json::Map<String, Value> =
        record.and_then(|u| u.oauth.as_ref()).and_then(|o| o.0.as_object().cloned()).unwrap_or_default();

    let api_keys_list = match record {
        Some(u) => {
            api_keys::Entity::find()
                .filter(api_keys::Column::UserId.eq(u.id.0))
                .filter(api_keys::Column::KeyType.eq(1))
                .order_by_asc(api_keys::Column::Id)
                .all(&st.db)
                .await?
        }
        None => Vec::new(),
    };
    let ssh_keys_list = match record {
        Some(u) => {
            user_ssh_keys::Entity::find()
                .filter(user_ssh_keys::Column::UserId.eq(u.id.0))
                .filter(user_ssh_keys::Column::DeletedAt.is_null())
                .order_by_asc(user_ssh_keys::Column::Id)
                .all(&st.db)
                .await?
        }
        None => Vec::new(),
    };

    // Activity tab entries (subject = this user) rendered like the profile page.
    let activity_entries = match record {
        Some(u) => {
            let logs = subject_activity(admin, u.id.0).await?;
            let see_ips = admin.user.gate_before("seeIps activityLog");
            Mj::from_serialize(crate::activity::entries(&st.db, logs, &admin.locale(), see_ips).await?)
        }
        None => Mj::from_serialize(Value::Null),
    };

    // Header DeleteAction state (EditUser): disabled for self or owners.
    let owned_servers = match record {
        Some(u) => count_owned_servers(st, u.id.0).await?,
        None => 0,
    };
    let is_self = record.map(|u| u.id.0) == Some(admin.user.id());
    let delete_tooltip = if is_self {
        admin.t("admin/user.self_delete")
    } else if owned_servers > 0 {
        admin.t("admin/user.has_servers")
    } else {
        admin.t("filament-actions::delete.single.modal.actions.delete.label")
    };
    let can_delete = record.is_some() && admin.can("delete user") && (admin.user.is_root_admin() || !root_target);

    let timezones: Vec<(String, String)> = PHP_LISTS["timezones"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(|tz| (tz.to_string(), tz.to_string())).collect())
        .unwrap_or_default();

    // Embedded servers relation manager (view/edit only).
    let (servers_table, suspend_all_available, unsuspend_all_available) = match record {
        Some(u) => servers_table(admin, u).await?,
        None => (Mj::from_serialize(Value::Null), false, false),
    };

    let (title, action) = match record {
        Some(u) => (
            admin.t_with("filament-panels::resources/pages/edit-record.title", &[("label", &u.username)]),
            format!("/admin/users/{}/edit", u.id.0),
        ),
        None => (
            admin.t_with(
                "filament-panels::resources/pages/create-record.title",
                &[("label", &admin.t("admin/user.model_label"))],
            ),
            "/admin/users/create".into(),
        ),
    };

    admin
        .page(
            SLUG,
            "admin/users/form.html",
            context! {
                title => title,
                action => action,
                tab => tab,
                tabs => TABS,
                record => record.map(|u| context! { id => u.id.0, name => u.username.clone() }),
                readonly => readonly,
                create_mode => record.is_none(),
                root_target => root_target,
                root_role_label => admin.t_with("admin/role.root_admin", &[("role", ROOT_ADMIN)]),
                can_delete => can_delete,
                delete_disabled => is_self || owned_servers > 0,
                delete_tooltip => delete_tooltip,
                managed_external => page.data.is_managed_externally,
                v => page.data.ctx(),
                errors => Mj::from_serialize(page.errors),
                role_options => role_opts,
                timezones => timezones,
                languages => languages(),
                avatar_file => avatar_file,
                oauth_entries => oauth.iter().map(|(schema, ident)| context! {
                    schema => schema.clone(),
                    identifier => ident.as_str().unwrap_or_default(),
                }).collect::<Vec<_>>(),
                has_oauth => !oauth.is_empty(),
                api_keys => api_keys_list.iter().map(|k| context! {
                    id => k.id.0,
                    identifier => k.identifier.clone().unwrap_or_default(),
                    memo => k.memo.clone().unwrap_or_default(),
                }).collect::<Vec<_>>(),
                has_api_keys => !api_keys_list.is_empty(),
                ssh_keys => ssh_keys_list.iter().map(|k| context! {
                    id => k.id.0, name => k.name.clone(), fingerprint => k.fingerprint.clone(),
                }).collect::<Vec<_>>(),
                has_ssh_keys => !ssh_keys_list.is_empty(),
                activity => activity_entries,
                servers_table => servers_table,
                suspend_all_available => suspend_all_available,
                unsuspend_all_available => unsuspend_all_available,
            },
        )
        .await
}

// ----- create / view / edit ---------------------------------------------------

fn hash_password(st: &AppState, plain: &str) -> Result<String, WebError> {
    laravel_compat::hashing::make(plain, st.config.bcrypt_rounds).map_err(|_| WebError::internal("RuntimeException"))
}

/// `GET /admin/users/create` (CreateRecord::form).
async fn create_form(admin: Admin) -> WebResult {
    admin.authorize("create user")?;
    let data = FormData {
        username: String::new(),
        email: String::new(),
        password: String::new(),
        external_id: String::new(),
        is_managed_externally: false,
        // Defaults from `protected $attributes` and config('app.timezone').
        timezone: admin.st.config.env.string_or("APP_TIMEZONE", "UTC"),
        language: "en".into(),
        roles: Vec::new(),
    };
    render_form(&admin, FormPage { record: None, data: &data, errors: &Errors::default() }).await
}

/// `GET /admin/users/{id}` (ViewUser: same form, read-only + relation manager).
async fn view(admin: Admin, Path(id): Path<i64>) -> WebResult {
    admin.authorize("view user")?;
    let u = find(&admin, id).await?;
    let role_ids = role_ids_of(&admin, &u).await?;
    render_form(
        &admin,
        FormPage { record: Some(&u), data: &FormData::from_model(&u, &role_ids), errors: &Errors::default() },
    )
    .await
}

/// `GET /admin/users/{id}/edit` (EditRecord::form).
async fn edit_form(admin: Admin, Path(id): Path<i64>) -> WebResult {
    let u = find(&admin, id).await?;
    authorize_for(&admin, "update user", &u).await?;
    let role_ids = role_ids_of(&admin, &u).await?;
    render_form(
        &admin,
        FormPage { record: Some(&u), data: &FormData::from_model(&u, &role_ids), errors: &Errors::default() },
    )
    .await
}

/// Inserts the user (`UserCreationService::handle`): hashes the submitted
/// password (or generates one plus a reset token), mails AccountCreated.
pub(crate) async fn store_new_user(st: &AppState, data: &FormData) -> Result<users::Model, WebError> {
    // The saving hook lowercases username/email.
    let email = data.email.to_lowercase();
    let username = data.username.to_lowercase();
    let now_ts = ts();
    let txn = st.db.begin().await?;
    // No submitted password → random one + a reset token (mailed afterwards).
    let hashed = if data.password.is_empty() {
        hash_password(st, &pelican_wings::jwt::str_random(30))?
    } else {
        hash_password(st, &data.password)?
    };
    let am = users::ActiveModel {
        uuid: Set(uuid::Uuid::new_v4().to_string()), // creating hook
        external_id: Set((!data.external_id.is_empty()).then(|| data.external_id.clone())),
        is_managed_externally: Set(Bool(data.is_managed_externally)),
        username: Set(username.clone()),
        email: Set(email.clone()),
        password: Set(hashed),
        language: Set(data.language.clone()),
        timezone: Set(if data.timezone.is_empty() {
            st.config.env.string_or("APP_TIMEZONE", "UTC")
        } else {
            data.timezone.clone()
        }),
        oauth: Set(Some(Json(json!([])))),
        customization: Set(None),
        mfa_app_secret: Set(None),
        mfa_app_recovery_codes: Set(None),
        mfa_email_enabled: Set(Bool(false)),
        created_at: Set(Some(now_ts)),
        updated_at: Set(Some(now_ts)),
        ..Default::default()
    };
    let user = pelican_db::write::insert(&txn, am).await?;
    // Roles are synced after creation ($user->syncRoles($roles)).
    sync_roles(&txn, user.id.0, &data.role_ids()).await?;
    let mut reset_token = None;
    if data.password.is_empty() {
        // Password broker createToken(): clear old tokens, insert hashed token.
        password_resets::Entity::delete_many()
            .filter(password_resets::Column::Email.eq(email.clone()))
            .exec(&txn)
            .await?;
        let plain = pelican_wings::jwt::str_random(64);
        txn.execute_raw(raw::stmt(
            st.db.get_database_backend(),
            "insert into {password_resets} ({email}, {token}, {created_at}) values (?, ?, ?)",
            vec![email.clone().into(), hash_password(st, &plain)?.into(), now_ts.into()],
        ))
        .await?;
        reset_token = Some(plain);
    }
    txn.commit().await?;
    // `UserCreationService::handle` → `User::query()->forceCreate([...])`; the
    // role pivot sync and the password-broker token rows fire nothing.
    model_events::created(st, &user).await;
    pelican_api::services::account_created(st, &user, reset_token.as_deref())
        .await
        .map_err(|e| WebError::internal(format!("{e:?}")))?;
    Ok(user)
}

/// `POST /admin/users/create` (`CreateUser::handleRecordCreation` +
/// `UserCreationService::handle`).
async fn create(admin: Admin, form: Form) -> WebResult {
    admin.authorize("create user")?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, None).await?;
    if !errors.is_empty() {
        return render_form(&admin, FormPage { record: None, data: &data, errors: &errors }).await;
    }
    let user = store_new_user(&admin.st, &data).await?;
    admin.flash_success(admin.t("filament-panels::resources/pages/create-record.notifications.created.title"));
    Ok(admin.redirect(&format!("/admin/users/{}/edit", user.id.0)))
}

/// `POST /admin/users/{id}/edit` (`EditUser::handleRecordUpdate` +
/// `UserUpdateService::handle`; roles synced separately via `syncRoles`).
async fn update(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let u = find(&admin, id).await?;
    authorize_for(&admin, "update user", &u).await?;
    let data = FormData::from_form(&form);
    let errors = validate(&admin, &data, Some(id)).await?;
    if !errors.is_empty() {
        return render_form(&admin, FormPage { record: Some(&u), data: &data, errors: &errors }).await;
    }
    let st = &admin.st;
    // forceFill($data)->saveOrFail(): fields the form dehydrates.
    let mut am = users::ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(u.id),
        username: Set(data.username.to_lowercase()),
        email: Set(data.email.to_lowercase()),
        external_id: Set((!data.external_id.is_empty()).then(|| data.external_id.clone())),
        is_managed_externally: Set(Bool(data.is_managed_externally)),
        language: Set(data.language.clone()),
        timezone: Set(data.timezone.clone()),
        updated_at: Set(Some(ts())),
        ..Default::default()
    };
    let password_changed = if data.password.is_empty() {
        pelican_db::write::update(&st.db, &am).await?;
        false
    } else {
        am.password = Set(hash_password(st, &data.password)?);
        pelican_db::write::update(&st.db, &am).await?;
        true
    };
    // Relationship saveRelationshipsUsing → syncRoles (root admins keep theirs).
    if !is_root_target(st, id).await? {
        sync_roles(&st.db, id, &data.role_ids()).await?;
    }
    // `UserUpdateService` → `$user->forceFill($data)->saveOrFail()`; fires only
    // when one of the submitted fields actually differs from the stored model.
    let user_changed = u.username != data.username.to_lowercase()
        || u.email != data.email.to_lowercase()
        || u.external_id != (!data.external_id.is_empty()).then(|| data.external_id.clone())
        || Bool(data.is_managed_externally) != u.is_managed_externally
        || u.language != data.language
        || u.timezone != data.timezone
        || password_changed;
    if user_changed {
        model_events::fire_id::<users::Entity>(st, model_events::Action::Updated, id).await;
    }
    if password_changed {
        // PasswordChanged → RevocationListener.
        pelican_api::jobs::revoke_sftp_access(st, &u).await.map_err(|e| WebError::internal(format!("{e:?}")))?;
    }
    admin.flash_success(admin.t("filament-panels::resources/pages/edit-record.notifications.saved.title"));
    Ok(admin.redirect(&format!("/admin/users/{id}/edit")))
}

// ----- delete ----------------------------------------------------------------

/// `POST /admin/users/{id}/delete` (DeleteAction; `User::booted` deleting hook
/// throws `DisplayException`; `App\Events\User\Deleting` → RevocationListener).
async fn delete(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let u = find(&admin, id).await?;
    authorize_for(&admin, "delete user", &u).await?;
    match delete_one(&admin, &u).await? {
        Ok(()) => {
            admin.flash_success(admin.t("filament-actions::delete.single.notifications.deleted.title"));
            Ok(admin.redirect("/admin/users"))
        }
        Err(msg) => {
            admin.flash_danger(msg, None);
            Ok(admin.redirect(&form.back("/admin/users")))
        }
    }
}

/// `DeleteBulkAction('exclude_bulk_delete')`.
async fn bulk_delete(admin: Admin, form: Form) -> WebResult {
    let ids = super::bulk_ids(&form);
    let mut failed = 0;
    for id in &ids {
        // Records failing the row policy are skipped with a failure counted.
        let deleted = match find(&admin, *id).await {
            Ok(u) if authorize_for(&admin, "delete user", &u).await.is_ok() => {
                matches!(delete_one(&admin, &u).await, Ok(Ok(())))
            }
            _ => false,
        };
        if !deleted {
            failed += 1;
        }
    }
    super::bulk_delete_notify(&admin, ids.len(), failed);
    Ok(admin.redirect(&form.back("/admin/users")))
}

/// User deletion (`User::booted` deleting hook + `Deleting` →
/// RevocationListener): refused while the user still owns servers or is the
/// acting admin themself.
async fn delete_one(admin: &Admin, u: &users::Model) -> Result<Result<(), String>, WebError> {
    let st = &admin.st;
    let owned = count_owned_servers(st, u.id.0).await?;
    if owned > 0 {
        return Ok(Err(admin.t("exceptions.users.has_servers")));
    }
    if admin.user.id() == u.id.0 {
        return Ok(Err(admin.t("exceptions.users.is_self")));
    }
    pelican_api::jobs::revoke_sftp_access(st, u).await.map_err(|e| WebError::internal(format!("{e:?}")))?;
    // DeleteAction → `$user->delete()`.
    users::Entity::delete_by_id(u.id).exec(&st.db).await?;
    model_events::deleted(st, u).await;
    Ok(Ok(()))
}

// ----- record-level extras ----------------------------------------------------

/// `POST .../oauth/unlink` (`OAuthService::unlinkUser`): remove the schema key
/// from the `oauth` attribute.
async fn oauth_unlink(admin: Admin, Path(uid): Path<i64>, form: Form) -> WebResult {
    let u = find(&admin, uid).await?;
    authorize_for(&admin, "update user", &u).await?;
    let schema = form.get("schema").to_string();
    if schema.is_empty() {
        return Ok(admin.redirect(&format!("/admin/users/{uid}/edit")));
    }
    let mut obj = u.oauth.as_ref().and_then(|o| o.0.as_object().cloned()).unwrap_or_default();
    if obj.remove(&schema).is_some() {
        let am = users::ActiveModel {
            id: sea_orm::ActiveValue::Unchanged(u.id),
            oauth: Set(Some(Json(json!(obj)))),
            updated_at: Set(Some(ts())),
            ..Default::default()
        };
        // OAuthService::unlinkUser → `$user->update(['oauth' => $oauth])`.
        pelican_db::write::update(&admin.st.db, &am).await?;
        model_events::fire_id::<users::Entity>(&admin.st, model_events::Action::Updated, uid).await;
        admin.flash_success(admin.t_with("profile.unlinked", &[("name", &schema)]));
    }
    Ok(admin.redirect(&format!("/admin/users/{uid}/edit")))
}

/// Keys-tab repeater delete action: account API key (`ApiKey::TYPE_ACCOUNT`,
/// activity `user:api-key.delete` with subject user + key).
async fn api_key_delete(admin: Admin, Path((uid, key_id)): Path<(i64, i64)>, form: Form) -> WebResult {
    let u = find(&admin, uid).await?;
    authorize_for(&admin, "update user", &u).await?;
    let st = &admin.st;
    let back = form.back(&format!("/admin/users/{uid}/edit?tab=keys"));
    if let Some(k) = api_keys::Entity::find_by_id(key_id)
        .filter(api_keys::Column::UserId.eq(u.id.0))
        .filter(api_keys::Column::KeyType.eq(1))
        .one(&st.db)
        .await?
    {
        // Repeater delete action → `$key->delete()`.
        api_keys::Entity::delete_by_id(k.id).exec(&st.db).await?;
        model_events::deleted(st, &k).await;
        use pelican_api::services::LogActivity;
        let _ = admin
            .activity("user:api-key.delete")
            .subject(morph::USER, u.id.0)
            .subject(morph::API_KEY, k.id.0)
            .property("identifier", k.identifier.clone().unwrap_or_default())
            .log(st)
            .await;
    }
    Ok(admin.redirect(&back))
}

/// Keys-tab repeater delete action: SSH key (soft-deleted, activity
/// `user:ssh-key.delete` with subject user + key).
async fn ssh_key_delete(admin: Admin, Path((uid, key_id)): Path<(i64, i64)>, form: Form) -> WebResult {
    use sea_orm::sea_query::Expr;
    let u = find(&admin, uid).await?;
    authorize_for(&admin, "update user", &u).await?;
    let st = &admin.st;
    let back = form.back(&format!("/admin/users/{uid}/edit?tab=keys"));
    if let Some(k) = user_ssh_keys::Entity::find_by_id(key_id)
        .filter(user_ssh_keys::Column::UserId.eq(u.id.0))
        .filter(user_ssh_keys::Column::DeletedAt.is_null())
        .one(&st.db)
        .await?
    {
        let stamp = ts();
        user_ssh_keys::Entity::update_many()
            .col_expr(user_ssh_keys::Column::DeletedAt, Expr::value(Some(stamp)))
            .col_expr(user_ssh_keys::Column::UpdatedAt, Expr::value(stamp))
            .filter(user_ssh_keys::Column::Id.eq(k.id))
            .exec(&st.db)
            .await?;
        // Repeater delete action → soft delete (`$key->delete()`); the row now
        // carries its deleted_at.
        model_events::fire_id::<user_ssh_keys::Entity>(st, model_events::Action::Deleted, k.id.0).await;
        use pelican_api::services::LogActivity;
        let _ = admin
            .activity("user:ssh-key.delete")
            .subject(morph::USER, u.id.0)
            .subject(morph::SSH_KEY, k.id.0)
            .property("fingerprint", k.fingerprint.clone())
            .log(st)
            .await;
    }
    Ok(admin.redirect(&back))
}

/// `POST .../avatar/delete`: removes the uploaded avatar from the public disk
/// (`FileUpload::deleteUploadedFileUsing`).
async fn avatar_delete(admin: Admin, Path(id): Path<i64>, form: Form) -> WebResult {
    let u = find(&admin, id).await?;
    authorize_for(&admin, "update user", &u).await?;
    let path = crate::models::public_disk(&admin.st).join("avatars").join(format!("{id}.png"));
    if path.exists() {
        std::fs::remove_file(path).map_err(WebError::internal)?;
    }
    Ok(admin.redirect(&form.back(&format!("/admin/users/{id}/edit"))))
}

// ----- ServersRelationManager header actions ---------------------------------

/// Bulk suspend/unsuspend the owned non-suspended/suspended servers
/// (`SuspensionService::handle` each).
async fn bulk_suspend(admin: Admin, uid: i64, suspend: bool) -> WebResult {
    let u = find(&admin, uid).await?;
    admin.authorize("update server")?;
    let back = format!("/admin/users/{uid}/edit?tab=servers");
    let owned = servers::Entity::find()
        .filter(servers::Column::OwnerId.eq(u.id.0))
        .order_by_asc(servers::Column::Id)
        .all(&admin.st.db)
        .await?;
    for s in owned {
        let is_suspended = s.status.as_deref() == Some("suspended");
        if is_suspended == suspend {
            continue;
        }
        let ctx = admin.api_ctx(Bound { server: Some(s.clone()), ..Default::default() });
        if let Err(e) = app_servers::set_suspension(&admin.st, &ctx, s, suspend).await {
            tracing::warn!(error = ?e, "bulk suspension failed");
            break;
        }
    }
    Ok(admin.redirect(&back))
}

async fn suspend_all(admin: Admin, Path(id): Path<i64>) -> WebResult {
    bulk_suspend(admin, id, true).await
}

async fn unsuspend_all(admin: Admin, Path(id): Path<i64>) -> WebResult {
    bulk_suspend(admin, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-render the form template in view / edit / create variants.
    #[test]
    fn form_template_renders() {
        crate::views::init(None);
        let v = context! {
            username => "jane", email => "jane@example.com", external_id => "",
            is_managed_externally => "0", timezone => "UTC", language => "en", roles => vec!["1"],
        };
        let errors = Mj::from_serialize(&Errors::default());
        let base = |record: Option<(i64, &str)>| {
            context! {
                title => "Users", action => "/admin/users/1/edit", tab => "account", tabs => TABS,
                record => record.map(|(id, name)| context! { id => id, name => name }),
                readonly => true, create_mode => record.is_none(), root_target => false,
                root_role_label => "The Root Admin has all permissions.",
                can_delete => true, delete_disabled => false, delete_tooltip => "Delete",
                managed_external => false, v => v.clone(), errors => errors.clone(),
                role_options => Vec::<(i64, String)>::new(), timezones => Vec::<(String, String)>::new(),
                languages => Vec::<(String, String)>::new(),
                oauth_entries => Vec::<Mj>::new(), has_oauth => false,
                api_keys => Vec::<Mj>::new(), has_api_keys => false,
                ssh_keys => Vec::<Mj>::new(), has_ssh_keys => false,
                activity => Mj::from_serialize(Vec::<Mj>::new()),
                servers_table => Mj::from_serialize(crate::table::Table::new("user-servers", "/admin/users/1/edit")),
                suspend_all_available => false, unsuspend_all_available => false,
            }
        };
        for ctx in [base(Some((1, "jane"))), base(None)] {
            let html = crate::views::render("admin/users/form.html", ctx).unwrap();
            assert!(html.contains("record-form"));
        }
    }
}
