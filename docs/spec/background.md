# Background processing (original Pelican panel)

Status: reference spec, derived from the PHP source in `panel/` (PHP 8.5, Laravel 13, Filament 5).
All paths are relative to `panel/` unless stated otherwise. Framework behaviour was checked against
`.ref/vendor` where noted. Line numbers refer to the files at the time of writing.
Plugin commands/jobs are only summarised here; see `docs/spec/plugins.md`. Egg import/export, the egg index and
`p:egg:*` commands are specified in `docs/spec/eggs.md`.

Everything below runs outside the HTTP request/response cycle (or is triggered by it but executes later):
artisan commands, the scheduler, queue jobs, event listeners, webhooks, activity logging, notifications,
backups and transfers driven by Wings callbacks, health checks, the installer and version checks.

---

## 1. Process model

| Process | How it runs | Source |
|---|---|---|
| Scheduler | cron `* * * * * php artisan schedule:run` (bare metal: installer shows `(sudo crontab -l -u www-data 2>/dev/null; echo "* * * * * php {base}/artisan schedule:run >> /dev/null 2>&1") \| sudo crontab -u www-data -`) | `app/Livewire/Installer/Steps/QueueStep.php:48-54`; Docker: `docker/crontab:1` via `supercronic -overlapping` (`docker/supervisord.conf`) |
| Extra cron (Docker only) | `0 0 * * * php artisan p:egg:check-updates` (duplicates the scheduler's daily run) | `docker/crontab:2` |
| Queue worker | `php artisan queue:work --tries=3` (systemd unit written by `p:environment:queue-service`); Docker: `queue:work --tries=3 --max-time=3600`, `stopwaitsecs=3600` | `app/Console/Commands/Environment/QueueWorkerServiceCommand.php:51-68`, `docker/supervisord.conf` (`[program:queue-worker]`) |
| Default queue connection | Laravel default `QUEUE_CONNECTION` (`database` unless changed; installer offers `database`, `redis`, `sync`) | no `config/queue.php` in the repo → framework default; `QueueStep.php:17-21` |
| Docker start-up | if `APP_INSTALLED=true`: wait for DB, `php artisan migrate --force` (unless `SKIP_MIGRATIONS=true`), `php artisan p:plugin:composer`; always `filament:optimize`, `view:cache`; config/route/event caches intentionally not built | `docker/entrypoint.sh` |

Every queued job gets `--tries=3` from the worker unless the job declares `$tries`. Jobs without `$tries` that
throw are therefore **re-run up to 3 times** (no backoff) before `failed()` is called. Before each job the worker
forgets scoped container instances and clears resolved facades
(`vendor/laravel/framework/src/Illuminate/Queue/QueueServiceProvider.php:247-276`), so per-job state such as
`LogTarget`/`Activity` facades starts clean.

### 1.1 Queue job catalogue

| Job | Queue options | Dispatched by | What it does |
|---|---|---|---|
| `App\Jobs\Schedule\RunTaskJob(Task, manualRun)` | none (worker tries) | `ProcessScheduleService`, itself (chaining) | one schedule task, §4.3 |
| `App\Jobs\ProcessWebhook(WebhookConfiguration, eventName, [data])` | `$tries = 3` (only used when a type asks for retry) | `WebhookConfiguration::run()` | one webhook delivery, §6.4 |
| `App\Jobs\InstallEgg(downloadUrl)` | `$timeout = 15` | installer, admin import "GitHub" tab | `EggImporterService::fromUrl()`, errors logged and swallowed (`eggs.md` §5.1) |
| `App\Jobs\RevokeSftpAccessJob(userUuid, Server\|Node)` (`app/Jobs/RevokeSftpAccessJob.php`) | `ShouldBeUnique` (`uniqueId = "revoke-sftp:{user}:server:{uuid}"` / `":node:{uuid}"`), `$tries = 3`, `$maxExceptions = 1`, `#[DeleteWhenMissingModels]`, target serialized `#[WithoutRelations]` | `RevocationListener`, subuser delete/update, owner change | Wings `POST /api/deauthorize-user {"user": uuid, "servers": [server uuid] or []}` (node-wide when the target is a node); on `ConnectionException` → `release(attempts * 10 s)` |
| `App\Jobs\Plugin\{InstallPlugin,UpdatePlugin,UninstallPlugin}(User, pluginId[, reinstall])` | `ShouldBeUnique` (`plugin:{install,update,uninstall}:{id}`) | admin plugin resource | see `plugins.md`; result sent as Filament DB notification to the user |
| Queued notifications (`AccountCreated`, `AddedToServer`, `RemovedFromServer`, `ServerInstalled`, `BackupCompleted`, Filament `DatabaseNotification`) | Laravel `SendQueuedNotifications` | listeners / services | §8 |
| `Spatie\Health\Jobs\HealthQueueJob` | — | `health:queue-check-heartbeat` every minute | writes the queue heartbeat, §11 |

---

## 2. Console commands

Registered by `$this->load(__DIR__ . '/Commands')` (`app/Console/Kernel.php:25-28`), i.e. every class under
`app/Console/Commands/**`.

### 2.1 Catalogue

| Signature | Class | Purpose |
|---|---|---|
| `p:info` | `InfoCommand.php:11-16` | Calls `about`. `AppServiceProvider.php:107-115` adds a "Pelican" section (Panel Version, Latest Version, Up-to-Date — performs the GitHub version check) and "Installation Directory". |
| `p:environment:setup {--url=}` | `Environment/AppSettingsCommand.php` | §2.2 |
| `p:environment:cache {--driver=} {--redis-host=} {--redis-user=} {--redis-pass=} {--redis-port=}` | `Environment/CacheSettingsCommand.php` | §2.2 |
| `p:environment:database {--driver=} {--database=} {--schema=} {--host=} {--port=} {--username=} {--password=}` | `Environment/DatabaseSettingsCommand.php` | §2.2 |
| `p:environment:mail {--driver=} {--email=} {--from=} {--encryption=} {--host=} {--port=} {--endpoint=} {--username=} {--password=}` | `Environment/EmailSettingsCommand.php` | §2.2 |
| `p:environment:queue {--driver=} {--redis-*}` | `Environment/QueueSettingsCommand.php` | §2.2 |
| `p:environment:queue-service {--service-name=} {--user=} {--group=} {--overwrite}` | `Environment/QueueWorkerServiceCommand.php` | §2.3 |
| `p:environment:session {--driver=} {--redis-*}` | `Environment/SessionSettingsCommand.php` | §2.2 |
| `p:redis:setup {--redis-*}` | `Environment/RedisSetupCommand.php` | sets `CACHE_STORE=redis`, `QUEUE_CONNECTION=redis`, `SESSION_DRIVER=redis`, asks Redis settings, runs `p:environment:queue-service --overwrite`, writes `.env` |
| `p:user:make {--email=} {--username=} {--password=} {--admin=} {--no-password}` | `User/MakeUserCommand.php` | §2.4 |
| `p:user:delete {--user=}` | `User/DeleteUserCommand.php` | §2.4 |
| `p:user:disable2fa {--email=}` | `User/DisableTwoFactorCommand.php` | sets `mfa_app_secret=null`, `mfa_app_recovery_codes=null`, `mfa_email_enabled=false` on the user with that e-mail (`firstOrFail`) |
| `p:node:make {--name=} {--description=} {--locationId=} {--fqdn=} {--public=} {--scheme=} {--proxy=} {--maintenance=} {--maxMemory=} {--overallocateMemory=} {--maxDisk=} {--overallocateDisk=} {--maxCpu=} {--overallocateCpu=} {--uploadSize=} {--daemonListeningPort=} {--daemonConnectingPort=} {--daemonSFTPPort=} {--daemonSFTPAlias=} {--daemonBase=}` | `Node/MakeNodeCommand.php` | `Node::create($data)` with prompts; defaults: scheme `https`, public `true`, memory/disk/cpu `0`, overallocate `-1`, upload `256`, listen/connect `8080`, sftp `2022`, alias `''`, base `/var/lib/pelican/volumes`. `--locationId` is accepted but unused. |
| `p:node:configuration {node} {--format=yaml}` | `Node/NodeConfigurationCommand.php` | `node` = id if all digits else uuid; missing → error + `exit(1)`; format `yaml\|yml\|json`; YAML = `Yaml::dump(getConfiguration(), 4, 2, DUMP_EMPTY_ARRAY_AS_SEQUENCE)`, JSON = `JSON_UNESCAPED_SLASHES\|JSON_PRETTY_PRINT` (`app/Models/Node.php:277-290`), followed by a newline |
| `p:node:list {--format=text}` | `Node/NodeListCommand.php` | table or JSON (`JSON_PRETTY_PRINT\|JSON_UNESCAPED_SLASHES`) of `{id, uuid, name, host=getConnectionAddress()}` |
| `p:server:bulk-power {action} {--servers=} {--nodes=}` | `Server/BulkPowerActionCommand.php` | §2.5 |
| `p:schedule:process` | `Schedule/ProcessRunnableCommand.php` | §4.1 |
| `p:maintenance:clean-service-backups` | `Maintenance/CleanServiceBackupFilesCommand.php` | deletes files in default disk `services/.bak` older than 5 minutes (legacy; nothing in the panel writes there any more) |
| `p:maintenance:prune-images {node?}` | `Maintenance/PruneImagesCommand.php` | `DELETE {node}/api/system/docker/image/prune` (connect 5 s, timeout 30 s) for one node or all; prints `Node {id}: Cleaned up {n} dangling docker images. ({x} MiB\|MB)` (binary prefix per `panel.use_binary_prefix`) or `No images to clean up.` when `ImagesDeleted` is null |
| `p:maintenance:prune-backups {--prune-age=}` | `Maintenance/PruneOrphanedBackupsCommand.php` | §9.6 |
| `p:egg:check-updates`, `p:egg:update-index`, `p:egg:normalize {file} {--delete-original}` | `Egg/*` | see `eggs.md` §6.3, §6.4, §7 |
| `p:plugin:{composer,disable,install,list,make,uninstall,update}` | `Plugin/*` | see `plugins.md` |
| `dev:generate-tabler-icons-enum` | `Dev/GenerateTablerIconsEnum.php` | dev-only code generator |
| overrides: `config:cache` | `Overrides/ConfigCacheCommand.php` | **disabled** (prints a warning; config caching breaks plugin configs) |
| overrides: `optimize` | `Overrides/OptimizeCommand.php` | parent tasks minus `config` |
| overrides: `key:generate` | `Overrides/KeyGenerateCommand.php` | interactive double confirmation if `app.key` already set |
| overrides: `db:seed`, `up` | `Overrides/SeedCommand.php`, `Overrides/UpCommand.php` | refuse (exit 1, big banner "Your database has not been properly migrated!") unless every migration file in `database/migrations` has run (`app/Traits/CheckMigrationsTrait.php`) |

### 2.2 Environment commands and `.env` writing

All write through `EnvironmentWriterTrait::writeToEnvironment` (`app/Traits/EnvironmentWriterTrait.php:19-23`):
`Env::writeVariables($values, App::environmentFilePath(), overwrite: true)` then `Artisan::call('config:clear')`.
Laravel 13 semantics (`vendor/.../Support/Env.php:131-290`), needed to stay byte-compatible with `.env` files:
- file split on `PHP_EOL`; for each key: a value is **quoted** unless it matches `^[a-zA-Z0-9]+$`
  (non-string values are written raw); quoting uses `"…"` with `addslashes` except `'`, or `'…'` (with
  addslashes except `"`) if the value contains `"`; `''` (empty string) → `KEY=`;
- if an identical line exists → unchanged; if `KEY=` (empty) or `KEY=…` exists → replaced in place;
- otherwise inserted after the last line starting with the same prefix (`APP_`, `DB_`, …), or appended at the end
  (preceded by an empty line if the last line is not empty).

| Command | Keys written | Notes |
|---|---|---|
| `p:environment:setup` | `APP_URL` | copies `.env.example` → `.env` if missing; URL required, must start with `http://` or `https://`; then `key:generate` if `app.key` empty, `storage:link`, `filament:optimize` |
| `p:environment:cache` | `CACHE_STORE` (`file`\|`database`\|`redis`) + Redis keys | with redis and `queue.default != sync` → `p:environment:queue-service --overwrite` |
| `p:environment:database` | `DB_CONNECTION` (`sqlite`\|`mariadb`\|`mysql`\|`pgsql`), `DB_HOST`, `DB_PORT`, `DB_DATABASE`, `DB_USERNAME`, `DB_PASSWORD`, `DB_SCHEMA` (pgsql) | always asks confirmation first ("Changing the database driver will NOT move any database data!"); tests the connection (mysql/mariadb: utf8mb4/utf8mb4_unicode_ci strict; pgsql: utf8, `search_path`=schema, `sslmode=prefer`); on failure optionally loops back; sqlite only asks `DB_DATABASE` (default `database.sqlite`) |
| `p:environment:mail` | `MAIL_MAILER` (`log`, `smtp`, `sendmail`, `mailgun`, `mandrill`, `postmark`), `MAIL_FROM_ADDRESS`, `MAIL_FROM_NAME`; smtp: `MAIL_HOST`, `MAIL_PORT`, `MAIL_USERNAME`, `MAIL_PASSWORD`, `MAIL_SCHEME` (`tls`\|`ssl`\|`''`); mailgun: `MAILGUN_DOMAIN`, `MAILGUN_SECRET`, `MAILGUN_ENDPOINT`; mandrill: `MANDRILL_SECRET`; postmark: `MAIL_DRIVER=smtp`, `MAIL_HOST=smtp.postmarkapp.com`, `MAIL_PORT=587`, `MAIL_USERNAME=MAIL_PASSWORD=<token>` | then `queue:restart` |
| `p:environment:queue` | `QUEUE_CONNECTION` (`database`\|`redis`\|`sync`) + Redis keys | redis → `p:environment:queue-service --overwrite` |
| `p:environment:session` | `SESSION_DRIVER` (`file`\|`redis`\|`database`\|`cookie`) + Redis keys | redis and queue not sync → queue-service |

Redis prompts (`app/Traits/Commands/RequestRedisSettingsTrait.php`): `REDIS_HOST`, `REDIS_USERNAME`,
`REDIS_PASSWORD`, `REDIS_PORT`; empty username/password are written as the literal string `null`
(`REDIS_USERNAME=null`).

### 2.3 `p:environment:queue-service`

- In Docker (`/.dockerenv` exists): `supervisorctl restart queue-worker` only.
- Else writes `/etc/systemd/system/{name}.service` (default name `pelican-queue`, user/group default `www-data`);
  asks before overwriting unless `--overwrite`. Exact content (note the trailing 8 spaces after the last line):

```
# Pelican Queue File
# ----------------------------------

[Unit]
Description=Pelican Queue Service
After=redis-server.service          <- only if queue/session/cache driver is redis

[Service]
User={user}
Group={group}
Restart=always
ExecStart=/usr/bin/php {base_path}/artisan queue:work --tries=3
StartLimitInterval=180
StartLimitBurst=30
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

  then `systemctl restart {name}.service` (existed) or `systemctl enable --now {name}.service` (new).

### 2.4 User commands

`p:user:make` (`MakeUserCommand.php:31-60`): checks the DB connection; `root_admin = --admin ?? confirm`;
`password` from option, or prompted unless `--no-password`; calls `UserCreationService::handle(compact('email',
'username', 'password', 'root_admin'))` (`app/Services/Users/UserCreationService.php:33-74`):
- hash password (bcrypt via Laravel hasher); if empty, random 30-char password and a password-reset token is
  created via the `app` panel's broker;
- empty username → `{email local part}{3 random chars}`; `uuid` = UUIDv4; `forceCreate` (User `saving` hook
  lowercases username and email, `app/Models/User.php:228-231`);
- root admin → `syncRoles(Role::getRootAdmin())`;
- after commit, if `panel.email.send_account_created_notification` (default true) → `AccountCreated($token)` mail.
- Prints a table UUID / Email / Username / Admin.

`p:user:delete` (`DeleteUserCommand.php`): search `id LIKE "x%" OR username LIKE "x%" OR email LIKE "x%"`;
interactive: table + pick id; non-interactive: exactly one match required; `$user->delete()` (fails if the
user owns servers or is the current request user; fires `App\Events\User\Deleting` → SFTP/WS revocation, §5).

### 2.5 `p:server:bulk-power {action}`

Validates `action in start,stop,kill,restart`, ids ≥1; servers with `status IS NULL` filtered by
`id IN servers` and/or `node_id IN nodes` (both given → `OR`); asks confirmation (non-interactive skips);
for each: `POST {node}/api/servers/{uuid}/power {"action": …}`; failures printed per server, continue.

---

## 3. Scheduler (`app/Console/Kernel.php:33-66`)

Timezone: `config('app.timezone') = 'UTC'` (`config/app.php`). Laravel frequency helpers map to cron as shown.

| Cron | Command | Condition / options |
|---|---|---|
| `0 * * * *` | `cache:prune-stale-tags` | only when `cache.default === 'redis'` |
| `* * * * *` | `p:schedule:process` | `withoutOverlapping(10)` (mutex expires after 10 minutes) |
| `0 0 * * *` | `p:maintenance:clean-service-backups` | |
| `0 0 * * *` | `p:maintenance:prune-images` (all nodes) | |
| `0 0 * * *` | `p:egg:check-updates` | |
| `0 0 * * *` | `p:egg:update-index` | |
| `0,30 * * * *` | `p:maintenance:prune-backups` | only if `backups.prune_age` truthy (env `BACKUP_PRUNE_AGE`, default 360) |
| `0 0 * * *` | `model:prune --model=App\Models\ActivityLog` | only if `activity.prune_days` truthy (env `APP_ACTIVITY_PRUNE_DAYS`, default 90) |
| `0 0 * * *` | `model:prune --model=App\Models\Webhook` | only if `panel.webhook.prune_days` truthy (env `APP_WEBHOOK_PRUNE_DAYS`, default 30) |
| `* * * * *` | `health:schedule-check-heartbeat` | writes `now()->timestamp` to cache key `health:checks:schedule:latestHeartbeatAt` (default store) |
| `* * * * *` | `health:queue-check-heartbeat` | dispatches Spatie `HealthQueueJob` per checked queue, which writes `health:checks:queue:latestHeartbeatAt.{queue}` |
| `*/5 * * * *` | `health:check` | runs all registered checks, stores JSON report in cache store `file` key `health:storeResults` (`config/health.php:22-24`) |

Pruning (`MassPrunable`): `ActivityLog::prunable()` = `timestamp <= now - prune_days days`
(`app/Models/ActivityLog.php:146-151`, throws if the config is null); `Webhook::prunable()` =
`created_at <= now - prune_days` (`app/Models/Webhook.php:47-50`). Laravel deletes with query-builder
`DELETE … LIMIT 1000` loops (no model events, so the append-only guards on `ActivityLog` do not fire);
`activity_log_subjects` rows go via FK `ON DELETE CASCADE`.

---

## 4. Server schedules (cron tasks)

### 4.1 `p:schedule:process` (`app/Console/Commands/Schedule/ProcessRunnableCommand.php`)

Selects (`:19-25`) schedules with tasks eager-loaded where: the server has `status IS NULL`, `is_active = true`,
`is_processing = false`, `next_run_at <= now('UTC')` (formatted `Y-m-d H:i:s`). None → prints
`There are no scheduled tasks for servers that need to be run.` For each: skip silently if it has no tasks
(its `next_run_at` is then never advanced); otherwise `ProcessScheduleService::handle($schedule)` and print
`Dispatching job for first task in \`{name}\` ({id}).`; exceptions are logged with `schedule_id` and printed,
never abort the loop.

### 4.2 `ProcessScheduleService::handle(Schedule $s, bool $now = false)` (`app/Services/Schedules/ProcessScheduleService.php:23-73`)

1. `task = first task by sequence_id`; none → `DisplayException('Cannot process schedule for task execution: no tasks are registered.')`.
2. Transaction: `is_processing = true`, `next_run_at = getNextRunDate()`; `task.is_queued = true`.
   `getNextRunDate()` (`app/Models/Schedule.php:150-153`, `app/Helpers/Utilities.php:41-47`): cron
   `"{minute} {hour} {day_of_month} {month} {day_of_week}"` evaluated with `dragonmantank/cron-expression`
   `getNextRunDate(now($schedule->timezone))` (strictly after now, in the schedule's timezone so DST is honoured),
   converted to UTC, stored as `Y-m-d H:i:s`. Manual runs also advance `next_run_at`.
3. `job = new RunTaskJob(task, manualRun: $now)`.
4. If `only_when_online`: `GET {node}/api/servers/{uuid}` with 5 s timeout; `state` mapped to `ContainerStatus`
   (unknown → `offline`); if `offline` or `missing` (only these two, `app/Enums/ContainerStatus.php:66-69`) or the
   request throws → log warning `Skipping schedule '{name}' ({id}) for server {uuid}: …` and `job->failed()`
   (marks task not queued, schedule not processing, `last_run_at = now`). Return.
5. Not manual: `dispatch(job->delay(task.time_offset seconds))`.
   Manual (`$now = true`, from client API `POST …/schedules/{id}/execute`, HTTP 202, and the Filament "Run now"
   button): `dispatchNow(job)` **synchronously in the request**; on exception call `job->failed()` and rethrow.
   The `server:schedule.execute` activity is logged by the caller only after success.

### 4.3 `RunTaskJob` (`app/Jobs/Schedule/RunTaskJob.php`)

Queued, `SerializesModels` (task re-fetched when the job runs), no `$tries` (worker `--tries=3` applies).
`handle()`:
1. If `!task.schedule.is_active && !manualRun` → task `is_queued=false`, schedule `is_processing=false,
   last_run_at=now`; stop.
2. If the server's `status` is not null (suspended, installing, restoring…) → `failed()`; stop.
3. `schema = TaskService::get(task.action)`; unknown → `InvalidArgumentException('Invalid task action provided: …')`.
   `schema->runTask(task)`. If it throws: swallowed **only** when `task.continue_on_failure` **and** the exception
   is `Illuminate\Http\Client\ConnectionException`; otherwise rethrown → job attempt fails → worker retries the
   whole task (up to 3 attempts total), finally `failed()` (task not queued, schedule complete).
4. Success (or tolerated failure): task `is_queued=false`; `queueNextTask()`: next task of the schedule with
   `sequence_id > current` (lowest first); none → mark schedule complete (`is_processing=false`,
   `last_run_at=now()` as `Y-m-d H:i:s`); else next task `is_queued=true` and
   `dispatch(new RunTaskJob(next, manualRun)->delay(next.time_offset))`.

So tasks are a **sequential chain**: each task's `time_offset` (0–900 s, `app/Models/Task.php:90`) is the delay
after the *previous task finished* (the first task's offset is relative to dispatch). Subsequent tasks of a manual
run are queued (not synchronous) and still bypass `is_active`. A lost job leaves `is_processing=true` forever (no
cleanup sweeper). Task `touches = ['schedule']`, so task updates bump `schedules.updated_at`.

Task actions (`app/Providers/Extensions/TaskServiceProvider.php:19-29`, `app/Extensions/Tasks/Schemas/*`):

| `action` | Implementation | Payload |
|---|---|---|
| `power` | `POST {node}/api/servers/{uuid}/power {"action": payload}` via daemon client (HTTP ≥400 throws `RequestException`; connection errors and node-token mismatch throw `ConnectionException`) | `start`\|`restart`\|`stop`\|`kill` (default `restart`) |
| `command` | `Server::send()` → `POST {node}/api/servers/{uuid}/commands {"commands":[payload]}` — plain `Http::daemon`, **HTTP errors do not throw**, only connection errors | console command |
| `backup` | `InitiateBackupService->setIsScheduled(true)->setIgnoredFiles(explode(PHP_EOL, payload))->handle(server, null, override: true)` (§9.1) | ignore list, newline separated |
| `delete_files` | `DeleteFilesService::handle(server, explode(PHP_EOL, payload))`: for each line, list `dirname(line)` on Wings and collect entries whose name matches `basename(line)` with `Str::is` wildcard; one `deleteFiles('/', paths)` call | newline-separated glob paths |

Plugins can register more task schemas (`TaskService::register`, first registration of an id wins).

---

## 5. Events and listeners

Listeners in `app/Listeners` are **auto-discovered** by the type-hint of `handle()` (Laravel default
`withEvents()` → `discoverEventsWithin = app/Listeners`, `vendor/.../Foundation/Configuration/ApplicationBuilder.php:100-119`,
`.../Support/Providers/EventServiceProvider.php:136-171`). `app/Providers/EventServiceProvider.php:14-20`
additionally maps `ActivityLogged`, the wildcard `App\Events\*` and `eloquent.created*`, `eloquent.deleted*`,
`eloquent.updated*` to `DispatchWebhooks` (synchronous listener).

| Event | Dispatched from | Listener(s) → effect |
|---|---|---|
| `Illuminate\Auth\Events\Login` / `Failed` | Laravel auth (all guards; `Login` also fires on remember-me re-authentication) | `Listeners/Auth/AuthenticationListener.php:19-36` → activity `auth:success` / `auth:fail` with request metadata; subject = user if known; for `Failed`, every credential key except `password`, `token`, `secret` is stored as a property |
| `Illuminate\Auth\Events\PasswordReset` | password broker | `PasswordResetListener` → `auth:password-reset` (+ metadata, subject user) |
| `App\Events\Auth\ProvidedAuthenticationToken` | **never dispatched** in core | `TwoFactorListener` → `auth:token` / `auth:recovery-token` |
| `App\Events\Auth\FailedCaptcha` | **never dispatched** in core | — (webhook-selectable only) |
| `App\Events\Server\Installed(server, successful, initialInstall)` | Wings `POST /api/remote/servers/{uuid}/install` (`ServerInstallController.php:39-67`) | `ServerInstalledListener` → Filament DB notification to the owner + `ServerInstalled` mail if `send_install_notification` (initial) / `send_reinstall_notification` (reinstall). **Mail is sent for failed installs too.** |
| `App\Events\Server\BackupCompleted(backup)` | Wings backup status callback (`BackupStatusController.php:77`) | `BackupCompletedListener` → for owner + subusers who opted in (`server_user_settings`, keys `manual_backup_notifications` / `scheduled_backup_notifications`, **default false**): DB notification + `BackupCompleted` mail |
| `App\Events\Server\SubUserAdded(subuser)` | `SubuserCreationService.php:69` (inside transaction) | `SubUserAddedListener` → DB notification + `AddedToServer` mail if `send_added_to_server_notification` |
| `App\Events\Server\SubUserRemoved(server, user)` | `SubuserDeletionService.php:23` | `SubUserRemovedListener` → DB notification + `RemovedFromServer` mail if `send_removed_from_server_notification` |
| `App\Events\User\Deleting(user)` | `User::deleting` hook (`User.php:233-239`) | `RevocationListener` → `RevokeSftpAccessJob(user.uuid, node)` for every node hosting a server the user can directly access (chunks of 50) |
| `App\Events\User\PasswordChanged(user)` | `UserUpdateService.php:35` when a password is set | `RevocationListener` (same) |
| `App\Events\ActivityLogged(model)` | `ActivityLogService::save`, `ActivityProcessingController` | `DispatchWebhooks` (§6) |
| `eloquent.{created,updated,deleted}: {Model}` | Eloquent | `DispatchWebhooks` (§6) |

Other job dispatches: `RevokeSftpAccessJob` also from `SubuserDeletionService.php:25`,
`SubuserUpdateService.php:43`, `DetailsModificationService.php:49` (owner change). Event hooks for plugins:
`server:webhook.events` and `global:webhook.events` (string events with `[&$events]`) let plugins add webhook
event names (`WebhookConfiguration.php:293,332`).

---

## 6. Webhooks

### 6.1 Data model

`webhook_configurations` (`app/Models/WebhookConfiguration.php`, soft deletes): `id, scope ('global'|'server',
default global), server_id (FK servers, cascade), type (nullable string; model default `regular`; a null type is
delivered by `FallbackSchema`), payload (json, array cast,
nullable — template for non-regular types), headers (json, array cast), endpoint, name, description, events (json
list of strings), created_at, updated_at, deleted_at`.
`webhooks` (delivery log, `app/Models/Webhook.php`): `id, webhook_configuration_id, event, endpoint,
successful_at (nullable), payload (json), created_at, updated_at`; pruned per §3.

Caches (forever, default store; rebuilt on every save/delete of a configuration, `WebhookConfiguration.php:99-135`):
`webhooks.{eventName}` = Collection of **global** configs listening to that event; `watchedWebhooks` = unique list
of all events of global configs.

### 6.2 Event names

Allowed per scope (enforced by `StoreWebhookRequest.php:99-119` / Filament checkbox list):
- **Global** (`allPossibleAdminOnlyEvents`, `WebhookConfiguration.php:299-335`): from the set of
  `discoverCustomEvents()` (every class file under `app/Events`, e.g. `App\Events\Server\Installed`,
  `App\Events\ActivityLogged`) ∪ `allModelEvents()` (`eloquent.created: App\Models\X`, `eloquent.updated: …`,
  `eloquent.deleted: …` for every file in `app/Models`), filtered by name fragments `User, Role, SSHKey, ApiKey,
  Token, HasAccessTokens, Node, Allocation, DatabaseHost, Mount, NodeRole, Egg, EggVariable, Plugin,
  WebhookConfiguration, Webhook, Captcha, Authentication, ActivityLogged` (fragment matches after `\` or `.`
  and before `\`, `:` or end, case-insensitive), plus every event containing `App\Models\Server` but not
  `Subuser`; `eloquent.created: App\Models\Webhook` is blacklisted; plus plugin additions.
- **Server** (`allPossibleServerEvents`, `:232-296`): fixed list of activity names: `server:file.{read,write,
  rename,copy,compress,decompress,delete,create-directory,uploaded,pull,download}`, `server:power.{start,stop,
  restart,kill}`, `server:console.command`, `server:startup.{edit,image}`, `server:settings.{rename,description,
  reinstall}`, `server:allocation.{notes,primary,create,delete}`, `server:schedule.{create,update,execute,delete}`,
  `server:task.{create,update,delete}`, `server:backup.{start,delete,download,rename,restore,restore-complete,
  restore-failed}`, `server:database.{create,rotate-password,delete}`, `server:subuser.{create,update,delete}`,
  `server:sftp.denied`, plus plugin additions. (Names that never fire because the emitter uses another
  spelling: `server:file.uploaded` vs emitted `server:files.uploaded`; `server:backup.restore-failed` vs emitted
  `server.backup.restore-failed`; `server:settings.reinstall` vs `server:reinstall` from the client API.)

Display names: `transformClassName` = drop up to `eloquent.`, remove `App\Models\`, replace `App\Events\` with
`event: ` (`created: Server`, `event: Server\Installed`); `transformServerEventName` =
`after('server:')`, `.` → ` → `, title case.

### 6.3 Matching / dispatch (`app/Listeners/DispatchWebhooks.php`)

- `eloquent.{action}: {FQCN}` (`:39-67`): query (uncached) global configs with
  `whereJsonContains('events', name)`; payload `{"event": name, "data": model->toArray(), "timestamp":
  now()->toIso8601String()}`; skipped if every non-`event` value is empty.
- `App\Events\*` class events except `ActivityLogged` (`:70-103`): only if the name is in `watchedWebhooks`;
  configs from `webhooks.{name}` cache; payload `{"event": FQCN, "timestamp": …, "data": …}` where data =
  `toArray()` for Arrayable, else public properties (Arrayable values converted, others as-is, e.g.
  `{"server": {...}, "successful": true, "initialInstall": true}`), or the array itself. Not skipped when data is
  missing.
- `ActivityLogged` (`:33-36`, `:105-225`), both branches run:
  - server-scope (`handleActivityLogged`): only events starting with `server:`; server = first subject with
    morph type `server`, else `Server::find(properties.server.id)`; configs = that server's server-scope
    webhooks whose `events` contains the activity event name.
  - global (`handleGlobalWebhooks`): global configs listening to the activity name (if watched) ∪ configs
    listening to `App\Events\ActivityLogged` (cached), unique by id.
  - payload (`buildActivityPayload`):
    ```json
    {"event": "server:power.start", "description": null, "ip": "1.2.3.4",
     "timestamp": "2026-10-14T12:00:00+00:00",
     "actor": {"id": 1, "type": "user", "username": "admin"},
     "properties": {"…": "…"},
     "subjects": [{"id": 5, "type": "server"}]}
    ```
    `actor` only when `actor_id` set (`username` null for non-User actors); `properties` only if non-empty;
    `subjects` only if any.
- Each matching config → `WebhookConfiguration::run(eventName, webhookData)` → `ProcessWebhook::dispatch(config,
  eventName, [webhookData])` (queued). `run()` with no args (test button / `POST
  /api/application/webhooks/{id}/test` → HTTP 202 `{"dispatched": true, "endpoint": …}`) uses
  `eloquent.created: App\Models\Server` + `getWebhookSampleData()` for global, `server:file.write` +
  `getServerWebhookSampleData()` for server scope (`WebhookConfiguration.php:451-688`).
- `App\Services\WebhookService::dispatch()` exists but is unused.

### 6.4 Delivery (`app/Jobs/ProcessWebhook.php`)

`$tries = 3`. `handle()`:
1. `data = normalize(this.data[0] ?? [])` (string → json_decode assoc, object → toArray, wrapped into array).
2. `data.event = transformClassName(eventName)` — overwrites the key in place (keeps its position; appended if
   absent). E.g. `eloquent.created: App\Models\Server` → `created: Server`; activity names unchanged;
   `App\Events\Server\Installed` → `event: Server\Installed`.
3. `schema = WebhookTypeService::get(config.type) ?? new FallbackSchema()`. Core registers only `regular`
   (`app/Providers/Extensions/WebhookServiceProvider.php:13-19`). **There is no built-in Discord type**:
   migration `2025_04_09_015500_add_webhook_configurations_type_column.php` set `type='discord'` for
   endpoints on `discord.com`/`discordapp.com`; those rows deliver through `FallbackSchema` unless a plugin
   registers `discord`.
4. `payload = schema.preparePayload(config, data)`:
   - `regular` (`BaseSchema.php:83-86`): `data` as-is → body = the event data object (e.g.
     `{"event":"created: Server","data":{…},"timestamp":"…"}`; tests: `tests/Feature/Webhooks/ProcessWebhooksTest.php`).
   - `FallbackSchema` (`FallbackSchema.php:24-38`): if `config.payload` blank → `data`; else the stored template
     with `{{…}}` replaced in every string leaf (`array_walk_recursive`).
5. `headers = schema.prepareHeaders(...)`: regular/fallback (`RegularSchema.php:87-96`) = `config.headers` with
   `{{…}}` substitution; UI default headers `{"X-Webhook-Event": "{{event}}"}` (`RegularSchema.php:16-18`);
   spaces in header names replaced by `-` on save; API forbids CR/LF in header values.
   Template substitution (`WebhookConfiguration::replaceVars`, `:433-448`): regex `/{{(.*?)}}/`, key trimmed,
   `data_get(data, key, key)` — dot paths; **a missing key is replaced by the key name itself**; non-string
   values are string-converted by PHP (arrays → error).
6. `deliver()` (`BaseSchema.php:102-107`): `Http::withHeaders(headers)->timeout(panel.webhook.timeout (env
   APP_WEBHOOK_TIMEOUT, default 30))->post(endpoint, payload)` → JSON body (`json_encode` flags 0, so `/` is
   escaped as `\/` and non-ASCII as `\uXXXX`), `Content-Type: application/json`, Guzzle default User-Agent. No
   signing, no HMAC, no secret in core (a plugin type may sign in `prepareHeaders`).
7. Success = `response->successful()` (2xx) → `successful_at = now()`; otherwise `report("Webhook #{id} delivery
   to {host} failed with status {code}.")` and `retryAfter = schema.retryAfter(response)` (null for core types).
   Exceptions (timeouts etc.) are reported and treated as unsuccessful, never rethrown.
8. Always inserts a `webhooks` row `{payload, successful_at, event: raw eventName, endpoint}` (one per attempt).
9. Retry only if a schema returned a `retryAfter` and `attempts() < 3` → `release(seconds)`.

---

## 7. Activity logging

### 7.1 Storage

`activity_logs` (`app/Models/ActivityLog.php`, migrations `2022_05_28_135717_*`, `2022_06_18_112822_*`,
`2025_09_03_080547_remove_batch_column_from_activity_logs.php`): `id, event (string, indexed), ip (string,
NOT NULL), description (text nullable), actor_type, actor_id (nullable morph), api_key_id (nullable),
properties (json, `collection` cast → encoded with `json_encode` flags 0; empty → `[]`), timestamp`.
`activity_log_subjects`: `id, activity_log_id (FK cascade), subject_type, subject_id`.
Morph types use the enforced morph map (`AppServiceProvider.php:127-140`): `allocation, api_key, backup, database,
egg, egg_variable, schedule, server, ssh_key, task, user, node`.
Model rules: append-only — `creating` sets `timestamp = now()` **unconditionally**; `updating`/`deleting` throw
`LogicException` (`ActivityLog.php:153-166`). `DISABLED_EVENTS = ['server:file.upload']` (hidden from display).

### 7.2 The builder (`app/Services/Activity/ActivityLogService.php`, facade `App\Facades\Activity`)

Fluent API: `Activity::event('x')->subject(...$models)->actor($m)->property($k, $v)|property([..])
->withRequestMetadata()->description('…')->log()` or `->transaction(fn)`, `->anonymous()`, `->clone()`.
The facade caches the service instance per process/job, so state accumulates until `log()`/`save()` resets it.

Lazy initialisation of a new entry (`getActivity()`, `:198-225`):
- `ip = Request::ip()` (console/queue: Laravel's synthetic request → `127.0.0.1`);
- `properties = {}`; `api_key_id = LogTarget::apiKeyId()` — set by `TrackAPIKey` middleware
  (`app/Http/Middleware/Activity/TrackAPIKey.php`) to the Sanctum `ApiKey` id when the request is authenticated
  with an API key, else null;
- default subject: `LogTarget::subject()` (set by `ServerSubject` middleware = the route server, by
  `AccountSubject` = the user) else the Filament tenant if it is a `Server`;
- default actor: `LogTarget::actor()` else the authenticated user of the default guard.
- `subject()` de-duplicates (`Model::is`), ignores nulls; `withRequestMetadata()` adds properties
  `ip` (`Request::getClientIp()`) and `useragent`.

`log()` (`:138-157`): saves in a DB transaction (activity row, then one subject row per subject with
`subject_type = getMorphClass()`), then dispatches `ActivityLogged` (→ webhooks), then resets. On error: rethrow
unless `app.env === 'production'`, where it is logged and the unsaved model returned.
`transaction(fn)` (`:175-184`): runs the callback inside a DB transaction and saves the log inside it
(log rolled back with the work; `ActivityLogged` fires before the outer commit).

### 7.3 Wings-reported activity (`POST /api/remote/activity`, `app/Http/Controllers/Api/Remote/ActivityProcessingController.php`)

Body `{"data": [{"user": uuid|null, "server": uuid, "event": string, "metadata": object|null, "ip": ip|null,
"timestamp": RFC3339 string}]}` (`ActivityEventRequest.php`). Per item: server must belong to the calling node
and event must start with `server:` (else skipped); timestamp parsed after stripping fractional seconds
(`/(\.\d+)Z$/` → `Z`), failure → now + `metadata.original_timestamp`; `ip` empty → `127.0.0.1`;
`properties = metadata ?? []`; actor = user by uuid if found (`actor_type = user`). Each log is
`forceCreate`d, one subject `server`, then `ActivityLogged` dispatched. Because of the `creating` hook the stored
`timestamp` is the **processing time, not Wings' timestamp** (the parsed value is overwritten).

### 7.4 Event names emitted by the panel

`auth:success`, `auth:fail`, `auth:password-reset`, `auth:token`, `auth:recovery-token` (unused), `auth:ip-blocked`
(`AuthenticateIPAccess.php:43`), `auth:sftp.fail` (`SftpAuthenticationController.php:48`);
`user:account.{username-changed,email-changed,password-changed}`, `user:api-key.{create,delete}`,
`user:ssh-key.{create,delete}`;
`server:power.{signal}` (lower-cased from input), `server:console.command`, `server:files.uploaded`,
`server:file.{read,write,rename,copy,compress,decompress,delete,create-directory,pull,download}`,
`server:startup.{edit,image,command}`, `server:settings.{rename,description,reinstall}`, `server:reinstall`,
`server:mount.update`, `server:allocation.{notes,primary,create,delete}`,
`server:schedule.{create,update,execute,delete}`, `server:task.{create,update,delete}`,
`server:backup.{start,delete,download,rename,restore,complete,fail,restore-complete}`, `server.backup.restore-failed`
(sic, `BackupStatusController.php:102`), `server:backup.restore-failed` (only from `ServerDetailsController::resetState`),
`server:database.{create,rotate-password,delete}`, `server:subuser.{create,update,delete}`, `server:sftp.denied`;
plus any `server:*` event Wings reports. Labels are translations `activity.{event with ':' → '.'}`.
Activity visibility: `activity.hide_admin_activity` (env `APP_ACTIVITY_HIDE_ADMIN`) hides entries whose actor
has a role but is not owner/subuser (`ActivityLog::scopeHideAdminActivity`).

---

## 8. Notifications and mails

Mail notifications are Laravel `MailMessage`s rendered with the framework's default markdown notification
template (`Hello …` greeting, lines, one action button, `Regards,<br>{app.name}`); no custom templates in
`resources/views/vendor`. Subject defaults to the class name in title case (e.g. "Account Created") except
`MailTested`. Locale = `notifiable.language ?? 'en'`. All except `MailTested` implement `ShouldQueue`.
Strings: `lang/en/mail.php`.

| Class | Trigger | Content (en) |
|---|---|---|
| `AccountCreated(?token)` | `UserCreationService` (CLI, admin, API, installer) if `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION` (default true) | "Hello {username}!" / "You are receiving this email because an account has been created for you on {app}." / "Username: …" / "Email: …"; action "Setup Your Account" → app panel password-reset URL **only if a token** (user created without password) |
| `AddedToServer(server)` | `SubUserAddedListener` | body, "Server Name: …", action "Visit Server" → server console URL |
| `RemovedFromServer(server)` | `SubUserRemovedListener` | error style, body, "Server Name: …", action "Visit Panel" → `url('')` |
| `ServerInstalled(server)` | `ServerInstalledListener` (also on failure) | "Your server has finished installing…", "Server Name: …", action "Login and Begin Using" → console URL |
| `BackupCompleted(backup)` | `BackupCompletedListener` (opt-in) | "The backup was created successfully." / "The backup creation failed.", backup name, server name, action "View Backups" |
| `MailTested(user)` | Admin settings "test mail" (`app/Filament/Admin/Pages/Settings.php:396`), sent synchronously | subject "Panel Test Message", body "This is a test of the Panel mail system. You're good to go!" |

Filament database notifications (`Notification::make()->…->sendToDatabase($user)`) are stored in the
`notifications` table through `Filament\Notifications\DatabaseNotification` (queued), `data` =
`{actions, body, color, duration: "persistent", icon, iconColor, iconSize, status, title, view, viewData,
format: "filament"}` (`vendor/filament/notifications/src/Notification.php:69-94,237-245`). Sent by the server
installed / backup / subuser listeners (titles from `lang/en/notifications.php`) and by plugin jobs.
Password-reset mails are Filament's built-in.

---

## 9. Backups lifecycle

Config `config/backups.php`: `presigned_url_lifespan` (env `BACKUP_PRESIGNED_URL_LIFESPAN`, 60 min),
`max_part_size` (`BACKUP_MAX_PART_SIZE`, 128 MiB; clamped to [5 MiB, 5 GiB] else default), `prune_age`
(`BACKUP_PRUNE_AGE`, 360 min), `max_concurrent_uploads` (`BACKUP_MAX_CONCURRENT_UPLOADS`, 10),
`throttles.limit` (`BACKUP_THROTTLE_LIMIT`, 2) / `throttles.period` (`BACKUP_THROTTLE_PERIOD`, 600 s).

### 9.1 Backup hosts and adapters

`backup_hosts (id, name, schema, configuration json, timestamps)` + pivot `backup_host_node (node_id UNIQUE,
backup_host_id)`; `backups.backup_host_id` NOT NULL FK (replaced the old `disk` column). Migration
`2026_01_16_081858_create_backup_hosts_table.php` created one host from the old env: `APP_BACKUP_DRIVER`
(`wings` → name "Local", no config; `s3` → name "Remote", configuration `{region: AWS_DEFAULT_REGION, key:
AWS_ACCESS_KEY_ID, secret: AWS_SECRET_ACCESS_KEY, bucket: AWS_BACKUPS_BUCKET, prefix: AWS_BACKUPS_BUCKET (sic),
endpoint: AWS_ENDPOINT, use_path_style_endpoint, use_accelerate_endpoint, storage_class}`).
Adapters (`app/Providers/Extensions/BackupAdapterServiceProvider.php:16-23`): `wings`
(`WingsBackupSchema.php`) and `s3` (`S3BackupSchema.php`); plugins may add more.

### 9.2 Create (`app/Services/Backups/InitiateBackupService.php:86-146`)

1. Throttle (if period > 0): count of the server's **non-failed** backups (`completed_at IS NULL OR
   is_successful`) created in the last `period` seconds ≥ `limit` → `TooManyRequestsHttpException` (429,
   retry-after = seconds until the last one ages out), message `Only {limit} backups may be generated within a
   {period} second span of time.`
2. Limit: if `backup_limit` is 0/null or non-failed count ≥ `backup_limit`: without override or with limit ≤ 0 →
   `TooManyBackupsException`; with override (scheduled backups) delete the oldest non-failed unlocked backup
   (`DeleteBackupService`), none → exception.
3. Host: first backup host attached to the server's node, else the first host with **no** nodes
   (`firstOrFail`). Unknown schema → `Exception('Backup host has unknown backup adapter.')`.
4. Transaction: create `backups` row `{server_id, uuid v4, name = trim(name) ?: "Backup at Y-m-d H:i:s",
   ignored_files (array), backup_host_id, is_locked, is_scheduled}` (defaults `is_successful=false`, `bytes=0`,
   `checksum=null`, `upload_id=null`), then `schema.createBackup()` → both adapters call Wings
   `POST /api/servers/{uuid}/backup {"adapter": schema id, "uuid": backup uuid, "ignore": ignored_files joined by "\n"}`.
   A daemon error rolls back the row.

### 9.3 Wings callbacks

- S3 upload URLs: `GET /api/remote/backups/{uuid}?size={bytes}` (`BackupRemoteUploadController.php:40-68`):
  size required (400); node must own the server (403); already completed → 409; host schema must be `s3` (400).
  `getUploadParts` (`S3BackupSchema.php:104-154`): `CreateMultipartUpload` on `{bucket}/{server_uuid}/{backup_uuid}.tar.gz`
  (`ContentType application/x-gzip`, optional `StorageClass`), stores `upload_id`, returns
  `{"parts": [presigned UploadPart URLs, PartNumber 1..ceil(size/part_size)], "part_size": n,
  "max_concurrent_uploads": n}` (URLs expire after `presigned_url_lifespan`).
- Status: `POST /api/remote/backups/{uuid}` (`BackupStatusController::index`, `:33-80`): node must own the
  server (403); already `is_successful` → 400. Activity `server:backup.complete` / `server:backup.fail` (subjects
  backup + server, property `name`) logged inside a transaction that sets `is_successful`, `is_locked` (forced
  false on failure), `checksum = "{checksum_type}:{checksum}"` (success) / null, `bytes = size` / 0,
  `completed_at = now`; for S3: `CompleteMultipartUpload` with `parts[{etag, part_number}]` (or `ListParts` if
  null), or `AbortMultipartUpload` on failure (no-op if no `upload_id`). Then `BackupCompleted` event. 204.
- Restore result: `POST /api/remote/backups/{uuid}/restore` (`:92-108`): node must match; server `status = null`;
  activity `server:backup.restore-complete` or `server.backup.restore-failed` (typo kept). 204.

### 9.4 Restore / download / delete

- Restore (client API, `BackupController.php:241-264`): server status must be null and backup completed;
  activity `server:backup.restore` (properties `name`, `truncate`) wraps: download link generation, `status =
  restoring_backup`, Wings `POST /api/servers/{uuid}/backup/{backup}/restore {"adapter", "truncate_directory",
  "download_url"}`.
- Download link (`DownloadLinkService`): wings → `{node address}/download/backup?token={JWT}` (15 min, scope
  `backup-download`, claims `backup_uuid`, `server_uuid`, user); s3 → presigned `GetObject` URL valid 5 min.
- Delete (`DeleteBackupService.php:22-46`): locked **and** successful+completed → `BackupLockedException`;
  wings: `DELETE /api/servers/{uuid}/backup/{backup}` (404 ignored); s3: `DeleteObject`; adapter errors are
  ignored for unsuccessful backups; then the row is deleted (hard delete; soft deletes dropped in
  `2026_08_06_100741_drop_backup_soft_deletes.php`).
- Wings boot: `POST /api/remote/servers/reset` (`ServerDetailsController::resetState`, `:84-130`) resets
  `installing`/`restoring_backup` servers of that node to `status = null`; it tries to log
  `server:backup.restore-failed` for servers whose latest `server:backup.restore-started` activity has a backup
  subject — that event name is never emitted, so nothing is logged.

### 9.5 States

`status` accessor: `completed_at` null → in progress; else successful / failed (`Backup.php:105-110`).

### 9.6 Orphan pruning `p:maintenance:prune-backups {--prune-age=}`

`since = option ?? config('backups.prune_age', 360)`; must be digits and non-zero. Backups with
`completed_at IS NULL AND created_at <= now - since minutes` are bulk-updated to `is_successful=false,
completed_at=now, updated_at=now` (no events, no S3 abort, no activity). Prints the count.

---

## 10. Server transfers

Tables: `server_transfers (id, server_id, old_node, new_node, old_allocation, new_allocation,
old_additional_allocations json, new_additional_allocations json, successful bool nullable, archived, timestamps)`.
`Server::transfer()` = latest transfer with `successful IS NULL` (`Server.php:423-426`).

1. Start (`TransferServerService::handle`, `app/Services/Servers/TransferServerService.php:64-117`; from the
   application API `POST /api/application/servers/{id}/transfer` `{node_id, allocation_id (unique among servers),
   allocation_additional[]}` → 204, or the admin UI which also passes selected backup uuids):
   - target node must be viable for the server's memory/disk/cpu (`NodeNotViableException`, 406);
   - `validateTransferState()`: server installed, not restoring, no pending transfer
     (`ServerStateConflictException`);
   - transaction: create transfer `{server_id, old_node, new_node}`; if the server has a primary allocation:
     record old/new primary and old/new additional ids and assign the requested **free** allocations on the new
     node to the server (`server_id = server`), so they cannot be taken meanwhile;
   - JWT for the destination node: expires 15 min, scope `transfer`, subject = server uuid;
   - Wings (source node) `POST /api/servers/{uuid}/transfer {"url": "{new node address}/api/transfers",
     "token": "Bearer {jwt}", "backups": [uuids of selected backups that use the wings adapter],
     "server": {"uuid": …, "start_on_completion": false}}`.
   - Admin UI then deletes (panel-side only) the server's wings backups that were not selected.
2. Success callback from the **new** node only: `POST /api/remote/servers/{uuid}/transfer/success`
   (`ServerTransferController.php:64-109`): transaction: release old allocations (`server_id=null, notes=null,
   is_locked=false, show_port=true`, `Allocation::RELEASE_ATTRIBUTES`) and set `allocation_id = new_allocation`
   (only if the server had allocations), `node_id = new_node`, transfer `successful = true`; then Wings (old node)
   `DELETE /api/servers/{uuid}` (connection errors only logged). 204.
3. Failure callback from either node: `…/transfer/failure` (`:35-57`): transfer `successful = false`; release the
   new allocations. 204. No pending transfer → 409; other node → 403.
4. Cancel (application API `POST /api/application/servers/{id}/transfer/cancel`, `cancelTransfer`,
   `ServerManagementController.php:115-135`, route `routes/api-application.php:76`): no pending transfer → 406;
   sets `successful = true` (sic) **before** calling Wings, does not release allocations, then
   `DELETE {old}/api/servers/{uuid}/transfer` and `DELETE {new}/api/transfers/{uuid}`
   (`DaemonServerRepository.php:106-121`).

---

## 11. Health checks

Registered in `AppServiceProvider.php:91-103` (not in unit tests), using spatie/laravel-health with the result
store `CacheHealthResultStore` (store `file`, key `health:storeResults`); notifications disabled
(`config/health.php`). Run every 5 minutes by `health:check` and on demand from the admin Health page
(`app/Filament/Admin/Pages/Health.php:61-77`, runs the command when there are no stored results / on refresh).

| Check | Logic | Result |
|---|---|---|
| `DebugModeCheck` (only if `app()->isProduction()`) | `config('app.debug') === false` | failed with actual/expected |
| `EnvironmentCheck` | `App::environment() === 'production'` | |
| `CacheCheck` | put/get/forget `laravel-health:check-{rand5}` on `cache.default` (10 s) | failed on mismatch/exception |
| `DatabaseCheck` | `DB::connection(default)->getPdo()` | |
| `QueueCheck` (Spatie) | heartbeat `health:checks:queue:latestHeartbeatAt.{queue}` younger than 5 min | |
| `ScheduleCheck` (`app/Checks/ScheduleCheck.php`) | heartbeat `health:checks:schedule:latestHeartbeatAt`: missing → failed "not ran"; age in minutes > 1 → failed | |
| `UsedDiskSpaceCheck` (`app/Checks/UsedDiskSpaceCheck.php`) | `100 - free*100/total` of `/` (PHP `disk_free_space`) | warning > 70 %, failed > 90 % (Spatie defaults) |
| `PanelVersionCheck` | latest GitHub release vs `config('app.version')` (`canary` → always latest) | warning if latest unknown |
| `NodeVersionsCheck` | for every node, `systemInformation()` (Wings `/api/system`) version vs latest Wings release; nodes with errors are ignored; no nodes → skipped | failed if any outdated |

Laravel's `/up` health route is enabled (`bootstrap/app.php:27`) and used by `docker/healthcheck.sh`.

---

## 12. Installer (`app/Livewire/Installer`)

Route `GET /installer` (`routes/base.php:7-8`), Filament `SimplePage`; available only while
`config('app.installed')` (env `APP_INSTALLED`, default `true`; `.env.example` and the Docker entrypoint set
`false`) is false — otherwise 404. While not installed, `RedirectIfNotInstalled` (in every Filament panel)
redirects everything to it. Language selector (sets locale live). Wizard steps (`PanelInstaller.php:106-117`,
plugins can customise via `CanCustomizeSteps`), state in `$data`, step persisted in the query string:

1. **Requirements** (`Steps/RequirementsStep.php`): PHP ≥ 8.3; extensions bcmath, curl, gd, intl, mbstring,
   pdo_mysql, pdo_sqlite, xml, zip; `storage/` and `bootstrap/cache/` octal perms ≥ 755 (string compare of the
   last 4 octal digits). Any failure blocks "next".
2. **Environment**: `env_general.APP_NAME` (default config), `env_general.APP_URL`; admin `user.email`,
   `user.username`, `user.password`. After validation → writes `env_general` to `.env`.
3. **Database**: `DB_CONNECTION` (`sqlite`, `mariadb`, `mysql`, `pgsql`), `DB_DATABASE` (sqlite path, default
   `database.sqlite`), `DB_HOST`, `DB_PORT` (3306/5432), `DB_USERNAME` (`pelican`), `DB_PASSWORD` (empty →
   configured password if same driver). Connection tested (non-sqlite) with a temporary `_panel_install_test`
   connection; then writes `env_database`.
4. **Eggs**: runs `p:egg:update-index`, shows the index (see `eggs.md` §6.4); selection stored in `data.eggs`.
5. **Cache**: `CACHE_STORE` (`file`, `redis`); redis host/port/username/password tested with a Predis `PING`;
   writes `env_cache` (`CACHE_STORE`, `REDIS_*`).
6. **Queue**: `QUEUE_CONNECTION` (`database`, `redis` only if cache is redis, `sync`); outside Docker a "done"
   toggle must be accepted and the crontab line and `sudo php {base}/artisan p:environment:queue-service`
   command are shown; writes `env_queue`.
7. **Session**: `SESSION_DRIVER` (`file`, `database`, `cookie`, `redis` only with redis cache); hidden
   `SESSION_SECURE_COOKIE = request()->isSecure()`.

`writeToEnv($key)` removes falsy values (`array_filter`) before writing, so empty fields and
`SESSION_SECURE_COOKIE=false` are never written (true is written as `SESSION_SECURE_COOKIE=1`).

Finish (`submit`, `PanelInstaller.php:139-164`): `migrate --force --seed` (seeder creates the root-admin role) and
verifies all migrations ran; create the admin with `UserCreationService` (`root_admin = true`; sends
`AccountCreated` if enabled) and log them in (remember = true); write `env_session` (last, to avoid "page expired");
dispatch one `InstallEgg` job per selected egg URL + persistent notification; write `APP_INSTALLED=true`; redirect
to the admin panel. Any step error shows a persistent danger notification and halts. `APP_KEY` is not generated by
the web installer (Docker entrypoint / `p:environment:setup` do it).

---

## 13. Version checks and telemetry

No telemetry exists. Outbound calls to third parties (`app/Services/Helpers/SoftwareVersionService.php`):
- `GET https://api.github.com/repos/pelican/panel/releases/latest` (timeout 5 s, connect 1 s) → `tag_name`
  with `v` trimmed, cached `panel:latest_version` for `panel.cdn.cache_time` (60) minutes; changelog `body` cached
  as `panel:latest_version_changelog`; failures cached as the string `error` (and evicted on the next read, so
  retried at most once per request).
- `GET https://api.github.com/repos/pelican/wings/releases/latest` → `wings:latest_version`.
- `isLatestPanel()`: true if current is `canary` or latest unknown or `version_compare(current, latest, '>=')`.
  `isLatestWings(v)`: `develop` or unknown → true.
- `currentPanelVersion()` (cached 5 min `panel:current_version`): `config('app.version')` (`canary` in this tree);
  for canary, reads `.git/HEAD` → `canary ({7-char sha})`.
- Other outbound: egg index and egg update URLs (`eggs.md`), plugin hub (`plugins.md`).

---

## 14. Open questions / uncertain framework behaviour

1. `RunTaskJob` has no `$tries`; with the documented `queue:work --tries=3` a failing task (e.g. a power action
   answered with HTTP 4xx/5xx) is executed up to three times before `failed()` runs. Confirm whether the Rust
   port should reproduce retries or execute once (recommendation: once, as the comments in the job imply).
2. When a `RunTaskJob`'s task/schedule row is deleted before the job runs, `SerializesModels` throws
   `ModelNotFoundException` during unserialisation; Laravel then fails the job without calling the job's own
   `failed()`, leaving `is_processing = true` on a surviving schedule. Not verified at runtime.
3. Model validation on save: `#[ObservedBy]` sits on the `HasValidation` trait and is most likely not picked up
   (PHP does not expose trait attributes through `ReflectionClass::getAttributes()` of the using class), so
   `$validationRules` are probably not enforced by `save()` anywhere (affects activity logs with null `ip`,
   backups, schedules, tasks…). No PHP runtime was available to confirm.
4. `p:environment:mail` writes `MAIL_SCHEME=tls|ssl` while `config/mail.php` passes `MAIL_SCHEME` to Symfony
   Mailer as the DSN scheme (expects `smtp`/`smtps`); `tls` may be rejected as an unsupported scheme by the
   mailer. Not verified.
5. Facade instance caching (`Activity`, `LogTarget`) inside a single HTTP request means an `Activity::…` chain
   that never calls `log()` leaks its event/subjects into the next chain of the same request.
6. Laravel's `schedule:run` with `withoutOverlapping(10)` uses a cache lock (`framework/schedule-*` mutex in the
   default cache store); with the `file` cache and multiple panel replicas the lock is not shared.
7. Webhook payload JSON uses default `json_encode` flags via Guzzle (escaped slashes, `\u` escapes); consumers that
   compare bodies byte-for-byte (signatures) depend on it. Guzzle's default User-Agent string depends on the
   Guzzle/curl/PHP versions.
8. `ActivityLogged` for server webhooks resolves the server via `properties.server.id` as a fallback; no current
   emitter sets that property shape (it seems intended for plugins).
9. `AuthenticationListener` logs `auth:success` on every `Login` event, including remember-me recall:
   `SessionGuard::user()` fires `Login` (remember = true) whenever it restores a user from the recaller cookie
   (`vendor/laravel/framework/src/Illuminate/Auth/SessionGuard.php:199-202`). Whether Filament's guard setup
   triggers this on every new session was not verified.
