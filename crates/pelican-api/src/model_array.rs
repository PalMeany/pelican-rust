//! Laravel `Model::toArray()` for the `eloquent.*` events (ADR-0019): one
//! function per table-backed model, building attributes in table-column order
//! with `$casts` applied. Key order follows the `pelican-db` entity field
//! order (table column order), matching the ADR's chosen convention.
//!
//! Rules mirrored from Laravel serialization:
//! - dates (`created_at`/`updated_at`/`deleted_at` and every
//!   `datetime`/`immutable_datetime` cast) go through the default
//!   `serializeDate`, i.e. `Carbon::toJSON()`: `Y-m-d\TH:i:s.u\Z` in UTC;
//! - `boolean` casts render as JSON booleans, integers as numbers;
//! - `array`/`collection`/`json` casts render decoded JSON with PHP associative
//!  semantics ([`pelican_core::server_config::php_assoc`]: an empty object is a
//!   list `[]`);
//! - enum casts (`ServerState`, `WebhookScope`) render their string value;
//! - `$hidden` attributes are omitted; encrypted-but-hidden columns never appear.

use pelican_core::server_config::php_assoc;
use pelican_core::time::iso_micro_z;
use serde_json::{Map, Value};

use pelican_db::types::{Json, Timestamp};

use pelican_db::entities::{
    activity_log_subjects, activity_logs, allocations, api_keys, backup_hosts, backups, database_hosts, databases,
    egg_variables, eggs, mounts, nodes, roles, schedules, server_transfers, server_user_settings, server_variables,
    servers, subusers, tasks, user_ssh_keys, users, webhook_configurations, webhooks,
};

/// Default `serializeDate`: `<naive-datetime>.000000Z`.
fn dt(t: Timestamp) -> Value {
    Value::String(iso_micro_z(&t.0))
}

fn opt_dt(t: Option<Timestamp>) -> Value {
    t.map(dt).unwrap_or(Value::Null)
}

/// Decoded JSON value of an `array`/`collection` cast.
fn json(v: &Json) -> Value {
    php_assoc(v.0.clone())
}

fn nullable_json(v: Option<&Json>) -> Value {
    v.map(json).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// Per-model toArray()
// ---------------------------------------------------------------------------

pub fn activity_log(m: &activity_logs::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("event".into(), Value::String(m.event.clone()));
    o.insert("ip".into(), Value::String(m.ip.clone()));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("actor_type".into(), m.actor_type.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("actor_id".into(), m.actor_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null));
    // 'properties' => 'collection' in app/Models/ActivityLog.php.
    o.insert("properties".into(), json(&m.properties));
    o.insert("timestamp".into(), dt(m.timestamp)); // 'timestamp' => 'datetime'
    o.insert("api_key_id".into(), m.api_key_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null));
    Value::Object(o)
}

pub fn activity_log_subject(m: &activity_log_subjects::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("activity_log_id".into(), Value::from(m.activity_log_id.0));
    o.insert("subject_type".into(), Value::String(m.subject_type.clone()));
    o.insert("subject_id".into(), Value::from(m.subject_id.0));
    Value::Object(o)
}

pub fn allocation(m: &allocations::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("node_id".into(), Value::from(m.node_id.0)); // 'integer'
    o.insert("ip".into(), Value::String(m.ip.clone()));
    o.insert("port".into(), Value::from(m.port.0)); // 'integer'
    o.insert("server_id".into(), m.server_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("ip_alias".into(), m.ip_alias.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("notes".into(), m.notes.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("is_locked".into(), Value::Bool(m.is_locked.0)); // 'bool'
    o.insert("show_port".into(), Value::Bool(m.show_port.0)); // 'bool'
    Value::Object(o)
}

pub fn api_key(m: &api_keys::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    // `token` ('encrypted') is in $hidden — omitted.
    o.insert("allowed_ips".into(), json(&m.allowed_ips)); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("user_id".into(), m.user_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'int'
    o.insert("memo".into(), m.memo.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("identifier".into(), m.identifier.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("key_type".into(), Value::from(m.key_type.0));
    o.insert("last_used_at".into(), opt_dt(m.last_used_at)); // 'datetime'
    o.insert("expires_at".into(), opt_dt(m.expires_at)); // 'datetime'
    o.insert("permissions".into(), nullable_json(m.permissions.as_ref())); // 'array'
    Value::Object(o)
}

pub fn backup(m: &backups::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0)); // 'int'
    o.insert("server_id".into(), Value::from(m.server_id.0));
    o.insert("uuid".into(), Value::String(m.uuid.clone()));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("ignored_files".into(), json(&m.ignored_files)); // 'array'
    o.insert("checksum".into(), m.checksum.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("bytes".into(), Value::from(m.bytes.0)); // 'int'
    o.insert("completed_at".into(), opt_dt(m.completed_at)); // 'immutable_datetime'
    o.insert("created_at".into(), opt_dt(m.created_at)); // 'immutable_datetime'
    o.insert("updated_at".into(), opt_dt(m.updated_at)); // 'immutable_datetime'
    o.insert("is_successful".into(), Value::Bool(m.is_successful.0)); // 'bool'
    o.insert("upload_id".into(), m.upload_id.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("is_locked".into(), Value::Bool(m.is_locked.0)); // 'bool'
    o.insert("backup_host_id".into(), Value::from(m.backup_host_id.0));
    o.insert("is_scheduled".into(), Value::Bool(m.is_scheduled.0)); // 'bool'
    Value::Object(o)
}

pub fn backup_host(m: &backup_hosts::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("schema".into(), Value::String(m.schema.clone()));
    o.insert("configuration".into(), nullable_json(m.configuration.as_ref())); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    Value::Object(o)
}

pub fn database(m: &databases::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'integer'
    o.insert("database_host_id".into(), Value::from(m.database_host_id.0)); // 'integer'
    o.insert("database".into(), Value::String(m.database.clone()));
    o.insert("username".into(), Value::String(m.username.clone()));
    o.insert("remote".into(), Value::String(m.remote.clone()));
    // `password` ('encrypted') is in $hidden — omitted.
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("max_connections".into(), m.max_connections.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    Value::Object(o)
}

pub fn database_host(m: &database_hosts::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0)); // 'integer'
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("host".into(), Value::String(m.host.clone()));
    o.insert("port".into(), Value::from(m.port.0));
    o.insert("username".into(), Value::String(m.username.clone()));
    // `password` ('encrypted') is in $hidden — omitted.
    o.insert("max_databases".into(), m.max_databases.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("created_at".into(), opt_dt(m.created_at)); // 'immutable_datetime'
    o.insert("updated_at".into(), opt_dt(m.updated_at)); // 'immutable_datetime'
    Value::Object(o)
}

pub fn egg(m: &eggs::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("config_from".into(), m.config_from.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("config_stop".into(), m.config_stop.clone().map(Value::String).unwrap_or(Value::Null));
    // No Eloquent casts for config_logs/config_startup/config_files: raw strings.
    o.insert("config_logs".into(), m.config_logs.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("config_startup".into(), m.config_startup.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("config_files".into(), m.config_files.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("script_install".into(), m.script_install.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("script_is_privileged".into(), Value::Bool(m.script_is_privileged.0)); // 'boolean'
    o.insert("script_entry".into(), Value::String(m.script_entry.clone()));
    o.insert("script_container".into(), Value::String(m.script_container.clone()));
    o.insert("copy_script_from".into(), m.copy_script_from.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("uuid".into(), m.uuid.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("author".into(), Value::String(m.author.clone()));
    o.insert("features".into(), nullable_json(m.features.as_ref())); // 'array'
    o.insert("docker_images".into(), nullable_json(m.docker_images.as_ref())); // 'array'
    o.insert("update_url".into(), m.update_url.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("file_denylist".into(), nullable_json(m.file_denylist.as_ref())); // 'array'
    o.insert("force_outgoing_ip".into(), Value::Bool(m.force_outgoing_ip.0)); // 'boolean'
    o.insert("tags".into(), nullable_json(m.tags.as_ref())); // 'array'
    o.insert("startup_commands".into(), nullable_json(m.startup_commands.as_ref())); // 'array'
    Value::Object(o)
}

pub fn egg_variable(m: &egg_variables::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("egg_id".into(), Value::from(m.egg_id.0)); // 'integer'
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("description".into(), Value::String(m.description.clone()));
    o.insert("env_variable".into(), Value::String(m.env_variable.clone()));
    o.insert("default_value".into(), Value::String(m.default_value.clone()));
    o.insert("user_viewable".into(), Value::Bool(m.user_viewable.0)); // 'bool'
    o.insert("user_editable".into(), Value::Bool(m.user_editable.0)); // 'bool'
    o.insert("rules".into(), json(&m.rules)); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at)); // 'immutable_datetime'
    o.insert("updated_at".into(), opt_dt(m.updated_at)); // 'immutable_datetime'
    o.insert("sort".into(), m.sort.map(|v| Value::from(v.0)).unwrap_or(Value::Null));
    Value::Object(o)
}

pub fn mount(m: &mounts::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0)); // 'int'
    o.insert("uuid".into(), Value::String(m.uuid.clone()));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("source".into(), Value::String(m.source.clone()));
    o.insert("target".into(), Value::String(m.target.clone()));
    o.insert("read_only".into(), Value::Bool(m.read_only.0)); // 'bool'
    o.insert("user_mountable".into(), Value::Bool(m.user_mountable.0)); // 'bool'
    Value::Object(o)
}

pub fn node(m: &nodes::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("uuid".into(), m.uuid.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("public".into(), Value::Bool(m.public.0)); // 'boolean'
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("fqdn".into(), Value::String(m.fqdn.clone()));
    o.insert("scheme".into(), Value::String(m.scheme.clone()));
    o.insert("behind_proxy".into(), Value::Bool(m.behind_proxy.0)); // 'boolean'
    o.insert("maintenance_mode".into(), Value::Bool(m.maintenance_mode.0)); // 'boolean'
    o.insert("memory".into(), Value::from(m.memory.0)); // 'integer'
    o.insert("memory_overallocate".into(), Value::from(m.memory_overallocate.0));
    o.insert("disk".into(), Value::from(m.disk.0)); // 'integer'
    o.insert("disk_overallocate".into(), Value::from(m.disk_overallocate.0));
    o.insert("cpu".into(), Value::from(m.cpu.0)); // 'integer'
    o.insert("cpu_overallocate".into(), Value::from(m.cpu_overallocate.0));
    o.insert("upload_size".into(), Value::from(m.upload_size.0));
    // `daemon_token_id` and `daemon_token` ('encrypted') are in $hidden — omitted.
    o.insert("daemon_listen".into(), Value::from(m.daemon_listen.0)); // 'integer'
    o.insert("daemon_sftp".into(), Value::from(m.daemon_sftp.0)); // 'integer'
    o.insert("daemon_sftp_alias".into(), m.daemon_sftp_alias.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("daemon_base".into(), Value::String(m.daemon_base.clone()));
    o.insert("daemon_connect".into(), Value::from(m.daemon_connect.0)); // 'integer'
    o.insert("tags".into(), nullable_json(m.tags.as_ref())); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    Value::Object(o)
}

pub fn role(m: &roles::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("guard_name".into(), Value::String(m.guard_name.clone()));
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    Value::Object(o)
}

pub fn schedule(m: &schedules::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0)); // 'integer'
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'integer'
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("cron_day_of_week".into(), Value::String(m.cron_day_of_week.clone()));
    o.insert("cron_day_of_month".into(), Value::String(m.cron_day_of_month.clone()));
    o.insert("cron_hour".into(), Value::String(m.cron_hour.clone()));
    o.insert("cron_minute".into(), Value::String(m.cron_minute.clone()));
    o.insert("is_active".into(), Value::Bool(m.is_active.0)); // 'boolean'
    o.insert("is_processing".into(), Value::Bool(m.is_processing.0)); // 'boolean'
    o.insert("last_run_at".into(), opt_dt(m.last_run_at)); // 'datetime'
    o.insert("next_run_at".into(), opt_dt(m.next_run_at)); // 'datetime'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("cron_month".into(), Value::String(m.cron_month.clone()));
    o.insert("only_when_online".into(), Value::Bool(m.only_when_online.0)); // 'boolean'
    o.insert("timezone".into(), Value::String(m.timezone.clone()));
    Value::Object(o)
}

pub fn server(m: &servers::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("uuid".into(), Value::String(m.uuid.clone()));
    o.insert("uuid_short".into(), Value::String(m.uuid_short.clone()));
    o.insert("node_id".into(), Value::from(m.node_id.0)); // 'integer'
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("owner_id".into(), Value::from(m.owner_id.0)); // 'integer'
    o.insert("memory".into(), Value::from(m.memory.0)); // 'integer'
    o.insert("swap".into(), Value::from(m.swap.0)); // 'integer'
    o.insert("disk".into(), Value::from(m.disk.0)); // 'integer'
    o.insert("io".into(), Value::from(m.io.0)); // 'integer'
    o.insert("cpu".into(), Value::from(m.cpu.0)); // 'integer'
    o.insert("egg_id".into(), Value::from(m.egg_id.0)); // 'integer'
    o.insert("startup".into(), Value::String(m.startup.clone()));
    o.insert("created_at".into(), opt_dt(m.created_at)); // 'datetime'
    o.insert("updated_at".into(), opt_dt(m.updated_at)); // 'datetime'
    o.insert("allocation_id".into(), m.allocation_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("image".into(), Value::String(m.image.clone()));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("skip_scripts".into(), Value::Bool(m.skip_scripts.0)); // 'boolean'
    o.insert("external_id".into(), m.external_id.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("database_limit".into(), m.database_limit.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("allocation_limit".into(), m.allocation_limit.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'integer'
    o.insert("threads".into(), m.threads.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("backup_limit".into(), Value::from(m.backup_limit.0)); // 'integer'
    // 'status' => ServerState::class — string-backed enum renders its value.
    o.insert("status".into(), m.status.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("installed_at".into(), opt_dt(m.installed_at)); // 'datetime'
    o.insert("oom_killer".into(), Value::Bool(m.oom_killer.0)); // 'boolean'
    o.insert("docker_labels".into(), nullable_json(m.docker_labels.as_ref())); // 'array'
    Value::Object(o)
}

pub fn server_transfer(m: &server_transfers::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'int'
    o.insert("successful".into(), m.successful.map(|v| Value::Bool(v.0)).unwrap_or(Value::Null)); // 'bool'
    o.insert("old_node".into(), Value::from(m.old_node.0)); // 'int'
    o.insert("new_node".into(), Value::from(m.new_node.0)); // 'int'
    o.insert("old_allocation".into(), m.old_allocation.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'int'
    o.insert("new_allocation".into(), m.new_allocation.map(|v| Value::from(v.0)).unwrap_or(Value::Null)); // 'int'
    o.insert(
        "old_additional_allocations".into(),
        nullable_json(m.old_additional_allocations.as_ref()), // 'array'
    );
    o.insert(
        "new_additional_allocations".into(),
        nullable_json(m.new_additional_allocations.as_ref()), // 'array'
    );
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("archived".into(), Value::Bool(m.archived.0)); // 'bool'
    Value::Object(o)
}

pub fn server_user_settings(m: &server_user_settings::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("user_id".into(), Value::from(m.user_id.0)); // 'int'
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'int'
    o.insert("settings".into(), nullable_json(m.settings.as_ref())); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    Value::Object(o)
}

pub fn server_variable(m: &server_variables::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'integer'
    o.insert("variable_id".into(), Value::from(m.variable_id.0)); // 'integer'
    o.insert("variable_value".into(), Value::String(m.variable_value.clone()));
    o.insert("created_at".into(), opt_dt(m.created_at)); // 'immutable_datetime'
    o.insert("updated_at".into(), opt_dt(m.updated_at)); // 'immutable_datetime'
    Value::Object(o)
}

pub fn subuser(m: &subusers::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("user_id".into(), Value::from(m.user_id.0)); // 'int'
    o.insert("server_id".into(), Value::from(m.server_id.0)); // 'int'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("permissions".into(), nullable_json(m.permissions.as_ref())); // 'array'
    Value::Object(o)
}

pub fn task(m: &tasks::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0)); // 'integer'
    o.insert("schedule_id".into(), Value::from(m.schedule_id.0)); // 'integer'
    o.insert("sequence_id".into(), Value::from(m.sequence_id.0)); // 'integer'
    o.insert("action".into(), Value::String(m.action.clone()));
    // No Eloquent cast on `payload`: raw string as stored.
    o.insert("payload".into(), Value::String(m.payload.clone()));
    o.insert("time_offset".into(), Value::from(m.time_offset.0)); // 'integer'
    o.insert("is_queued".into(), Value::Bool(m.is_queued.0)); // 'boolean'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("continue_on_failure".into(), Value::Bool(m.continue_on_failure.0)); // 'boolean'
    Value::Object(o)
}

pub fn user(m: &users::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("uuid".into(), Value::String(m.uuid.clone()));
    o.insert("email".into(), Value::String(m.email.clone()));
    // $hidden: password, remember_token, mfa_app_secret, mfa_app_recovery_codes, oauth.
    o.insert("language".into(), Value::String(m.language.clone()));
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("username".into(), Value::String(m.username.clone()));
    o.insert("external_id".into(), m.external_id.clone().map(Value::String).unwrap_or(Value::Null));
    o.insert("timezone".into(), Value::String(m.timezone.clone()));
    o.insert("customization".into(), nullable_json(m.customization.as_ref())); // 'array', not hidden
    o.insert("mfa_email_enabled".into(), Value::Bool(m.mfa_email_enabled.0)); // 'boolean'
    o.insert("is_managed_externally".into(), Value::Bool(m.is_managed_externally.0)); // 'boolean'
    Value::Object(o)
}

pub fn user_ssh_key(m: &user_ssh_keys::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("user_id".into(), Value::from(m.user_id.0));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("fingerprint".into(), Value::String(m.fingerprint.clone()));
    o.insert("public_key".into(), Value::String(m.public_key.clone()));
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("deleted_at".into(), opt_dt(m.deleted_at)); // SoftDeletes column
    Value::Object(o)
}

pub fn webhook(m: &webhooks::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("webhook_configuration_id".into(), Value::from(m.webhook_configuration_id.0));
    o.insert("event".into(), Value::String(m.event.clone()));
    o.insert("endpoint".into(), Value::String(m.endpoint.clone()));
    o.insert("successful_at".into(), opt_dt(m.successful_at)); // 'datetime'
    o.insert("payload".into(), json(&m.payload)); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    Value::Object(o)
}

pub fn webhook_configuration(m: &webhook_configurations::Model) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), Value::from(m.id.0));
    o.insert("endpoint".into(), Value::String(m.endpoint.clone()));
    o.insert("name".into(), Value::String(m.name.clone()));
    o.insert("events".into(), json(&m.events)); // 'array'
    o.insert("created_at".into(), opt_dt(m.created_at));
    o.insert("updated_at".into(), opt_dt(m.updated_at));
    o.insert("deleted_at".into(), opt_dt(m.deleted_at)); // SoftDeletes column
    o.insert("type".into(), m.r#type.clone().map(Value::String).unwrap_or(Value::Null)); // 'string'
    o.insert("payload".into(), nullable_json(m.payload.as_ref())); // 'array'
    o.insert("headers".into(), nullable_json(m.headers.as_ref())); // 'array'
    // 'scope' => WebhookScope::class — string-backed enum renders its value.
    o.insert("scope".into(), Value::String(m.scope.clone()));
    o.insert("server_id".into(), m.server_id.map(|v| Value::from(v.0)).unwrap_or(Value::Null));
    o.insert("description".into(), m.description.clone().map(Value::String).unwrap_or(Value::Null));
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
    use serde_json::json;

    use super::*;
    use pelican_db::types::{Bool, Int, SmallBool};

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Timestamp {
        Timestamp(NaiveDateTime::new(
            NaiveDate::from_ymd_opt(y, mo, d).expect("date"),
            NaiveTime::from_hms_opt(h, mi, s).expect("time"),
        ))
    }

    #[test]
    fn server_to_array() {
        let s = servers::Model {
            id: Int(4),
            uuid: "4864a058-9a3b-44a9-a6cf-c1355e89406e".into(),
            uuid_short: "4864a058".into(),
            node_id: Int(1),
            name: "Example Server".into(),
            owner_id: Int(1),
            memory: Int(6144),
            swap: Int(0),
            disk: Int(20480),
            io: Int(500),
            cpu: Int(300),
            egg_id: Int(1),
            startup: "java {{SERVER_JARFILE}}".into(),
            created_at: Some(ts(2025, 9, 5, 1, 15, 43)),
            updated_at: Some(ts(2025, 9, 11, 22, 45, 14)),
            allocation_id: Some(Int(4)),
            image: "ghcr.io/pelican-eggs/yolks:java_21".into(),
            description: None,
            skip_scripts: Bool(false),
            external_id: None,
            database_limit: Some(Int(5)),
            allocation_limit: Some(Int(5)),
            threads: None,
            backup_limit: Int(5),
            status: None,
            installed_at: None,
            oom_killer: SmallBool(false),
            docker_labels: Some(Json(json!({}))),
        };
        let v = server(&s);
        assert_eq!(v["created_at"], json!("2025-09-05T01:15:43.000000Z"), "dates serialize via Carbon::toJSON");
        assert_eq!(v["database_limit"], json!(5));
        assert_eq!(v["skip_scripts"], json!(false));
        assert_eq!(v["oom_killer"], json!(false));
        // Empty PHP object from the json decode becomes [].
        assert_eq!(v["docker_labels"], json!([]));
        // Column order = entity field order.
        let keys: Vec<&str> = v.as_object().expect("object").keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "uuid",
                "uuid_short",
                "node_id",
                "name",
                "owner_id",
                "memory",
                "swap",
                "disk",
                "io",
                "cpu",
                "egg_id",
                "startup",
                "created_at",
                "updated_at",
                "allocation_id",
                "image",
                "description",
                "skip_scripts",
                "external_id",
                "database_limit",
                "allocation_limit",
                "threads",
                "backup_limit",
                "status",
                "installed_at",
                "oom_killer",
                "docker_labels",
            ]
        );
    }

    #[test]
    fn user_hidden_fields() {
        let u = users::Model {
            id: Int(2),
            uuid: "b5f8ac41-1fda-4bb7-bc50-f7ddcd99babd".into(),
            email: "admin@example.com".into(),
            password: "$2y$12$hash".into(),
            remember_token: Some("tok".into()),
            language: "en".into(),
            created_at: Some(ts(2025, 9, 5, 1, 15, 42)),
            updated_at: Some(ts(2025, 9, 5, 1, 15, 43)),
            username: "admin".into(),
            external_id: None,
            timezone: "UTC".into(),
            oauth: Some(Json(json!({"github": {"id": 1}}))),
            customization: Some(Json(json!({}))),
            mfa_app_secret: Some("enc-secret".into()),
            mfa_app_recovery_codes: Some("enc-codes".into()),
            mfa_email_enabled: Bool(false),
            is_managed_externally: Bool(true),
        };
        let v = user(&u);
        let keys: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        for hidden in ["password", "remember_token", "mfa_app_secret", "mfa_app_recovery_codes", "oauth"] {
            assert!(!keys.contains(&hidden.to_string()), "{hidden} must be hidden");
        }
        // customization is cast to array but not hidden.
        assert_eq!(v["customization"], json!([]));
        assert_eq!(v["is_managed_externally"], json!(true));
        assert_eq!(v["email"], json!("admin@example.com"));
    }

    #[test]
    fn node_hidden_daemon_token() {
        let n = nodes::Model {
            id: Int(1),
            uuid: Some("6bf57cbd-24be-4954-ab75-18f65678ccb6".into()),
            public: SmallBool(true),
            name: "Local".into(),
            description: None,
            fqdn: "node.example.com".into(),
            scheme: "https".into(),
            behind_proxy: Bool(true),
            maintenance_mode: Bool(false),
            memory: Int(1024),
            memory_overallocate: Int(0),
            disk: Int(4096),
            disk_overallocate: Int(0),
            cpu: Int(100),
            cpu_overallocate: Int(0),
            upload_size: Int(100),
            daemon_token_id: Some("XyZ1234567890ab".into()),
            daemon_token: "secret-token".into(),
            daemon_listen: Int(8080),
            daemon_sftp: Int(2022),
            daemon_sftp_alias: None,
            daemon_base: "/var/lib/pelican/volumes".into(),
            daemon_connect: Int(8080),
            tags: Some(Json(json!(["tag"]))),
            created_at: Some(ts(2025, 7, 1, 20, 12, 41)),
            updated_at: Some(ts(2025, 9, 9, 17, 47, 22)),
        };
        let v = node(&n);
        let keys: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        assert!(!keys.contains(&"daemon_token".to_string()));
        assert!(!keys.contains(&"daemon_token_id".to_string()));
        assert_eq!(v["behind_proxy"], json!(true));
        assert_eq!(v["public"], json!(true));
        assert_eq!(v["tags"], json!(["tag"]));
        assert_eq!(v["memory"], json!(1024));
    }

    #[test]
    fn egg_json_fields() {
        let e = eggs::Model {
            id: Int(1),
            name: "Minecraft Egg".into(),
            description: Some("Java-based edition".into()),
            created_at: Some(ts(2024, 1, 1, 0, 0, 0)),
            updated_at: Some(ts(2024, 1, 1, 0, 0, 1)),
            config_from: None,
            config_stop: Some("^stop".into()),
            config_logs: Some("{\"files\":[]}".to_owned()),
            config_startup: Some("{}".to_owned()),
            config_files: Some("[]".to_owned()),
            script_install: None,
            script_is_privileged: Bool(true),
            script_entry: "bash".into(),
            script_container: "ghcr.io/yolk/alpine:latest".into(),
            copy_script_from: None,
            uuid: Some("29177ba2-1b8c-4381-a236-e235ea28e14a".into()),
            author: "panel@example.com".into(),
            features: Some(Json(json!(["eula"]))),
            docker_images: Some(Json(json!({"java_17": "ghcr.io/yolk/java:17"}))),
            update_url: None,
            file_denylist: Some(Json(json!([]))),
            force_outgoing_ip: Bool(false),
            tags: Some(Json(json!(["mc"]))),
            startup_commands: Some(Json(json!({"run": "start"}))),
        };
        let v = egg(&e);
        // Array casts decode the stored JSON.
        assert_eq!(v["features"], json!(["eula"]));
        assert_eq!(v["docker_images"], json!({"java_17": "ghcr.io/yolk/java:17"}));
        assert_eq!(v["file_denylist"], json!([]));
        // config_* columns have no Eloquent cast: raw strings.
        assert_eq!(v["config_logs"], json!("{\"files\":[]}"));
        assert_eq!(v["script_is_privileged"], json!(true));
    }

    #[test]
    fn backup_bool_and_int_casts() {
        let b = backups::Model {
            id: Int(1),
            server_id: Int(10),
            uuid: "c1d9a08e-5cbc-443b-81cc-98cffc62ef29".into(),
            name: "Backup 1".into(),
            ignored_files: Json(json!(["*.log"])),
            checksum: Some("sha256:abc".into()),
            bytes: Int(1024),
            completed_at: Some(ts(2025, 9, 5, 2, 0, 0)),
            created_at: Some(ts(2025, 9, 5, 1, 59, 59)),
            updated_at: Some(ts(2025, 9, 5, 2, 0, 1)),
            is_successful: Bool(true),
            upload_id: None,
            is_locked: SmallBool(false),
            backup_host_id: Int(3),
            is_scheduled: Bool(true),
        };
        let v = backup(&b);
        assert_eq!(v["is_successful"], json!(true));
        assert_eq!(v["is_locked"], json!(false));
        assert_eq!(v["bytes"], json!(1024));
        assert_eq!(v["ignored_files"], json!(["*.log"]));
        assert_eq!(v["completed_at"], json!("2025-09-05T02:00:00.000000Z"));

        // Null nullable date serializes to null.
        let mut b2 = b.clone();
        b2.completed_at = None;
        b2.upload_id = None;
        let v2 = backup(&b2);
        assert_eq!(v2["completed_at"], Value::Null);
        assert_eq!(v2["upload_id"], Value::Null);
    }
}
