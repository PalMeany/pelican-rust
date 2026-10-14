# Pelican Panel: Models, Services, Enums, Settings (DB-compatibility spec)

Source of truth: `panel/` (PHP 8.5, Laravel 13). Framework behaviour was checked against the vendored
sources in `.ref/vendor` (laravel/framework **v13.35.0**, laravel/sanctum, spatie/laravel-permission,
filament/notifications, s1lentium/iptools). Paths below are relative to `panel/` unless they start
with `vendor/` (meaning `.ref/vendor/`).

Goal: a Rust rewrite that runs against the **same database** and produces the **same row-level diff**
for the same API call. Anything that affects that diff (column values, JSON encodings, timestamps,
insert order, side-table rows, rollback behaviour) is called out. Items marked **[QUIRK]** are
behaviours that look like bugs but must be reproduced for DB-diff parity. Items marked
**[UNCERTAIN]** are listed again in the final section.

---

## 0. Global Eloquent and framework behaviour (applies to every model)

### 0.1 Timestamps

* Default columns: `created_at`, `updated_at`. No model in `app/Models` renames them, and none sets
  `const UPDATED_AT = null`.
* On insert, `created_at` and `updated_at` get **the same** value from one `freshTimestamp()` call
  (`vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php:1616-1618`, `performInsert`).
* On update, `updated_at` is set only when the model is dirty. A `save()` on a clean model issues
  **no SQL**, but the `saving` and `saved` events still fire (relevant for `Server::saved`, §1.2).
* Storage format: `Y-m-d H:i:s` (`vendor/.../Database/Grammar.php:282-285`; MySQL, MariaDB, SQLite
  and PostgreSQL all use it). Seconds precision, no fraction. The app timezone is `UTC`
  (`config/app.php:11`).
* Eloquent **builder** `update()`, `increment()` and `decrement()` add `updated_at = now()`
  automatically (`vendor/.../Eloquent/Builder.php:1315-1318, 1382-1402, 1438-1460`). This also covers
  relation queries such as `$server->allocations()->update(...)` and `$schedule->tasks()->increment(...)`.
  Builder updates do **not** fire model events (`updating`, `updated`, webhooks).
* Models without timestamps: `ActivityLog` (`$timestamps=false`, has its own `timestamp` column),
  `ActivityLogSubject`, `Mount`. `NodeRole` is a pivot whose table has no timestamp columns.
* `$touches`: `Task` touches `schedule` (`app/Models/Task.php:60`). Every Task save that changed
  something, and every Task delete, issues `UPDATE schedules SET updated_at=now WHERE id=?`. This is a
  raw relation update (`Relation::touch()` → `rawUpdate`). The parent's `saved` event is then fired
  manually (`vendor/.../Eloquent/Concerns/HasRelationships.php:1000-1015`;
  `Model.php:1500-1509`, `finishSave`).
* Soft deletes: only `UserSSHKey` and `WebhookConfiguration`. A soft delete sets `deleted_at` **and**
  `updated_at` to the same now. `Backup` soft deletes were removed
  (`database/migrations/2026_08_06_100741_drop_backup_soft_deletes.php`), so backups are hard-deleted.

### 0.2 JSON-family casts (`array`, `json`, `collection`, `encrypted:array`)

* Encoded with `Json::encode($value, $flags)` → `json_encode($value, 0)`
  (`vendor/.../Eloquent/Concerns/HasAttributes.php:1392-1432`, `vendor/.../Eloquent/Casts/Json.php`).
  The panel never calls `Json::encodeUsing`, and no model uses `json:unicode`. That means:
  * `/` is escaped as `\/` (for example `"logs\/*"`, `"Mozilla\/5.0"`).
  * Non-ASCII characters are escaped as `\uXXXX`.
  * No whitespace (compact) and no pretty-printing.
  * Key order is PHP insertion order.
  * A PHP list (sequential keys `0..n-1`) becomes a JSON array. Any other keyed array becomes an
    object. An **empty array is always `[]`**, never `{}`.
  * A `Collection` encodes through `jsonSerialize()`. The same list/object rules apply.
* `null` is never JSON-encoded. Setting a JSON-cast attribute to `null` stores SQL `NULL`
  (`HasAttributes.php:1117`, the `! is_null($value)` guard).
* A string assigned to a JSON-cast attribute is JSON-encoded as a JSON **string**. For example,
  `'rules' => 'required|string'` is stored as `"required|string"` including the quotes.
* `$attributes` defaults written as raw strings (for example `'tags' => '[]'`) are inserted
  verbatim without re-encoding.
* Caution: on **MySQL**, columns declared `json` are normalised by the server (canonical spacing and
  key ordering on read-back). MariaDB, SQLite and `TEXT` columns keep the bytes verbatim. The
  DB-diff harness should compare JSON columns semantically, and byte-for-byte only for TEXT columns
  (for example Egg `config_*`).

### 0.3 `encrypted` and `encrypted:array` casts (Laravel Encrypter format)

`HasAttributes.php:1468-1471` calls `Crypt::encrypt($value, serialize: false)`
(`vendor/.../Encryption/Encrypter.php:104-131`). There is **no PHP `serialize()`**.

```
iv     = random 16 bytes;                       cipher = AES-256-CBC (config app.cipher)
value  = base64(openssl_encrypt(plaintext, aes-256-cbc, key, 0, iv))   // options=0 ⇒ base64 output
mac    = hex(HMAC-SHA256(key, base64(iv) . value))
stored = base64( json_encode({"iv":base64(iv),"value":value,"mac":mac,"tag":""}, JSON_UNESCAPED_SLASHES) )
```

* `key` is `APP_KEY` with the `base64:` prefix decoded. Decryption also tries `APP_PREVIOUS_KEYS`.
* `encrypted:array` JSON-encodes first (flags 0) and then encrypts the JSON string.
* Ciphertext is non-deterministic, so DB diffs must decrypt before comparing.
* Encrypted columns: `api_keys.token`, `databases.password`, `database_hosts.password`,
  `nodes.daemon_token`, `users.mfa_app_secret`, `users.mfa_app_recovery_codes` (`encrypted:array`).
* Dirty checking compares decrypted values, so re-saving an unchanged value does not rewrite it.

### 0.4 Scalars, booleans and enums

* The `boolean`/`bool`/`integer`/`int` casts only affect reads. On write, PHP `true`/`false` is bound
  as `1`/`0` (Laravel `prepareBindings`). Strings are bound as given.
* Enum casts (`Server.status`, `WebhookConfiguration.scope`) store `->value`. Enum instances in
  `$attributes` defaults are also bound as `->value` by the query builder.
* Datetime attributes assigned a Carbon or DateTime object are converted with `fromDateTime()` to
  `Y-m-d H:i:s`.

### 0.5 Random values and identifiers

* `Str::random(n)`: characters `[A-Za-z0-9]` (base64 with `+/=` stripped;
  `vendor/.../Support/Str.php:1158-1175`).
* `Str::uuid()` is `Uuid::uuid4()` (`Str.php:2111-2116`). `Ramsey\Uuid\Uuid::uuid4()->toString()`
  gives a lowercase, hyphenated v4 UUID.
* Password hashing: `bcrypt`, cost `BCRYPT_ROUNDS` (default 12)
  (`vendor/laravel/framework/config/hashing.php:18,32`).

### 0.6 Mass assignment

* Models with `$guarded` (not `['*']`) drop keys that are not real table columns: Laravel checks
  `isGuardableColumn` against the schema listing. `forceFill`/`forceCreate` bypass both checks.
* Unlisted keys passed to `fill()` on `$fillable` models are dropped silently.

### 0.7 Morph map (enforced)

`app/Providers/AppServiceProvider.php:127-140` calls `Relation::enforceMorphMap`:

| morph key | class |
|---|---|
| `allocation` | Allocation |
| `api_key` | ApiKey |
| `backup` | Backup |
| `database` | Database |
| `egg` | Egg |
| `egg_variable` | EggVariable |
| `schedule` | Schedule |
| `server` | Server |
| `ssh_key` | UserSSHKey |
| `task` | Task |
| `user` | User |
| `node` | Node |

These keys are what gets stored in `activity_logs.actor_type`, `activity_log_subjects.subject_type`,
`mountables.mountable_type`, `model_has_roles.model_type` (`user`) and
`notifications.notifiable_type`. A morph of any other class (Role, Mount, Subuser, DatabaseHost, …)
would throw `ClassMorphViolationException`.

### 0.8 Model-level validation (`HasValidation`): probably not active **[UNCERTAIN]**

* `app/Traits/HasValidation.php:17` declares `#[ObservedBy([ValidationObserver::class])]` **on the
  trait**. `ValidationObserver::saving` (`app/Observers/ValidationObserver.php:12-19`) would call
  `$model->validate()` and turn failures into `DataValidationException` (HTTP 500).
* Laravel resolves observers only from attributes on the concrete class, and on its parent class
  for grandchildren (`vendor/.../Eloquent/Concerns/HasEvents.php:49-64`). PHP does not copy
  class-level attributes from a trait into the using class. **The observer is therefore most likely
  never registered**, which would make the `$validationRules` below active only where a FormRequest
  or controller reuses them (`Model::getRules()`, `getRulesForUpdate()`). Nothing else calls
  `validate()`.
* The rules are still documented for each model because requests reuse them.
* `getRulesForUpdate($model)` (`HasValidation.php:57-80`) rewrites every `unique:table[,col]` rule
  to `Rule::unique(table, col ?? field)->ignore(id, keyName)`.
* If validation did run, the data validated is the raw attributes with casts applied
  (`HasValidation.php:93-104`). A model with a `skipValidation` property skips it; no model has one.

### 0.9 Side effects that accompany writes

1. **Webhook fan-out** (`app/Providers/EventServiceProvider.php:14-20`). `DispatchWebhooks` listens
   to `eloquent.created*`, `eloquent.updated*`, `eloquent.deleted*`, `App\Events\*` and
   `ActivityLogged`.
   * For model events it queries `webhook_configurations WHERE scope='global' AND
     JSON_CONTAINS(events, '"eloquent.created: App\\Models\\X"')`
     (`app/Listeners/DispatchWebhooks.php:39-67`). For each match it dispatches the
     `ProcessWebhook` job with `data = $model->toArray()`.
   * The job writes one `webhooks` row per delivery attempt: `payload` (array cast), `successful_at`
     (now or NULL), `event`, `endpoint`, `webhook_configuration_id`
     (`app/Jobs/ProcessWebhook.php:69-74`).
   * With no matching configuration, nothing is written.
   * Builder-level updates and deletes (§0.1) do **not** trigger this.
2. **Queue**. The queue connection defaults to `database`
   (`vendor/laravel/framework/config/queue.php:16`; the panel has no `config/queue.php`). Each of
   the following inserts a `jobs` row instead of running inline:
   * Jobs: `ProcessWebhook`, `RevokeSftpAccessJob` (also `ShouldBeUnique`, which takes a cache lock),
     `RunTaskJob`.
   * Mail notifications: `AccountCreated`, `AddedToServer`, `RemovedFromServer`, `ServerInstalled`,
     `BackupCompleted`.
   * Filament database notifications (`Filament\Notifications\DatabaseNotification implements
     ShouldQueue`; `vendor/filament/notifications/src/DatabaseNotification.php:11`).

   With `QUEUE_CONNECTION=sync` they execute inline instead: Wings HTTP calls, plus `notifications`
   rows with `type = Filament\Notifications\DatabaseNotification`, `notifiable_type='user'` and
   `data` = Filament JSON. **The DB-diff harness must pin the queue driver.**
3. **Activity log**. See §2.1. Rows go to `activity_logs` and `activity_log_subjects`.
4. **Sanctum `last_used_at`**. Every request authenticated with a bearer API key runs
   `UPDATE api_keys SET last_used_at=now, updated_at=now WHERE id=?`
   (`vendor/laravel/sanctum/src/Guard.php:56-58,162-173`; `config('sanctum.last_used_at', true)`).
   It also fires `eloquent.updated: App\Models\ApiKey`, which feeds webhooks.
5. **Event discovery**. It is on (`Application::configure()->withEvents()`), so listeners in
   `app/Listeners` are auto-registered by their `handle()` type hints.

### 0.10 Wings HTTP error semantics (affects rollback decisions)

* `Http::daemon($node)` macro (`app/Providers/AppServiceProvider.php:68-78`): JSON, bearer token
  `daemon_token`, TLS verify only in production, timeout `GUZZLE_TIMEOUT` (15 s), connect timeout
  `GUZZLE_CONNECT_TIMEOUT` (5 s), base URL `scheme://fqdn:daemon_connect`.
* `DaemonRepository::getHttpClient()` adds `throwIf(callback)`
  (`app/Repositories/Daemon/DaemonRepository.php:48-67`). Laravel only invokes the callback for
  **non-2xx** responses (`vendor/.../Http/Client/PendingRequest.php:1093-1108`). The outcomes are:
  * Connection or timeout failure: `ConnectionException`.
  * Non-2xx response whose `User-Agent` is empty, or matches `Pelican Wings/vX.Y.Z (id:<id>)` with
    `<id> != node.daemon_token_id`: `ConnectionException` (code 0).
  * Any other non-2xx response: `RequestException` (code = HTTP status).
  * 2xx responses are never token-checked.
* Calls made with `Http::daemon()` directly (transfer notify, `Server::send`) do **not** throw on
  non-2xx responses.

### 0.11 Database-engine specifics and the diff-test configuration

Schema dumps are in `crates/pelican-db/schema/{mysql,mariadb,pgsql,sqlite}.sql`. The diff harness
(`docs/decisions/0007-differential-testing.md`) runs the PHP panel with `QUEUE_CONNECTION=sync`,
`CACHE_STORE=array` and `SESSION_DRIVER=array`.

* **Booleans on bind**: PHP bools become `1`/`0`
  (`vendor/.../Database/Connection.php:771-787`). On PostgreSQL they also become `1`/`0` unless
  emulated prepares are on, in which case they become `'true'`/`'false'`
  (`vendor/.../Database/PostgresConnection.php:31-46`). PostgreSQL coerces either form into
  `boolean` or `smallint` columns. In pgsql some flag columns are `smallint`, for example
  `backups.is_locked`, `egg_variables.user_viewable`, `mounts.read_only`; others are `boolean`.
* **JSON columns**:
  * pgsql `json` (not `jsonb`) stores the input text **verbatim**, so PHP's exact bytes (§0.2)
    are visible there.
  * MySQL `JSON` re-serializes on read.
  * MariaDB `JSON` is `LONGTEXT` and stores verbatim.
  * SQLite `TEXT` stores verbatim.
  * Several "array" columns are plain `text` in pgsql (`nodes.tags`, `eggs.tags`,
    `backups.ignored_files`) and are verbatim on every engine.
* **Timestamps**: pgsql uses `timestamp(0) without time zone`. A `Y-m-d H:i:s` UTC string goes
  in unchanged.
* **`whereJsonContains`** (read-only; used by webhook matching and `NodeAutoDeployService`):
  * MySQL/MariaDB: `json_contains(col, '"x"')`
  * pgsql: `(col)::jsonb @> '["x"]'`
  * SQLite: `json_each`
* **`inRandomOrder()`**: MySQL `RAND()`, pgsql and SQLite `RANDOM()`. **`lockForUpdate()`**
  compiles to `FOR UPDATE` and is a no-op on SQLite.
* **Effects of `QUEUE_CONNECTION=sync`**: every queued item from §0.9 runs inline, **inside**
  whatever transaction dispatched it. No `jobs` rows are written. Instead:
  * **`notifications` rows** come from Filament DB notifications, sent by the `SubUserAdded`,
    `SubUserRemoved`, `Installed` and `BackupCompleted` listeners. Columns:
    * `id = (string) Str::uuid()` (v4; `vendor/.../Notifications/NotificationSender.php:117`)
    * `type = 'Filament\Notifications\DatabaseNotification'`
    * `notifiable_type = 'user'`, `notifiable_id`
    * `read_at = NULL`, timestamps
    * `data` = json (flags 0) of Filament's `getDatabaseMessage()`
      (`vendor/filament/notifications/src/Notification.php:69-94,237-245`), with keys in this
      order: `actions`, `body`, `color`, `duration` (`"persistent"`), `icon`, `iconColor`,
      `iconSize`, `status`, `title`, `view`, `viewData`, `format` (`"filament"`).
  * **Mail notifications** go to `MAIL_MAILER` (default `log`). They write no DB rows.
  * **`RevokeSftpAccessJob`** becomes an inline Wings `POST /api/deauthorize-user
    {user: uuid, servers: [server uuid] | []}`
    (`app/Repositories/Daemon/DaemonServerRepository.php:147-153`). A `ConnectionException` is
    swallowed through `release()`.
  * **`ProcessWebhook`** inserts a `webhooks` row inline, but only when a matching
    `webhook_configurations` row exists.
  * **`RunTaskJob`** with a `delay` still runs immediately under `sync`. Manual schedule
    execution always uses `dispatchNow`.

---

## 1. Models

### 1.0 Table summary

All tables have an auto-increment integer `id` primary key unless noted otherwise.

| Model | Table | Timestamps | Soft delete | Notes |
|---|---|---|---|---|
| ActivityLog | `activity_logs` | no (`timestamp` col) | – | append-only, MassPrunable |
| ActivityLogSubject (Pivot) | `activity_log_subjects` | no | – | `incrementing=true` |
| Allocation | `allocations` | yes | – | |
| ApiKey (extends Sanctum PAT) | `api_keys` | yes | – | |
| Backup | `backups` | yes | no (dropped) | custom builder |
| BackupHost | `backup_hosts` | yes | – | |
| Database | `databases` | yes | – | |
| DatabaseHost | `database_hosts` | yes | – | |
| Egg | `eggs` | yes | – | |
| EggVariable | `egg_variables` | yes | – | |
| File | – (Sushi, in-memory) | – | – | not in DB |
| Mount | `mounts` | **no** | – | |
| Node | `nodes` | yes | – | |
| NodeRole (Pivot) | `node_role` | no | – | `primaryKey=null` |
| Plugin | – (Sushi, from `plugins/*/plugin.json`) | – | – | string PK `id`; not in DB |
| Role (spatie) | `roles` | yes | – | |
| Schedule | `schedules` | yes | – | |
| Server | `servers` | yes | – | |
| ServerTransfer | `server_transfers` | yes | – | |
| ServerUserSettings | `server_user_settings` | yes | – | |
| ServerVariable | `server_variables` | yes | – | |
| Subuser | `subusers` | yes | – | |
| Task | `tasks` | yes | – | touches schedule |
| User | `users` | yes | – | |
| UserSSHKey | `user_ssh_keys` | yes | **yes** | |
| Webhook | `webhooks` | yes | – | MassPrunable |
| WebhookConfiguration | `webhook_configurations` | yes | **yes** | |

Implicit or pivot tables (no model):

| Table | Columns | Used by |
|---|---|---|
| `mountables` | `mount_id`, `mountable_type` (`egg`/`node`/`server`), `mountable_id`; PK (`mount_id`,`mountable_id`,`mountable_type`); FK only on `mount_id` (cascade) | `morphToMany(Mount,'mountable')` / `morphedByMany`. No FK on `mountable_id`, so rows are orphaned when a server or node is deleted **[QUIRK]** |
| `database_host_node` | `id`, `node_id`, `database_host_id`, `created_at`, `updated_at` | `DatabaseHost::nodes()` / `Node::databaseHosts()`. No `withTimestamps()`, so **timestamps stay NULL** on attach |
| `backup_host_node` | `node_id` (unique), `backup_host_id`, `created_at`, `updated_at` | `BackupHost::nodes()` / `Node::backupHosts()`. Timestamps NULL on attach |
| `node_role` | `node_id`, `role_id` (unique pair) | `Role::nodes()`, `Node::roles()` (HasManyThrough) |
| `model_has_roles` | `role_id`, `model_type`, `model_id` | spatie; `model_type='user'` |
| `model_has_permissions`, `role_has_permissions`, `permissions` | spatie defaults | |
| `notifications` | Laravel default (uuid `id`, `type`, `notifiable_type`, `notifiable_id`, `data`, `read_at`, timestamps) | `Notifiable`: User, Server, Node, Subuser |
| `password_resets` | `email`, `token` (bcrypt hash), `created_at` | `config/auth.php:19-26`; written by `UserCreationService` |
| `passkeys` | | laravel/passkeys |
| `jobs`, `failed_jobs` | | queue (§0.9) |
| `settings` | `id`, `key`, `value` | legacy, **unused by any code** (§4) |

### 1.1 Common relation key conventions

`belongsTo(X)` uses `<x>_id` and `hasMany(X)` uses `<parent>_id` unless noted. Pivot table names
follow Laravel's alphabetical singular snake case (`database_host_node`, `backup_host_node`).

### 1.2 Server (`app/Models/Server.php`)

* **Table** `servers`.
* **Columns**: `id`, `uuid`, `uuid_short`, `node_id`, `name`, `owner_id`, `memory`, `swap`, `disk`,
  `io`, `cpu`, `egg_id`, `startup`, `created_at`, `updated_at`, `allocation_id` (nullable), `image`,
  `description`, `skip_scripts`, `external_id`, `database_limit`, `allocation_limit`, `threads`,
  `backup_limit`, `status` (nullable), `installed_at`, `oom_killer`, `docker_labels`.
  The `icon` column was dropped; icons are files at `storage/app/public/icons/server/{uuid}.{png|jpg|webp}`
  (`app/Models/Traits/HasIcon.php`).
* **`$attributes`** (153-157): `status => ServerState::Installing` (stored `installing`),
  `oom_killer => false`, `installed_at => null`.
* **`$with`** (162): `['allocation']`.
* **`$guarded`** (167): `id`, `created_at`, `updated_at`, `installed_at`.
* **`$validationRules`** (170-192):
  ```
  external_id      sometimes|nullable|string|between:1,255|unique:servers
  owner_id         required|integer|exists:users,id
  name             required|string|min:1|max:255
  node_id          required|exists:nodes,id
  description      string
  status           nullable|string
  memory           required|numeric|min:0
  swap             required|numeric|min:-1
  io               required|numeric|between:0,1000
  cpu              required|numeric|min:0
  threads          nullable|regex:/^[0-9-,]+$/
  oom_killer       sometimes|boolean
  disk             required|numeric|min:0
  allocation_id    sometimes|nullable|unique:servers|exists:allocations,id
  egg_id           required|exists:eggs,id
  startup          required|string
  skip_scripts     sometimes|boolean
  image            required|string|max:255
  database_limit   present|nullable|integer|min:0
  allocation_limit sometimes|nullable|integer|min:0
  backup_limit     present|nullable|integer|min:0
  ```
* **Casts** (194-217): `node_id`, `owner_id`, `memory`, `swap`, `disk`, `io`, `cpu`, `allocation_id`,
  `egg_id`, `database_limit`, `allocation_limit` and `backup_limit` are integer; `status` is the
  `ServerState` enum; `skip_scripts` and `oom_killer` are boolean; `created_at`, `updated_at` and
  `installed_at` are datetime; `docker_labels` is array.
* **Hooks** (219-228): `saved` runs on **every** save, including no-op saves. If a `subusers` row
  exists with `user_id = owner_id`, it runs `SubuserDeletionService::handle()` (§2.10). That logs
  `server:subuser.delete`, deletes the subuser, and dispatches `RevokeSftpAccessJob` and
  `SubUserRemoved`.
* **Relations**:

  | Relation | Definition |
  |---|---|
  | `user` | belongsTo User via `owner_id` |
  | `subusers` | hasMany Subuser |
  | `userSettings` | hasMany ServerUserSettings |
  | `allocation` | belongsTo Allocation (primary) |
  | `allocations` | hasMany Allocation |
  | `egg` | belongsTo Egg |
  | `eggVariables` | hasMany EggVariable, `egg_id` → `egg_id` |
  | `variables` | EggVariable LEFT JOIN `server_variables` on `variable_id` AND `server_id = this.id`; selects `egg_variables.*`, `server_variables.variable_value AS server_value`; cannot be eager-loaded (365-377) |
  | `serverVariables` | hasMany ServerVariable |
  | `node` | belongsTo Node |
  | `schedules` | hasMany Schedule |
  | `databases` | hasMany Database |
  | `transfer` | hasOne ServerTransfer `whereNull('successful')->orderByDesc('id')` (423-426) |
  | `backups` | hasMany Backup |
  | `mounts` | morphToMany Mount `'mountable'` |
  | `webhookConfigurations` | hasMany WebhookConfiguration where `scope='server'` (444-448) |
  | `activity` | morphToMany ActivityLog `'subject'` via `activity_log_subjects` |
* **Write helpers**:
  * `releaseAllocations(array $ids, bool $keepOne)` (258-284). Inside a transaction:
    1. Re-read `allocation_id` with `SELECT … FOR UPDATE`.
    2. If `keepOne` and no other allocation on the server's node would remain, throw DisplayException.
    3. Builder update `RELEASE_ATTRIBUTES` (+`updated_at`) on those ids.
    4. If the primary was released (or was null), set `allocation_id` to the lowest-id remaining
       allocation on the same node (or NULL) and save.
    5. If a new primary was set, log activity `server:allocation.primary` with subjects
       [targetable, server, primary] and property `allocation` = address.
  * `ensureVariablesExist()` (384-394): `firstOrCreate` a ServerVariable for each egg variable with
    `variable_value = default_value`. Used only by Filament.
* `getAllocationMappings()` (235-247) returns `['' => []]` when there is no primary allocation
  (Wings needs a JSON map).

### 1.3 Allocation (`app/Models/Allocation.php`)

* **Table** `allocations`.
* **Columns**: `id`, `node_id`, `ip`, `port`, `server_id`, `created_at`, `updated_at`, `ip_alias`,
  `notes`, `is_locked`, `show_port`. Unique (`node_id`,`ip`,`port`).
* **`RELEASE_ATTRIBUTES`** (70-75): `server_id=NULL`, `notes=NULL`, `is_locked=false`,
  `show_port=true`.
* **`$attributes`** (77-80): `is_locked=false`, `show_port=true`. Both are inserted explicitly on
  create.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules** (88-97):
  ```
  node_id   required|exists:nodes,id
  ip        required|ip
  port      required|numeric|between:1024,65535
  ip_alias  nullable|string
  server_id nullable|exists:servers,id
  notes     nullable|string|max:256
  is_locked boolean
  show_port boolean
  ```
* **Casts**: `node_id`, `port` and `server_id` are integer; `is_locked` and `show_port` are bool.
* **Hooks** (99-111):
  * `updating` (model saves only): if `server_id` is null, force `is_locked=false` and
    `show_port=true`.
  * `deleting`: throw `ServerUsingAllocationException` if `server_id` is set.
* **Accessors** (read-only): `alias` (`ip_alias ?? ip`), `has_alias`, `address` (`[v6]:port` or
  `ip:port`, using the alias), `display_address`.
* **Relations**: `server`, `node` (belongsTo).

### 1.4 ApiKey (`app/Models/ApiKey.php`, extends `Laravel\Sanctum\PersonalAccessToken`)

* **Table** `api_keys`. No `$table` override; the name comes from the class name.
* **Columns**: `id`, `user_id`, `key_type`, `identifier`, `token`, `allowed_ips`, `memo`,
  `last_used_at`, `expires_at`, `permissions`, timestamps.
* **Constants** (63-78): `TYPE_NONE=0`, `TYPE_ACCOUNT=1`, `TYPE_APPLICATION=2`,
  `IDENTIFIER_LENGTH=16`, `KEY_LENGTH=32`.
* **`$fillable`** (83-93): `user_id`, `key_type`, `identifier`, `token`, `permissions`,
  `allowed_ips`, `memo`, `last_used_at`, `expires_at`.
* **`$attributes`** (98-101): `allowed_ips='[]'`, `permissions='[]'`.
* **`$hidden`**: `token`.
* **Rules** (110-122):
  ```
  user_id        required|exists:users,id
  key_type       present|integer|min:0|max:2
  identifier     required|string|size:16|unique:api_keys,identifier
  token          required|string
  permissions    array
  permissions.*  integer|min:0|max:3
  memo           required|nullable|string|max:500
  allowed_ips    array
  allowed_ips.*  string
  last_used_at   nullable|date
  expires_at     nullable|date
  ```
* **Casts** (124-136): `permissions` and `allowed_ips` are array; `user_id` int; `last_used_at` and
  `expires_at` datetime; **`token` is `encrypted`**. The parent `$casts` (`abilities`, json) is
  merged but has no column.
* **Identifier**: `generateTokenIdentifier(type)` (223-228) is a prefix plus `Str::random(16 - 5)`.
  The prefix is `pacc_` for account keys and `papp_` for application keys (213-218), so identifiers
  are `pacc_XXXXXXXXXXX`.
* The plain bearer token is `identifier . token`. `findToken()` (197-208) splits off the first 16
  characters.
* `permissions` JSON for application keys is a map of resource name to bitmask, for example
  `{"node":1}`. ACL bits: `NONE=0`, `READ=1`, `WRITE=2`, read+write=3
  (`app/Services/Acl/Api/AdminAcl.php`). Resource names are in `DEFAULT_RESOURCE_NAMES` (160-172):
  `server`, `node`, `allocation`, `user`, `egg`, `database_host`, `server_database`, `mount`,
  `role`, `plugin`, `webhook`.
* **Account key creation** (`app/Models/Traits/HasAccessTokens.php:31-44`): `forceCreate` with
  `user_id`, `key_type=1`, `identifier`, `token=Str::random(32)` (encrypted), `memo = memo ?? ''`,
  and `allowed_ips = ips ?? []`. `permissions` comes from the default `'[]'`; `expires_at` and
  `last_used_at` are NULL.

### 1.5 ActivityLog (`app/Models/ActivityLog.php`) and ActivityLogSubject

* **Table** `activity_logs`.
* **Columns**: `id`, `event`, `ip`, `description` (nullable), `actor_type`, `actor_id` (nullable
  numeric morph), `api_key_id` (nullable), `properties` (json, NOT NULL), `timestamp`
  (DB default CURRENT_TIMESTAMP).
* `$timestamps = false`; `$guarded = ['id','timestamp']`; `$with = ['subjects']`.
* **Rules** (80-85): `event required|string`, `ip required|string`, `description nullable|string`,
  `properties array`.
* **Casts**: `properties` is `collection` (JSON, §0.2); `timestamp` is datetime.
* **Hooks** (153-166):
  * `creating` **always** overwrites `timestamp = Carbon::now()`, even when a value was supplied
    (see the Wings activity ingestion quirk in §3).
  * `updating` and `deleting` throw LogicException (append-only). Pruning uses `MassPrunable`
    (builder delete); `prunable()` deletes rows with `timestamp <= now - APP_ACTIVITY_PRUNE_DAYS`
    (146-151).
* **Relations**: `actor` (morphTo, withTrashed), `subjects` (hasMany ActivityLogSubject),
  `apiKey` (hasOne ApiKey by `id = api_key_id`).
* **ActivityLogSubject** (`app/Models/ActivityLogSubject.php:30-49`): table `activity_log_subjects`
  with `id`, `activity_log_id` (FK cascade), `subject_type`, `subject_id`; no timestamps,
  `$guarded=['id']`. `subject` is morphTo without the SoftDeleting scope.

### 1.6 Backup (`app/Models/Backup.php`)

* **Table** `backups`.
* **Columns**: `id`, `server_id`, `uuid`, `name`, `ignored_files`, `backup_host_id`, `checksum`,
  `bytes`, `completed_at`, `is_successful`, `upload_id`, `is_locked`, `is_scheduled`, timestamps.
  The `disk` column was replaced by `backup_host_id`
  (`database/migrations/2026_01_16_081858_create_backup_hosts_table.php`).
* **`$attributes`** (64-71): `is_successful=false`, `is_locked=false`, `is_scheduled=false`,
  `checksum=null`, `bytes=0`, `upload_id=null`. All are inserted.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules** (76-88):
  ```
  server_id      bail|required|numeric|exists:servers,id
  uuid           required|uuid
  is_successful  boolean
  is_locked      boolean
  is_scheduled   boolean
  name           required|string
  ignored_files  array
  backup_host_id required|integer|exists:backup_hosts,id
  checksum       nullable|string
  bytes          numeric
  upload_id      nullable|string
  ```
* **Casts** (90-103): `id` int; `is_successful`, `is_locked`, `is_scheduled` bool; `ignored_files`
  array; `bytes` int; `completed_at`, `created_at`, `updated_at` immutable_datetime.
* **Derived** `status`: `in_progress` if `completed_at` is null; otherwise `successful` or `failed`
  depending on `is_successful`.
* **Builder**: `nonFailed()` adds `(completed_at IS NULL OR is_successful = 1)`
  (`app/Eloquent/BackupQueryBuilder.php:14-23`).
* **Relations**: `server`, `backupHost` (belongsTo).

### 1.7 BackupHost (`app/Models/BackupHost.php`)

* **Table** `backup_hosts`: `id`, `name`, `schema`, `configuration` (json, nullable), timestamps.
* **`$fillable`**: `name`, `schema`, `configuration`.
* **Rules**: `name required|string|max:255`, `schema required|string|max:255`,
  `configuration nullable|array`.
* **Casts**: `configuration` is array.
* **Relations**: `nodes` (belongsToMany via `backup_host_node`, no pivot timestamps), `backups`.
* Schema ids: `wings` and `s3`. A backup host with no nodes acts as the default.

### 1.8 Database (`app/Models/Database.php`)

* **Table** `databases`.
* **Columns**: `id`, `server_id`, `database_host_id`, `database`, `username`, `remote`
  (DB default `%`), `password`, `max_connections` (nullable), timestamps.
* **`$hidden`**: `password`.
* **`$fillable`**: `server_id`, `database_host_id`, `database`, `username`, `password`, `remote`,
  `max_connections`.
* **Rules** (68-76):
  ```
  server_id        required|numeric|exists:servers,id
  database_host_id required|exists:database_hosts,id
  database         required|string|alpha_dash|between:3,48
  username         string|alpha_dash|between:3,100
  max_connections  nullable|integer
  remote           required|string|regex:/^[\w\-\/.%:]+$/
  password         string
  ```
* **Casts**: `server_id`, `database_host_id`, `max_connections` integer; **`password` encrypted**.
* **Remote SQL** (131-209), run through `DatabaseHost::buildConnection()` (driver `mysql`, database
  `mysql`, charset `utf8`, collation `utf8_unicode_ci`):
  * `CREATE DATABASE IF NOT EXISTS \`db\``
  * `CREATE USER \`u\`@\`remote\` IDENTIFIED BY 'pw'`, plus ` WITH MAX_USER_CONNECTIONS n` when
    `max_connections` is non-empty
  * `GRANT SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, REFERENCES, INDEX, LOCK TABLES,
    CREATE ROUTINE, ALTER ROUTINE, EXECUTE, CREATE TEMPORARY TABLES, CREATE VIEW, SHOW VIEW, EVENT,
    TRIGGER ON \`db\`.* TO \`u\`@\`remote\``
  * `FLUSH PRIVILEGES`
  * `DROP DATABASE IF EXISTS \`db\``
  * `DROP USER IF EXISTS \`u\`@\`remote\``
* **Relations**: `host` (belongsTo DatabaseHost via `database_host_id`), `server`.

### 1.9 DatabaseHost (`app/Models/DatabaseHost.php`)

* **Table** `database_hosts`: `id`, `name`, `host`, `port`, `username`, `password`,
  `max_databases` (nullable), timestamps.
* **`$hidden`**: `password`.
* **`$fillable`**: `name`, `host`, `port`, `username`, `password`, `max_databases`.
* **Rules** (69-77):
  ```
  name       required|string|max:255
  host       required|string
  port       required|numeric|between:1,65535
  username   required|string|max:32
  password   nullable|string
  node_ids   nullable|array
  node_ids.* required|integer|exists:nodes,id
  ```
* **Casts**: `id` and `max_databases` integer; **`password` encrypted**; timestamps
  immutable_datetime.
* **Relations**: `nodes` (belongsToMany via `database_host_node`, no pivot timestamps), `databases`.

### 1.10 Egg (`app/Models/Egg.php`)

* **Table** `eggs`.
* **Columns**: `id`, `uuid`, `name`, `author`, `description`, `features`, `docker_images`,
  `update_url`, `file_denylist`, `force_outgoing_ip`, `tags`, `startup_commands`, `config_from`,
  `config_stop`, `config_logs`, `config_startup`, `config_files`, `script_install`,
  `script_is_privileged`, `script_entry`, `script_container`, `copy_script_from`, timestamps.
  The `image` column was dropped; the icon is a file at `icons/egg/{uuid}.*`.
* **`$fillable`** (114-136): `uuid`, `name`, `author`, `description`, `features`, `docker_images`,
  `force_outgoing_ip`, `file_denylist`, `config_files`, `config_startup`, `config_logs`,
  `config_stop`, `config_from`, `startup_commands`, `update_url`, `script_is_privileged`,
  `script_install`, `script_entry`, `script_container`, `copy_script_from`, `tags`.
* **Rules** (139-159):
  ```
  uuid               required|string|size:36
  name               required|string|max:255
  description        string|nullable
  features           array|nullable
  author             required|string|email
  file_denylist      array|nullable
  file_denylist.*    string
  docker_images      required|array|min:1
  docker_images.*    required|string
  startup_commands   required|array|min:1
  startup_commands.* required|string|distinct
  config_from        sometimes|bail|nullable|numeric|exists:eggs,id
  config_stop        required_without:config_from|nullable|string|max:255
  config_startup     required_without:config_from|nullable|json
  config_logs        required_without:config_from|nullable|json
  config_files       required_without:config_from|nullable|json
  update_url         sometimes|nullable|string
  force_outgoing_ip  sometimes|boolean
  tags               array
  ```
* **`$attributes`** (161-170): `features`, `file_denylist`, `config_stop`, `config_startup`,
  `config_logs`, `config_files` and `update_url` default to null; `tags` defaults to `'[]'`.
* **Casts**: `config_from` and `copy_script_from` integer; `script_is_privileged` and
  `force_outgoing_ip` boolean; `features`, `docker_images`, `file_denylist`, `startup_commands` and
  `tags` array. `docker_images` and `startup_commands` are **maps** (`{"Display Name": "value"}`).
  The `config_*` columns are plain TEXT holding JSON strings and are **not** cast.
* **Hooks** (187-200):
  * `creating`: `uuid ??= Str::uuid()`.
  * `deleting`: throw if the egg has servers (HasActiveServersException) or children
    (`config_from` → this egg; HasChildrenException).
* **Relations**: `mounts` (morphToMany), `servers`, `variables` (hasMany EggVariable), `scriptFrom`
  (belongsTo self via `copy_script_from`), `configFrom` (belongsTo self via `config_from`),
  `children` (hasMany self via `config_from`).
* `inherit_*` and `copy_script_*` accessors are read-only fallbacks to the parent egg.

### 1.11 EggVariable (`app/Models/EggVariable.php`)

* **Table** `egg_variables`.
* **Columns**: `id`, `egg_id`, `name`, `description`, `env_variable`, `default_value`,
  `user_viewable`, `user_editable`, `rules` (json array), `sort` (nullable), timestamps. Unique on
  (`egg_id`,`env_variable`) and (`egg_id`,`name`).
* **`RESERVED_ENV_NAMES`** (65): `P_SERVER_UUID`, `P_SERVER_ALLOCATION_LIMIT`, `SERVER_MEMORY`,
  `SERVER_IP`, `SERVER_PORT`, `ENV`, `HOME`, `USER`, `STARTUP`, `MODIFIED_STARTUP`, `SERVER_UUID`,
  `UUID`, `INTERNAL_IP`, `HOSTNAME`, `TERM`, `LANG`, `PWD`, `TZ`, `TIMEZONE`.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules** (73-83, 90-97):
  ```
  egg_id         exists:eggs,id
  sort           nullable
  name           required|string|between:1,255
  description    string
  default_value  string
  user_viewable  boolean
  user_editable  boolean
  rules          array
  rules.*        string
  env_variable   required|alphaDash|between:1,255|notIn:<RESERVED_ENV_NAMES>   (added in getRules())
  ```
* **`$attributes`** (99-103): `user_editable=0`, `user_viewable=0`, `rules='[]'`.
* **Casts**: `egg_id` integer; `user_viewable` and `user_editable` bool; `rules` array; timestamps
  immutable_datetime.
* **Relations**: `serverVariable` (hasMany ServerVariable via `variable_id`).
  `egg()` is declared as `hasOne(Egg::class)` (122-125), which would look up `eggs.egg_variable_id`.
  **[QUIRK]** It is a broken relation and unused for writes.

### 1.12 Mount (`app/Models/Mount.php`)

* **Table** `mounts`: `id`, `uuid`, `name`, `description`, `source`, `target`, `read_only`,
  `user_mountable`. **No timestamps** (`$timestamps = false`, line 98).
* **`$guarded`**: `id`.
* **`$attributes`** (62-65): `read_only=false`, `user_mountable=false`.
* **Rules** (72-79, 86-93):
  ```
  name            required|string|min:2|max:64|unique:mounts,name
  description     nullable|string|max:255
  source          required|string|not_in:/etc/pelican,/var/lib/pelican/volumes,/srv/daemon-data
  target          required|string|not_in:/home/container
  read_only       sometimes|boolean
  user_mountable  sometimes|boolean
  ```
* **Casts**: `id` int; `read_only` and `user_mountable` bool.
* **Relations**: `eggs`, `nodes`, `servers` (`morphedByMany(X, 'mountable')` → `mountables`).

### 1.13 Node (`app/Models/Node.php`)

* **Table** `nodes`.
* **Columns**: `id`, `uuid`, `public`, `name`, `description`, `fqdn`, `scheme`, `behind_proxy`,
  `maintenance_mode`, `memory`, `memory_overallocate`, `disk`, `disk_overallocate`, `cpu`,
  `cpu_overallocate`, `upload_size` (DB default 100), `daemon_token_id`, `daemon_token`,
  `daemon_listen`, `daemon_connect`, `daemon_sftp`, `daemon_sftp_alias`, `daemon_base`, `tags`,
  timestamps.
* **Constants**: `DAEMON_TOKEN_ID_LENGTH = 16`, `DAEMON_TOKEN_LENGTH = 64` (118-120). Verified.
* **`$hidden`**: `daemon_token_id`, `daemon_token`.
* **`$fillable`** (130-138): `public`, `name`, `fqdn`, `scheme`, `behind_proxy`, `memory`,
  `memory_overallocate`, `disk`, `disk_overallocate`, `cpu`, `cpu_overallocate`, `upload_size`,
  `daemon_base`, `daemon_sftp`, `daemon_sftp_alias`, `daemon_listen`, `daemon_connect`,
  `description`, `maintenance_mode`, `tags`.
* **Rules** (141-162):
  ```
  name                required|string|min:1|max:100
  description         string|nullable
  public              boolean
  fqdn                required|string|notIn:0.0.0.0,127.0.0.1,localhost
  scheme              required|string|in:http,https
  behind_proxy        boolean
  memory              required|numeric|min:0
  memory_overallocate required|numeric|min:-1
  disk                required|numeric|min:0
  disk_overallocate   required|numeric|min:-1
  cpu                 required|numeric|min:0
  cpu_overallocate    required|numeric|min:-1
  daemon_base         sometimes|required|regex:/^([\/][\d\w.\-\/]+)$/
  daemon_sftp         required|numeric|between:1,65535
  daemon_sftp_alias   nullable|string
  daemon_listen       required|numeric|between:1,65535
  daemon_connect      required|numeric|between:1,65535
  maintenance_mode    boolean
  upload_size         int|min:1
  tags                array
  ```
* **`$attributes`** (167-182): `public=true`, `behind_proxy=false`, `memory=0`,
  `memory_overallocate=0`, `disk=0`, `disk_overallocate=0`, `cpu=0`, `cpu_overallocate=0`,
  `daemon_base='/var/lib/pelican/volumes'`, `daemon_sftp=2022`, `daemon_listen=8080`,
  `daemon_connect=8080`, `maintenance_mode=false`, `tags='[]'`.
* **Casts**: `memory`, `disk`, `cpu`, `daemon_listen`, `daemon_connect`, `daemon_sftp` integer;
  **`daemon_token` encrypted**; `behind_proxy`, `public`, `maintenance_mode` boolean; `tags` array.
* **Hooks** (201-214):
  * `creating`: `uuid = Str::uuid()` (v4); `daemon_token = Str::random(64)` (stored encrypted);
    `daemon_token_id = Str::random(16)` (plain). These **always override** any supplied values.
  * `deleting`: throw if the node has servers.
* **Relations**: `mounts` (morphToMany), `servers`, `allocations`, `databaseHosts`,
  `backupHosts` (belongsToMany), `roles` (hasManyThrough Role via NodeRole).
* **`isViable(memory, disk, cpu)`** (339-366). For each resource R where `R > 0 && R_overallocate >= 0`:
  the node is not viable if `sum(servers.R) + requested > R * (1 + R_overallocate/100)`.
  `-1` overallocation or a `0` limit means unlimited.

### 1.14 NodeRole and Role

* **NodeRole** (`app/Models/NodeRole.php:17-22`): Pivot; table `node_role`; `primaryKey=null`.
* **Role** (`app/Models/Role.php`) extends spatie Role.
  * Table `roles`: `id`, `name`, `guard_name`, `team_id`?(teams off), timestamps.
  * `ROOT_ADMIN='Root Admin'`, `DEFAULT_GUARD_NAME='web'`.
  * `getRootAdmin()` is `findOrCreate('Root Admin','web')` (195-201).
  * `nodes()` is `belongsToMany(Node, NodeRole::class)` (203-206).
  * Permission names are `"{prefix} {model}"`. Prefixes: `viewList`, `view`, `create`, `update`,
    `delete`. Models: `apiKey`, `allocation`, `backupHost`, `databaseHost`, `database`, `egg`,
    `mount`, `node`, `role`, `server`, `user`, `webhook`. Extras: `egg` (`import`, `export`),
    `settings` (`view`, `update`), `health` (`view`), `activityLog` (`seeIps`), `panelLog` (`view`),
    `plugin` (`viewList`, `create`, `update`, `delete`) (52-79, 121-163).
  * Spatie `HasPermissions` deleting hook detaches `role_has_permissions` and `model_has_roles` for
    the role.

### 1.15 Schedule (`app/Models/Schedule.php`)

* **Table** `schedules`.
* **Columns**: `id`, `server_id`, `name`, `cron_minute`, `cron_hour`, `cron_day_of_month`,
  `cron_month`, `cron_day_of_week`, `timezone` (DB default `UTC`), `is_active`, `is_processing`,
  `only_when_online`, `last_run_at`, `next_run_at`, timestamps.
* **`$with`**: `['tasks']`.
* **`$fillable`** (79-93): `server_id`, `name`, the five `cron_*` columns, `timezone`, `is_active`,
  `is_processing`, `only_when_online`, `last_run_at`, `next_run_at`.
* **`$attributes`** (95-106): `name=null`, the five `cron_*` columns default to `'*'`,
  `timezone='UTC'`, `is_active=true`, `is_processing=false`, `only_when_online=false`.
* **Rules** (109-123):
  ```
  server_id         required|exists:servers,id
  name              required|string|max:255
  cron_day_of_week  required|string
  cron_month        required|string
  cron_day_of_month required|string
  cron_hour         required|string
  cron_minute       required|string
  timezone          string|timezone
  is_active         boolean
  is_processing     boolean
  only_when_online  boolean
  last_run_at       nullable|date
  next_run_at       nullable|date
  ```
* **Casts**: `id` and `server_id` integer; `is_active`, `is_processing`, `only_when_online` boolean;
  `last_run_at` and `next_run_at` datetime.
* **Derived** `status`: `inactive` if not active, otherwise `processing` or `active`.
* `getNextRunDate()` (150-153) returns `Utilities::getScheduleNextRunDate(...)->toDateTimeString()`
  (see §2.17).
* `firstTask()` returns the task with the lowest `sequence_id`.

### 1.16 ServerTransfer (`app/Models/ServerTransfer.php`)

* **Table** `server_transfers`.
* **Columns**: `id`, `server_id` (FK cascade), `successful` (nullable, default NULL), `old_node`,
  `new_node`, `old_allocation` (nullable), `new_allocation` (nullable),
  `old_additional_allocations` (json, nullable), `new_additional_allocations` (json, nullable),
  `archived` (default 0), timestamps.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules** (63-74):
  ```
  server_id                    required|numeric|exists:servers,id
  old_node                     required|numeric
  new_node                     required|numeric
  old_allocation               nullable|numeric
  new_allocation               nullable|numeric
  old_additional_allocations   nullable|array
  old_additional_allocations.* numeric
  new_additional_allocations   nullable|array
  new_additional_allocations.* numeric
  successful                   sometimes|nullable|boolean
  ```
* **Casts**: `server_id`, `old_node`, `new_node`, `old_allocation`, `new_allocation` int; the two
  `*_additional_allocations` columns array; `successful` and `archived` bool.
* **Relations**: `server`; `oldNode` and `newNode` (hasOne Node by `id`).

### 1.17 ServerUserSettings (`app/Models/ServerUserSettings.php`)

* **Table** `server_user_settings`: `id`, `user_id`, `server_id` (both FK cascade, unique pair),
  `settings` (json, nullable), timestamps.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules**: `user_id required|numeric|exists:users,id`,
  `server_id required|numeric|exists:servers,id`, `settings nullable|array`,
  `settings.manual_backup_notifications boolean`, `settings.scheduled_backup_notifications boolean`.
* **Casts**: `user_id` and `server_id` int; `settings` array.
* Written by `User::updateServerSetting()` (`app/Models/User.php:374-383`):
  1. `firstOrNew(server_id)`.
  2. Merge the key, then `array_intersect_key` with the defaults (only the two known keys survive,
     order taken from the stored array).
  3. Save.

### 1.18 ServerVariable (`app/Models/ServerVariable.php`)

* **Table** `server_variables`: `id`, `server_id` (FK cascade), `variable_id` (FK → egg_variables
  cascade), `variable_value` (text), timestamps.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules**: `server_id required|int`, `variable_id required|int`, `variable_value string`.
* **Casts**: `server_id` and `variable_id` integer; timestamps immutable_datetime.
* **Relations**: `server`, `variable` (belongsTo EggVariable via `variable_id`).

### 1.19 Subuser (`app/Models/Subuser.php`)

* **Table** `subusers`: `id`, `user_id`, `server_id` (both FK cascade), `permissions` (json list),
  timestamps.
* **`$guarded`**: `id`, `created_at`, `updated_at`.
* **Rules**: `user_id required|numeric|exists:users,id`,
  `server_id required|numeric|exists:servers,id`, `permissions nullable|array`,
  `permissions.* string`.
* **Casts**: `user_id` and `server_id` int; `permissions` array.
* The permission key universe is `Subuser::allPermissionKeys()`: the `SubuserPermission` enum values
  (§3) plus plugin-registered ones.

### 1.20 Task (`app/Models/Task.php`)

* **Table** `tasks`: `id`, `schedule_id` (FK cascade), `sequence_id`, `action`, `payload`,
  `time_offset`, `is_queued`, `continue_on_failure`, timestamps.
* **`$touches`**: `['schedule']` (§0.1).
* **`$fillable`**: `schedule_id`, `sequence_id`, `action`, `payload`, `time_offset`, `is_queued`,
  `continue_on_failure`.
* **`$attributes`**: `time_offset=0`, `is_queued=false`, `continue_on_failure=false`.
* **Rules** (85-93):
  ```
  schedule_id         required|numeric|exists:schedules,id
  sequence_id         required|numeric|min:1
  action              required|string
  payload             required_unless:action,backup|string
  time_offset         required|numeric|between:0,900
  is_queued           boolean
  continue_on_failure boolean
  ```
* **Casts**: `id`, `schedule_id`, `sequence_id`, `time_offset` integer; `is_queued` and
  `continue_on_failure` boolean.
* `action` values (task schemas): `command`, `power`, `backup`, `delete_files`.
* **Relations**: `schedule`; `server` (hasOneThrough via schedules).

### 1.21 User (`app/Models/User.php`)

* **Table** `users`.
* **Columns**: `id`, `uuid`, `external_id`, `is_managed_externally`, `username`, `email`,
  `password`, `remember_token`, `language`, `timezone`, `oauth`, `customization`, `mfa_app_secret`,
  `mfa_app_recovery_codes`, `mfa_email_enabled`, timestamps.
* **`$fillable`** (147-160): `external_id`, `is_managed_externally`, `username`, `email`,
  `password`, `language`, `timezone`, `mfa_app_secret`, `mfa_app_recovery_codes`,
  `mfa_email_enabled`, `oauth`, `customization`.
* **`$hidden`**: `password`, `remember_token`, `mfa_app_secret`, `mfa_app_recovery_codes`, `oauth`.
* **`$attributes`** (170-180): `external_id=null`, `is_managed_externally=false`, `language='en'`,
  `timezone='UTC'`, `mfa_app_secret=null`, `mfa_app_recovery_codes=null`,
  `mfa_email_enabled=false`, `oauth='[]'`, `customization=null`.
* **Rules** (183-205, plus 246-254):
  ```
  uuid                       nullable|string|size:36|unique:users,uuid
  email                      required|email|between:1,255|unique:users,email
  external_id                sometimes|nullable|string|max:255|unique:users,external_id
  is_managed_externally      boolean
  username                   required|between:1,255|unique:users,username
  password                   sometimes|nullable|string
  language                   string|In(ResourceBundle::getLocales(''))
  timezone                   string|In(DateTimeZone::listIdentifiers())
  mfa_app_secret             nullable|string
  mfa_app_recovery_codes     nullable|array ; .* string
  mfa_email_enabled          boolean
  oauth                      array|nullable
  customization              array|nullable
  customization.console_rows         integer|min:1
  customization.console_font         string
  customization.console_font_size    integer|min:1
  customization.console_graph_period integer|min:1
  customization.top_navigation       boolean
  customization.dashboard_layout     string|in:grid,table
  customization.servers_per_page     integer|min:0
  ```
  The username has **no character-class restriction** at model or request level.
* **Casts** (207-217): `is_managed_externally` and `mfa_email_enabled` boolean;
  **`mfa_app_secret` encrypted**; **`mfa_app_recovery_codes` encrypted:array**; `oauth` and
  `customization` array.
* **Mutators** (256-268): `username` and `email` setters store `lower(trim(value))`.
* **Hooks** (219-240):
  * `creating`: `uuid ??= Str::uuid()`; `timezone ??= config('app.timezone')` ('UTC').
  * `saving`: lowercase `username` and `email` again. A null username becomes `''` (`str(null)`)
    **[QUIRK]**.
  * `deleting`: throw if the user owns servers, or if deleting yourself (`request()->user()`). It
    fires `App\Events\User\Deleting`, which makes `RevocationListener` dispatch
    `RevokeSftpAccessJob` for each node of the user's directly accessible servers.
  * Spatie `HasRoles` and `HasPermissions` deleting hooks detach `model_has_roles` and
    `model_has_permissions`.
  * DB cascades: `api_keys`, `user_ssh_keys`, `subusers`, `server_user_settings`, `passkeys`.
* **Relations**:
  * `servers` (hasMany Server via `owner_id`)
  * `apiKeys` (hasMany ApiKey where `key_type=1`)
  * `tokens` (hasMany ApiKey; `HasAccessTokens.php:23-26`)
  * `sshKeys`, `subusers`, `serverSettings`
  * `subServers` (belongsToMany Server via `subusers`)
  * `activity` (morphToMany ActivityLog `'subject'`)
  * spatie `roles` and `permissions`
* **`setCustomization()`** (394-407): in a transaction, lock and read the raw `customization` value,
  json-decode it, set the key, and `update(['customization' => array])`.

### 1.22 UserSSHKey (`app/Models/UserSSHKey.php`)

* **Table** `user_ssh_keys`: `id`, `user_id`, `name`, `fingerprint`, `public_key`, timestamps,
  `deleted_at`.
* **`$fillable`**: `name`, `public_key`, `fingerprint`.
* **Rules**: all three are `required|string`.
* Soft deletes. The `sshKeys()` relation excludes trashed rows.

### 1.23 Webhook and WebhookConfiguration

* **Webhook** (`app/Models/Webhook.php`):
  * Table `webhooks`: `id`, `webhook_configuration_id`, `event`, `endpoint`, `successful_at`,
    `payload` (json), timestamps.
  * `$fillable`: `payload`, `successful_at`, `event`, `endpoint`.
  * Casts: `payload` array, `successful_at` datetime.
  * `prunable()` deletes rows with `created_at <= now - APP_WEBHOOK_PRUNE_DAYS(30)`.
* **WebhookConfiguration** (`app/Models/WebhookConfiguration.php`):
  * Table `webhook_configurations`: `id`, `scope` (default `global`), `server_id` (nullable, FK
    cascade), `type`, `name`, `description` (nullable text), `endpoint`, `events` (json),
    `payload` (json, nullable), `headers` (json, nullable), timestamps, `deleted_at`. The `name`
    column is the renamed former `description`
    (`database/migrations/2025_10_28_152500_rename_description_webhooks.php`).
  * `$fillable` (67-77): `name`, `scope`, `server_id`, `type`, `payload`, `endpoint`,
    `description`, `events`, `headers`.
  * `$attributes` (82-86): `scope=global`, `type='regular'` (`WebhookTypeService::Default`),
    `payload=null`.
  * Casts: `scope` enum WebhookScope; `events`, `payload`, `headers` array; `type` string.
  * Hooks (99-113): `saved` and `deleted` only refresh cache keys (`webhooks.{event}`,
    `watchedWebhooks`), with no DB writes.
  * Soft deletes; `webhooks` rows are kept.

### 1.24 Sushi (non-DB) models

`File` (`app/Models/File.php`) holds directory listings fetched from Wings. `Plugin`
(`app/Models/Plugin.php`) is built from `plugins/*/plugin.json`; plugin status and load order are
written back into `plugin.json` (`meta.status`, `meta.status_message`, `meta.load_order`) and never
into the panel DB. Neither touches the main DB. Plugin install and update can run plugin
migrations and seeders (`app/Services/Helpers/PluginService.php:233-273`). That is out of scope.

---

## 2. Services and DB-mutating flows

Notation: `T{…}` is one DB transaction (`ConnectionInterface::transaction`). Nested transactions
use savepoints. "Wings:" marks an HTTP call to the daemon. `RS` = `Allocation::RELEASE_ATTRIBUTES`.

### 2.1 ActivityLogService (`app/Services/Activity/ActivityLogService.php`)

Facade `Activity`. The service is a new instance per container resolve; the facade caches one per
request.

* **Lazy instance** `getActivity()` (198-225): `new ActivityLog([ip => Request::ip(),
  properties => Collection([]), api_key_id => targetable.apiKeyId])`, then:
  * **Subject first**: the targetable subject set by middleware. `ServerSubject` (on
    `/api/client/servers/{server}/*`, `routes/api-client.php:54`) sets the route's Server.
    `AccountSubject` (on `/api/client/account/*`, `routes/api-client.php:22`) sets the user.
    Otherwise it is the Filament tenant if that is a Server. Application and remote routes have no
    subject middleware.
  * **Actor**: the targetable actor, else `auth()->guard()->user()`. After `auth:sanctum` the
    default guard is `sanctum`. `actor_type` is `'user'`.
* `event()`, `description()`, `property(key|array, value)` (merge or put, preserving insertion
  order), `subject(...models)` (de-duplicated with `is()`), `actor()`, `anonymous()`.
* `withRequestMetadata()` adds properties `ip` (`getClientIp`) and `useragent`.
* **`save()`** (232-257), in `T{}`:
  1. `INSERT activity_logs (event, ip, description, actor_type, actor_id, api_key_id,
     properties(JSON), timestamp=now)`.
  2. For each subject **in insertion order**: `INSERT activity_log_subjects (activity_log_id,
     subject_id, subject_type)`.
  3. After commit, dispatch `ActivityLogged`, which triggers server-scoped and global webhooks
     (§0.9).
* `log()` (138-157) swallows exceptions only when `APP_ENV=production`.
* **`transaction(cb)`** (175-184) wraps `T{ cb(); save(); }`, so the log rows are written **after**
  the callback's rows, in the same transaction, and roll back with it.
* `TrackAPIKey` middleware sets `api_key_id` to the bearer `ApiKey.id` (NULL for session auth).
* Application API controllers do **not** log, except through `DatabaseManagementService`,
  `SubuserDeletionService`, `SubuserUpdateService` and `Server::releaseAllocations`.

### 2.2 ServerCreationService (`app/Services/Servers/ServerCreationService.php`)

Entry point: `POST /api/application/servers`. `StoreServerRequest::validated()`
(`app/Http/Requests/Api/Application/Servers/StoreServerRequest.php:122-150`) maps the request to:
`external_id`, `name`, `description`, `owner_id←user`, `egg_id←egg`, `image←docker_image`,
`startup`, `environment`, `memory`/`swap`/`disk`/`io`/`cpu`/`threads←limits.*`,
`skip_scripts` (default **false**), `allocation_id←allocation.default`,
`allocation_additional←allocation.additional`, `start_on_completion` (default false),
`database_limit`/`allocation_limit`/`backup_limit←feature_limits.*`, and `oom_killer`.
`getDeploymentObject()` (189-201) is null unless `deploy` is present.

`handle(data, deployment)` (54-132):

1. If `oom_killer` is unset and `oom_disabled` is set, then `oom_killer = !oom_disabled`.
2. `Egg::findOrFail(egg_id)`. Default `image` to the egg's first `docker_images` **value** and
   `startup` to the first `startup_commands` value (61-65).
3. **Node and allocation choice**:
   * **With deployment** (68-91):
     1. `FindViableNodesService::handle(memory, disk, cpu, tags)` (§2.3) returns node ids. If empty,
        throw `NoViableNodeException`.
     2. If deployment ports are non-empty, run `AllocationSelectionService` (§2.4) with
        `dedicated`, nodes and ports. That sets `allocation_id` and `node_id` from the chosen
        allocation (**random**).
     3. If `node_id` is still empty, use the first viable node (lowest id; collection order of
        `Node::query()->get()`).
     4. `allocation_additional` is never auto-filled. Without ports, `allocation_id` stays as given
        (possibly NULL).
   * **Without deployment**: `node_id ??= Allocation::find(allocation_id)?->node_id` (93).
   * Then assert that `node_id` is non-empty.
4. `VariableValidatorService` at **admin level** with `handle(egg_id, environment)` (98-100; §2.5).
   A failure throws ValidationException (422) before any write.
5. **`T{}` with up to 5 attempts** (108-119):
   1. `createModel` (141-171). `generateUniqueUuidCombo()` (213-223) makes a uuid4 and retries
      while `uuid` or `uuid_short` (the first 8 characters) already exists. Then
      `Server::create([...])` with:
      ```
      external_id      = data.external_id
      uuid, uuid_short = uuid[0..8]
      node_id, name
      description      = data.description ?? ''
      status           = 'installing'
      skip_scripts     = data.skip_scripts ?? isset(data.skip_scripts)
      owner_id, memory, swap, disk, io, cpu, threads
      oom_killer       = data.oom_killer ?? false
      allocation_id, egg_id, startup, image
      database_limit   = ?? 0
      allocation_limit = ?? 0
      backup_limit     = ?? 0
      docker_labels    = data.docker_labels   (always NULL from the API)
      ```
      `installed_at=NULL` comes from `$attributes`. The `saved` hook then queries subusers (none).
   2. If `allocation_id` is set, run `storeAssignedAllocations` (178-195): `SELECT allocations
      WHERE id IN (primary + additional, unique) AND server_id IS NULL FOR UPDATE`. For each row in
      query order (id), save the model: `UPDATE allocations SET server_id=?, is_locked=1,
      updated_at=?`. Ids that are already taken are skipped silently.
   3. `storeEggVariables` (200-208): for **every** egg variable (admin level, query order),
      `INSERT server_variables (server_id, variable_id, variable_value, timestamps)` with
      `variable_value = provided value ?? ''`. **[QUIRK]** Variables missing from `environment` are
      stored as `''`, **not** `default_value`; the default is only used for validation.
6. After commit, Wings `POST /api/servers {uuid, start_on_completion}` (121-129):
   * `ConnectionException` triggers `ServerDeletionService::withForce()->handle($server)` (§2.6),
     which deletes everything just written, then rethrows (HTTP 500).
   * **Any other exception (Wings 4xx/5xx with a valid UA, giving `RequestException`) propagates
     and the server rows REMAIN** **[QUIRK]**.
7. No activity log entry.

### 2.3 FindViableNodesService (`app/Services/Deployment/FindViableNodesService.php:22-34`)

1. `Node::withSum('servers','memory'|'disk'|'cpu')->where('public', true)->get()`, in natural id
   order.
2. Keep nodes whose `tags` intersect the requested tags (when tags are given).
3. Keep nodes where `isViable(memory, disk, cpu)` is true (§1.13).

### 2.4 AllocationSelectionService (`app/Services/Deployment/AllocationSelectionService.php`)

* **`setPorts`** (55-75):
  * Entries where `is_digit` is true are stored as-is. They are **not** range-checked here.
  * Entries matching `^(\d{4,5})-(\d{4,5})$` are stored as `[start, end]`. If
    `end - start + 1 > 1000`, throw DisplayException("too_many_ports").
  * Everything else is ignored silently.
* **`handle()`** (82-89) → `getRandomAllocation` (97-133):
  ```sql
  SELECT * FROM allocations
  WHERE server_id IS NULL AND node_id IN (:nodes)
    AND (port BETWEEN a AND b OR … OR port IN (:singles))   -- only if ports given
    AND ip NOT IN (:ips_with_any_assigned_allocation_on_these_nodes)  -- only if dedicated
  ORDER BY RAND() LIMIT 1
  ```
  `null` throws `NoViableAllocationException`. The choice is **non-deterministic**.

### 2.5 VariableValidatorService (`app/Services/Servers/VariableValidatorService.php:25-54`)

1. Select the egg's variables. Below admin level, keep only those with `user_editable = 1` and
   `user_viewable = 1`.
2. Validate `environment.{ENV} = fields[ENV] ?? default_value` against `variable.rules`.
3. Return `[{id, key=env_variable, value = fields[ENV] ?? null}]` for **every** selected variable.

### 2.6 ServerDeletionService (`app/Services/Servers/ServerDeletionService.php:48-109`)

1. Wings `DELETE /api/servers/{uuid}`:
   * `ConnectionException`: rethrow unless `force` (the 404 check never matches a
     ConnectionException).
   * **Any other exception (including Wings 404 or 500) is reported and ignored**.
2. `T{}`:
   1. For each `$server->databases`, run `DatabaseManagementService::delete()` (§2.13). It drops on
      the host, logs `server:database.delete`, and deletes the row. On error: rethrow if not force,
      otherwise just `$database->delete()`.
   2. For each backup whose adapter is not wings (for example s3), delete the remote object; errors
      are ignored if force. Backup rows are **not** deleted in PHP; the FK cascade removes them.
   3. `$server->allocations()->update(RS)` plus `updated_at`.
   4. `$server->delete()`. DB cascades remove `server_variables`, `subusers`, `schedules` (→
      `tasks`), `backups`, `server_transfers`, `server_user_settings` and server-scoped
      `webhook_configurations`. **Orphans remain** in `mountables` (`mountable_type='server'`),
      `activity_log_subjects` and `notifications` **[QUIRK]**.
3. Entry: `DELETE /api/application/servers/{id}[/force]` → `withForce($force === 'force')`.

### 2.7 BuildModificationService (`app/Services/Servers/BuildModificationService.php`)

Entry: `PATCH /api/application/servers/{id}/build`. `UpdateServerBuildConfigurationRequest::validated()`
(`app/Http/Requests/Api/Application/Servers/UpdateServerBuildConfigurationRequest.php:63-83`):
`allocation_id = data['allocation']` (a missing key raises ErrorException → 500 **[QUIRK]**); the
three limits come from `feature_limits.*` (`?? null`); `limits.*` is flattened to top level.

`T{}` (33-59):

1. **`processAllocations`** (91-126), only when `add_allocations` or `remove_allocations` is
   non-empty:
   * add: `UPDATE allocations SET server_id=:server, updated_at WHERE node_id=:node AND id IN (:add)
     AND server_id IS NULL`. This is a builder update, so `is_locked` is not changed.
   * remove: the set is `server_id=:server AND id IN (remove − add)`. If it contains the current
     primary, `data.allocation_id` becomes the first `$server->allocations` (lazy-loaded *after*
     the add update) whose id is not in `remove`, otherwise `add[0]`, otherwise null. Then
     `UPDATE … SET RS + updated_at` on that set.
2. If `allocation_id` is set and differs from the current primary, run
   `$server->allocations()->findOrFail(id)`. If the allocation is not assigned, a 404 rolls back.
3. `oom_killer = !oom_disabled` fallback.
4. `forceFill(only(data, [oom_killer, memory, swap, io, cpu, threads, disk, allocation_id]) +
   {database_limit: get(data,'database_limit',0) ?? null, allocation_limit: … ?? null,
   backup_limit: … ?? 0})` then `saveOrFail()`. An explicit `allocation: null` clears the primary
   **[QUIRK]**.
5. `refresh()`.

After commit, Wings `POST /api/servers/{uuid}/sync`. It is always sent because `build` is never
empty. `ConnectionException` is logged and ignored. `RequestException` propagates (500) **after
commit**.

### 2.8 DetailsModificationService (`app/Services/Servers/DetailsModificationService.php:33-54`)

In `T{}`:

1. `forceFill({external_id, owner_id, name, description: ?? ''})` then `saveOrFail()`. The
   `UpdateServerDetailsRequest::validated()` request reads raw input, so null is possible.
2. The `Server::saved` hook removes a subuser that matches the new owner (§1.2).
3. If the owner changed, dispatch `RevokeSftpAccessJob(oldOwner.uuid, server)`.

### 2.9 StartupModificationService (`app/Services/Servers/StartupModificationService.php`)

Entry: `PATCH /api/application/servers/{id}/startup` at **admin level**.
`UpdateServerStartupRequest::validated()` returns `{startup?, environment, skip_scripts, egg_id←egg,
docker_image←image}`.

In `T{}` (30-64):

1. If `environment` is non-empty:
   1. The egg is `data.egg_id ?? server.egg_id` (admin) or `server.egg_id`.
   2. The validator (§2.5) runs at the same user level.
   3. For **each returned variable**, `ServerVariable::updateOrCreate({server_id, variable_id},
      {variable_value: value ?? ''})`. The update only issues SQL if the value changed.
   **[QUIRK]** At admin level, every egg variable missing from the request is overwritten with
   `''`. When changing eggs, the new egg's variables are created and the old egg's rows are **not**
   deleted.
2. Admin only: `updateAdministrativeSettings` (76-97).
   * If `egg_id` is digits and differs: `forceFill(egg_id)`, and default `docker_image` and
     `startup` from the new egg (first values).
   * Then `fill({startup: data.startup ?? server.startup, skip_scripts: data.skip_scripts ??
     isset(data.skip_scripts) (so false when absent or null), image: data.docker_image ??
     server.image})->save()`.
3. Return `fresh()`.

No Wings call. No activity log (application API).

### 2.10 Subuser services (`app/Services/Subusers/*`)

**Creation** (`SubuserCreationService.php:39-73`). Entry: `POST /api/client/servers/{s}/users`.
The controller first computes `permissions ∩ allKeys`, appends `websocket.connect`, then unique
(`app/Http/Controllers/Api/Client/Servers/SubuserController.php:143-151`). In `T{}`:

1. `User where email = :email` (exact match; emails are stored lowercase).
2. **No user**: `UserCreationService::handle({email, root_admin:false})` (§2.12), as a nested
   savepoint. The username is `localpart + Str::random(3)`, lowercased. A random password hash and
   a `password_resets` row are created, and an `AccountCreated` mail is queued.
3. **Existing user**: throw if the user is the owner (`UserIsServerOwnerException`) or already a
   subuser (`ServerSubuserExistsException`).
4. **cleanedPermissions**: unique, keep entries that equal `websocket.connect` or where
   `user()->can(perm, server)` (the acting user), `sort()` (byte-wise `strcmp` for these strings),
   then values.
5. `Subuser::updateOrCreate({user_id, server_id}, {permissions})`. This INSERTs `subusers` with
   `permissions` as a JSON list.
6. Fire `SubUserAdded`. The listener sends a Filament DB notification and the `AddedToServer` mail
   if `PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION`; both are queued.

After that, the controller logs `server:subuser.create` with subjects [server, user] and properties
`{email, permissions}` (`SubuserController.php:90-93`).

**Update** (`SubuserUpdateService.php:16-48`):

1. Clean permissions as in creation, then sort.
2. Compare with `sort(current)`.
3. If different, `Activity('server:subuser.update')` with subjects [server, user] and properties
   `{email, old, new, revoked:true}`. In `.transaction{}`: `UPDATE subusers SET permissions`, then
   dispatch `RevokeSftpAccessJob(user.uuid, server)`. The log is written after the update.
4. If nothing changed, nothing is written.

**Deletion** (`SubuserDeletionService.php:13-27`): `Activity('server:subuser.delete')` with subject
[targetable…, user] and properties `{email, revoked:true}`. In `.transaction{}`:
`DELETE subusers`, fire `SubUserRemoved` (Filament notification plus `RemovedFromServer` mail), and
dispatch `RevokeSftpAccessJob`.

### 2.11 Other server services

* **ReinstallServerService** (26-35): in `T{}`, set `status='installing'` and save, then Wings
  `POST /api/servers/{uuid}/reinstall`. **Any Wings exception rolls back the status.**
* **SuspensionService** (27-52):
  1. If already in the requested state, return (a flash notification only).
  2. If a transfer is active, throw ConflictHttpException.
  3. `update(status = 'suspended' | NULL)` is committed immediately, with no transaction.
  4. Wings `POST …/sync`. Failure means HTTP 500, but **the DB change stays**.

  Unsuspend sets status to NULL, even if the server was previously in another state.
* **ToggleInstallService** (Filament only): abort 500 if install failed; otherwise
  `status = isInstalled ? 'installing' : NULL`.
* **TransferServerService** (`app/Services/Servers/TransferServerService.php:64-117`). Entry:
  `POST /api/application/servers/{id}/transfer` with `{node_id, allocation_id (required, unique on
  servers, exists), allocation_additional[]}`.
  1. Load the target node with server sums and run `isViable`; if it fails, throw
     `NodeNotViableException`.
  2. `validateTransferState()`: the server must be installed, not restoring, and not already
     transferring.
  3. `T{}`:
     1. `ServerTransfer::create({server_id, old_node, new_node})`, which INSERTs with
        `successful=NULL`, `archived=0`.
     2. If the server has a primary, set `old_allocation = primary`, `new_allocation = allocation_id`,
        `old_additional_allocations = [ids of other allocations]` (JSON list) and
        `new_additional_allocations = intval(additional)`.
     3. `assignAllocationsToServer`: `UPDATE allocations SET server_id, updated_at WHERE id IN
        (requested ids that are currently unassigned on the target node)`.
     4. `$transfer->save()`, which UPDATEs the allocation columns plus `updated_at`. It is a no-op
        when the server has no primary.
  4. After commit, mint a JWT (scope `transfer`, 15 min) and send `Http::daemon(oldNode)->post(
     /api/servers/{uuid}/transfer, {url: newNode/api/transfers, token: "Bearer …", backups:
     [wings backup uuids], server: {uuid, start_on_completion:false}})`. A non-2xx response is
     **ignored**. A connection error throws, but the transfer row and allocations remain.
  5. **Completion** is driven by Wings through `app/Http/Controllers/Api/Remote/Servers/ServerTransferController.php`:
     * **success** (64-…): in `T{}`, `UPDATE allocations SET RS WHERE id IN (old_allocation +
       old_additional)`; `server.update({allocation_id: new_allocation (only if old allocations
       existed), node_id: new_node})`; `transfer.update({successful: true})`. Then Wings
       `DELETE /api/servers/{uuid}` on the old node, with ConnectionException ignored.
     * **failure** (35-…): in `T{}`, set `successful = false` and `UPDATE allocations SET RS WHERE id
       IN (new_allocation + new_additional)`.
     * **cancel** (`app/Http/Controllers/Api/Application/Servers/ServerManagementController.php:115-137`):
       sets `transfer.successful = **true**` without releasing the new allocations **[QUIRK]**,
       then calls Wings `DELETE` on both nodes.

### 2.12 User services

* **UserCreationService** (`app/Services/Users/UserCreationService.php:33-74`):
  1. If a password is given, bcrypt it.
  2. **Manual** `beginTransaction()`. There is no explicit rollback on error; an open transaction
     is abandoned and rolled back when the connection closes **[UNCERTAIN]**.
  3. If there is no password, set the password to `bcrypt(Str::random(30))` and plan a reset token.
  4. Extract `root_admin`.
  5. `username ??= before(email,'@') + Str::random(3)` when empty.
  6. `User::forceCreate(data + {uuid: uuid4})`. Mutators and hooks lowercase and trim
     username/email; defaults are `language 'en'`, `timezone 'UTC'`, `oauth '[]'`.
  7. If `root_admin`, run `syncRoles(Role::getRootAdmin())`. This may INSERT
     `roles('Root Admin','web')`, then deletes and inserts `model_has_roles`.
  8. If a reset token is needed, the broker `createToken()` deletes `password_resets` rows for the
     email and INSERTs `{email, token: bcrypt(hmac token), created_at}`.
  9. Commit, then queue `AccountCreated` mail if `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION`.

  Application API input (`StoreUserRequest`): `external_id`, `is_managed_externally`, `email`,
  `username`, `password`, `language`, `timezone`.
* **UserUpdateService** (24-39):
  1. Hash `password` if non-empty, otherwise remove the key.
  2. `forceFill(data)->saveOrFail()`.
  3. If the password changed, fire `PasswordChanged`, which makes `RevocationListener` queue
     `RevokeSftpAccessJob` for each node.
  4. Return `refresh()`.

  Client account endpoints log `user:account.username-changed`, `email-changed` (rate-limited 3
  per day) and `password-changed` (inside `Activity::transaction`).

### 2.13 Database services

* **`generateUniqueDatabaseName(name, serverId)`**
  (`app/Services/Databases/DatabaseManagementService.php:48-52`): `sprintf('s%d_%s', id,
  substr(name, 0, 48 - strlen("s{id}_")))`.
* **`create(server, data)`** (74-109):
  1. Throw unless `PANEL_CLIENT_DATABASES_ENABLED`. This also applies to the application API.
  2. If limit validation is on (the default), throw `TooManyDatabasesException` when
     `database_limit` is not null and `count >= database_limit`. A limit of 0 blocks creation.
  3. `database` must match `^(s\d+_)(.*)$`.
  4. `username = sprintf('u%d_%s', serverId, Str::random(10))`.
  5. `password = Utilities::randomStringWithSpecialCharacters(24)`
     (`app/Helpers/Utilities.php:17-34`): `Str::random(24)`, then a loop
     `for (i=0; i < random_int(2,6); i++)` (the bound is re-evaluated each iteration) that replaces
     a random position with one of `! @ = . + ^`.
  6. `T{}`:
     1. Duplicate check on (`server_id`, `database`) → DuplicateDatabaseNameException.
     2. `forceFill(data)->saveOrFail()` INSERTs `databases` with `database_host_id`, `database`,
        `remote`, `server_id`, `username`, encrypted `password` and timestamps;
        `max_connections` stays NULL.
     3. Run the host SQL (§1.8: create database, create user, grant, flush).
     4. Log `server:database.create` with subjects [targetable…, database] and property `name`.
     5. Host or SQL failure rolls back the panel rows. Host-side objects are not cleaned up.
* **`delete(database)`** (116-131): `T{ drop database; drop user; flush; log
  server:database.delete (subject database, name); DELETE databases }`.
* **`rotatePassword(database)`** (138-153): `T{ UPDATE password (new random 24, encrypted) +
  updated_at; drop user; create user; grant; flush }`. The client controller logs
  `server:database.rotate-password`; the application API does not log.
* **DeployServerDatabaseService** (client `POST …/databases`;
  `app/Services/Databases/DeployServerDatabaseService.php:18-37`): the controller locks the server
  row first. The host is `random()` among the node's database hosts, or among **all** hosts if the
  node has none and `PANEL_CLIENT_DATABASES_ALLOW_RANDOM` is set (**non-deterministic**). It then
  calls `create(...)` with `s{id}_name` and `remote`.
* **HostCreationService** (33-52): `T{ INSERT database_hosts (password encrypted); sync node_ids
  (INSERT database_host_node rows without timestamps); open a PDO connection to the host — failure
  rolls back }`.
* **HostUpdateService** (25-43):
  1. Drop an empty `password`.
  2. `T{ $host->update(data)` — fillable only, so **`node_ids` are ignored** **[QUIRK]** — `; test
     connection }`.
  3. Failure rolls back.

### 2.14 Backup services

* **InitiateBackupService** (`app/Services/Backups/InitiateBackupService.php:86-146`). Entry: client
  `POST …/backups`. The controller wraps everything in `Activity('server:backup.start')->transaction`
  and locks the server row (`app/Http/Controllers/Api/Client/Servers/BackupController.php:72-103`).
  `is_locked` is honoured only if the actor has `backup.delete`. Ignored files come from
  `explode(PHP_EOL, ignored ?? '')` with empty strings dropped (61-77).
  1. **Throttle**: if `BACKUP_THROTTLE_PERIOD` (600) > 0, count non-failed backups with
     `created_at >= now - period`. If the count is `>= BACKUP_THROTTLE_LIMIT` (2), return 429.
  2. **Limit**: `successful = server.backups().nonFailed()`. If `backup_limit` is 0 or null, or
     `count >= backup_limit`:
     * Throw `TooManyBackupsException` unless `override` (only scheduled tasks pass `true`) and
       `backup_limit > 0`.
     * Otherwise delete the oldest (`created_at`) non-failed, unlocked backup through
       DeleteBackupService, or throw if there is none.
  3. **Host**: the first `backupHosts` row of the node, else the first BackupHost with no nodes
     (`firstOrFail`).
  4. `T{}`:
     1. `Backup::create({server_id, uuid: uuid4, name: trim(name) ?: 'Backup at Y-m-d H:i:s',
        ignored_files: list, backup_host_id, is_locked, is_scheduled})`. Defaults are
        `is_successful=0`, `checksum=NULL`, `bytes=0`, `upload_id=NULL`; `completed_at` is NULL.
     2. Wings `POST /api/servers/{uuid}/backup {adapter: schema id, uuid, ignore: implode("\n",
        ignored)}` (`app/Repositories/Daemon/DaemonBackupRepository.php:16-25`). **Failure rolls
        back the backup row and the activity log.**
  5. The controller's log has subjects [server, backup] and properties `{name, locked}`.
* **DeleteBackupService** (`app/Services/Backups/DeleteBackupService.php:22-46`):
  1. Throw `BackupLockedException` if `is_locked && is_successful && completed_at`.
  2. `T{}`: schema `deleteBackup`. For Wings that is `DELETE /api/servers/{uuid}/backup/{buuid}`,
     where a 404 is tolerated (`app/Extensions/BackupAdapter/Schemas/WingsBackupSchema.php:31-42`).
     For S3 it is `DeleteObject {uuid}/{buuid}.tar.gz`. If the backup is not successful, any
     exception is swallowed.
  3. `DELETE backups` (hard delete).
  4. The controller logs `server:backup.delete` with properties `{name, failed}`.
* **Lock toggle** (controller, `BackupController.php:115-128`): `update(is_locked = !is_locked)`,
  then log `server:backup.lock` or `server:backup.unlock`.
* **Rename** logs `server:backup.rename` only if the name changed.
* **Restore** (241-264): `T{ update server.status='restoring_backup'; Wings restore }`, logging
  `server:backup.restore`.
* **Wings completion** (`app/Http/Controllers/Api/Remote/Backups/BackupStatusController.php:33-78`):
  1. Reject if already successful.
  2. Activity `server:backup.complete` or `server:backup.fail` with subjects [backup, server] and
     property `name`; no actor.
  3. In `.transaction{}`, `fill({is_successful, is_locked: successful ? is_locked : false,
     checksum: successful ? "{checksum_type}:{checksum}" : NULL, bytes: successful ? size : 0,
     completed_at: now})->save()`. For S3, also complete the multipart upload.
  4. Fire `BackupCompleted`, which sends notifications to the owner and subusers who opted in via
     `server_user_settings`; the default is off.

  Restore completion (92-108) sets server `status = NULL` and logs `server:backup.restore-complete`
  or (sic) `server.backup.restore-failed`.
* **S3 multipart**: `getUploadParts()` sets `backups.upload_id`
  (`app/Extensions/BackupAdapter/Schemas/S3BackupSchema.php:147`).
* **Pruning** (cron): `p:maintenance:prune-backups` sets `is_successful=0, completed_at=now,
  updated_at=now` on backups that are incomplete and older than `BACKUP_PRUNE_AGE` (360) minutes
  (`app/Console/Commands/Maintenance/PruneOrphanedBackupsCommand.php`).

### 2.15 Allocation services

* **AssignmentService** (`app/Services/Allocations/AssignmentService.php:45-110`). Entry:
  `POST /api/application/nodes/{id}/allocations {ip, alias?, ports[]}`, mapped to `allocation_ip`,
  `allocation_alias`, `allocation_ports`.

  | Constant | Value |
  |---|---|
  | `CIDR_MAX_BITS` | 25 |
  | `CIDR_MIN_BITS` | 32 |
  | `PORT_FLOOR` | 1024 |
  | `PORT_CEIL` | 65535 |
  | `PORT_RANGE_LIMIT` | 1000 |
  | `PORT_RANGE_REGEX` | `/^(\d{4,5})-(\d{4,5})$/` |

  1. If the IP contains `/`, the suffix must be all digits and between 25 and 32, otherwise
     `CidrOutOfRangeException`. This applies to IPv6 too.
  2. `IPTools\Network::parse(ip)`. A failure throws DisplayException. Iteration covers **every**
     address from the network to the broadcast address inclusive (for example a /30 gives 4).
     Addresses are written via `inet_ntop`, which compresses IPv6. Purely numeric input is parsed as
     a long.
  3. **Manual `beginTransaction()`**. On exception there is no explicit rollback; it effectively
     rolls back at connection close **[UNCERTAIN]**.
  4. For each IP, and each port entry **in the given order**:
     * Reject entries that are neither digits nor a range (`InvalidPortMappingException`).
     * Range: if more than 1000 ports, `TooManyPortsInRangeException`; if `start < 1024` or
       `end > 65535`, `PortOutOfRangeException`; otherwise expand it in ascending order.
     * Single port: if outside 1024–65535, `PortOutOfRangeException`.
     * One `Allocation::create({node_id, ip, port, ip_alias, server_id: server?->id, is_locked:
       data.is_locked ?? false})` per port. That is one INSERT each, with `show_port=1` from
       defaults and `notes=NULL`. Duplicates hit the unique index and raise an exception.
  5. If a server is given and has no primary, set `server.allocation_id = first id`.
  6. Commit.
* **FindAssignableAllocationService**
  (`app/Services/Allocations/FindAssignableAllocationService.php:36-129`). Client
  `POST …/network/allocations`, inside `Activity('server:allocation.create')->transaction`; the
  controller enforces `count < allocation_limit`.
  1. Require `PANEL_CLIENT_ALLOCATIONS_ENABLED` and an existing primary allocation.
     `range_start` and `range_end` must be integer-ish.
  2. Pick a **random** free allocation on the same node and primary IP with port in
     [start, end] (`FOR UPDATE`).
  3. If none and `create_new` is set: choose a random port in the range not present for that
     node and IP, run `AssignmentService` for one port, then re-select it.
  4. `allocation.update({server_id})`, a model update that does not change `is_locked`.
  5. The log has subjects [server, allocation] and property `allocation` = address.
* **Client allocation edits**
  (`app/Http/Controllers/Api/Client/Servers/NetworkAllocationController.php`):
  * notes (57-73): `forceFill(notes)->save()`; log `server:allocation.notes` only if the notes
    changed.
  * setPrimary (84-97): save `allocation_id`; log `server:allocation.primary`.
  * delete (134-148): requires a non-empty `allocation_limit`; runs
    `Server::releaseAllocations([id], keepOne:true)` (§1.2); log `server:allocation.delete`.
* **Application allocation delete**: `$allocation->delete()`. The model `deleting` hook throws if
  the allocation is assigned.

### 2.16 Node services

* **Creation** (no service): `Node::create($request->validated())`
  (`app/Http/Controllers/Api/Application/Nodes/NodeController.php:77-89`). `daemon_base` defaults to
  `/var/lib/pelican/volumes`. The `creating` hook generates `uuid`, `daemon_token` (64 characters,
  encrypted) and `daemon_token_id` (16 characters). `MakeNodeCommand` is the CLI equivalent.
* **NodeUpdateService** (`app/Services/Nodes/NodeUpdateService.php:30-67`):
  1. `data.id = node.id`. If `reset_secret === true`, generate a new `daemon_token` (64) and
     `daemon_token_id` (16).
  2. `T{}`:
     1. `$updated = $node->replicate()` with `exists = true`, then
        `forceFill(data + {created_at: node.created_at, updated_at: now})->save()`.
        Because `replicate()`'s original attributes are the class defaults, the UPDATE writes
        nearly every column, with the same values for unchanged ones; the net row diff is the same
        as a normal update.
     2. Wings `POST /api/update` with the **new** configuration, sent to the new fqdn with the old
        token and port (`app/Repositories/Daemon/DaemonSystemRepository.php:55-58`).
        `ConnectionException` keeps the DB change and throws `ConfigurationNotPersistedException`
        after commit. **Other Wings errors roll back.**
  3. The API controller **catches and reports all exceptions** (`NodeController.php:100-115`), so
     the response is 200 either way.
* **NodeDeletionService** (15-24): throws if the node has servers, then `delete()`.
  * Cascades: `allocations`, `database_host_node`, `backup_host_node`, `node_role`.
  * Orphans `mountables` rows with type `node`.
* **NodeAutoDeployService** (27-59):
  1. Find an application key whose `permissions->node` contains 1 (any user).
  2. Otherwise create one through Api KeyCreationService with `TYPE_APPLICATION`,
     `memo 'Automatically generated node deployment key.'`, `user_id` = requester and
     `permissions {"node":1}`.
  3. Return a `wings configure` command string.
* **NodeJWTService** issues HS256 tokens signed with `daemon_token`, `jti =
  sha256(identifiedBy)`, plus a `unique_id` claim. There are no DB writes.

### 2.17 Schedules and tasks

* **Next run** (`app/Helpers/Utilities.php:41-47`):
  `CronExpression("min hour dom month dow")->getNextRunDate(now(tz))` (dragonmantank/cron-expression;
  strictly after now, seconds = 0, standard OR semantics when both day fields are restricted). The
  result is converted to **UTC**, then stored as `Y-m-d H:i:s`. An invalid expression gives
  DisplayException (`ScheduleController.php:189-203`).
* **Client create** (`app/Http/Controllers/Api/Client/Servers/ScheduleController.php:65-89`):
  `Schedule::create({server_id, name, cron_* from day_of_week, month, day_of_month, hour, minute,
  is_active: (bool), only_when_online: (bool), next_run_at (computed in UTC)})`. `timezone` is not
  settable through the API, so the default `'UTC'` is used. Log `server:schedule.create`.
* **Client update** (119-151):
  1. Set the same fields. `next_run_at` uses the schedule's own `timezone`.
  2. If `is_active` changed, also set `is_processing = false`.
  3. Log `server:schedule.update` with properties `{name, active}`.
* **Execute** (161-168): `ProcessScheduleService::handle(schedule, now:true)`, then log
  `server:schedule.execute`.
* **Delete**: `DELETE schedules` cascades to tasks; log `server:schedule.delete`.
* **Task create** (`ScheduleTaskController.php:46-95`):
  1. Reject if `count >= PANEL_PER_SCHEDULE_TASK_LIMIT` (10).
  2. Reject `backup` when `backup_limit === 0`.
  3. `T{}`:
     1. `seq = max(sequence_id) + 1`. The requested sequence is clamped to a minimum of 1.
     2. If requested < seq, `UPDATE tasks SET sequence_id = sequence_id + 1, updated_at WHERE
        schedule_id=? AND sequence_id >= requested` and use the requested value.
     3. `Task::create({schedule_id, sequence_id, action, payload ?? '', time_offset,
        continue_on_failure: boolean(...)})`. `is_queued` defaults to 0. The schedule's
        `updated_at` is touched.
  4. Log `server:task.create` with subjects [server, schedule, task] and properties
     `{name, action, payload}`.
* **Task update** (106-149): in `T{}`, shift the tasks between the old and new sequence by ±1
  (builder increment or decrement, which also sets `updated_at`), then `task.update(...)`.
* **Task delete** (159-173): decrement tasks with a higher `sequence_id`, then delete the task
  (which touches the schedule).
* **ProcessScheduleService** (`app/Services/Schedules/ProcessScheduleService.php:23-73`):
  1. Require a first task (lowest `sequence_id`).
  2. `T{ schedule.forceFill({is_processing: true, next_run_at: getNextRunDate()})->saveOrFail();
     firstTask.update({is_queued: true}) (which touches the schedule) }`.
  3. If `only_when_online`, Wings `GET /api/servers/{uuid}`. If the state is offline or the call
     fails, run `RunTaskJob::failed()`: `task.is_queued = 0`, and the schedule builder update
     `is_processing = 0, last_run_at = now, updated_at = now`.
  4. Otherwise dispatch `RunTaskJob` with delay `time_offset`, or run it immediately for a manual
     run. A failed manual run calls `failed()` and rethrows.
* **RunTaskJob** (`app/Jobs/Schedule/RunTaskJob.php:34-120`):
  1. If the schedule is inactive and this is not a manual run, mark the task not queued and the
     schedule complete.
  2. If the server `status` is not null, call `failed()`.
  3. Run the schema for the action:
     * `command`: `POST commands`.
     * `power`: `POST power {action: payload}`.
     * `backup`: `InitiateBackupService` with `isScheduled`, ignored files from
       `explode(PHP_EOL, payload)`, and `override=true`.
     * `delete_files`: Wings list and delete.
  4. An exception is rethrown unless `continue_on_failure` is set and it is a ConnectionException.
  5. Then set `is_queued = 0`. The next task (lowest higher `sequence_id`) gets `is_queued = 1`
     and is dispatched with its delay. If there is none, the schedule is completed with
     `is_processing = 0` and `last_run_at = now`.
* **Cron** `p:schedule:process` (every minute; `app/Console/Commands/Schedule/ProcessRunnableCommand.php`)
  selects schedules where the server `status` is NULL, `is_active = 1`, `is_processing = 0` and
  `next_run_at <= now(UTC)`.
* **ScheduleImporterService** (Filament): creates the schedule and tasks from JSON. Cron defaults
  are minute `0`, hour `0`, day-of-month/month/day-of-week `*`; timezone defaults to `UTC`.

### 2.18 Egg services

* **EggImporterService** (`app/Services/Eggs/Sharing/EggImporterService.php`). API entry:
  `POST /api/application/eggs/import`. **`parse()`** (146-232):
  * Accepted versions: `PTDL_v1` (legacy images become a docker_images map; startup becomes
    `{"Default": startup}`), `PTDL_v2`, `PLCN_v1`, `PLCN_v2` (startup becomes
    `startup_commands {"Default": …}`), and `PLCN_v3`. Anything else is rejected.
  * `config.*` and `scripts.installation.*` values that are arrays or objects are encoded with
    `JSON_UNESCAPED_SLASHES`. In `config.files`, the legacy placeholders are rewritten
    (`UPGRADE_VARIABLES`, 23-32).
  * Boolean `default_value` becomes `'true'` or `'false'`.
  * Reserved env names are uppercased and renamed to `SERVER_<NAME>`, and those names are
    rewritten in `startup_commands`.
  * A `data:` `image` becomes an icon file.

  **`fromParsed()`** (102-137), in `T{}`:
  1. The egg is the given one, or the egg with `uuid = parsed.uuid`, or a new Egg.
  2. `forceFill({uuid: parsed.uuid ?? uuid4, author, copy_script_from: NULL, config_from: NULL})`.
  3. **`fillFromParsed`** (237-261) sets:
     * `name`, `description`, `tags` (default `[]`), `features`, `docker_images`,
       `startup_commands`
     * `file_denylist` = a Collection with empty values filtered and **keys preserved**, so a gap
       makes it a JSON object; absent gives `[]` **[QUIRK]**
     * `update_url` from `meta.update_url`
     * **`config_files`, `config_startup`, `config_logs` = `json_encode(json_decode(str),
       JSON_PRETTY_PRINT)`**: 4-space indent, `\n`, slashes **escaped**, objects kept. A missing
       value gives the string `"null"` **[QUIRK]**.
     * `config_stop`, `script_install`, `script_entry`, `script_container`
  4. `save()`.
  5. For each variable, unguarded `updateOrCreate({egg_id, env_variable}, all other keys)`. A
     string `rules` value is exploded on `|`.
  6. Delete variables not present in the import.
  7. `refresh()`.
* **Egg delete** (API): model hooks block it if the egg has servers or children. Variables cascade.
* **VariableCreationService** (`app/Services/Eggs/Variables/VariableCreationService.php:45-68`):
  1. Reject reserved names (uppercase comparison) and invalid rule names.
  2. `create({egg_id, name ?? '', description ?? '', env_variable ?? '', default_value ?? '',
     user_viewable: 'user_viewable' ∈ options, user_editable: 'user_editable' ∈ options,
     rules ?? []})`. `sort` stays NULL.
* **VariableUpdateService** (46-77): reserved and unique `env_variable` checks within the egg,
  then the same mapping as an update. Missing fields are blanked to `''`.
* **EggChangerService** (Filament; `app/Services/Eggs/EggChangerService.php:12-47`):
  1. `forceFill({egg_id, image: first docker image, startup: first startup command})->saveOrFail()`.
  2. Snapshot old values by env name.
  3. `DELETE server_variables WHERE server_id`.
  4. Insert the new egg's variables with `old value ?? default_value`.

  There is no transaction.

### 2.19 API keys, SSH keys, mounts, roles, webhooks

* **Api\KeyCreationService** (`app/Services/Api/KeyCreationService.php:33-46`):
  `forceCreate(data + {key_type, identifier: prefix + random 11, token: Str::random(32)})`.
  `permissions` is kept only for application keys.
* The client `POST /api/client/account/api-keys` uses `HasAccessTokens::createToken` instead
  (§1.4). It enforces `API_KEYS_LIMIT` (25) and logs `user:api-key.create` with subjects
  [user, api_key] and property `identifier`. Delete logs before the hard delete.
* **Ssh\KeyCreationService** (Filament; `app/Services/Ssh/KeyCreationService.php:18-41`):
  phpseclib load; reject DSA and RSA under 2048 bits; fingerprint
  `getFingerprint('sha256')`; stored `public_key` is `toString('PKCS8')`. The client controller
  uses the request's helpers and logs `user:ssh-key.create`. Delete is a soft delete and logs
  `user:ssh-key.delete`.
* **Mounts** (application API, `app/Http/Controllers/Api/Application/Mounts/MountController.php`):
  * create (72-…): `fill(validated)` + `uuid = uuid4` → `saveOrFail`.
  * update: `forceFill`.
  * delete: blocked if any server is attached.
  * attach (`addEggs`/`addNodes`/`addServers`): `INSERT mountables(mount_id, mountable_id,
    mountable_type)`, with no de-duplication, so duplicates fail on the PK.
  * detach: DELETE the matching rows.
* **Roles** (API): `Role::create({name})`. `guard_name` defaults to spatie
  `Guard::getDefaultName(Role)`, which during an API request resolves to `auth.defaults.guard`.
  That is `sanctum` after `auth:sanctum` → `shouldUse` **[UNCERTAIN, QUIRK]**. Filament uses
  `web`.
* **Users roles** (API): `assignRole` and `removeRole` on `model_has_roles`. The Root Admin role is
  skipped.
* **Webhook configurations** (API): `fill(resolvedAttributes)->saveOrFail()`; delete is a soft
  delete. `test` dispatches `ProcessWebhook`, which writes a `webhooks` row.

---

## 3. Other write paths triggered by Wings (remote API)

* **Install status** (`app/Http/Controllers/Api/Remote/Servers/ServerInstallController.php:39-67`):
  1. `status` is NULL on success. On failure it is `reinstall_failed` if the `reinstall` flag is
     set, otherwise `install_failed`. It stays `suspended` if the server was suspended.
  2. `installed_at = now` (always overwritten). Save.
  3. Fire `Installed`, which sends a Filament DB notification and mail. The flag
     `initialInstall = installed_at was null`.
* **Boot reset** (`app/Http/Controllers/Api/Remote/Servers/ServerDetailsController.php:84-132`):
  1. For servers on the node in `restoring_backup` whose latest `server:backup.restore-started`
     activity has a backup subject, log `server:backup.restore-failed`.
  2. Then builder-update `status = NULL` (+`updated_at`) for servers on the node with status
     `installing` or `restoring_backup`.
* **Activity ingestion** (`app/Http/Controllers/Api/Remote/ActivityProcessingController.php`): for
  each `server:*` event, `ActivityLog::forceCreate({ip (or '127.0.0.1'), event, properties:
  metadata, timestamp: parsed, actor (user)})` followed by one `server` subject row.
  **[QUIRK]** The `creating` hook overwrites `timestamp` with the panel's now, so the Wings
  timestamp is discarded.
* **SFTP auth** logs `auth:sftp.fail` and `server:sftp.denied`.

---

## 4. Enums (`app/Enums`): values as stored or exchanged

| Enum | Values | Stored where |
|---|---|---|
| ServerState | `installing`, `install_failed`, `reinstall_failed`, `suspended`, `restoring_backup` | `servers.status` (NULL = normal) |
| WebhookScope | `global`, `server` | `webhook_configurations.scope` |
| ScheduleStatus | `inactive`, `processing`, `active` | derived only |
| BackupStatus | `in_progress`, `successful`, `failed` | derived only |
| ContainerStatus | `created`, `starting`, `running`, `restarting`, `exited`, `paused`, `dead`, `removing`, `stopping`, `offline`, `missing` | Wings state; not stored |
| SuspendAction | `suspend`, `unsuspend` | API only |
| NodeJwtScope | `backup-download`, `file-download`, `file-upload`, `transfer`, `websocket` | JWT `scope` claim |
| SubuserPermission | `websocket.connect`, `control.{console,start,stop,restart}`, `file.{read,read-content,create,update,delete,archive,sftp}`, `backup.{read,create,delete,download,restore}`, `schedule.{read,create,update,delete}`, `user.{read,create,update,delete}`, `database.{read,create,update,delete,view-password}`, `allocation.{read,create,update,delete}`, `activity.read`, `mount.{read,update}`, `startup.{read,update,docker-image}`, `settings.{rename,description,reinstall,change-icon}` | `subusers.permissions` JSON |
| RolePermissionModels | `apiKey`, `allocation`, `backupHost`, `databaseHost`, `database`, `egg`, `mount`, `node`, `role`, `server`, `user`, `webhook` | spatie `permissions.name` = `"{prefix} {model}"` |
| RolePermissionPrefixes | `viewList`, `view`, `create`, `update`, `delete` | same as above |
| ServerUserSettingKey | `manual_backup_notifications`, `scheduled_backup_notifications` (default false) | `server_user_settings.settings` keys |
| CustomizationKey | `console_rows` (30), `console_font` ('monospace'), `console_font_size` (14), `console_graph_period` (30), `top_navigation` (config `FILAMENT_DEFAULT_NAVIGATION`='sidebar'), `dashboard_layout` ('grid'), `button_style` (true), `redirect_to_admin` (false), `servers_per_page` (0) | `users.customization` keys (defaults are not stored) |
| PluginStatus | `not_installed`, `disabled`, `enabled`, `errored`, `incompatible`, `crashed` | `plugin.json` |
| PluginCategory | `plugin`, `theme`, `language` | `plugin.json` |
| StartupVariableType | `text`, `number`, `select`, `toggle` | UI only |
| EggFormat | `yaml`, `json` | import/export |
| ResourceLimit | `websocket`, `allocation-create`, `backup-restore`, `database-create`, `schedule-create`, `subuser-create`, `file-pull` | rate-limiter names |
| ServerResourceType | `uptime`, `cpu_absolute`, `memory_bytes`, `disk_bytes`, `cpu`, `memory`, `disk` | Wings stats |

The remaining enums (TablerIcon, EditorLanguages, *Position, CustomRenderHooks, HttpStatusCode)
are UI or HTTP only.

---

## 5. Settings storage and environment keys

* **There is no settings table in use.** `settings` (`id`, `key`, `value`;
  `database/migrations/2017_12_12_220426_MigrateSettingsTableToNewFormat.php`) is never read or
  written by `app/`.
* Panel settings live in **`.env`**. The admin Settings page and `p:environment:*` commands call
  `EnvironmentWriterTrait::writeToEnvironment()` (`app/Traits/EnvironmentWriterTrait.php:19-23`):
  `Env::writeVariables($values, .env, overwrite)` followed by `artisan config:clear`.
* Plugin state lives in `plugins/*/plugin.json`. Icons are files on the `public` disk.
* Per-user settings are DB rows: `users.customization`, `users.language`, `users.timezone` and
  `server_user_settings`.

### 5.1 Config keys read by the panel (env key → default)

**`config/panel.php`**

| Env key | Default |
|---|---|
| `APP_2FA_REQUIRED` | 0 |
| `APP_2FA_WINDOW` | 4 |
| `GUZZLE_TIMEOUT` | 15 |
| `GUZZLE_CONNECT_TIMEOUT` | 5 |
| `PANEL_EGG_INDEX_URL` | |
| `PANEL_CLIENT_DATABASES_ENABLED` | true |
| `PANEL_CLIENT_DATABASES_ALLOW_RANDOM` | true |
| `PANEL_PER_SCHEDULE_TASK_LIMIT` | 10 |
| `PANEL_CLIENT_ALLOCATIONS_ENABLED` | false |
| `PANEL_CLIENT_ALLOCATIONS_CREATE_NEW` | true |
| `PANEL_CLIENT_ALLOCATIONS_RANGE_START` | null |
| `PANEL_CLIENT_ALLOCATIONS_RANGE_END` | null |
| `PANEL_FILES_MAX_EDIT_SIZE` | 4 MiB |
| `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION` | true |
| `PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION` | true |
| `PANEL_SEND_REMOVED_FROM_SERVER_NOTIFICATION` | true |
| `PANEL_SEND_INSTALL_NOTIFICATION` | true |
| `PANEL_SEND_REINSTALL_NOTIFICATION` | true |
| `FILAMENT_WIDTH` | screen-2xl |
| `FILAMENT_AVATAR_PROVIDER` | gravatar |
| `FILAMENT_UPLOADABLE_AVATARS` | false |
| `FILAMENT_DEFAULT_NAVIGATION` | sidebar |
| `PANEL_USE_BINARY_PREFIX` | true |
| `PANEL_EDITABLE_SERVER_DESCRIPTIONS` | true |
| `API_KEYS_LIMIT` | 25 |
| `API_KEYS_EXPIRE_TIME` | 720 |
| `APP_WEBHOOK_PRUNE_DAYS` | 30 |
| `APP_WEBHOOK_TIMEOUT` | 30 |
| `PANEL_PLUGIN_DEV_MODE` | false |
| `PANEL_PLUGIN_SAFE_MODE` | false |
| `PANEL_PLUGIN_MAX_IMPORT_SIZE` | 100 MiB |
| `PANEL_PLUGIN_HUB_URL` | |
| `PANEL_PLUGIN_HUB_API_KEY` | |

`PANEL_USE_BINARY_PREFIX` also changes what is sent to Wings: when false, memory, swap and disk
are divided by 1.048576 (`app/Services/Servers/ServerConfigurationStructureService.php:84-89`).

**`config/app.php`**: `APP_NAME` ('Pelican'), `APP_LOGO`, `APP_FAVICON`, `APP_INSTALLED` (true),
`APP_REPORT_ALL_EXCEPTIONS`. `timezone` is fixed to `UTC` and `version` is `canary`. From framework
defaults: `APP_KEY`, `APP_PREVIOUS_KEYS`, `APP_URL`, `APP_ENV`, `APP_DEBUG`, `APP_LOCALE`.
`app.url` is used for the JWT issuer, the node `remote` and URL generation.

**`config/auth.php`**: `AUTH_DISABLE_PASSWORD_LOGIN` (false); lockout 2 minutes / 3 attempts;
password broker `users` with **table `password_resets`**, expire 60, throttle 60; guard `api`
(token driver).

**`config/backups.php`**: `BACKUP_PRESIGNED_URL_LIFESPAN` (60), `BACKUP_MAX_PART_SIZE`,
`BACKUP_PRUNE_AGE` (360), `BACKUP_MAX_CONCURRENT_UPLOADS`, `BACKUP_THROTTLE_LIMIT` (2),
`BACKUP_THROTTLE_PERIOD` (600).

**`config/activity.php`**: `APP_ACTIVITY_PRUNE_DAYS` (90), `APP_ACTIVITY_HIDE_ADMIN` (false).

**`config/http.php`**: `APP_API_{CLIENT|APPLICATION|PASSWORD_RESET|WEBSOCKET|BACKUP_RESTORE|DATABASE_CREATE|SUBUSER_CREATE|FILE_PULL|DEFAULT}_RATELIMIT[_PERIOD]`.
Defaults: 256/1, 256/1, 2/1, 5/1, 3/15, 2/1, 10/15, 5/10, 2/1.

**`config/database.php`**: `DB_CONNECTION` (**sqlite** default, file `database/database.sqlite`),
`DB_URL`, `DB_HOST`, `DB_PORT`, `DB_DATABASE` (panel), `DB_USERNAME` (pelican), `DB_PASSWORD`,
`DB_SOCKET`, `DB_CHARSET` (utf8mb4), `DB_COLLATION` (utf8mb4_unicode_ci), `DB_PREFIX`,
`DB_STRICT_MODE` (**false**), `DB_FOREIGN_KEYS` (true, sqlite), `MYSQL_ATTR_SSL_CA`, `DB_SCHEMA`
(pgsql), `REDIS_*`.

**Cache, session and queue**: `CACHE_STORE` / `CACHE_DRIVER` (file), `SESSION_DRIVER` (file),
`SESSION_COOKIE` (pelican_session), `QUEUE_CONNECTION` (framework default **database**).

**Mail**: `MAIL_MAILER` (log), `MAIL_HOST`, `MAIL_PORT` (2525), `MAIL_USERNAME`, `MAIL_PASSWORD`,
`MAIL_SCHEME`/`MAIL_ENCRYPTION`, `MAIL_FROM_ADDRESS`, `MAIL_FROM_NAME`, `MAILGUN_*`.

**Other**: `TRUSTED_PROXIES`, `APP_CORS_ALLOWED_ORIGINS`, `PASSKEYS_*`, `LOG_CHANNEL` (daily),
`HEALTH_*`, `HASH_DRIVER`, `BCRYPT_ROUNDS` (12).

**Dynamic**: `OAUTH_{ID}_{ENABLED|CLIENT_ID|CLIENT_SECRET|SHOULD_CREATE_MISSING_USERS|SHOULD_LINK_MISSING_USERS}`
(`app/Extensions/OAuth/Schemas/OAuthSchema.php`), `OAUTH_AUTHENTIK_{BASE_URL|DISPLAY_NAME|DISPLAY_COLOR}`,
`OAUTH_GITLAB_HOST`, `OAUTH_STEAM_CLIENT_SECRET`,
`CAPTCHA_{ID}_{SITE_KEY|SECRET_KEY}`, `CAPTCHA_TURNSTILE_{ENABLED|SECRET_KEY|VERIFY_DOMAIN}`.

**Read only during migrations**: `APP_BACKUP_DRIVER`, `AWS_*`, `PANEL_SEND_BACKUP_COMPLETED_NOTIFICATION`.

**Spatie permission**: tables `roles`, `permissions`, `model_has_roles`, `model_has_permissions`,
`role_has_permissions`; teams off; models `App\Models\Role` and spatie `Permission`
(`config/permission.php`).

---

## 6. Open questions and uncertain framework behaviour

1. **Model validation observer** (§0.8). It is very likely inactive because `#[ObservedBy]` sits on
   the trait. Confirm with a quick runtime check, for example in tinker:
   `Server::first()->forceFill(['io'=>99999])->save()` succeeding means it is inactive. If it is
   active, invalid model writes must raise `DataValidationException` (HTTP 500).
2. **Manual `beginTransaction()` without rollback** in `AssignmentService` and
   `UserCreationService`. On exception the transaction is left open. Under PHP-FPM, the connection
   close at request end rolls it back. When nested inside an outer `transaction()`, the outer
   rollback only rolls back one level. The net effect should be a full rollback, but it is not
   explicitly proven.
3. **Role `guard_name` via the application API** (§2.19). It is inferred as `sanctum` because
   `Authenticate` → `shouldUse('sanctum')` changes `auth.defaults.guard` and Role has no
   `guard_name` default. Verify on a live panel; this affects later `assignRole` (GuardDoesNotMatch).
4. **Queue driver** decides whether `jobs` rows or real side effects appear (notifications rows,
   `webhooks` rows, Wings deauthorize calls). The harness pins `QUEUE_CONNECTION=sync` (decision
   0007), so the Rust in-process queue (decision 0009) must reproduce the inline effects in §0.11,
   with the same rows and the same transaction placement. Production PHP defaults to `database`,
   which writes `jobs` rows with serialized-PHP payloads; that path is deliberately not reproduced.
5. **JSON byte equality**. PHP emits compact JSON with escaped `/` and `\uXXXX`. On MySQL `JSON`
   columns the server re-serializes; on MariaDB and SQLite the bytes are verbatim. Decide per
   column whether to compare byte-exact (TEXT: Egg `config_*`, `server_variables.variable_value`)
   or semantically.
6. **Non-determinism**: UUIDs, `Str::random` tokens and passwords, encrypted values (random IV),
   bcrypt hashes, `inRandomOrder()` allocation picks (deployment and client auto-allocation),
   random database-host choice, and `now()` timestamps. The DB-diff must normalise these.
7. **Wings error classification** (§0.10). A non-2xx from a proxy without the Wings `User-Agent` is
   a `ConnectionException` (code 0). That changes rollback and cleanup paths in server create
   (cleanup only on ConnectionException), delete (abort only on ConnectionException unless force),
   build modification and node update.
8. **Unique checks vs case**. Validation `unique:users,username|email` uses the raw input, while
   the stored value is lowercased. On case-sensitive SQLite a differently cased duplicate passes
   validation and then hits the unique index; MySQL `*_ci` catches it in validation.
9. `PHP_EOL` splitting of backup `ignored` and task payloads assumes `\n`. `\r\n` input keeps
   trailing `\r` in the stored JSON.
10. `trim(null)` in `InitiateBackupService` and `json_decode(null)` in the egg importer trigger PHP
    deprecations. Laravel logs them and does not throw; this was assumed from default
    `HandleExceptions` behaviour.
11. `ServerTransfer` completion with `old_additional_allocations = NULL` would hit
    `array_merge(..., null)` (TypeError). This only happens when the transfer started while the
    server had no primary allocation.
12. MySQL `TIMESTAMP` columns are written as UTC strings with no connection `timezone` set, so they
    depend on the MySQL session time zone. Keep the Rust connection's session time zone identical
    to PHP's (server default).
