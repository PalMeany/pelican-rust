# Plugin system (original Pelican panel)

Status: reference spec, derived from the PHP source in `panel/` (PHP 8.5, Laravel 13, Filament 5).
All paths are relative to `panel/` unless stated otherwise. Vendor behaviour was checked against
`.ref/vendor` (the exact framework versions the panel uses).

The plugin system is not a separate runtime. A plugin is a folder of PHP code that the panel loads in-process:
it is PSR-4 autoloaded, its Laravel service providers are registered, and its Filament plugin class is
attached to one or more Filament panels. "Extension points" are mostly (a) the normal Laravel/Filament APIs a
plugin can call from that code, plus (b) a set of Pelican-specific static registries and service registries that
the core consults. Section 9 is the inventory of those extension points.

---

## 1. Source map

| Concern | File |
|---|---|
| Plugin metadata model (Sushi, rows read from disk) | `app/Models/Plugin.php` |
| Status / category enums | `app/Enums/PluginStatus.php`, `app/Enums/PluginCategory.php` |
| All lifecycle logic (load, import, install, update, uninstall, crash handling) | `app/Services/Helpers/PluginService.php` |
| Hub credential helper (adds `X-Panel-Api-Key`) | `app/Services/Helpers/HubCredentials.php` |
| Plugin settings contract | `app/Contracts/Plugins/HasPluginSettings.php` |
| Queue jobs (install / update / uninstall) | `app/Jobs/Plugin/InstallPlugin.php`, `UpdatePlugin.php`, `UninstallPlugin.php` |
| Artisan commands + scaffolding stubs | `app/Console/Commands/Plugin/*` (`Plugin.stub`, `PluginProvider.stub`, `PluginConfig.stub`) |
| Admin UI | `app/Filament/Admin/Resources/Plugins/PluginResource.php`, `.../Pages/ListPlugins.php` |
| Application API | `app/Http/Controllers/Api/Application/Plugins/PluginController.php`, `app/Http/Requests/Api/Application/Plugins/*`, `app/Data/Api/Application/PluginData.php`, `routes/api-application.php` (lines ~178-198) |
| Policy (admin permissions) | `app/Policies/PluginPolicy.php` (uses `DefaultAdminPolicies`), `app/Models/Role.php` (`SPECIAL_PERMISSIONS['plugin']`) |
| Boot hooks | `app/Providers/AppServiceProvider.php::register()` → `loadPlugins()`; `app/Providers/Filament/{Admin,App,Server}PanelProvider.php` → `loadPanelPlugins($panel)` |
| Crash handling | `app/Exceptions/Handler.php` (`handlePluginCrash`, `crashedPlugin`) |
| Path helper | `app/helpers.php` → `plugin_path(string $plugin, string ...$paths)` = `base_path('plugins')/<plugin>/<paths...>` |
| Config | `config/panel.php` → `panel.plugin.*` |
| Language list integration | `app/Services/Helpers/LanguageService.php` |
| Asset build integration | `vite.config.js` (globs `plugins/*/resources/{css,js}/**`) |
| Docker integration | `docker/entrypoint.sh` (`php artisan p:plugin:composer` on start), `Dockerfile` (composer binary shipped for runtime installs), compose files mount `/var/www/html/plugins` as a volume |
| Tests that pin behaviour | `tests/Integration/Services/PluginServiceTest.php`, `tests/Feature/PluginLoaderCrashTest.php`, `tests/Unit/Models/PluginCompatibilityTest.php`, `tests/Unit/Models/PluginApiVersionTest.php`, `tests/Integration/Api/Application/PluginHubCredentialsTest.php`, `tests/Integration/Webhooks/WebhookSchemaExtensionTest.php` |

There is no `plugins` database table. Plugin state lives entirely in `plugins/<id>/plugin.json` (the `meta` block).

---

## 2. On-disk layout of a plugin

`plugins/` sits at the panel base path (`plugins/.gitignore` ignores everything). Each subdirectory that contains a
`plugin.json` is a plugin; directories starting with `.` are temp import/backup dirs and are ignored by crash
attribution (`.import-<random16>`, `.<id>.bak`).

```
plugins/<id>/
  plugin.json                     # manifest + mutable meta (status, load order)  [required]
  README.md                       # shown in admin "View" slide-over (cached 5 min)
  config/<id>.php                 # merged into config('<id>') at boot
  src/<Class>.php                 # Filament plugin class (namespace from plugin.json)  [required unless errored]
  src/Providers/**/*.php          # every file here is registered as a Laravel ServiceProvider
  src/Console/Commands/**/*.php   # every file here is registered as an artisan command
  lang/<locale>/*.php             # translations (namespace "<id>::", or global for language packs)
  resources/views/**              # Blade views, namespace "<id>::"
  resources/css/**/*.css          # picked up by vite build (global input glob)
  resources/js/**/*.js            # picked up by vite build
  database/migrations/*.php       # run on install, reset on uninstall, also added to global migrator paths
  database/Seeders/<StudlyName>Seeder.php   # class \Database\Seeders\<Studly(name)>Seeder, run on install
  database/Factories/*.php        # PSR-4 "Database\Factories\" (shared namespace with core)
```

The `<id>` directory name must equal `plugin.json:id` case-insensitively (see §3.3).

---

## 3. `plugin.json` schema

### 3.1 Fields

| Key | Type | Required | Default when missing | Notes |
|---|---|---|---|---|
| `id` | string | yes | — | Lower-cased on read. Must equal the folder name (lower-cased) or the row becomes an error row. On import it must match `^[a-z0-9][a-z0-9._-]*$` after lower-casing/trim. |
| `name` | string | yes | — | Display name. Also used to derive the seeder class `Database\Seeders\{Str::studly(name)}Seeder`. |
| `author` | string | yes | — | |
| `version` | string | no | `"1.0.0"` | Compared with `version_compare` against the update feed. |
| `description` | string\|null | no | `null` | Table shows first 80 chars. |
| `category` | `"plugin"` \| `"theme"` \| `"language"` | yes | — | `PluginCategory` enum. Invalid value → exception → error row. |
| `url` | string\|null | no | `null` | Website; "Visit website" button and fallback for the View action when no README. |
| `update_url` | string\|null | no | `null` | URL of the update feed JSON (§6.6). |
| `namespace` | string | yes | — | PSR-4 root for `src/` (e.g. `Author\\MyPlugin`). |
| `class` | string | yes | — | Filament plugin class name inside `namespace`; full class is `\<namespace>\<class>`. |
| `panels` | string[] \| comma string \| null | no | `null` | Filament panel ids the plugin attaches to: `admin`, `app`, `server`. `null`/empty = all panels. Stored internally as comma-joined string. |
| `panel_version` | string\|null | no | `null` | Compatibility constraint: exact `"1.2.3"` (strict) or caret `"^1.2"` (§4.3). |
| `api_version` | int\|null | no | `null` (treated as 1) | Plugin API version. Panel supports up to `Plugin::SUPPORTED_API_VERSION = 1`. Admin table shows a warning when missing. |
| `composer_packages` | object\|string\|null | no | `null` | Map `"vendor/package": "constraint"`. Arrays are JSON-encoded into a string internally. |
| `meta` | object | no | — | Mutable state written by the panel (below). |
| `meta.status` | string | no | `"not_installed"` | `PluginStatus` value (lower-cased on read). |
| `meta.status_message` | string\|null | no | `null` | Last error / incompatibility message (shown as tooltip). |
| `meta.load_order` | int | no | `0` | Sort key for loading; editable by drag-reorder in admin. |

Unknown keys are preserved when the panel rewrites the file (it decodes, merges `meta`, re-encodes with
`JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES`).

### 3.2 Example (as written by `p:plugin:make`)

```json
{
    "id": "my-plugin",
    "name": "My Plugin",
    "author": "Jane",
    "version": "1.0.0",
    "description": "Does things",
    "category": "plugin",
    "url": "https://example.com",
    "update_url": "https://example.com/my-plugin/update.json",
    "namespace": "Jane\\MyPlugin",
    "class": "MyPluginPlugin",
    "panels": ["admin", "server"],
    "panel_version": "^1.0.0",
    "api_version": 1,
    "composer_packages": null,
    "meta": { "status": "enabled", "status_message": null }
}
```

### 3.3 How rows are built (`Plugin::getRows()`)

`Plugin` is a Sushi model (in-memory SQLite) whose rows are produced by scanning `base_path('plugins/')`:

1. For each directory, skip if no `plugin.json`.
2. Decode JSON (`JSON_THROW_ON_ERROR`), lower-case `id`; throw `PluginIdMismatchException` if it differs from the
   lower-cased folder name.
3. Normalise `panels` (array → comma string) and `composer_packages` (array → JSON string); apply defaults above.
4. On any exception:
   - if `panel.plugin.dev_mode` → rethrow;
   - otherwise `report()` it; for `JsonException` the plugin is **dropped entirely** (not listed);
   - for other exceptions an **error row** is added: `id` = folder name (id mismatch) or manifest id or random UUID,
     `name` = manifest name or `Str::headline(folder)`, `author` = `'Unknown'`, `version` = `'0.0.0'`,
     `description` = mismatch message or `'Plugin.json is invalid!'`, `category=plugin`,
     `namespace='Error'`, `class='Error'`, `status=errored`, `status_message` = exception message.
5. `Plugin::refreshRows()` resets Sushi so the next query re-scans disk (used after imports/updates).

Model columns: `id, name, author, version, description, category, url, update_url, namespace, class, panels,
panel_version, api_version, composer_packages, status, status_message, load_order`. Only `status`,
`status_message`, `load_order` are fillable; `Plugin::update()` is called after writing `meta` so the in-memory
row matches disk.

---

## 4. Plugin model semantics

### 4.1 Statuses (`PluginStatus`)

| Value | Label | Meaning |
|---|---|---|
| `not_installed` | Not Installed | Files present, install steps never run (or uninstalled). Default. |
| `disabled` | Disabled | Installed but not loaded. |
| `enabled` | Enabled | Installed and loaded. |
| `errored` | Errored | Threw while loading (non-fatal). Still loaded/retried on every boot; flips back to `enabled` the next time `loadPanelPlugins` succeeds. |
| `incompatible` | Incompatible | Fails `api_version` (production only) or `panel_version` check. Re-evaluated every boot; if it becomes compatible it is set to `disabled`. |
| `crashed` | Crashed | Caused a fatal error / boot-time error. Its files are not touched at all on boot (not even config) until re-enabled. Root admins get a DB notification. |

### 4.2 Predicates

- `fullClass()` = `'\' . namespace . '\' . class`.
- `shouldLoad(?panelId)`: `fullClass() !== '\Error\Error'` AND status ∈ {`enabled`, `errored`} AND
  (panelId null OR `panels` empty OR panelId ∈ panels).
- `canEnable()`: status ∈ {`disabled`, `crashed`} AND `isCompatible()`.
- `canDisable()`: status ∈ {`enabled`, `incompatible`}.
- `isTheme()` / `isLanguage()`: category check.
- `effectiveApiVersion()` = `api_version ?? 1`; `isApiVersionSupported()` = effective ≤ 1.

### 4.3 Panel-version compatibility (`isCompatible()`)

- Current version = `config('app.version')`, or `null` when it is `canary` (canary bypasses every constraint).
- No constraint, or canary → compatible.
- Strict (no leading `^`): `version_compare(current, panel_version, '=')`.
- Caret `^X.Y.Z` (composer-like): min = constraint padded to 3 parts; upper bound =
  - `X+1.0.0` if X ≠ 0 or only one part given,
  - `0.(Y+1).0` if X = 0 and (Y ≠ 0 or no patch part),
  - `0.0.(Z+1)` otherwise.
  Compatible iff `current >= min && current < upper` (`version_compare`). Prereleases of the upper bound are not
  excluded (documented quirk), but a prerelease below the minimum is (`1.0.0-rc1` vs `^1.0` → incompatible).
  The full truth table is in `tests/Unit/Models/PluginCompatibilityTest.php`.

### 4.4 Other model helpers

- `getProviders()`: every file under `src/Providers/` (recursive) → `<namespace>\Providers\<relative path without .php>`.
- `getCommands()`: every file under `src/Console/Commands/` → `<namespace>\Console\Commands\<...>`.
- `getSeeder()`: `\Database\Seeders\{Studly(name)}Seeder` if the class exists.
- `getReadme()`: contents of `README.md`, cached 5 min under `plugins.<id>.readme`.
- `hasSettings()` / `getSettingsForm()` / `getSettingsFormData()` / `saveSettings()`: instantiate the plugin class
  (`new <fullClass>()`, no constructor args) and delegate if it implements `HasPluginSettings`; all errors swallowed.
- Update helpers: §6.6.

---

## 5. Boot / loading sequence

Order of providers (`bootstrap/providers.php`): ActivityLog, **App** (plugins loaded in `register()`), Event,
the 7 extension providers (Avatar, BackupAdapter, Captcha, Feature, OAuth, Task, Webhook — all register
singletons in `register()`), FilamentServiceProvider, **Admin/App/Server panel providers** (panel plugins),
RouteServiceProvider, Socialite manager.

### 5.1 `PluginService::loadPlugins()` (called from `AppServiceProvider::register()`)

Skipped entirely when running unit tests or when `panel.plugin.safe_mode` is true. For each plugin ordered by
`load_order`:

1. `crashed` → skip completely.
2. In production only: if `!isApiVersionSupported()` → set `incompatible` with message
   "This Plugin requires plugin api version N but this Panel only supports up to version 1!" → skip.
3. If `!isCompatible()` → set `incompatible` ("…only compatible with Panel version X [or a compatible newer
   version] but you are using version Y!") → skip. Else if status was `incompatible` → set `disabled`.
   (These writes happen on every boot, i.e. every request.)
4. `namespace === 'Error'` → skip.
5. Register PSR-4: `<namespace>\` → `plugins/<id>/src/`; add `Database\Factories\` → `database/Factories/` and
   `Database\Seeders\` → `database/Seeders/` (done even for disabled plugins so migrations/classes resolve).
6. If `config/<id>.php` exists → `config()->set('<id>', require file)` (also for disabled plugins).
7. If `!shouldLoad()` → stop here for this plugin.
8. Translations: if `lang/` exists, after the translator resolves: language packs → `$translator->addPath(lang)`
   (merged into the global, un-namespaced groups); other categories → `$translator->addNamespace('<id>', lang)`.
9. Service providers: for each class from `getProviders()` that exists and extends `Illuminate\Support\ServiceProvider`
   → `$app->register($provider)`.
10. Artisan commands: for each class from `getCommands()` extending `Illuminate\Console\Command` →
    `Artisan::starting(fn ($artisan) => $artisan->resolve($command))`.
11. Migrations: if `database/migrations` exists, `$migrator->path(dir)` after the migrator resolves (so plain
    `php artisan migrate` also runs plugin migrations).
12. Views: if `resources/views` exists, `$view->addNamespace('<id>', dir)`.
13. Any `Throwable` → `handlePluginException()`: rethrow in dev mode; else `report()`, and unless the report
    already marked it `crashed`, set status `errored` with the message.

### 5.2 `PluginService::loadPanelPlugins(Panel $panel)` (called at the end of each panel provider)

Same test/safe-mode guard. For each plugin by `load_order` where `shouldLoad($panel->getId())`:
- assert the class exists, `$panel->plugin(new <fullClass>())` (Filament calls `register($panel)` immediately and
  `boot($panel)` later when the panel is actually serving a request);
- if status was `errored` → set `enabled`;
- exceptions → `handlePluginException()` as above.

Panel ids: `admin` (path `/admin`), `app` (default panel, `/`), `server` (path `/server`, tenant = Server by
`uuid_short`).

### 5.3 Crash handling (`app/Exceptions/Handler.php`)

- Every reported throwable goes through `PluginService::handlePluginCrash()`. `crashedPluginFrom($error)` attributes
  an error to a plugin only if: (error is a `FatalError` OR the app has not finished booting) AND not dev mode AND
  the message does not start with "Allowed memory size"/"Maximum execution time" AND the error's file is under
  `plugins/<id>/` with `<id>` not starting with `.`.
- The plugin is set to `crashed` (status message = error message) and a danger DB notification
  "A plugin crashed and has been disabled" is sent to the current user if root admin, else to all Root Admins.
- Rendering: for non-JSON requests whose error is attributable, a minimal 500 page is returned with
  ":name crashed and has been disabled, reload the page to continue." (Filament error pages cannot render because
  the crash happens while panels register.)
- Recovery path when the panel cannot even boot: set `PANEL_PLUGIN_SAFE_MODE=true` (delete
  `bootstrap/cache/config.php` if config is cached). The plugin list then shows an alert banner
  "Safe mode is on, so no plugins are loaded…".

---

## 6. Lifecycle operations

All mutating operations write `meta` back into `plugin.json` via `setMetaData()` (merge into existing meta, rewrite
file, then `$plugin->update($meta)`).

### 6.1 Import from file — `downloadPluginFromFile(UploadedFile $file, ?string $expectedId = null): string`

1. Reject if compressed size > `panel.plugin.max_import_size` (default 100 MiB) — "Zip file too large. (N MiB)".
2. Open with `ZipArchive`; walk entries: reject names containing `..` or starting with `/`
   ("Zip file contains invalid path traversal sequences."); sum uncompressed sizes and reject once the total exceeds
   the same limit ("Zip file contents too large.") — before anything is written.
3. Extract into `plugins/.import-<random16>/contents` (same filesystem as target → atomic renames; auto-deleted).
4. Locate `plugin.json` at archive root or exactly one directory deep (first match; deeper ones are ignored).
   Missing → "The zip does not contain a valid plugin.json".
5. `id` must be a non-empty string → lower-case + trim → must match `^[a-z0-9][a-z0-9._-]*$`
   ("The plugin.json contains an invalid id"). The id from the manifest (not the zip filename) decides the folder.
6. If `$expectedId` given and differs → "The zip is for plugin \":actual\", not \":expected\"".
7. If `plugins/<id>` already exists (import over existing = update):
   - carry over the existing `meta` (status, load order…) into the new manifest; if the existing copy had no meta,
     strip any `meta` shipped in the archive; rewrite the new manifest first;
   - move existing dir to `plugins/.<id>.bak` (rollback copy).
8. Move the folder containing `plugin.json` to `plugins/<id>`; on failure restore the backup. Delete backup.
9. Return the id.

Callers (admin UI, API): if `isInstalled($id)` (status ≠ `not_installed` after refresh) → reinstall
(UI: queued `InstallPlugin(reinstall: true)`; API: synchronous `installPlugin($plugin, false)`).

### 6.2 Import from URL — `downloadPluginFromUrl(string $url, ?string $expectedId = null, ?string $expectedSha256 = null): string`

GET via `HubCredentials::request($url)` (timeout 60 s, connect 5 s, throws on HTTP error). If `$expectedSha256 !== null`
it must `hash_equals` the lower-cased value against `sha256(body)` — an empty or malformed advertised checksum also
fails ("The downloaded zip does not match the checksum from the update feed"). Size check on body, write to a temp
file named after the URL basename, then delegate to 6.1. Admin UI validates the URL as `required|url|ends_with:.zip`.

### 6.3 Install — `installPlugin(Plugin $plugin, bool $enable = true)`

1. `manageComposerPackages(json_decode(composer_packages))`:
   - new set = this plugin's packages ∪ packages of every plugin where `shouldLoad()` (enabled/errored);
   - `composer remove <old…>` for packages in `$oldPackages` not in the new set (only on uninstall);
   - `composer require --update-no-dev vendor/pkg:constraint …` (600 s timeout) for the whole new set;
   - process failures throw ("Could not require new composer packages: …").
2. `buildAssets(throw: isTheme())`: `yarn install` (300 s) then `yarn build` (600 s) in base path. Failures throw
   only for themes or in dev mode; otherwise logged as warnings and ignored.
3. `runPluginMigrations()`: `Migrator::run(plugins/<id>/database/migrations)`.
4. `runPluginSeeder()`: resolve seeder class, `setContainer`, run inside `Model::unguarded`.
5. If `$enable` → status `enabled`; else if status is `not_installed` → `disabled` (otherwise unchanged).
6. `clearCachedComponents()` on every Filament panel.
7. On exception: status `not_installed` with the error message, rethrow.

Admin "Install" dispatches `InstallPlugin(user, id)` which calls
`installPlugin($plugin, !$reinstall && (!$plugin->isTheme() || !$pluginService->hasThemePluginEnabled()))`
— i.e. a theme is installed but left disabled if another theme is already enabled.

### 6.4 Enable / Disable

`enablePlugin()` / `disablePlugin()` only flip `meta.status` (no composer/migrations). The UI enable action asks for
confirmation when enabling a theme while another theme is enabled ("Theme already enabled … can result in visual
bugs"). Takes effect on the next request (providers/panels are wired at boot).

### 6.5 Uninstall — `uninstallPlugin(Plugin $plugin, bool $deleteFiles = false)`

1. `rollbackPluginMigrations()` (`Migrator::reset(dir)`).
2. Status → `not_installed`.
3. If `$deleteFiles` → delete `plugins/<id>`.
4. `buildAssets()` (non-throwing).
5. `manageComposerPackages(oldPackages: <this plugin's packages>)` → removes packages no longer needed by other
   loaded plugins, then re-requires the remaining set.
6. Clear Filament panel component caches.

The admin UI's uninstall never deletes files (separate Delete action, only allowed for `not_installed`/`errored`,
which just deletes the directory). CLI (`--delete` / prompt) and API (`delete: true`) can delete.

### 6.6 Update — feed format and flow

`update_url` returns JSON in one of two shapes (the second lets one feed serve several plugins):

```json
{ "1.2.0": { "version": "1.4.0", "download_url": "https://…/plugin.zip", "sha256": "…" },
  "*":     { "version": "1.3.0", "download_url": "https://…/plugin.zip" } }
```
```json
{ "<plugin-id>": { "<panel-version>" | "*": { "version": "...", "download_url": "...", "sha256": "..." } } }
```

- Fetched via `HubCredentials::request()` (timeout 5 s, connect 1 s); cached 10 min in `plugins.<id>.update`
  (errors are reported and cached as `[]`).
- Entry selection: key equal to the current comparable panel version, else `*`; none (or canary) → no update.
- `isUpdateAvailable()` = `version_compare(entry.version, plugin.version, '>')`.
- `getUpdateDownload()` returns `{url, sha256}` from the **same** entry; `sha256` null only if the key is absent.
- `updatePlugin($plugin)`: require a download (else "No download url found."), `downloadPluginFromUrl(url, id, sha256)`,
  `refreshRows`, re-find; if status ≠ `not_installed` → `installPlugin($plugin, false)` (status/load order kept);
  forget the update cache; return refreshed plugin. Admin runs this in the queued `UpdatePlugin` job.

### 6.7 Load order

Admin table is drag-reorderable on `load_order` (authorize: `update plugin`). `ListPlugins::reorderTable()` calls
`updateLoadOrder($order)` which writes `meta.load_order = index` for each id. (See open question about
`Plugin::firstOrFail()`.)

### 6.8 Background jobs

`InstallPlugin(User $user, string $pluginId, bool $reinstall = false)`, `UpdatePlugin(User, id)`,
`UninstallPlugin(User, id)` — all `ShouldQueue + ShouldBeUnique` with unique ids `plugin:{install|update|uninstall}:<id>`.
Each refreshes rows, runs the service call, and sends a Filament DB notification to `$user`: success
("Plugin installed/updated/uninstalled" + "Go to Plugins" link) or danger with the exception message. The UI
immediately shows "…started in the background" / "This process can take a few seconds…".

### 6.9 Pelican Hub integration

There is no in-panel hub browser. The Hub (default `https://hub.pelican.dev`) is an external site that can enroll a panel:

- `PUT /api/application/plugins/hub` with `{hub_url, api_key}` (validation: `hub_url` required, ≤255, `url:https`,
  no whitespace; `api_key` required, ≤128, `^pnl_[A-Za-z0-9]+$`; requires plugin **write** API permission) writes
  `PANEL_PLUGIN_HUB_URL` (trailing `/` trimmed) and `PANEL_PLUGIN_HUB_API_KEY` to `.env`, runs `queue:restart`, and
  forgets every `plugins.<id>.update` cache entry. Returns 204.
- `HubCredentials::request($url)`: if a key is configured and the URL's origin (`lower(host):port`, https only,
  default 443) equals the hub origin → add header `X-Panel-Api-Key: <key>` and **disable redirects**; otherwise a
  plain client. Used for update-feed fetches and zip downloads (so the Hub can serve private beta builds).

---

## 7. Operator interfaces

### 7.1 Admin UI (`/admin/plugins`, nav label "Plugins", badge = plugin count)

Functional summary (full UI inventory lives in `ui.md`):
- Tabs: All + one per category (Plugin, Theme, Language Pack) with counts.
- Columns: name (+ description ≤80 chars, icon red/green for update available), author, version, category (All tab
  only), status badge (tooltip = status message; description warns when `api_version` missing). Searchable by name;
  name/author/version/category/status sortable; default sort `load_order`; rows open in new tab.
- Row actions: View (README markdown slide-over + Visit Website, or direct link to `url`), Settings (only `enabled` +
  `HasPluginSettings`; slide-over form filled from `getSettingsFormData()`, saved via `saveSettings()`), and a group:
  Install (status `not_installed`), Update (update available), Enable (`canEnable`), Disable (`canDisable`),
  Delete (`not_installed`/`errored`, confirm), Uninstall (not `not_installed`/`errored`, confirm).
- Header actions: Import from File (zip upload: mimes `application/zip`, `application/zip-compressed`,
  `application/x-zip-compressed`), Import from URL.
- Authorization: `viewList/create/update/delete plugin` admin permissions (Root Admin bypasses via `Gate::before`).

### 7.2 Artisan commands

| Command | Behaviour |
|---|---|
| `p:plugin:list` | Table of name, author, status, version, panels, category. |
| `p:plugin:make {--name} {--author} {--description} {--category} {--url} {--updateUrl} {--panels} {--panelVersion}` | Interactive scaffold. Name/author stripped to `[A-Za-z0-9 ]` (ASCII); `id = Str::slug(name)`; `namespace = Studly(author)\Studly(name)`; `class = Studly(name . 'Plugin')`; author remembered in cache `plugin.author`. Panels: all, or a multi-choice of admin/server/app. Panel version defaults to current, optional caret. Writes `plugin.json` (with `api_version: 1`, `meta.status: enabled` — i.e. scaffolded plugins skip install), `src/<Class>.php` (Filament `Plugin` with `getId/register/boot`), `src/Providers/<Class>Provider.php`, `config/<id>.php`. Refuses if dir or class exists. |
| `p:plugin:install {id?}` | Errors unless `not_installed`; `installPlugin()` (enable). |
| `p:plugin:update {id?}` | Errors unless update available; `updatePlugin()`. |
| `p:plugin:uninstall {id?} {--delete}` | Errors if `not_installed`; asks about deleting files when interactive. |
| `p:plugin:disable {id?}` | Errors unless `canDisable()`. (There is no `enable` command.) |
| `p:plugin:composer` | `manageComposerPackages()` with no args — re-requires packages of all loaded plugins (run by the Docker entrypoint on every container start because `vendor/` is not persisted). |

### 7.3 Application API (`/api/application/plugins`, bearer application key)

Permission resource name `plugin` (`Plugin::RESOURCE_NAME`); read endpoints need `READ`, others `WRITE`
(`AdminAcl`, bitmask NONE=0, READ=1, WRITE=2).

| Method & path | Request | Behaviour / errors | Response |
|---|---|---|---|
| `GET /` | `filter[id|name|author|category]`, `sort=id|name|author|category`, `per_page` (default 10) | Spatie QueryBuilder over the Sushi model | paginated list of `plugin` objects |
| `GET /{id}` | — | | `plugin` object |
| `POST /import/file` | multipart field `plugin` | `PanelException` "No 'plugin' file in request"; import; reinstall synchronously if installed | `201` empty |
| `POST /import/url` | `{url}` (`required|string`) | import from URL (no checksum); reinstall if installed | `201` empty |
| `PUT /hub` | `{hub_url, api_key}` | §6.9 | `204` |
| `POST /{id}/install` | — | `PanelException` "Plugin is already installed" unless `not_installed`; synchronous install+enable | `plugin` object |
| `POST /{id}/update` | — | "Plugin doesn't need updating" unless update available | `plugin` object |
| `POST /{id}/uninstall` | `{delete?: bool}` | "Plugin is not installed" if `not_installed` | `plugin` object |
| `POST /{id}/enable` | — | "Plugin can't be enabled" unless `canEnable()` | `plugin` object |
| `POST /{id}/disable` | — | "Plugin can't be disabled" unless `canDisable()` | `plugin` object |

Error mapping: plain `PanelException`s and generic `Exception`s thrown by the service (most import/install errors) have no status code in `app/Exceptions/Handler.php`, so they surface as HTTP 500 with the generic detail "An unexpected error was encountered…" (real message only with `APP_DEBUG`). `InvalidFileUploadException` (a `DisplayException`: URL import too large / checksum mismatch / temp write failure) returns HTTP 400 with its message. Validation errors → 422.

`plugin` object (`PluginData`, envelope `{ "object": "plugin", "attributes": {…} }`): `id, name, author, version,
description, category, url, update_url, namespace, class, panels (string[]|null), panel_version,
composer_packages (decoded object|null), meta {status, status_message, load_order, is_compatible, update_available,
can_enable, can_disable}`. Note `api_version` is **not** exposed.

### 7.4 Configuration (`config/panel.php` → `panel.plugin`)

| Key | Env | Default | Effect |
|---|---|---|---|
| `dev_mode` | `PANEL_PLUGIN_DEV_MODE` | `false` | Rethrow plugin load/manifest errors; asset build failures throw; crash attribution disabled. |
| `safe_mode` | `PANEL_PLUGIN_SAFE_MODE` | `false` | Load no plugins at all (core + panel loaders return early). |
| `max_import_size` | `PANEL_PLUGIN_MAX_IMPORT_SIZE` | `104857600` (100 MiB) | Max compressed and total uncompressed import size. |
| `hub_url` | `PANEL_PLUGIN_HUB_URL` | `https://hub.pelican.dev` | Origin allowed to receive the hub key. |
| `hub_api_key` | `PANEL_PLUGIN_HUB_API_KEY` | `null` | Sent as `X-Panel-Api-Key` to the hub only. |

Related: `app.version` (`canary` disables compatibility checks and update detection).

---

## 8. Plugin-facing contracts

### 8.1 Filament plugin class (required) — `Filament\Contracts\Plugin`

```php
interface Plugin {
    public function getId(): string;            // must be unique per panel; stub returns the plugin id
    public function register(Panel $panel): void; // called immediately by $panel->plugin(...) at panel construction
    public function boot(Panel $panel): void;     // called when the panel is in use (Filament middleware)
}
```
Instantiated with `new <fullClass>()` (no DI). The same class is attached to every panel listed in `panels`;
it typically branches on `$panel->getId()`.

### 8.2 `App\Contracts\Plugins\HasPluginSettings` (optional, implemented by the plugin class)

```php
public function getSettingsFormData(): array;      // initial form state
/** @return \Filament\Schemas\Components\Component[] */
public function getSettingsForm(): array;          // Filament form schema shown in the slide-over
public function saveSettings(array $data): void;   // persist however the plugin likes
```
The core gives no storage; plugins usually write their own config/env/db. (The `Plugin` model itself implements this
interface purely to proxy to the plugin class.)

---

## 9. Inventory of extension points ("hooks")

Notation: "register via" shows the call a plugin makes (normally from a service provider `boot()`/`register()` or
from the Filament plugin's `register(Panel)`).

### 9.1 Filament panel registration (per panel)

Through `register(Panel $panel)` a plugin can use any Filament 5 panel API. Panels and what is relevant:

| Panel id | URL | Tenancy | Notes |
|---|---|---|---|
| `admin` | `/admin` | none | nav groups: "Server", "User" (non-collapsible), "Advanced"; discovers `app/Filament/Admin/{Resources,Pages,Widgets}` |
| `app` | `/` (default) | none | navigation disabled, topbar only; server list resource |
| `server` | `/server/{uuid_short}` | `App\Models\Server` by `uuid_short` | `ServerSubject` tenant middleware; `Filament::getTenant()` is the current server |

Typical calls: `$panel->resources([...])`, `->pages([...])`, `->widgets([...])`,
`->discoverResources(in: plugin_path('<id>','src/Filament/<Panel>/Resources'), for: '<ns>\Filament\<Panel>\Resources')`
(same for pages/widgets; this directory convention is customary but not enforced), `->navigationItems([...])`,
`->navigationGroups([...])`, `->userMenuItems([...])`, `->renderHook($name, fn () => ...)`, `->viteTheme(...)` /
`->colors([...])` (themes), `->plugin(...)` for third-party Filament plugins, `->middleware()` /
`->authMiddleware()` / `->tenantMiddleware()`. Authorisation inside resources uses Laravel policies/gates
(§9.6). Plugins may also ship whole new panels via their own `Filament\PanelProvider` in `src/Providers`.

### 9.2 Render hooks

- All Filament `PanelsRenderHook` names (e.g. `panels::head.end`, `panels::body.start/end`, `panels::page.start/end`,
  `panels::page.header.actions.before/after`, `panels::topbar.*`, `panels::sidebar.*`, `panels::auth.login.form.before/after`,
  `panels::resource.pages.list-records.table.before/after`, `panels::footer`, …), registered with
  `FilamentView::registerRenderHook($name, $closure, scopes: ...)` or `$panel->renderHook(...)`.
- Pelican custom hooks (`app/Enums/CustomRenderHooks.php`, rendered in `resources/views/filament/layouts/footer.blade.php`):
  `pelican::footer.start`, `pelican::footer.end`.
- Core already uses: `PAGE_START` (alert banner container), `FOOTER` (footer view), `STYLES_BEFORE`/`SCRIPTS_AFTER`
  (vite `resources/css/app.css` / `resources/js/app.js`), `BODY_END` in the server panel (persistent
  `FileUploadManager`).

### 9.3 Customisable core pages/resources (static registries on core classes)

Traits in `app/Traits/Filament/`; positions are enums `HeaderActionPosition`, `HeaderWidgetPosition`, `TabPosition`,
`StepPosition` (each `Before`/`After`) and `ConsoleWidgetPosition`.

| Trait / method | Signature | Effect |
|---|---|---|
| `CanCustomizeHeaderActions` | `static registerCustomHeaderActions(HeaderActionPosition $pos, Action\|ActionGroup ...$actions)` | Prepend/append page header actions. |
| `CanCustomizeHeaderWidgets` | `static registerCustomHeaderWidgets(HeaderWidgetPosition $pos, string\|WidgetConfiguration ...$widgets)` | Prepend/append header widgets. |
| `CanCustomizePages` | `static registerCustomPages(array<string, PageRegistration> $pages)` | Add routes/pages to a resource (`'key' => MyPage::route('/path')`). |
| `CanCustomizeRelations` | `static registerCustomRelations(class-string<RelationManager> ...$relations)` | Add relation managers to a resource. |
| `CanCustomizeTabs` (instance) / `CanCustomizeStaticTabs` (static) | `static registerCustomTabs(TabPosition $pos, Tab ...$tabs)` | Add tabs to a tabbed form. |
| `CanCustomizeSteps` | `static registerCustomSteps(StepPosition $pos, Step ...$steps)` | Add wizard steps. |
| `CanModifyForm` | `static modifyForm(Closure(Schema): Schema $c)` | Post-process a resource form schema. |
| `CanModifyTable` | `static modifyTable(Closure(Table): Table $c)` | Post-process a resource table (columns, filters, actions…). |
| `Console::registerCustomWidgets` | `static registerCustomWidgets(ConsoleWidgetPosition $pos, class-string<Widget>[] $widgets)` | Server console page widgets at `top`, `above_console`, `below_console`, `bottom` (core order: Top → ServerOverview → AboveConsole → ServerConsole → BelowConsole → CPU/Memory/Network charts → Bottom; 3-column grid). |

Where each is available (`app/Filament/**`):

| Target | Header actions | Header widgets | Pages | Relations | Tabs | Steps | modifyForm | modifyTable |
|---|---|---|---|---|---|---|---|---|
| Admin resources: ApiKeys, BackupHosts, DatabaseHosts, Mounts, Roles, Users, Webhooks | all their pages | all their pages | ✓ | ✓ | Users (static) | — | ✓ | ✓ |
| Admin Nodes | list/create/edit/view | ✓ | ✓ | ✓ | Edit, View | Create | — | — |
| Admin Servers | list/create/edit/view | ✓ | ✓ | ✓ | Edit, View | Create | — | — |
| Admin Eggs | list/create/edit/view | ✓ | ✓ | ✓ | Create, Edit, View | — | — | — |
| Admin Settings page | ✓ | ✓ | — | — | ✓ | — | — | — |
| Server resources: Activities, Allocations, Backups, Databases, Files, Schedules, Subusers, Webhooks | list (+create/edit/view where present) | ✓ | ✓ | ✓ | — | — | Backups, Databases, Schedules, Webhooks | all except Files |
| Server Console page | ✓ (around power buttons) | — | — | — | — | — | — | — |
| Server `ServerFormPage` (Settings/Startup base) | ✓ | ✓ | — | — | — | — | — | — |
| App `ListServers` | ✓ | ✓ | — | — | — | — | — | — |
| Profile `EditProfile` | ✓ | ✓ | — | — | ✓ | — | — | — |
| Installer `PanelInstaller` (Livewire) | — | — | — | — | — | ✓ | — | — |

Notes:
- Settings page tabs: every form field's state is written to `.env` on save (`EnvironmentWriterTrait`,
  bools → `"true"/"false"`, enums → value), followed by `queue:restart`. A custom Settings tab therefore
  persists its fields as env vars named after the field keys.
- Default tab ids — Settings: `general`, `captcha`, `mail`, `backup`, `oauth`, `misc`. Profile: `account`, `oauth`,
  `2fa`, `passkeys`, `api_keys`, `ssh_keys`, `activity`, `customization`.

### 9.4 Extension registries (`app/Extensions/**`, singletons bound in `app/Providers/Extensions/*`)

All registries ignore a second registration with an existing id (first wins; webhooks also log a warning). Plugins
register from a service provider: `$this->app->make(XService::class)->register(new MySchema())` (or via
`afterResolving`). Because the core binds the singletons in `register()`, plugin providers registered later can
resolve and extend them.

#### 9.4.1 Avatar providers — `App\Extensions\Avatar\AvatarSchemaInterface`
```php
public function getId(): string;
public function getName(): string;
public function get(User $user): ?string;   // avatar URL, null = Filament default (UI Avatars)
```
Service `AvatarService(bool $allowUploadedAvatars, string $activeSchema)`: `register()`, `get($id)`,
`getActiveSchema()`, `getAvatarUrl(User)` (uploaded `storage/public/avatars/<user_id>.png` wins when
`FILAMENT_UPLOADABLE_AVATARS`), `getMappings()` (id → name, used for the Settings "Avatar provider" select).
Active provider: `FILAMENT_AVATAR_PROVIDER` (default `gravatar`). Built-ins: `gravatar`
(`https://gravatar.com/avatar/md5(email)`), `uiavatars` (null).

#### 9.4.2 Backup adapters — `App\Extensions\BackupAdapter\BackupAdapterSchemaInterface`
```php
public function getId(): string;
public function getName(): string;
public function createBackup(Backup $backup): void;
public function deleteBackup(Backup $backup): void;
public function getDownloadLink(Backup $backup, User $user): string;
/** @return Component[] */ public function getConfigurationForm(): array;  // fields under "configuration.*"
```
Base class `Schemas\BackupAdapterSchema` (name = `Str::title(id)`). Service: `register`, `get`, `getAll`,
`getMappings`. A `backup_hosts` row stores `schema` (adapter id) + `configuration` (JSON from the form). Consumers:
`InitiateBackupService` (`createBackup`), `DeleteBackupService`, `DownloadLinkService`, `ServerDeletionService`,
`TransferServerService` (only `WingsBackupSchema` backups are transferred), `BackupRemoteUploadController`
(multipart upload URLs only for `S3BackupSchema`). Built-ins: `wings` (Wings local; download via 15-min node JWT
`/download/backup?token=`), `s3` (Wings uploads using presigned multipart URLs; delete/download via AWS SDK; config
keys region, key, secret, bucket, endpoint, use_path_style_endpoint, optional storage_class).

#### 9.4.3 Captcha providers — `App\Extensions\Captcha\Schemas\CaptchaSchemaInterface`
```php
public function getId(): string;
public function getName(): string;
public function getConfig(): array;          // copied to config('captcha.<id>') at registration
public function isEnabled(): bool;
public function getFormComponent(): Component;   // field injected into the login form
/** @return Component[] */ public function getSettingsForm(): array;   // admin Settings > Captcha
public function getIcon(): null|string|BackedEnum;
public function validateResponse(?string $captchaResponse = null): void;  // throw on failure
```
Base `Schemas\BaseSchema` gives `getName()` = upper id, `getConfig()`/`getSettingsForm()` for env keys
`CAPTCHA_<ID>_SITE_KEY`, `CAPTCHA_<ID>_SECRET_KEY`; enable flag `CAPTCHA_<ID>_ENABLED`. Service:
`register`, `get`, `getAll`, `getActiveSchemas()`, `getActiveSchema()` (first enabled). Built-in: `turnstile`
(Cloudflare siteverify; extra `CAPTCHA_TURNSTILE_VERIFY_DOMAIN`, default true, compares returned hostname with
request host).

#### 9.4.4 OAuth providers — `App\Extensions\OAuth\OAuthSchemaInterface`
```php
public function getId(): string;
public function getName(): string;
public function getConfigKey(): string;                 // env flag, default OAUTH_<ID>_ENABLED
public function getSocialiteProvider(): ?string;        // class-string for SocialiteProviders, null = core Socialite driver
public function getServiceConfig(): array;              // merged into config('services.<id>') + redirect '/auth/oauth/callback/<id>'
/** @return Component[] */ public function getSettingsForm(): array;
/** @return Step[] */ public function getSetupSteps(): array;   // wizard shown when enabling in Settings
public function getIcon(): null|string|BackedEnum;
public function getHexColor(): ?string;
public function isEnabled(): bool;
public function shouldCreateMissingUser(OAuthUser $user): bool;
public function shouldLinkMissingUser(User $user, OAuthUser $oauthUser): bool;
```
Base `Schemas\OAuthSchema`: env keys `OAUTH_<ID>_CLIENT_ID`, `_CLIENT_SECRET`, `_SHOULD_CREATE_MISSING_USERS`,
`_SHOULD_LINK_MISSING_USERS`, `_ENABLED`. `Schemas\CommonSchema(id, ?name, ?configName, ?icon, ?hexColor)` is a
ready-made concrete schema for any Socialite driver. Registration also hooks `SocialiteWasCalled` to extend Socialite
when a provider class is given. Service: `register`, `get`, `getAll`, `getEnabled`, `linkUser`, `unlinkUser`
(`users.oauth` JSON map provider id → remote user id). Consumers: Login page buttons, `OAuthController`
(`/auth/oauth/redirect/{driver}`, `/auth/oauth/callback/{driver}`), Profile OAuth tab, admin Settings, admin user edit.
Built-ins: facebook, x, linkedin, google, github, gitlab, bitbucket, slack, authentik, discord, steam.

#### 9.4.5 Server features (console-triggered modals) — `App\Extensions\Features\FeatureSchemaInterface`
```php
/** @return string[] */ public function getListeners(): array;   // lower-case substrings; matching against console output is done by Wings
public function getId(): string;                                  // value listed in egg.features
public function authorize(User $user, Server $server): bool;
public function getAction(): Filament\Actions\Action;              // modal mounted on the console page (name = id)
```
Service: `register`, `get`, `getAll`, `getActiveSchemas(?array $features)` (only ids in the egg's `features`),
`getMappings($features)` → `{id: listeners[]}`. Flow: `ServerConfigurationStructureService` sends
`egg.features = getMappings(egg.features)` to Wings; Wings emits websocket event `feature match` with `{key: <id>}`;
the console JS dispatches Livewire `mount-feature`; `Console::mountFeature()` checks `authorize()` and mounts the
cached action. Built-ins (`id`: listeners → action):
- `eula`: "you need to agree to the eula in order to run the server" → write `eula.txt` = `eula=true`, restart
  (needs `file.update`).
- `java_version`: UnsupportedClassVersionError / "unsupported major.minor version" / "compiled by a more recent version…"
  / "minecraft 1.17|1.18|1.19 requires … java 16|17" → select another egg docker image, save, log
  `server:startup.image`, restart (needs `startup.docker-image`).
- `gsl_token`: "(gsl token expired)", "(account not found)" → edit server variable `STEAM_ACC` with its egg rules, restart
  (needs `startup.update`).
- `pid_limit`: pthread/thread-creation errors → informational modal (admin vs user text).
- `steam_disk_space`: "steamcmd needs 250mb of free disk space to update", "0x202 after update job" → informational modal.

#### 9.4.6 Schedule task types — `App\Extensions\Tasks\TaskSchemaInterface`
```php
public function getId(): string;                         // stored in tasks.action
public function getName(): string;
public function runTask(Task $task): void;               // executed by RunTaskJob
public function canCreate(Schedule $schedule): bool;     // disables the option in the task form
public function getDefaultPayload(): ?string;
public function getPayloadLabel(): ?string;
/** @return null|string|string[] */ public function formatPayload(string $payload): null|string|array;  // table display
/** @return Component[] */ public function getPayloadForm(): array;   // fields bound to "payload"
```
Base `Schemas\TaskSchema`: name = `trans('server/schedule.tasks.actions.<id>.title')`, `canCreate` true, payload
form = autosizing textarea, `formatPayload` splits lines. Service: `register`, `get`, `getAll`, `getMappings`.
`RunTaskJob` looks up the schema by `task.action` (unknown → `InvalidArgumentException`); a `ConnectionException`
is swallowed when `continue_on_failure`. Built-ins: `power` (payload start/restart/stop/kill), `command`
(send console command), `backup` (payload = ignored files; only creatable when `backup_limit > 0`),
`delete_files` (payload = newline-separated paths).

#### 9.4.7 Webhook types — `App\Extensions\Webhooks\Schemas\WebhookSchemaInterface`
```php
public function getId(): string;  public function getLabel(): string;
public function getColor(): ?string;  public function getIcon(): string|BackedEnum|null;
/** @return Component[] */ public function getFormComponents(WebhookScope $scope): array;  // payload section
public function getPreviewComponent(): ?string;     // Livewire component name (extend App\Extensions\Webhooks\WebhookPreview)
/** @return string[] */ public function getPreviewFields(): array;
public function matchesEndpoint(string $endpoint): bool;   // auto-select type from URL
public function getPayloadRules(): array;                  // rules relative to "payload"
public function mutateFormDataBeforeSave(array $data): array;
public function mutateFormDataBeforeFill(array $data): array;
public function preparePayload(WebhookConfiguration $c, array $eventData): array;
public function prepareHeaders(WebhookConfiguration $c, array $payload, array $eventData): array;
public function deliver(WebhookConfiguration $c, array $payload, array $headers): Illuminate\Http\Client\Response;
public function wasSuccessful(Response $response): bool;
public function retryAfter(Response $response): ?int;      // seconds, null = no retry (job $tries = 3)
```
Base `Schemas\BaseSchema` (JSON POST with `panel.webhook.timeout` seconds, success = 2xx). Register via facade
`App\Facades\WebhookTypes::register($schema)` (→ `WebhookTypeService`, default type id `regular`). Built-in
`regular` (custom headers KeyValue, default `X-Webhook-Event: {{event}}`, `{{var}}` substitution via
`WebhookConfiguration::replaceVars`). `FallbackSchema` delivers rows whose type is no longer registered (e.g. a
removed plugin) using the stored payload template. Detection: `detect($endpoint)` / `detectFor(...)` never
overrides a deliberate or plugin-chosen type. (A Discord webhook type is not in core; it is expected to come from a
plugin.)

#### 9.4.8 Webhook event lists (event hooks with by-reference arrays)
- `Event::listen('server:webhook.events', fn (array &$events) => $events[] = 'server:myplugin.thing')` — extends the
  server-scope event checklist (`WebhookConfiguration::allPossibleServerEvents()`, base list of `server:*`
  activity events).
- `Event::listen('global:webhook.events', fn (array &$events) => ...)` — extends the global/admin checklist
  (`allPossibleAdminOnlyEvents()`; base = `eloquent.{created,updated,deleted}: App\Models\*` filtered by model
  name, plus `App\Events\*` classes).
Plugin models are not auto-discovered (discovery scans `app/Models` and `app/Events` only), so a plugin must add
its own event names here.

### 9.5 Events a plugin can listen to

Standard `Event::listen()` from a provider. Panel events (`app/Events`):

| Event | Payload | Fired by |
|---|---|---|
| `App\Events\ActivityLogged` | `ActivityLog $model` (+ helpers `is()`, `actor()`, `isServerEvent()`, `isSystem()`) | every activity log write (`ActivityLogService`, Wings activity ingestion) |
| `App\Events\Auth\FailedCaptcha` | `string $ip, ?string $message` | captcha failure |
| `App\Events\Auth\ProvidedAuthenticationToken` | `User $user, bool $recovery` | 2FA token provided |
| `App\Events\Server\BackupCompleted` | `Backup $backup` | Wings backup status callback |
| `App\Events\Server\Installed` | `Server $server, bool $successful, bool $initialInstall` | Wings install callback |
| `App\Events\Server\SubUserAdded` | `Subuser $subuser` | subuser creation |
| `App\Events\Server\SubUserRemoved` | `Server $server, User $user` | subuser deletion |
| `App\Events\User\Deleting` | `User $user` | user deletion |
| `App\Events\User\PasswordChanged` | `User $user` | password update |

Also: Eloquent model events (`eloquent.created|updated|deleted: App\Models\X`, all wired to `DispatchWebhooks`
in `EventServiceProvider`), Laravel auth events (`Login`, `Failed`, `PasswordReset` — used by core listeners),
`SocialiteWasCalled`, the two webhook-list events above. Core listeners live in `app/Listeners/**`
(auto-discovered).

### 9.6 Permissions

- **Admin role permissions** (spatie/permission, strings `"<prefix> <model>"`, guard `web`):
  - `Role::registerCustomPermissions(array<string model, string[] prefixes>)` — adds checkboxes to the role form.
  - `Role::registerCustomDefaultPermissions(string $model)` — adds the 5 default prefixes
    `viewList, view, create, update, delete` for a model.
  - `Role::registerCustomModelIcon(string $model, string|BackedEnum $icon)`.
  - Built-in lists: `RolePermissionModels` × prefixes (apiKey, allocation, backupHost, databaseHost, database, egg,
    mount, node, role, server, user, webhook) + `egg: import, export` + special `settings: view, update`,
    `health: view`, `activityLog: seeIps`, `panelLog: view`, `plugin: viewList, create, update, delete`.
  - Labels: `admin/role.permissions_list.<prefix>` and `admin/role.models.<model>` if translated, else headline.
  - Permission rows are created on demand (`Permission::firstOrCreate`) when a role is saved.
  - Policies: use the `DefaultAdminPolicies` trait (`viewAny`→`viewList <m>`, `view`, `create`, `update`, `delete`,
    `deleteAny`, `replicate`→`create`). Root Admin passes everything via `Gate::before`.
- **Subuser (per-server) permissions**: `Subuser::registerCustomPermissions(string $name, string[] $permissions,
  ?string $translationPrefix = null, null|string|BackedEnum $icon = null, ?bool $hidden = null)` — adds/extends a
  permission group shown in the subuser form; keys become `"<name>.<permission>"`. `ServerPolicy::before()` grants a
  registered key to the owner and to subusers holding it, so plugins check with `$user->can('<name>.<perm>', $server)`.
  Labels default to the `server/user.permissions` translation prefix.
- **Application API key resources**: `ApiKey::registerCustomResourceName(string $resource)` adds a row to the API key
  permission form (none/read/read-write). A plugin API request extends `ApplicationApiRequest` with
  `protected ?string $resource = '<resource>'; protected int $permission = AdminAcl::READ|WRITE;`.
- **Client API**: request classes implementing `App\Contracts\Http\ClientPermissionsRequest`
  (`public function permission(): SubuserPermission|string;`) are authorised by `ClientApiRequest::authorize()`
  against the `{server}` route parameter.

### 9.7 Custom routes / API

No automatic route loading for plugins; a provider registers routes itself, e.g.
`Route::middleware(['api', 'client-api', 'throttle:api.client'])->prefix('/api/client/...')->group(...)`.
Available middleware groups (`bootstrap/app.php`): `web`, `api` (sanctum + JSON + key tracking + IP allow-list),
`application-api` (`AuthenticateApplicationUser`), `client-api` (`RequireClientApiKey`), `daemon`
(`DaemonAuthenticate`, Wings → panel), aliases `guest`, `node.maintenance`. Plugin controllers/requests can reuse
`ApplicationApiController`, `ApplicationApiRequest`, `ClientApiRequest`. Docker deliberately does not cache routes
because plugins add them at runtime.

### 9.8 Files: special-file banners

`App\Models\File::registerSpecialFile(string $fileName, string|Closure $bannerTitle, string|Closure|null $bannerBody = null, ?Closure $nameCheck = null)`
— shows an info banner in the file editor when the edited path matches (default check: path ends with `$fileName`).
Built-in: `.pelicanignore`.

### 9.9 Translations

- Normal plugins: `lang/<locale>/<group>.php` → `trans('<plugin-id>::<group>.<key>')`.
- Language packs (`category: language`): `lang/` added as an extra global path, so its groups merge into/override
  core groups (`profile.php`, `admin/*.php`, …); every directory under the plugin's `lang/` becomes a selectable
  language in `LanguageService::getAvailableLanguages()` (shown with its `profile.current_language_name` translation
  or the ICU display name). Only `en` is flagged as completely translated.
- Core activity descriptions are looked up as `activity.<event with ':' → '.'>` in the global namespace, so plugin
  activity events have no translation unless a language-pack path provides it.

### 9.10 Views, assets, themes

- Views: `view('<plugin-id>::path')`, Blade components, Livewire components (register via `Livewire::component` in
  a provider).
- Assets: `vite.config.js` inputs include `plugins/*/resources/css/**/*.css` and `plugins/*/resources/js/**/*.js`;
  they are compiled by `yarn build` during install/uninstall. Plugins reference them with `@vite`/`viteTheme`.
- Themes: category `theme`; install fails if the asset build fails; only one theme is expected to be enabled
  (install leaves a second theme disabled; enabling a second one asks for confirmation).
- Core global look: Filament colours (danger=Red, gray=Zinc, info=Sky, primary=Blue, success=Green,
  warning=Amber) set in `FilamentServiceProvider`; a theme overrides via panel colours/CSS.

### 9.11 Database

Migrations in `database/migrations` (run on install, reset on uninstall, and included in `php artisan migrate`
while the plugin is loaded); seeder `Database\Seeders\<Studly(name)>Seeder`; factories under `Database\Factories\`.
Plugin models should add themselves to webhook events manually (§9.4.8). Note `Relation::enforceMorphMap` is
enforced for core models; plugin polymorphic relations must add map entries.

### 9.12 Artisan commands, config, scheduling, misc

- Commands: any `Illuminate\Console\Command` under `src/Console/Commands/**` is registered automatically.
- Config: `config/<id>.php` → `config('<id>.*')`.
- Anything else Laravel offers from a service provider: scheduled tasks, gates, policies, macros, health checks
  (`Spatie\Health\Facades\Health::checks([...])` — core registers its checks in `AppServiceProvider::boot()`),
  activity logging (`App\Facades\Activity::event('...')->property(...)->log()`), alert banners
  (`App\Livewire\AlertBanner::make(id)->title()->body()->warning()->send()`), DB notifications.

### 9.13 Not plugin hooks (internal extensions with similar names)

`app/Extensions/Dedoc/Scramble/PanelResponseTypeInfer.php` (OpenAPI type inference for the API docs),
`app/Extensions/Laravel/Sanctum/NewAccessToken.php`, `app/Extensions/Lcobucci/JWT/Encoding/TimestampDates.php` are
framework adapters, not registries.

---

## 10. Porting notes for the Rust rewrite

- A Rust panel cannot load PHP plugins. What must be preserved for parity is the **manifest format** (§3), the
  **status machine** (§4.1), the **lifecycle semantics** (§6: safe import, checksum'd updates, meta carry-over,
  load order), the **application API** (§7.3, including the Hub enrollment endpoint and `X-Panel-Api-Key` rules),
  and an equivalent set of **extension registries** (§9.4) and **permission registries** (§9.6). The concrete
  plugin ABI (WASM, out-of-process, scripting) is a design decision for `docs/decisions/`.
- Operations that today shell out (composer, yarn) and per-request manifest writes (incompatible status) are
  PHP-runtime artefacts; a Rust design should treat them as optional.

---

## Open questions

1. `PluginService::updateLoadOrder()` calls `Plugin::firstOrFail($id)`; Eloquent's `firstOrFail` takes a column
   list, not a key, so this looks like a bug (intended `findOrFail`). Does drag-reordering actually work in
   production? (With SQLite's double-quoted-literal quirk it would return a row without an `id`.)
2. `Plugin.php` imports `App\Facades\Plugins`, which does not exist in `app/Facades` — dead import or a removed facade?
3. `api_version` is read but not exposed via the API (`PluginData`) — intentional?
4. The update-feed `sha256` check is skipped for `POST /plugins/import/url` (no checksum parameter); acceptable?
5. `loadPlugins()` rewrites `plugin.json` for incompatible plugins on every boot (every request); is this relied upon,
   or would a "write only on change" behaviour be acceptable?
6. Plugin settings have no core persistence — what do real plugins use (own config, env, DB)? Should the Rust
   design provide a key/value store?
7. Errored plugins are retried on every boot and re-enabled on success; is there any back-off expected?
8. Is there an official Hub API contract (feed URLs, download auth, enrollment) beyond what the panel code implies?
9. `MakePluginCommand` writes `meta.status: enabled` so scaffolded plugins never run migrations/seeders unless
   reinstalled — intended developer workflow?
10. How should plugin translations of activity events and webhook event labels be handled (core looks them up only
    in the global namespace)?
11. Which Filament APIs do real-world plugins actually use (resource discovery paths, render hooks, `viteTheme`)?
    That set defines the minimum hook surface the Rust UI needs to emulate.
