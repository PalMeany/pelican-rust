# Web UI: functional inventory (original Pelican panel)

Status: reference spec, derived from the PHP source in `panel/` (PHP 8.5, Laravel 13, Filament 5 / Livewire).
All paths are relative to `panel/`. Framework behaviour was checked against `.ref/vendor` (exact versions the panel
uses) and is marked *(Filament default)* / **[Filament default]** where Pelican does not override it.

This document describes **what users can do** in the web UI: URLs, access rules, navigation, list/table behaviour,
forms (fields, validation, conditional visibility), actions and their side effects (service class, DB write, Wings
call), widgets, and the special pages (console, file manager, settings, auth, profile, installer, health, API docs,
translations). It deliberately does not describe visual styling. The plugin system and its UI hooks are specified
separately in `plugins.md`; the REST APIs and Wings protocol are specified in their own documents.

## 0. Orientation

### 0.1 Source map

| Area | Sources |
|---|---|
| Panel providers (one per panel + shared base + global defaults) | `app/Providers/Filament/{PanelProvider,AdminPanelProvider,AppPanelProvider,ServerPanelProvider,FilamentServiceProvider}.php` |
| Admin panel | `app/Filament/Admin/{Pages,Resources,Widgets}/**` |
| App panel (server list) | `app/Filament/App/Resources/Servers/**` |
| Server panel | `app/Filament/Server/{Pages,Resources,Widgets,Components}/**` |
| Auth pages (login, profile) | `app/Filament/Pages/Auth/{Login,EditProfile}.php` |
| Shared Filament components | `app/Filament/Components/**` |
| Livewire components | `app/Livewire/**` (alert banners, file upload manager, installer, passkeys, server entry card, node info) |
| Blade views | `resources/views/**` (console, file manager, editor, installer, health, settings, errors, passkeys, API docs) |
| Front-end JS/CSS | `resources/js/{app,console,file-uploader}.js`, `resources/css/{app,console,monaco-editor}.css` (built by Vite) |
| Web routes outside Filament | `routes/base.php` (installer), `routes/auth.php` (OAuth), `routes/docs.php` (API docs) |
| Translations | `lang/<locale>/**` |
| Extension registries used by the UI | `app/Extensions/**` (see `plugins.md` §9.4) |

### 0.2 The three panels

| Panel id | URL prefix | Who can enter | Navigation | Purpose |
|---|---|---|---|---|
| `app` (default) | `/` | any authenticated user | none (topbar only) | Server list, profile (`/profile`), login (`/login`) |
| `admin` | `/admin` | Root Admin, or any user with ≥1 role that grants ≥1 permission (`User::canAccessPanel` → `isAdmin()`); each page/resource then checks its own permission | sidebar / topbar / mixed per user; groups "Server", "User", "Advanced" | Panel administration |
| `server` | `/server/{uuid_short}` (tenant = `Server`) | owner, subuser, or admin with `view server` on an accessible node (`User::canAccessTenant`) | per-user layout; items gated by subuser permissions | Per-server management (console, files, backups, …) |

All panels share: Filament login with username-or-email, password reset, MFA (TOTP with recovery codes + email
code), optional captcha, OAuth buttons, passkeys (HTTPS only), database notifications (bell), alert banners,
SPA navigation (except the server console page), per-user locale and timezone, the `RequireTwoFactorAuthentication`
policy and `RedirectIfNotInstalled` (→ `/installer`). Registration does not exist.

### 0.3 How to read this document

| Section | Content |
|---|---|
| Cross-cutting, App panel, Auth, Profile | Shared panel configuration, middleware, alert banners, error pages, server list, login/captcha/password reset/MFA/passkeys/OAuth/logout, profile tabs and customization keys |
| Admin panel (part A) | Admin shell, Dashboard, Health, Panel Logs, **Settings (all tabs and env keys)**, Users, Roles + admin permission model, API Keys, Webhooks, Plugins, Mounts, Database Hosts, Backup Hosts |
| Admin panel (part B) | Nodes, Servers, Eggs, shared components (actions, form fields, columns, filters), Wings endpoints used |
| Server panel | Tenancy and subuser permission model, state gating, navigation, **Console (xterm + Wings websocket protocol + feature modals)**, **Files (list/upload/editor/download/search)**, Backups, Databases, Schedules + tasks, Users (subusers), Network, Activity, Settings, Startup, Mounts, Webhooks, Wings endpoints used |
| Installer, API docs, routes, translations | Web installer steps, API docs pages, other routes, `lang/` layout and locales, notifications |
| Open questions | Collected from all sections |

## Cross-cutting, App panel, Auth, Profile

Notation: "(Filament default)" means the behaviour comes from Filament 5 / Laravel 13 framework code, not from Pelican code. These were checked against the vendor sources in `.ref/vendor` where possible. All paths are relative to `panel/`.

### 1. Cross-cutting panel setup

#### 1.1 Panels overview
There are three Filament panels. All of them extend the shared base `app/Providers/Filament/PanelProvider.php`. Each panel therefore gets its own login, password reset, logout and profile routes (Filament default per-panel routing).

| Panel id | Path | Provider | Notes |
|---|---|---|---|
| `app` | `/` (default panel) | `app/Providers/Filament/AppPanelProvider.php` | Server list only. No navigation, no breadcrumbs, topbar always on. |
| `admin` | `/admin` | `AdminPanelProvider.php` | `homeUrl('/')`. Sidebar is collapsible on desktop when top navigation is off. User menu has "Exit Admin", which goes to the app panel. |
| `server` | `/server/{uuid_short}` (tenant = `Server`) | `ServerPanelProvider.php` | `homeUrl` = app panel. User menu has "Server List" and "Admin" (the latter only if the user can access the admin panel). |

#### 1.2 Shared panel configuration (`PanelProvider::panel`)
| Feature | Behaviour |
|---|---|
| SPA mode | Enabled (`->spa()`) on every route **except** the server console page (`filament.server.pages.console`), which always does a full page load. `spaUrlExceptions(['*/oauth/redirect/*'])` makes OAuth redirect links do a real browser navigation. |
| Database notifications | `->databaseNotifications()` adds the bell icon and slide-over in the topbar. The list polls every 30s (Filament default). Notifications live in the Laravel `notifications` table. They are sent with `Notification::...->sendToDatabase($user)`, see §8.4. |
| Branding | Brand name = `config('app.name')` (`APP_NAME`, default "Pelican"). Logo = `APP_LOGO` (optional), shown 2rem tall. Favicon = `APP_FAVICON` (default `/pelican.ico`). |
| Max content width | `FILAMENT_WIDTH` (default `screen-2xl`). |
| Login | Custom page `App\Filament\Pages\Auth\Login` (§3.1). |
| Password reset | `->passwordReset()` (Filament default flow, §3.3). |
| Registration | **Not enabled anywhere.** No `->registration()` call exists in `app/` or `plugins/`. |
| Email verification / email-change verification | Not enabled. |
| MFA | `->multiFactorAuthentication([AppAuthentication::make()->recoverable(), EmailAuthentication::make()])`, i.e. TOTP with recovery codes, plus email codes. MFA is optional at the Filament level. The "required" policy is enforced by the custom middleware described in §3.4. |
| Profile | `->profile(EditProfile::class, isSimple: false)`, so the profile renders inside the panel layout. The "profile" user-menu item always links to the **app** panel profile (`/profile`), even from `/admin` or `/server`. |
| Top navigation | Driven by the user's `top_navigation` customization. `topNavigation()` is true when the value is `'topbar'` (or the legacy boolean `true`). `topbar()` is true when the value is `'topbar'`, `'mixed'` or `true`. So **sidebar** means sidebar only with no topbar, **topbar** means horizontal nav in the topbar with no sidebar, and **mixed** means sidebar plus topbar. The app panel overrides this with `navigation(false)->topbar(true)`. Some admin resources change their navigation group when the value is `'topbar'` (`NodeResource`, `UserResource`, `EggResource`, `ServerResource`, `RoleResource`). |
| Theme switcher | Light / dark / system switcher in the user menu (Filament default). The choice is stored in browser `localStorage.theme`. |

**Web middleware stack for panel routes** (order as declared): `EncryptCookies`, `AddQueuedCookiesToResponse`, `StartSession`, `AuthenticateSession` (logs out other sessions after a password change), `ShareErrorsFromSession`, `App\Http\Middleware\PreventRequestForgery` (CSRF, excludes `remote/*` and `daemon/*`), `SubstituteBindings`, `DisableBladeIconComponents`, `DispatchServingFilamentEvent`, `LanguageMiddleware`, `SetSecurityHeaders`, `RedirectIfNotInstalled`.
**Auth middleware** (authenticated panel routes only): Filament `Authenticate` (redirects to login), then `RequireTwoFactorAuthentication`.

`bootstrap/app.php` also adds `LanguageMiddleware` and `SetSecurityHeaders` to the global `web` group. It sets the guest redirect to `route('filament.app.auth.login')` (`/login`) and aliases `guest` to `RedirectIfAuthenticated`, which sends logged-in users to `/`.

#### 1.3 Middleware details
| Middleware | Effect |
|---|---|
| `LanguageMiddleware` | `app()->setLocale($request->user()->language ?? config('app.locale','en'))`. |
| `RedirectIfNotInstalled` | If `config('app.installed')` (`APP_INSTALLED`, **default `true`**) is false, every panel request except the `installer` route is redirected to `/installer`. |
| `SetSecurityHeaders` | Adds these headers when not already set: `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `X-XSS-Protection: 1; mode=block`, `Referrer-Policy: no-referrer-when-downgrade`. No CSP. |
| `PreventRequestForgery` | Laravel CSRF check. URIs `remote/*` and `daemon/*` are exempt. |
| `RequireTwoFactorAuthentication` | See §3.4. |
| `MaintenanceMiddleware` (alias `node.maintenance`) | Would render `errors.maintenance` for servers whose node is in maintenance. **The alias is not used on any route, and the view `errors/maintenance.blade.php` does not exist.** |
| `AddPasskeyErrorMessage`, `AllowPasskeyOrigin` | Applied only to passkey routes (§3.5). |

#### 1.4 Render hooks and global UI defaults (`app/Providers/Filament/FilamentServiceProvider.php`)
- **Color palette:** danger=Red, gray=Zinc, info=Sky, primary=Blue, success=Green, warning=Amber.
- **Alert banners:** the `PAGE_START` hook renders `App\Livewire\AlertBannerContainer` (§1.5) on every panel page.
- **Footer:** the `FOOTER` hook renders `resources/views/filament/layouts/footer.blade.php`:
  - It shows "© {current year} {appName}", linking to `https://pelican.dev/docs/#core-team`.
  - `appName` is `APP_NAME`. If that name is not "pelican" (case-insensitive), it shows "{APP_NAME} - Powered by Pelican" instead.
  - When `APP_DEBUG` is on, it also shows the request render time in seconds.
  - Plugin hook points `pelican::footer.start` and `pelican::footer.end` come from `app/Enums/CustomRenderHooks.php`.
- **Assets:** `STYLES_BEFORE` loads `resources/css/app.css`. `SCRIPTS_AFTER` loads `resources/js/app.js`, which provides a clipboard polyfill and the passkey JS glue `window.registerPasskey` / `window.authenticateWithPasskey`.
- **Livewire hook:** before dehydrate, if this is a Livewire request with no redirect pending and the session has `alert-banners`, the component dispatches the browser event `alertBannerSent`. This lets banners created during AJAX actions appear without a reload.
- **Global component defaults:**
  - `Select` and `SelectColumn` are non-native (searchable JS dropdown).
  - KeyValue, Repeater, show/hide-password and copy actions get tooltips and large icons.
  - The `Field::hintCopy()` macro adds a copy-to-clipboard hint action.
- **Action button style** is driven by the per-user `button_style` customization (§4.8):
  - `Create`, `Edit`, `View`, `Delete`, `Associate` and `Dissociate` actions are always icon-only with a tooltip (Plus / Pencil / Eye / Trash / Link / Unlink).
  - If `button_style` is truthy (the default), they are rendered as bare `iconButton()` with an XL icon.
  - **Every other `Action`** also becomes an icon button when `button_style` is truthy. Exceptions are names prefixed `enable_oauth_`, `disable_oauth_`, `enable_captcha_`, `disable_captcha_`, `oauth_`, `db_`, `fm_`, `hint_`, `exclude_`, and the names `profile`, `logout`, `start`, `stop`, `restart`, `kill`, `fileUpload`.
  - When `button_style` is false, actions render as normal labelled buttons.
- **Icons:** Filament icon aliases are remapped to Tabler icons.
- **Passkeys component:** the Livewire alias `filament-passkeys` is registered to `App\Livewire\Passkeys`.

#### 1.5 Alert banners (`app/Livewire/AlertBanner*.php`, `resources/views/livewire/alerts/*`)
- `AlertBanner::make(id)` takes `title`, `body`, `status` (info/success/warning/danger; defaults to info), an `icon`, `closable(bool)` and Filament `actions`.
- `->send()` pushes the banner into session key `alert-banners`. Banners sent during a Livewire request are flagged `from_livewire`.
- `AlertBannerContainer` behaviour:
  - On mount it pulls the session banners and renders them. Banners flagged `from_livewire` are dropped on mount because they belong to the previous page.
  - On the `alertBannerSent` event it pulls again.
  - On `removeAlertBanner(id)`, or the close button (only shown when closable), it removes the banner.
  - Banners are keyed by id, so a second banner with the same id replaces the first.
- Rendering: a Filament callout per banner. Title and body are HTML-sanitized but may contain safe HTML. Actions render in the footer.
- Known banner ids in the code base: `2fa_must_be_enabled`, `files_node_error`, `plugin_safe_mode`, `webhook_help`, `file_too_large`, `file_not_found`, `file_is_directory`, `{file}_info`, `file_already_exists`, `folder_already_exists`, `server_conflict`, `websocket_error`.

#### 1.6 Layout and error pages
- `resources/views/components/layouts/app.blade.php` is a bare HTML shell (`<html lang=locale>`, `$title`, `$slot`). It is used by full-page Livewire components outside Filament.
- `resources/views/errors/layout.blade.php` renders inside Filament's "simple" layout as heading `"{code} | {title}"` with an icon and a subtitle:

| Code | Title | Subtitle | Icon |
|---|---|---|---|
| 400 | Bad request | exception message | exclamation-circle |
| 401 | Unauthorized | "You are not authorized to access this resource." | exclamation-circle |
| 403 | Forbidden | exception message | exclamation-circle |
| 404 | Not found | exception message, or "The requested resource was not found." | zoom-question |
| 418 | I am a teapot | "What happened?" | teapot |
| 500 | Server Error | exception message **only for root admins**, otherwise "Something went wrong." | bug |
| 503 | Maintenance | "This site is currently down for maintenance." | traffic-cone |

- Other codes such as 419 or 429 use Laravel's default views.
- The error strings are hard-coded English (not translated).
- If a plugin crashes, `app/Exceptions/Handler.php` renders `errors::minimal` with 500 and `trans('admin/plugin.crashed_reload')`, after auto-disabling the plugin.

### 2. App panel (default panel `/`): server list

#### 2.1 Resource and page
- `app/Filament/App/Resources/Servers/ServerResource.php`:
  - Model `Server`, slug `/`, so the list is the panel home page. Single page `ListServers`.
  - `canAccess()` always returns true. Navigation registration is off.
  - `embedServerList(bool)` is a static switch that plugins can use to move the list off `/` and into navigation. When used, the navigation badge shows the count of servers the user directly owns.
- `app/Filament/App/Resources/Servers/Pages/ListServers.php`:
  - Header actions and widgets are plugin-extensible via the `CanCustomizeHeaderActions` / `CanCustomizeHeaderWidgets` traits. There are none by default.
- The app panel also registers `FilamentLogViewerPlugin` with `authorize(false)`, i.e. it is never accessible there. Plugins can also add pages to this panel via `PluginService::loadPanelPlugins`.

#### 2.2 Which servers a user sees
`$baseQuery = user()->accessibleServers()` (`app/Models/User.php`):
- If the user passes `viewAny Server`, they see the union (`DISTINCT servers.id`) of servers they own, servers they are a subuser of, and **all servers on nodes they can access**. `viewAny` passes for root admins via `Gate::before`, and for role-based admins via `ServerPolicy` / `DefaultAdminPolicies::viewAny`, which requires role permission `viewList server`.
- Accessible nodes:
  - Root admin: all nodes.
  - Otherwise: if none of the user's roles has node restrictions (`node_role` rows), all nodes. If any does, only nodes linked to the user's roles.
- Everyone else sees owned servers plus servers where they are a subuser (`directAccessibleServers()`).
- There is no separate "show all servers" toggle. Admins see the extra servers through the tabs below.

**Tabs** (`getTabs`). Each tab shows a count badge. The default active tab is the first one, `my` (Filament default). Switching tabs resets the table (`updatedActiveTab`).

| Tab key | Label (`server/dashboard.tabs.*`) | Query |
|---|---|---|
| `my` | My Servers | accessible AND `owner_id = me` |
| `other` | Others' Servers | accessible AND `owner_id != me` |
| `all` | All Servers | all accessible |

Empty state: docker icon. The heading is "You don't own any servers!" on the `my` tab and "You don't have access to any servers!" otherwise.

#### 2.3 Layouts (`dashboard_layout` customization: `grid` default | `table`)
| Aspect | Grid (`grid`) | Table (`table`) |
|---|---|---|
| Columns | One `Stack` containing `ServerEntryColumn('server_entry')`, a card that embeds the lazy `ServerEntry` Livewire component | Icon, Status, Name, Address, CPU, Memory, Disk (see below) |
| Content grid | 1 column, 2 from `md` | normal table rows |
| Search | by `name` (column `searchable(['name'])`) | `name` is searchable and **sortable** |
| Row click | Card click goes to server console: `Livewire.navigate` in SPA mode. Middle-click (`auxclick` button 1) opens a new tab. | `recordUrl` goes to `Console::getUrl(panel:'server', tenant:$server)` |
| Power actions | Action group inside the card (click does not propagate to the card) | Row action group, centred |
| Polling | Each card polls on its own: `wire:poll.15s` on the `ServerEntry` component | Whole table `->poll('15s')` |
| Page size options | 10, 20, 30, 40 (default 10) | 10, 20, 50, 100 (default 20) |

- **Pagination persistence:** the per-page choice is saved in the user's `customization.servers_per_page`, but only when the value is one of the currently offered options (`updatedTableRecordsPerPage`). The stored value is used as the default when it is valid for the current layout. The session key for per-page is suffixed `_grid` / `_list` so the two layouts don't overwrite each other.
- **Filters** (persisted in session):
  - **Egg:** select over the eggs used by accessible servers. Searchable, preloaded.
  - **Owner:** select by username over the owners of accessible servers. Searchable, preloaded. Hidden on the `my` tab.
- **No tags, no grouping, no bulk actions** exist on this list.

**Table-mode columns:**
| Column | Content |
|---|---|
| `icon` | `server.icon`, falling back to `egg.icon`, 46px. No label. |
| `condition` (Status) | Badge with the condition label, icon and colour. Tooltip shows uptime (`formatResource(Uptime)`). |
| `name` (Title) | Name, with the description as subtext. Grows to fill space. |
| `allocation.display_address` | Badge, copyable, visible from `md`. Shows "None" if there is no allocation. |
| `cpuUsage` | Progress bar: `cpu_absolute` / limit. When the CPU limit is 0, the max is `node.systemInformation().cpu_count * 100`. Label "x% / y%" (∞ for unlimited). |
| `memoryUsage` | Progress bar: `memory_bytes` / limit in bytes. When the limit is 0, the max is `node.statistics().memory_total`. |
| `diskUsage` | Progress bar: `disk_bytes` / limit in bytes. When the limit is 0, the max is `node.statistics().disk_total`. |

**Grid card** (`resources/views/livewire/server-entry.blade.php`):
- Coloured status stripe on the left.
- Faded egg or server icon as a background image.
- Status icon button with a label tooltip.
- **Name (uptime)**, the power action group, and the description truncated to 40 chars.
- Three progress bars (CPU, Memory, Disk) using the same maxima as table mode.
- "Network" with the primary allocation address, hidden on small screens.
- The lazy placeholder (`server-entry-placeholder.blade.php`) shows the name with "(Loading...)", empty bars with limit labels, and the address.

**Progress bars** (`app/Filament/Components/Tables/Columns/ProgressBarColumn.php`, view `livewire/columns/progress-bar-column.blade.php`):
- Percentage is clamped to 0–100.
- Status is `danger` at ≥90% (`DANGER_THRESHOLD = 0.9`), `warning` at ≥70% (`WARNING_THRESHOLD = 0.7`), otherwise `success`.
- Bar colours: primary for success, then warning, then danger.
- In danger state the label is bold, pulses, and is exposed as `role=status aria-live=assertive`.
- The bar exposes `role=progressbar` with aria values.

#### 2.4 How stats are fetched (Wings calls and caching)
| Data | Source | Cache |
|---|---|---|
| Status (`Server::condition`) | If `servers.status` is not null (installing, install_failed, reinstall_failed, suspended, restoring_backup), that value is used. Otherwise `retrieveStatus()` calls Wings `GET /api/servers/{uuid}` (1s connect and request timeout) and maps `state` to `ContainerStatus`. Unknown or unreachable maps to `Missing`. Node in maintenance maps to `Missing` without calling Wings. | `servers.{uuid}.status`, 15s |
| Live usage (`retrieveResources()`) | Same Wings `GET /api/servers/{uuid}`, field `utilization` (`cpu_absolute`, `memory_bytes`, `disk_bytes`, `uptime` ms, …). Returns `[]` if the node is in maintenance. | `servers.{uuid}.resources`, 15s |
| Node totals for unlimited servers (`Node::statistics()`) | Wings `GET /api/system/utilization` (1s timeout). Defaults to zeros on failure. | `cache()->flexible("nodes.{id}.statistics", [5, 30])` |
| Node CPU count (`Node::systemInformation()`) | Wings `GET /api/system` (3s connect). Must contain architecture, cpu_count, kernel_version, os, version. | `flexible("nodes.{id}.system_information", [30, 360])` |

- Wings HTTP client (`Http::daemon` macro in `app/Providers/AppServiceProvider.php`): Bearer `node.daemon_token`, JSON, TLS verification only in production, timeouts from `GUZZLE_TIMEOUT` / `GUZZLE_CONNECT_TIMEOUT` (15s / 5s) unless overridden.
- A Cloudflare 502 (with a `Cf-Ray` header and no cache status) triggers a "cloudflare issue" toast.
- **Uptime display:** if `servers.status` is set, its label is shown. Otherwise "Offline" when uptime is 0, otherwise a short absolute diff (e.g. "2d 3h 4m 5s").
- **Byte formatting:** limits are entered in MiB and converted with 1024² when `PANEL_USE_BINARY_PREFIX` is true (default), otherwise with 1000². A limit of 0 is shown as ∞.

#### 2.5 Power actions from the list
`ListServers::getPowerActionGroup()` is a Power icon group, hidden when `server.isInConflictState()`. Conflict means suspended, node under maintenance, not installed, restoring backup, or transfer in progress.

| Action | Permission | Visible when status… |
|---|---|---|
| start | `control.start` | `isStartable()`: not running/starting/stopping/restarting/missing |
| restart | `control.restart` | `isRestartable()`: startable, or not offline/missing |
| stop | `control.stop` | `isStoppable()` and not `isKillable()` |
| kill (with tooltip) | `control.stop` | `isKillable()`: not offline/running/exited/missing |

Flow:
1. Each action dispatches the Livewire event `powerAction(server, action)`. `ListServers::powerAction` handles it.
2. The handler re-checks the permission (owner, admin, or subuser permission) and that the server is not in a conflict state; either failure returns 403.
3. It sends Wings `POST /api/servers/{uuid}/power {"action": …}`.
4. On success: toast "`{action}` sent to `{name}`", clear the status cache, and redirect to the same tab.
5. On `ConnectionException`: danger toast "error connecting to node {node}".

### 3. Authentication

#### 3.1 Login page (`app/Filament/Pages/Auth/Login.php`, extends Filament `Auth\Pages\Login`)
**Routes:** `GET /login` (app), `/admin/login`, `/server/login`. Logging in through `/admin/login` also requires `canAccessPanel(admin)`, i.e. `isAdmin()`.

**Form fields** (when password login is enabled):
| Field | Type | Rules / behaviour |
|---|---|---|
| `login` | text, label "Login" | required, autofocus, autocomplete. Accepts **username or email**: `filter_var(FILTER_VALIDATE_EMAIL)` picks the `email` or `username` column. The value is lowercased (`mb_strtolower`). |
| `password` | password | required. Shows a "Forgot password?" link (Filament default, because password reset is enabled). Reveal toggle (Filament default). |
| `remember` | checkbox | "Remember me" (Filament default). |
| OAuth buttons | action row | One button per **enabled** OAuth provider (§3.6). Label = provider name, with icon (if it resolves) and brand hex colour. Links to `/auth/oauth/redirect/{id}`. |
| Passkey button | full-width gray button "Authenticate using a passkey" | Calls `window.authenticateWithPasskey()`. **Only rendered when the request is HTTPS** (`$request->isSecure()`). |
| Captcha | Turnstile widget | Present only if a captcha provider is enabled. Hidden while an MFA challenge is in progress. |

- **Password login disabled** (`AUTH_DISABLE_PASSWORD_LOGIN=true`, env only, not in the admin settings UI): the form contains only the OAuth buttons (centred) and the passkey button. There is no submit button. Any credential submission errors with `auth.password_login_disabled`.
- **Failure:**
  - The error is attached to `data.login` with the message from `filament-panels::auth/pages/login.messages.failed`.
  - The browser event `reset-captcha` is dispatched so a fresh Turnstile token is needed.
  - Laravel's `Failed` event fires and is logged as activity `auth:fail` (credentials other than password/token/secret are recorded as properties).
- **Rate limits (Filament default):**
  - `authenticate()` allows 5 attempts per 60s per IP and component. The throttled toast reads "Too many login attempts…".
  - The MFA challenge is limited to 5 attempts per user.
  - Credential checks run inside a timebox (`auth.timebox_duration`, default 200ms) to equalise timing.
  - `config/auth.php` `lockout` (time 2, attempts 3) is **not referenced** anywhere.
- **Flow (Filament default):**
  1. Validate the credentials and the panel access check.
  2. If the user has an MFA provider enabled, an encrypted `userUndertakingMultiFactorAuthentication` token is set and the same page swaps to the challenge form. If both providers are enabled, a provider picker is shown. App provider: 6-digit TOTP, or a recovery code via a "use recovery code" toggle. Email provider: a code is emailed on challenge, with a resend action.
  3. Credentials are re-validated, `attemptWhen(..., $remember)` runs, the session is regenerated, and Laravel's `Login` event fires (logged as activity `auth:success`).
- **Post-login redirect** (`app/Http/Responses/LoginResponse.php`): if the user's `redirect_to_admin` customization is true and they can access the admin panel, go to `redirect()->intended('/admin')`. Otherwise `intended('/')`.
- If the session contains `authenticatePasskey::message`, it is shown as a danger toast on mount. Nothing in the app currently writes this key (legacy).

#### 3.2 Captcha (`app/Extensions/Captcha/**`, `app/Providers/Extensions/CaptchaServiceProvider.php`)
- `CaptchaService` is a registry of `CaptchaSchemaInterface` providers. The only built-in provider is **Cloudflare Turnstile** (`turnstile`), and plugins can register more.
- **The first enabled provider** is used, and **only on the login form**.
- On registration, each schema's config is copied into `config('captcha.{id}')`.

| Env key | Meaning |
|---|---|
| `CAPTCHA_TURNSTILE_ENABLED` | Enable (default false) |
| `CAPTCHA_TURNSTILE_SITE_KEY` / `CAPTCHA_TURNSTILE_SECRET_KEY` | Keys (configured in admin Settings, rendered as password inputs) |
| `CAPTCHA_TURNSTILE_VERIFY_DOMAIN` | default true: require the hostname in Turnstile's response to equal the request host |

- Widget: `resources/views/filament/components/turnstile-captcha.blade.php` loads `https://challenges.cloudflare.com/turnstile/v0/api.js`. It sets the field state to the token through a callback, and listens for `reset-captcha`.
- The field is `required` and validated by `Turnstile\Rule`, which POSTs `https://challenges.cloudflare.com/turnstile/v0/siteverify {secret, response}` (15s timeout, 5s connect). Error codes are mapped to messages. Failure message: "Captcha validation failed: {reason}".

#### 3.3 Password reset (Filament default)
| Step | Route | Behaviour |
|---|---|---|
| Request | `GET /password-reset/request` (also under `/admin`, `/server`) | Single email field. Rate limit 2 per 60s. Sends the queued `Filament\Auth\Notifications\ResetPassword` mail with the reset URL, but only if the user can access the current panel; otherwise the mail is silently skipped. On success it shows a toast with `__(status)`. If the broker returns `INVALID_USER` or `THROTTLED`, it shows a **danger toast with the broker message** ("We can't find a user with that email address." / "Please wait before retrying."). That reveals whether the account exists. |
| Reset | `GET /password-reset/reset?email&token&signature` (`signed` middleware) | Fields: email, new password and confirmation, with `Password::default()` rules (no app override, so Laravel's default min 8). Rate limit 2 per 60s. On success the password is rehashed, `remember_token` is rotated, and `PasswordReset` fires, which is logged as activity `auth:password-reset`. Then redirect to login. |

- Broker config (`config/auth.php`): table `password_resets`, tokens expire after 60 min, minimum 60s between token requests.
- The same reset URL (from the app panel) is used as the "Setup Your Account" link in the `AccountCreated` email for users created without a password.

#### 3.4 MFA and the "2FA required" policy
- **Providers (Filament default UI and flows):**
  - **App (TOTP):** setup modal wizard (cannot be closed by click-away or Escape). The user enters their **current password**, scans the QR code or copies the secret, enters a 6-digit code, and is then shown **8 recovery codes** with copy and download (`recovery-codes.txt`) actions.
    - TOTP window is 8 (Filament default; note that the `panel.auth.2fa.window` config is unused by this path).
    - Recovery codes can be regenerated (password plus code).
    - Disabling requires the password plus a code or recovery code.
    - The setup attempts are rate limited at 5.
    - Storage: `users.mfa_app_secret` (encrypted) and `users.mfa_app_recovery_codes` (encrypted array).
    - Issuer = brand name; holder = the user's email.
  - **Email:** setup sends a code (`VerifyEmailAuthentication` notification). The code expires in 4 min and can be resent. Disabling also requires an emailed code. Flag stored in `users.mfa_email_enabled`.
- **Enforcement** (`app/Http/Middleware/RequireTwoFactorAuthentication.php`, auth middleware of every panel):
  - Level = `(int) config('panel.auth.2fa_required')` (`APP_2FA_REQUIRED`). It is set in admin Settings as "2FA Requirement" with options 0 = Not required, 1 = Admins only, 2 = All users.
  - Guests and any route matching `*auth.*` (login, logout, profile, password reset) always pass.
  - "Has 2FA" means `mfa_email_enabled` is true or `mfa_app_secret` is filled. **Passkeys do not count.**
  - Level 0, or the user has 2FA: pass. Level 1 and the user is not an admin (`isAdmin()` = root admin, or has at least one role with at least one permission): pass.
  - Otherwise:
    - JSON requests and paths starting `/api` get `TwoFactorAuthRequiredException`: HTTP 400 "Two-factor authentication is required on this account in order to access this endpoint."
    - Web requests get the warning alert banner `2fa_must_be_enabled` (`auth.2fa_must_be_enabled`) and a redirect to `/profile?tab=2fa::data::tab`.
- The activity events `auth:checkpoint`, `auth:token` and `auth:recovery-token` exist in `lang/en/activity.php`, and `TwoFactorListener` listens for `App\Events\Auth\ProvidedAuthenticationToken`. However, **nothing dispatches that event** (there is a "TODO: add event to filament" comment).

#### 3.5 Passkeys (WebAuthn via `laravel/passkeys`)
**Routes** (registered by the package, middleware `web`, `AddPasskeyErrorMessage`, `AllowPasskeyOrigin`; throttle `6,1` per package default):
| Method + path | Guard | Purpose |
|---|---|---|
| `GET /passkeys/login/options` | `guest:web` | Assertion options. Stored in session `passkey.verification_options`. |
| `POST /passkeys/login` | `guest:web` | Verify the assertion and log in (`remember` = request boolean, default false). Response JSON `{redirect: intended('/')}`. |
| `GET /passkeys/confirm/options`, `POST /passkeys/confirm` | auth | Password-confirmation substitute (package feature; no UI uses it). |
| `GET /user/passkeys/options` | auth + `auth` management middleware | Registration options. |
| `POST /user/passkeys` | auth + `auth` management middleware | Store the credential. Validation: `name` required string max 255, plus `credential.*`. |
| `DELETE /user/passkeys/{passkey}` | auth | Package delete. The UI deletes through Livewire instead. |

- **Config** (`config/passkeys.php`):
  - Relying party ID = host of `PASSKEYS_RELYING_PARTY_ID`, or else of `APP_URL`.
  - Allowed origins = `APP_URL` origin plus the comma list `PASSKEYS_ALLOWED_ORIGINS`.
  - `PASSKEYS_STRICT_ORIGIN` (default false). When false, `AllowPasskeyOrigin` adds the request's own origin if its host equals both the relying party ID and the request host, and it is HTTPS (or HTTP on localhost / 127.0.0.1 / [::1]). This makes non-standard ports work.
  - `management_middleware = ['auth']`, so registering does **not** require password confirmation (the package default would be `password.confirm`).
- **Errors:**
  - `AddPasskeyErrorMessage` copies `errors[0].detail` into a top-level `message`, which is what the JS client displays.
  - `Handler::invalidPasskey` returns 422 with detail `passkeys.invalid_origin` ("Passkeys are tied to :domain…") for origin or scheme failures, and `passkeys.invalid` otherwise. It also logs the ceremony origin, the request origin, the relying party ID and the enforced origins.
- **Login UI:** `Passkeys.verify()`, then navigate to the returned `redirect`. If the user cancels, nothing happens. Any other error produces a danger toast with the server message.
  - Passkey login **skips the MFA challenge**, the Filament `LoginResponse` (`redirect_to_admin` is ignored) and the panel access check.
- **Storage:** table `passkeys`, with `id`, `user_id` (FK cascade), `name` (text), `credential_id` (unique, ≤512), `credential` (JSON), `last_used_at`, and timestamps. Display name = username; WebAuthn username = email.
- Profile management UI: §4.4.

#### 3.6 OAuth (Socialite) login and account linking
**Registry** (`app/Extensions/OAuth/OAuthService.php`, `OAuthServiceProvider.php`):
- Built-in providers: `facebook`, `x`, `linkedin`, `google`, `github`, `gitlab`, `bitbucket`, `slack` (core Socialite), plus `authentik`, `discord`, `steam` (SocialiteProviders). Plugins can register more, e.g. via `CommonSchema(id, name, configName, icon, hexColor)`.
- On registration the service sets `config('services.{id}') = getServiceConfig() + ['redirect' => '/auth/oauth/callback/{id}']`.

**Per-provider env keys** (edited in admin Settings: per-provider settings form and setup wizard):
| Key | Meaning |
|---|---|
| `OAUTH_{ID}_ENABLED` | provider shown and usable (`isEnabled`) |
| `OAUTH_{ID}_CLIENT_ID`, `OAUTH_{ID}_CLIENT_SECRET` | credentials |
| `OAUTH_{ID}_SHOULD_CREATE_MISSING_USERS` | auto-create a user when no account with that email exists (default false) |
| `OAUTH_{ID}_SHOULD_LINK_MISSING_USERS` | auto-link to an existing user with the same email (default false) |
| `OAUTH_AUTHENTIK_BASE_URL`, `OAUTH_AUTHENTIK_DISPLAY_NAME` (default "Authentik"), `OAUTH_AUTHENTIK_DISPLAY_COLOR` (default `#fd4b2d`) | Authentik specifics |
| `OAUTH_GITLAB_HOST` | self-hosted GitLab |
| `OAUTH_STEAM_CLIENT_SECRET` | Steam Web API key (no client id). `allowed_hosts` = APP_URL host. Steam has **no** create/link toggles in its settings form, but the env keys are still honoured. |

Brand colours: Bitbucket #205081, Discord #5865F2, Facebook #1877f2, GitHub #4078c0, GitLab #fca326, Google #4285f4, LinkedIn #0a66c2, Slack #6ecadc, Steam #00adee, X #1da1f2.

**Routes** (`routes/auth.php`, prefix `/auth`, `web` plus `guest` middleware):
| Route | Behaviour |
|---|---|
| `GET /auth/login` (name `auth.login`) | Redirects to `/login`. |
| `GET /auth/oauth/redirect/{driver}` | `guest` only. If the provider is disabled, go back to login. Otherwise `Socialite::driver($driver)->redirect()`. (An unknown driver would null-deref.) |
| `GET /auth/oauth/callback/{driver}` | `guest` middleware **removed**, so it works both for login and for linking while logged in. |

**Callback algorithm** (`app/Http/Controllers/Auth/OAuthController.php`):
1. Unknown or disabled provider: redirect to `/auth/login`.
2. A provider `error` query parameter is reported, then the user is sent to the error redirect.
3. Fetch `$oauthUser`.
4. **Logged-in user:** link the account. This sets `users.oauth[{driver}] = oauthUserId`. It does **not** check whether that id is already linked to another user. Then redirect to `/profile?tab=oauth::data::tab`.
5. Otherwise, find a user with `users.oauth->{driver}` equal to the provider id (JSON contains). If found, log them in.
6. Otherwise (missing user):
   - Provider email missing: error "No email was linked to your account on the OAuth provider."
   - Provider sends `email_verified` = false: error "Email not verified on OAuth provider."
   - A user with that email exists: link if `SHOULD_LINK_MISSING_USERS`, else the generic error.
   - No such user: if `SHOULD_CREATE_MISSING_USERS`, create a user via `UserCreationService` with `username` = provider nickname (if empty, the email local part plus 3 random chars), the email, and the oauth map. There is no password, so a random hash is stored and a reset token issued, and the `AccountCreated` mail contains the setup link. Creation failure gives the generic error.
7. **Login:** `auth()->login($user, remember: true)`, then redirect to `/`. This **bypasses MFA**, captcha and `redirect_to_admin`.
8. **Error redirect:** a persistent danger toast titled "Something went wrong" (with the error text as body), or "No linked User found" when there is no message. Then redirect to `/auth/login`. These strings are hard-coded English.

#### 3.7 Logout
- `POST /logout` (each panel; Filament `LogoutController`) logs out, invalidates the session, and redirects to that panel's login URL (Filament default).
- It is offered as the user-menu "Sign out" item.

### 4. Profile page (`app/Filament/Pages/Auth/EditProfile.php`)

#### 4.1 General
- **Route:** `GET /profile` (`filament.app.auth.profile`). The admin and server panels also register `/admin/profile` and `/server/profile`, but menus always link to the app one.
- **Layout:** full panel layout, width `FILAMENT_WIDTH`.
- **Tabs:** persisted in the query string as `?tab={key}::data::tab`. Plugins can add tabs before or after via `CanCustomizeTabs::registerCustomTabs(TabPosition, ...)`.
- **Header actions** (no footer form buttons): Back (arrow icon; `history.back()`, or the panel home if there is no referrer) and **Save** (floppy icon, shortcut `mod+s`).
- **Save flow** (Filament default `save()`):
  - Rate limit 5 per 60s per IP and component, plus 5 per user.
  - Runs in a DB transaction and calls `$user->update($data)`. Email-change verification is not enabled, so email changes apply immediately.
  - On success it shows the "Saved" toast. If the password changed, it updates the session's `password_hash_web` so the current session survives `AuthenticateSession`.
- **Model-level validation on every save** (`ValidationObserver` with `User::$validationRules`):
  - `email` is a unique email of 1–255 chars; `username` is unique, 1–255 chars.
  - `language` must be an ICU locale (`ResourceBundle::getLocales('')`).
  - `timezone` must be a valid identifier.
  - Customization sub-keys: see §4.8.
  - Username and email are lowercased and trimmed when saved.
- **Externally managed users** (`users.is_managed_externally = true`): username and email are disabled and the password field is hidden.

#### 4.2 Tab "Account" (`account`)
| Field | Control | Validation / behaviour |
|---|---|---|
| `username` | text, user icon | required, max 255, unique (ignoring self). Disabled if managed externally. |
| `email` | email, mail icon | required, email, max 255, unique. Disabled if managed externally. |
| `password` | password (revealable) | Optional. `Password::default()` (Laravel default: min 8). Must equal `passwordConfirmation`. Only saved when filled, then hashed. Validated live with a 500ms debounce. **No current-password field.** The page replaces Filament's form, which normally requires the current password. Hidden if managed externally. |
| `passwordConfirmation` | password | Required and visible only when the password is filled. Not saved. |
| `timezone` | searchable select of all `DateTimeZone::listIdentifiers()` | required. Default `config('app.timezone')` (UTC). Used for date tooltips (`DateTimeColumn`), chart timestamps and schedule next-run display. |
| `language` | select from `LanguageService::getAvailableLanguages()` (§8.2) | required, live, default `en`. For any language other than `en`, helper text reads "Your language :state has not been translated yet!" plus a link "Update On Crowdin" (`https://crowdin.com/project/pelican-dev/`). |
| `avatar` | avatar file upload with image editor | Only shown if `FILAMENT_UPLOADABLE_AVATARS` is true. PNG only. Stored on the `public` disk as `avatars/{user_id}.png`. Can be deleted. Otherwise avatars come from the configured provider: `gravatar` (`https://gravatar.com/avatar/md5(email)`, the default) or `uiavatars` (Filament's initials avatar), set by `FILAMENT_AVATAR_PROVIDER`. An uploaded file takes precedence. |

#### 4.3 Tab "OAuth" (`oauth`), visible only if at least one provider is enabled
- One button per enabled provider, in the brand colour.
- If the provider is **not linked**: "Link {name}". This redirects straight to the Socialite provider URL from the Livewire action; the callback then links and returns to this tab.
- If the provider **is linked** (key present in `users.oauth`): "Unlink {name}". This removes the key via `UserUpdateService`, refills the form and shows the toast "{name} unlinked".
- Nothing stops a user from unlinking their last login method when password login is disabled.

#### 4.4 Tab "2FA" (`2fa`), visible when MFA is configured (always)
- Each Filament MFA provider's management component is listed, with enabled providers first. Each has an Enabled/Disabled badge and set-up / regenerate / disable actions (§3.4).

#### 4.5 Tab "Passkeys" (`passkeys`), **hidden unless the request is HTTPS**
- Section heading "Passkeys" with a description (`passkeys.description`).
- Livewire component `App\Livewire\Passkeys`:
  - **Create:** a name text input (no client-side rule; the server requires 1–255 chars) and a **Create** button. The button calls `window.registerPasskey(name)` → `Passkeys.register({name})`.
    - On success: `onPasskeyRegistered()` clears the name and shows the toast "Your passkey has been created".
    - On failure: `registrationFailed(message)` shows the danger toast "Passkey registration failed" with the message.
  - **List:** each passkey shows its name and "Last used: {diffForHumans}" or "Not used yet".
  - **Delete:** a confirmation modal, then the user's own passkey is deleted (`findOrFail` scoped to the user) and the toast "Your passkey has been deleted" is shown.

#### 4.6 Tab "API Keys" (`api_keys`): client API keys
- **Create section:**
  - `description` (text, live; the Create action stays disabled until it is non-empty).
  - `allowed_ips` (tags input, split on `,`, space or Tab; placeholder "127.0.0.1 or 192.168.1.1"; helper "Press enter to add a new IP address or leave blank to allow any IP address").
  - Header action **Create**:
    - Calls `$user->createToken(description, allowed_ips)` to create an `api_keys` row: `key_type = 1` (account), identifier `pacc_` plus 11 random chars (16 total), token = 32 random chars stored **encrypted**, memo = description.
    - Logs activity `user:api-key.create` (property `identifier`).
    - Shows a **persistent** success toast "API Key Created" whose body is the full key `identifier + token` (48 chars). **This is the only time the plaintext key is shown.**
    - Redirects back to the tab.
  - **No server-side validation on this path:**
    - The IP/CIDR validation done by the client API's `StoreApiKeyRequest` is not applied.
    - The `memo` max 500 is not applied.
    - The per-account limit `API_KEYS_LIMIT` (default 25) is not checked; the client API endpoint does check it.
- **List section:**
  - A repeater over `apiKeys` (account-type keys), each item labelled with its identifier and showing the memo.
  - Delete asks for confirmation, deletes only a key belonging to the current user (tamper-safe), and logs `user:api-key.delete`.
  - When the user has no keys: "No API Keys".
- Key expiry: `API_KEYS_EXPIRE_TIME` (default 720) exists in config. Its enforcement is outside this page.

#### 4.7 Tab "SSH Keys" (`ssh_keys`)
- **Create section:** `name` (text) and `public_key` (auto-sizing textarea). The **Create** action is disabled until both are filled. It runs `app/Services/Ssh/KeyCreationService.php`:
  - The key is parsed with phpseclib. Invalid keys fail with "The public key provided is not valid".
  - DSA is rejected. RSA keys shorter than 2048 bits are rejected.
  - The fingerprint is SHA256. A duplicate fingerprint on the same account is rejected ("The public key provided already exists on your account").
  - The key is stored in PKCS8 form with its name and fingerprint.
  - Success: toast "SSH Key Created" with body `SHA256:{fingerprint}`, activity `user:ssh-key.create`.
  - Failure: danger toast "Could not create ssh key" plus the reason.
- **List section:** a repeater over `sshKeys` (label = name, shows `SHA256:{fingerprint}`). Delete asks for confirmation, is scoped to the user, and logs `user:ssh-key.delete`. Empty state: "No SSH Keys".
- These keys are used for SFTP authentication (consumed by Wings via the remote API; out of scope here).

#### 4.8 Tab "Customization" (`customization`)
- Values are stored in `users.customization` (JSON).
- `User::getCustomization()` merges the stored values over the enum defaults (`app/Enums/CustomizationKey.php`).
- `setCustomization()` updates a single key under a row lock. It is only used for `servers_per_page`.
- The profile save **rebuilds the whole object** from the form (see the open questions).

| Key (`CustomizationKey`) | Type | Default | Profile control | Validation (form / model) | Effect |
|---|---|---|---|---|---|
| `dashboard_layout` | string `grid`\|`table` | `grid` | Section "Dashboard": ToggleButtons Grid/Table | required / `in:grid,table` | Server list layout (§2.3). |
| `top_navigation` | string `sidebar`\|`topbar`\|`mixed` (legacy bool true/false is mapped to topbar/sidebar on load) | `FILAMENT_DEFAULT_NAVIGATION` (default `sidebar`) | ToggleButtons Sidebar/Topbar/Mixed | none in form / model rule says `boolean` (mismatch) | Panel navigation mode (§1.2). |
| `button_style` | bool | `true` | ToggleButtons: true = "Icon", false = "Icon Button" (form default `'icon'` is overridden on fill) | none | true: most actions render as bare icon buttons with XL icons. false: normal or labelled buttons (§1.4). Also used by the server file manager list. |
| `redirect_to_admin` | bool | `false` | Section "Admin" (only visible to `isAdmin()` users): boolean ToggleButtons with helper text | none | After password login, go to `/admin` (§3.1). |
| `console_font_size` | int | 14 | Section "Console": numeric, live | required, min 1 / int min 1 | Server console font size (px). |
| `console_font` | string | `monospace` | Select: `monospace`, plus every `*.ttf` under `storage/app/public/fonts` (the directory is created if missing; files are served from `/storage/fonts/{name}.ttf`) | required / string | Console font family (the `@font-face` is loaded from storage). The live "Font Preview" shows sample text in the chosen font and size. |
| `console_graph_period` | int seconds | 30 | numeric, suffix "Seconds", hint icon | required, min 10, max 120 / int min 1 | Number of data points (seconds) in the console CPU/memory graphs. |
| `console_rows` | int | 30 | numeric | required, min 1 / int min 1 | Number of terminal rows in the console. |
| `servers_per_page` | int | 0 (= unset) | not in profile; written by the server-list page-size selector | model: int min 0 | Default server-list page size if it is a valid option (§2.3). |

#### 4.9 Tab "Activity" (`activity`)
- Read-only list labelled "Showing last 50 activity logs".
- It contains the 50 most recent `activity_logs` where the user is a **subject** (`activity_log_subjects` morph), newest first.
- Each row shows the actor avatar, "username — event", the translated description (`ActivityLog::getLabel()`, from `lang/*/activity.php`), the IP if present, and the relative time with the absolute time as a tooltip ("M j, Y g:ia").
- System actions show as user "system".

## Admin panel (part A)

This part covers the admin panel shell (provider, access, navigation), the Dashboard, Health, Panel Logs and Settings pages, and the Users, Roles, API Keys, Webhooks, Plugins, Mounts, Database Hosts and Backup Hosts resources. Labels are the English strings from `lang/en/**`. Statements marked **[Filament default]** describe framework behaviour that Pelican code does not override; they were checked against the vendored Filament 5 sources where possible.

Conventions used throughout the admin panel:

- **Header actions instead of form footers.** Every Create/Edit page clears the form footer buttons (`getFormActions(): []`) and shows icon-only header actions instead. Create is Ctrl/Cmd+S (`keyBindings(['mod+s'])`) and Save is also Ctrl/Cmd+S. "Create another" is disabled on every create page (`$canCreateAnother = false`).
- **View or Edit, not both.** In list tables, the row "View" action is hidden whenever the user is allowed to edit the record (`ViewAction::hidden(fn ($record) => static::getEditAuthorizationResponse($record)->allowed())`). Each row therefore shows either View (read-only users) or Edit.
- **Plugin extension points.** Most resources use the traits `CanModifyTable`, `CanModifyForm`, `CanCustomizePages`, `CanCustomizeRelations`, `CanCustomizeStaticTabs` / `CanCustomizeTabs`, `CanCustomizeHeaderActions` and `CanCustomizeHeaderWidgets` (`app/Traits/Filament/*`). Through these, plugins can post-process the table and form closures, add pages and relation managers, insert tabs before or after the defaults (`TabPosition::Before|After`), and insert header actions before or after the defaults (`HeaderActionPosition`).
- **Model validation on save.** Models that use `App\Traits\HasValidation` (User, ApiKey, Mount, DatabaseHost, BackupHost, Server, Node, …) are validated again against `static::$validationRules` on every `saving` event (`app/Observers/ValidationObserver.php`). `unique:` rules ignore the current row on update. Model validation can therefore reject a save even after the form-level rules have passed.
- **Root Admin bypass.** `Gate::before(fn (User $user) => $user->isRootAdmin() ? true : null)` in `app/Providers/AppServiceProvider.php` means the Root Admin passes every ability check.

### Panel shell

Sources: `app/Providers/Filament/AdminPanelProvider.php` and `app/Providers/Filament/PanelProvider.php` (the shared base for the `admin`, `app` and `server` panels).

| Aspect | Behaviour |
|---|---|
| Panel id / URL prefix | `admin`, served under `/admin`. The Dashboard is at `/admin` (Dashboard `$routePath = '/'`). |
| Home URL (logo click) | `/` (the `app` panel), via `homeUrl('/')`. |
| Breadcrumbs | Disabled. |
| SPA mode | On (`spa()`); the only exception is the server console route in the server panel. URLs matching `*/oauth/redirect/*` bypass SPA. |
| Branding | Brand name `config('app.name')` (`APP_NAME`), logo `config('app.logo')` (`APP_LOGO`) at 2rem height, favicon `config('app.favicon')` (`APP_FAVICON`, default `/pelican.ico`). If the app name is not "pelican", the footer shows "{APP_NAME} - Powered by Pelican" (`FilamentServiceProvider`). |
| Content width | `config('panel.filament.display-width')` (`FILAMENT_WIDTH`, default `screen-2xl`). |
| Navigation layout | Chosen per user from the `top_navigation` customization; the default comes from `FILAMENT_DEFAULT_NAVIGATION`. `topbar` (or legacy `true`) gives top navigation only. `mixed` gives a sidebar plus a topbar. `sidebar` gives the sidebar only. On desktop the sidebar is collapsible when top navigation is off. |
| Auth | Shared login page `App\Filament\Pages\Auth\Login`, so each panel has its own `/admin/login` [Filament default route]. Password reset is enabled. MFA offers an app (TOTP, with recovery codes) and email codes. |
| Middleware | Cookies/session/CSRF (`PreventRequestForgery`), `LanguageMiddleware`, `SetSecurityHeaders`, and `RedirectIfNotInstalled` (redirects to the `installer` route when `APP_INSTALLED` is false). Auth middleware: `Authenticate` and `RequireTwoFactorAuthentication`, described below. |
| Database notifications | Enabled (bell icon). Background plugin jobs report their results here. |
| Alert banners | A Livewire `AlertBannerContainer` is rendered at `PAGE_START`. It is used for help banners, safe-mode warnings and 2FA nags. |
| Discovery | Resources, pages and widgets are auto-discovered from `app/Filament/Admin/{Resources,Pages,Widgets}`. After that, `PluginService::loadPanelPlugins($panel)` lets enabled plugins register their own admin resources, pages and widgets. |

**Who can enter `/admin`**: `User::canAccessPanel()` in `app/Models/User.php`.

- A Root Admin (has the Spatie role named exactly `Root Admin`) is always allowed.
- For the `admin` panel, any other user needs `isAdmin()`. That means at least one role **and** at least one permission in total (`roles()->count() >= 1 && getAllPermissions()->count() >= 1`). Which permission does not matter.
- Every page and resource then applies its own permission check (see the permission model under Roles).

**2FA enforcement** (`app/Http/Middleware/RequireTwoFactorAuthentication.php`, level from `APP_2FA_REQUIRED`):

| Level | Effect |
|---|---|
| 0 | Not required. |
| 1 | Required only for users where `isAdmin()` is true. |
| 2 | Required for every user. |

- A user counts as having 2FA when email MFA is on or a TOTP secret is set.
- Routes matching `*auth.*` are always allowed through.
- JSON and `/api` requests get `TwoFactorAuthRequiredException`.
- Web requests get a warning banner ("auth.2fa_must_be_enabled") and are redirected to the `app` panel profile page, 2FA tab.

**User menu**:

| Item | Behaviour |
|---|---|
| Profile | Links to the `app` panel's `EditProfile` page. The admin panel has no profile page of its own (`profile(EditProfile::class, false)` plus the override). |
| Exit Admin | Label `profile.exit_admin` = "Exit Admin". Links to the `app` panel root. |
| Logout and theme switcher | [Filament default] |

**Navigation groups**, in this order:

1. "Server" (`admin/dashboard.server`), not collapsible.
2. "User" (`admin/dashboard.user`), not collapsible.
3. "Advanced" (`admin/dashboard.advanced`), collapsible.

Ungrouped items render before all groups [Filament default]. No admin item sets `navigationSort`, so items without a sort get −1 and keep registration order: pages first, then resources, both alphabetical by class file [Filament default]. Exceptions: the Dashboard has −2 [Filament default] and Panel Logs has 1 (log-viewer plugin default), which puts it last in Advanced.

Resulting sidebar (sidebar or mixed mode; part B covers Nodes, Eggs and Servers):

| Group | Items, top to bottom | Badge |
|---|---|---|
| (none) | Dashboard | — |
| (none) | Settings | — |
| (none) | Plugins | Plugin count |
| Server | Eggs | Count |
| Server | Nodes | Count |
| Server | Servers | Count |
| User | Roles | Count |
| User | Users | Count |
| Advanced | Health | Number of failed checks (red) |
| Advanced | API Keys | Count of application keys |
| Advanced | Backup Hosts | Count |
| Advanced | Database Hosts | Count (node-scoped) |
| Advanced | Mounts | Count (node-scoped) |
| Advanced | Webhooks | Count |
| Advanced | Panel Logs | — |

When the user's navigation type is exactly `topbar`:

- Users, Eggs, Nodes and Servers return `null` as their group and become top-level items.
- Roles moves to the "Advanced" group (`RoleResource::getNavigationGroup`).

An item is shown only when the user passes that page's or resource's access check (policy `viewAny` → `viewList <model>`, or the page's `canAccess()`).

**Global search**: Filament global search is on [Filament default], rendered as a modal by the `charrafimed/global-search-modal` plugin (`GlobalSearchModalPlugin::make()`, no options set).

- The plugin keeps recent searches and favourites in browser localStorage (plugin behaviour).
- A resource is searchable when it has a `$recordTitleAttribute` [Filament default]. In this part: Users (`username`), Roles, Mounts, Database Hosts, Backup Hosts, Webhooks and Plugins (all on `name`). API Keys are **not** searchable.
- Pelican configures no keyboard shortcut.

**Log viewer plugin**: `FilamentLogViewerPlugin` (`gboquizosanchez/filament-log-viewer`), configured as follows:

- Authorised by `user()->can('view panelLog')`.
- Uses the custom pages `ListLogs` and `ViewLogs`.
- Navigation label "Panel Logs" (`admin/log.navigation.panel_logs`), group Advanced, icon `FileInfo`.

### Dashboard (`/admin`)

Source: `app/Filament/Admin/Pages/Dashboard.php`, extending Filament's `Dashboard`.

- No `canAccess` override, so any user who can enter the panel sees it.
- Heading: "Welcome to {APP_NAME}!" (`admin/dashboard.heading`). Subheading: "Version: {version}" (`admin/dashboard.version`).
- The version is `SoftwareVersionService::currentPanelVersion()`. That is `config('app.version')`, or `canary ({7-char git HEAD sha})` when the version is `canary`; it is cached for 5 minutes.
- Layout is a single column (`getColumns(): 1`).
- Widgets are auto-discovered from `app/Filament/Admin/Widgets`. Each is a `FormWidget` (an abstract base, eager-loaded with `$isLazy = false`) that renders its schema through `resources/views/filament/admin/widgets/form-widget.blade.php`, which only outputs `{{ $this->form }}`. Plugins may add more widgets.
- The widgets are display-only. Their actions are external or internal links, and each section's collapse state is persisted in the browser (`persistCollapsed`).

| Sort | Widget | Visible when | Content | Actions |
|---|---|---|---|---|
| 0 | `UpdateWidget` | Always | See the three states listed below this table. | "Update available" (links to `https://pelican.dev/docs/panel/update`, new tab), only in the update-available state |
| 1 | `CanaryWidget` | `currentComparableVersion() === null`, i.e. `APP_VERSION` is `canary` | Collapsed section "Information for Developers": "Thank you for trying out the development version!" plus "If you run into any issues, please report them on GitHub." | "Create Issue" (links to `https://github.com/pelican/panel/issues`, new tab) |
| 2 | `NoNodesWidget` | `Node::count() <= 0` | Section "No Nodes Detected" with the intro text | "Create first Node" (goes to the admin Create Node page) |
| 3 | `SupportWidget` | Always | Section "Support Pelican" with a thank-you text | "Donate Directly" (links to `https://pelican.dev/donate`, new tab) |
| 4 | `HelpWidget` | Always | Section "Need Help?": "Check out the documentation first! …" | "Read Documentation" (links to `https://pelican.dev/docs`, new tab) |

`UpdateWidget` shows one of three states:

- **Version check unavailable.** The latest version is unknown (GitHub fetch failed) and this is not canary. Grey section "Version check unavailable", including the current version.
- **Up to date.** `isLatestPanel()` is true. This also covers canary builds and any build where either version is unknown. Section "Your Panel is up to date" with "You are currently using {version}…".
- **Update available.** Otherwise. Warning section "Update available" with ":latestVersion is now available! …"; the text has a container variant used when `/.dockerenv` exists. It includes a collapsible "What's New?" panel that renders the latest release's markdown body from GitHub.

Version data comes from `app/Services/Helpers/SoftwareVersionService.php`:

- Source: `GET https://api.github.com/repos/pelican/panel/releases/latest` (5 s timeout, 1 s connect timeout).
- The tag is stripped of `v` and cached for `panel.cdn.cache_time` (60 minutes).
- A failed fetch caches the string `"error"`. That entry is discarded on the next read, so a failure is retried on the next request.

### Health (`/admin/health`)

Sources: `app/Filament/Admin/Pages/Health.php`, `resources/views/filament/pages/health.blade.php`, `app/Checks/*` and `spatie/laravel-health`.

**Access and navigation**:

- Access: `can('view health')`.
- Navigation: "Health", group Advanced.
- Navigation icon: heart-question when there are no stored results, heart-exclamation when any check is failing, heart-check otherwise.
- Badge: number of checks with status `failed` (danger colour), hidden when 0. Badge tooltip: "Failed {comma-separated check names}".

**Registration** (`app/Providers/AppServiceProvider.php`, skipped during unit tests): `Health::checks([...])` registers, in this order: DebugMode (only `if ($app->isProduction())`), Environment, Cache, Database, Queue (Spatie's `QueueCheck`), Schedule, UsedDiskSpace, PanelVersion, NodeVersions.

**Scheduling** (`app/Console/Kernel.php`):

| Command | Frequency | Effect |
|---|---|---|
| `ScheduleCheckHeartbeatCommand` | Every minute | Writes the cache key `health:checks:schedule:latestHeartbeatAt`. |
| `DispatchQueueCheckJobsCommand` | Every minute | Queues a job that writes `health:checks:queue:latestHeartbeatAt.{queue}`. |
| `RunHealthChecksCommand` | Every 5 minutes | Runs all checks. |

Results are stored by `CacheHealthResultStore` using the `file` cache store (`config/health.php`). Health failure notifications are disabled (`notifications.enabled = false`).

**Page behaviour**:

- If no stored results exist when the page renders, it runs `RunHealthChecksCommand` synchronously and refreshes itself.
- Header action **Refresh** (icon only, tooltip "Refresh") runs all checks synchronously, re-renders, and shows the notification "Health check results updated".
- Each check is shown as a card with a status icon. Status colours: ok = success, warning = warning, skipped = info, failed or crashed = danger.
- The card title is `trans('admin/health.results.{lowercased label without spaces}.label')`.
- The card body is the result's `notificationMessage` if it is set, otherwise its `shortSummary`.
- Footer: "Checked results from {relative time}". It turns red when the stored results are more than 5 minutes old.

| Check (card title) | Class | Logic and threshold | OK text | Non-OK text |
|---|---|---|---|---|
| Debug Mode | `App\Checks\DebugModeCheck` (production only) | `config('app.debug')` must equal `false`. | Short summary `false` | failed: "The debug mode was expected to be false, but actually was true" |
| Environment | `App\Checks\EnvironmentCheck` | `App::environment()` must equal `production`. | "Ok, Set to :actual". The placeholder is not substituted in code, so this literal text appears; see Open questions. | failed: "Environment is set to {actual}, Expected production" |
| Cache | `App\Checks\CacheCheck` | Writes a random 5-char value to `laravel-health:check-{value}` on `config('cache.default')` with a 10 s TTL, reads it back, then forgets it. | "Ok" | failed: "Could not set or retrieve an application cache value." or "An exception occurred with the application cache: {error}" |
| Database | `App\Checks\DatabaseCheck` | `DB::connection(default)->getPdo()` | "Ok" | failed: "Could not connect to the database: {error}" |
| Queue | `Spatie\Health\Checks\Checks\QueueCheck` | For the default queue of the default connection, the heartbeat must exist and be at most 5 minutes old. | — | failed: "Queue jobs running failed. Check meta for more information." (English text from the package, not translated) |
| Schedule | `App\Checks\ScheduleCheck` (extends Spatie's) | The heartbeat must exist and be at most `heartbeatMaxAgeInMinutes` old (package default **1 minute**). | "Ok" | failed: "The schedule did not run yet." or "The last run of the schedule was more than {n} minutes ago" |
| Disk Space | `App\Checks\UsedDiskSpaceCheck` (extends Spatie's) | Used % = `100 - disk_free_space('/') * 100 / disk_total_space('/')`, computed in PHP rather than with `df`. Warning above 70 %, failed above 90 % (package defaults). | Short summary `{n}%` | warning or failed: "The disk is almost full ({n}% used)." (package English text) |
| Panel Version | `App\Checks\PanelVersionCheck` | Compares the comparable version with the latest GitHub release (cached, see Dashboard). | "Panel is up-to-date" (summary "Up to date") | Latest version unknown: **warning** "The latest version could not be fetched from GitHub" (summary "Unknown"). Older: failed "Installed version is {current} but latest is {latest}" (summary "Outdated") |
| Node Versions | `App\Checks\NodeVersionsCheck` | No nodes: status **skipped** with "No Nodes created" (summary "No Nodes"). Otherwise each node's `systemInformation()['version']` is compared with the latest `pelican/wings` GitHub release. The system info comes from Wings `GET /api/system`, cached with `cache()->flexible(..., [30, 360])`. Unreachable nodes, whose result contains `exception`, are not counted as outdated. A version of `develop` always counts as latest. | "Nodes are up-to-date" (summary "All up-to-date") | failed: "{outdated}/{all} Nodes are outdated" |

### Panel Logs (`/admin/logs`, `/admin/logs/{date}`)

Sources: `app/Filament/Admin/Pages/ListLogs.php`, `app/Filament/Admin/Pages/ViewLogs.php` and `resources/views/filament/components/list-logs.blade.php` (renders only `{{ $this->table }}`). The base classes come from `gboquizosanchez/filament-log-viewer`.

**Setup**:

- Access: `view panelLog` on both pages, through the plugin's `authorize`.
- Pelican has no `config/filament-log-viewer.php`, so the package defaults apply.
- Files are `storage/logs/laravel-YYYY-MM-DD.log` (prefix `laravel-`, extension `.log`).
- The driver is `FILAMENT_LOG_VIEWER_DRIVER` if set, otherwise `LOG_CHANNEL`. The panel's `config/logging.php` defaults `LOG_CHANNEL` to `daily`.
- Pagination options: 5, 10, 25, 30.

**List page (heading "Panel Logs")**: one row per daily log file.

- Columns: Date (formatted; searchable, sortable), then entry counts per level: All ("Total Logs"), Error, Warning, Notice, Info, Debug. Count columns are sortable and have tooltips.
- Pelican removes the plugin's Emergency, Alert and Critical columns.
- Empty state: "Yay! No Errors!".

| Row action | Behaviour |
|---|---|
| View | Opens the view page `/admin/logs/{date}`. |
| Download | Downloads that `.log` file. |
| Upload | Tooltip "Upload to logs.pelican.dev". Asks for confirmation ("Upload Logs?" – "This will upload {date} to https://logs.pelican.dev Are you sure…"). It then reads the file, keeps the **last 1000 non-empty lines**, and POSTs `multipart/form-data` to `https://logs.pelican.dev` with fields `c` (content) and `e=14d` (expiry), 10 s timeout. On success a persistent notification "Log Uploaded!" shows the returned `url` with a "View Logs" button (opens in a new tab). Errors produce "Failed to upload." with the HTTP status or exception text, or "Log not found!". |
| Delete | Deletes the file after confirmation. |

Bulk actions:

- Download: zips the selected files.
- Delete: deletes the selected files.

Pelican removes the plugin's "clear log" row and bulk actions.

**View page**:

- Title: the plugin's default title, which includes the log date.
- Header actions: Back, Delete, Download, Upload. Upload works exactly as on the list page.
- A header block shows file information.
- Tabs filter by level. There is one tab per level that has entries, plus "All"; tabs are hidden when every entry has a single level.
- Entries table columns: env, datetime (sortable, searchable), level, message (searchable, sortable), stack (searchable), context (searchable).
- Row actions open the stack trace and the context. These behaviours are package defaults.

### Settings (`/admin/settings`)

Sources: `app/Filament/Admin/Pages/Settings.php`, `resources/views/filament/pages/settings.blade.php`, `app/Traits/EnvironmentWriterTrait.php`, and the schema classes in `app/Extensions/{Captcha,OAuth,Avatar}`.

**Access and navigation**:

- Navigation: "Settings", ungrouped, icon Settings.
- Access: `can('view settings')`.
- The whole tab set is **disabled** (read-only) unless the user has `update settings`.
- The Save header action is authorised by `update settings`, and `save()` additionally calls `abort_unless(... 'update settings', 403)`.

**Persistence model**: everything except the console font is written to the **`.env` file**. There is no settings table.

`save()` runs these steps:

1. `$this->form->getState()` validates the form and returns state only for visible, dehydrated fields. Hidden fields, such as a disabled provider's credentials or SMTP fields while another driver is selected, are **not** rewritten and keep their current `.env` values [Filament default dehydration].
2. Removes `console_font` from the data.
3. Converts booleans to the strings `'true'` / `'false'` and backed enums to their value.
4. `Env::writeVariables($data, App::environmentFilePath(), overwrite: true)` writes every key, creating or replacing lines.
5. `Artisan::call('config:clear')`, then `Artisan::call('queue:restart')`.
6. Redirects to the same page and shows the notification "Settings saved". On an exception it shows "Failed to save Settings" with the message.

All fields in a visible section are written on every save, not just the ones that changed.

**Defaults**: field defaults are read with `env('KEY', config(...))` at render time. The "Default" column below gives the effective default when the key is absent from `.env`.

**UI details**:

- The active tab is kept in the query string (`persistTabInQueryString`).
- Header action: Save (icon floppy disk, tooltip "Save", Ctrl/Cmd+S).
- Plugins can add tabs before or after the defaults (`Settings::registerCustomTabs`) and add header actions and widgets.

Tabs, in order: **General, Captcha, Mail, Backup, OAuth, Misc**.

#### General tab

| Label | Env key | Control | Default | Validation / notes |
|---|---|---|---|---|
| App Name | `APP_NAME` | Text | `Pelican` | Required. Becomes `config('app.name')`: brand name, mail templates, footer. |
| App Logo | `APP_LOGO` | Text (placeholder `/pelican.svg`) | null | Optional. Hint: logo must live in `public/`; leave blank to show the app name instead. |
| App Favicon | `APP_FAVICON` | Text (placeholder `/pelican.ico`) | `/pelican.ico` | Required. Hint: file must live in `public/`. |
| Debug Mode | `APP_DEBUG` | Toggle | `config('app.debug')` (Laravel default false) | Boolean. |
| Avatar Provider | `FILAMENT_AVATAR_PROVIDER` | Select, no empty option | `gravatar` | Options come from `AvatarService::getMappings()`: `gravatar` = "Gravatar" (`https://gravatar.com/avatar/{md5(email)}`) and `uiavatars` = "UI Avatars" (returns null, so Filament's built-in UI Avatars is used). |
| Allow users to upload own avatar? | `FILAMENT_UPLOADABLE_AVATARS` | Toggle | false | When true, an uploaded file at public disk `avatars/{user_id}.png` takes precedence over the provider. |
| Unit Prefix | `PANEL_USE_BINARY_PREFIX` | Toggle buttons: 0 = "Decimal Prefix (MB/GB)", 1 = "Binary Prefix (MiB/GiB)" | true (binary) | Stored as `true` / `false`. Controls 1000 vs 1024 in byte formatting and MB/MiB suffixes across the panel. |
| Default Navigation Type | `FILAMENT_DEFAULT_NAVIGATION` | Toggle buttons: `sidebar` / `topbar` / `mixed` | `sidebar` | Default value of each user's `top_navigation` customization. |
| 2FA Requirement | `APP_2FA_REQUIRED` | Toggle buttons: 0 = "Not Required", 1 = "Required for Admins Only", 2 = "Required for All Users" | 0 | Cast to an integer. See the 2FA enforcement table under Panel shell. |
| Display Width | `FILAMENT_WIDTH` | Select, no empty option | `screen-2xl` | Options: every Filament `Width` enum value (`3xs` … `7xl`, `none`, `full`, `min`, `max`, `fit`, `prose`, `container`, `screen-sm` … `screen-2xl`, `screen`). |
| Trusted Proxies | `TRUSTED_PROXIES` | Tags input. A tag is split off on Tab or Space; the placeholder reads "New IP or IP Range". | `implode(',', config('trustedproxy.proxies'))` | Saved as a **comma-joined string**. `*` and `**` are honoured by config. See the hint actions below. |

Hint actions on Trusted Proxies, both authorised by `update settings`:

- **Clear** (danger, asks for confirmation) empties the list.
- **Set to Cloudflare IPs** calls `GET https://api.cloudflare.com/client/v4/ips` (3 s timeout and 3 s connect timeout) and replaces the list with `result.ipv4_cidrs` plus `result.ipv6_cidrs`. If the request fails, the list is set to **empty** without a notification.

#### Captcha tab

The tab has one collapsible section per registered captcha schema (`CaptchaService::getAll()`). Core registers only **Turnstile** (`app/Extensions/Captcha/Schemas/Turnstile/TurnstileSchema.php`, section title `TURNSTILE`, Cloudflare icon). A section starts collapsed when that captcha is not enabled.

| Element | Env key | Behaviour |
|---|---|---|
| Hidden enabled flag | `CAPTCHA_TURNSTILE_ENABLED` | Default `env(...)`, which may be null. The **Enable** (success) and **Disable** (danger) buttons only flip this in-page state; the value persists only on **Save**. The buttons are disabled without `update settings`. |
| Site Key | `CAPTCHA_TURNSTILE_SITE_KEY` | Password input with reveal. Required. Visible only while enabled. |
| Secret Key | `CAPTCHA_TURNSTILE_SECRET_KEY` | Password input with reveal. Required. Visible only while enabled. |
| Verify Domain? | `CAPTCHA_TURNSTILE_VERIFY_DOMAIN` | Toggle, default true. When on, the `hostname` returned by Cloudflare must equal the request host. |
| Info | — | Static text: "You can generate the keys on your Cloudflare Dashboard…" (with a link). |

Runtime behaviour:

- Validation posts `{secret, response}` to `https://challenges.cloudflare.com/turnstile/v0/siteverify` (15 s timeout, 5 s connect timeout).
- Cloudflare error codes are mapped to readable messages.
- The first enabled captcha schema (`getActiveSchema()`) is the one used on login forms.

#### Mail tab

| Label | Env key | Control | Default | Validation / visibility |
|---|---|---|---|---|
| Mail Driver | `MAIL_MAILER` | Toggle buttons: `log` = "/storage/logs Directory", `smtp` = "SMTP Server", `mailgun` = "Mailgun", `mandrill` = "Mandrill", `postmark` = "Postmark", `sendmail` = "sendmail (PHP)" | `log` | Live; drives which sections below are visible. |
| *From Settings* section: "Set the Address and Name used as 'From' in mails." | | | | |
| From Address | `MAIL_FROM_ADDRESS` | Text | `hello@example.com` | Required, must be an email. |
| From Name | `MAIL_FROM_NAME` | Text | `Example` | Required. |
| *SMTP Configuration* section (only when the driver is `smtp`) | | | | |
| Host | `MAIL_HOST` | Text | `127.0.0.1` | Required. |
| Port | `MAIL_PORT` | Numeric | 2525 | Required, 1–65535. |
| Username | `MAIL_USERNAME` | Text | null | Optional. |
| Password | `MAIL_PASSWORD` | Password input with reveal | `env('MAIL_PASSWORD')` | Optional. |
| Scheme | `MAIL_SCHEME` | Toggle buttons: `smtp` = "SMTP", `smtps` = "SMTPS" | `smtps` if legacy `MAIL_ENCRYPTION` is `ssl` or `tls`, otherwise `smtp` | Live. Changing it **sets the Port** to 587 for `smtps` and 2525 for `smtp`. |
| *Mailgun Configuration* section (only when the driver is `mailgun`) | | | | |
| Domain | `MAILGUN_DOMAIN` | Text | null | Required. |
| Secret | `MAILGUN_SECRET` | Text (not masked) | null | Required. |
| Endpoint | `MAILGUN_ENDPOINT` | Text | `api.mailgun.net` | Required. |

Mandrill, Postmark and sendmail have no extra fields. Their credentials or paths, such as `MAIL_SENDMAIL_PATH`, must be set in `.env` by hand.

**Send Test Mail** (hint action on Mail Driver; hidden when the driver is `log`; authorised by `update settings`) works as follows:

1. Temporarily overrides runtime config with the **unsaved** form values: driver, SMTP host/port/username/password/scheme, from address/name, and Mailgun domain/secret/endpoint.
2. Sends the `App\Notifications\MailTested` notification to the current admin's email address, in the admin's language.
3. Shows the notification "Test Mail sent", or "Test Mail failed" with the exception message.
4. Always restores the original config afterwards.

#### Backup tab

The tab has one section, "Throttles": "Configure how many backups can be created in a period. Set period to 0 to disable this throttle."

| Label | Env key | Control | Default | Validation |
|---|---|---|---|---|
| Limit | `BACKUP_THROTTLE_LIMIT` | Numeric | 2 (`config('backups.throttles.limit')`) | Required, min 1. |
| Period | `BACKUP_THROTTLE_PERIOD` | Numeric, suffix "Seconds" | 600 | Required, min 0. A value of 0 disables the throttle. |

These values are consumed by `InitiateBackupService`. S3 settings are no longer on this tab; they moved to Backup Hosts. The `admin/setting.backup.s3.*` strings are reused as labels there.

#### OAuth tab

The tab has one collapsible section per registered OAuth schema, in this registration order (`app/Providers/Extensions/OAuthServiceProvider.php`): **Facebook, X, Linkedin, Google, Github, Gitlab, Bitbucket, Slack, Authentik, Discord, Steam**.

- The section title is `Str::title(id)`. Authentik is the exception: its title is `OAUTH_AUTHENTIK_DISPLAY_NAME`, default "Authentik".
- The icon comes from the schema (Tabler brand icons). If the icon SVG is missing, the generic OAuth icon is used.
- A section starts collapsed when the provider is disabled.
- At registration, each provider's Socialite config is placed at `services.{id}`, with redirect `/auth/oauth/callback/{id}`.

| Element | Env key (`{ID}` = upper-case provider id) | Behaviour |
|---|---|---|
| Hidden enabled flag | `OAUTH_{ID}_ENABLED` | Default is the provider's current enabled state. |
| **Disable** button (danger) | — | Sets the flag to false in page state. The settings fields become hidden and are therefore not rewritten (credentials stay in `.env`). |
| **Enable** button (success) | — | Opens a wizard modal "Enable {name}". Its steps are the provider-specific instruction steps followed by the common "OAuth Config" step (the settings fields). The wizard has no Cancel button and its submit label is "Enable". On submit it sets the flag to `'true'` and copies every entered value into page state. **Save** is still required to persist. |
| Client ID | `OAUTH_{ID}_CLIENT_ID` | Password input with reveal. Required. |
| Client Secret | `OAUTH_{ID}_CLIENT_SECRET` | Password input with reveal. Required. |
| Auto Create Missing Users? | `OAUTH_{ID}_SHOULD_CREATE_MISSING_USERS` | Toggle (boolean), default from env (false when unset). |
| Auto Link Missing Users? | `OAUTH_{ID}_SHOULD_LINK_MISSING_USERS` | Toggle (boolean), default from env (false when unset). |

The settings fields are visible only while the provider is enabled.

Provider-specific differences (`app/Extensions/OAuth/Schemas/*`):

| Provider | Extra or changed fields | Wizard instruction step(s), showing the copyable callback `{APP_URL}/auth/oauth/callback/{id}` | Brand colour |
|---|---|---|---|
| Facebook | — | "Register new Facebook Application" (field "Valid OAuth Redirect URIs") | #1877f2 |
| X | — | "Register new X App" (fields "Website URL" = app URL and "Callback URI / Redirect URL") | #1da1f2 |
| Linkedin | — | "Obtain Linkedin App OAuth Config" | #0a66c2 |
| Google | — | "Register new OAuth client" (fields "Authorized JavaScript origins" = app URL and "Authorized redirect URIs") | #4285f4 |
| Github | — | "Register new GitHub OAuth App" ("Authorization callback URL"), then "Create Client Secret" | #4078c0 |
| Gitlab | `OAUTH_GITLAB_HOST` "Custom Host" (optional URL, for self-hosted GitLab) | "Register new Gitlab OAuth App" ("Redirect URI") | #fca326 |
| Bitbucket | — | "Register new Bitbucket Consumer" | #205081 |
| Slack | — | "Register new Slack OAuth" | #6ecadc |
| Authentik | `OAUTH_AUTHENTIK_BASE_URL` "Base URL" (required URL); `OAUTH_AUTHENTIK_DISPLAY_NAME` "Display Name" (default "Authentik"); `OAUTH_AUTHENTIK_DISPLAY_COLOR` "Display Color" (hex colour picker, default `#fd4b2d`) | "Create Authentik Application" | Configurable |
| Discord | — | "Register new Discord OAuth App" (label "Redirect URL") | #5865F2 |
| Steam | The common fields are **replaced** by a single field, `OAUTH_STEAM_CLIENT_SECRET`, labelled "Web API Key" (required). There are no create/link toggles. `client_id` is null and `allowed_hosts` is the `APP_URL` host. | "Create API Key" (link to `https://steamcommunity.com/dev/apikey`) | #00adee |

The wizard steps also include disabled `_noenv_callback` / `_noenv_origin` inputs that are marked `dehydrated()`. They are copied into page state on enable but have no matching field in the main form, so they are probably dropped by validation pruning (see Open questions).

#### Misc tab

The tab has seven collapsible sections, all collapsed by default.

**Automatic Allocation Creation**: "Toggle if Users can create allocations via the client area. Servers need a primary allocation first."

| Label | Env key | Control | Default | Validation / visibility |
|---|---|---|---|---|
| Allow Users to create Allocations? | `PANEL_CLIENT_ALLOCATIONS_ENABLED` | Toggle | false | Live. |
| Create new allocations if none available? | `PANEL_CLIENT_ALLOCATIONS_CREATE_NEW` | Toggle | true | Visible only when enabled. Helper text: when on, new allocations are created; when off, only existing free allocations are assigned. |
| Start Port | `PANEL_CLIENT_ALLOCATIONS_RANGE_START` | Numeric | null | Required, 1024–65535. Visible only when enabled. |
| End Port | `PANEL_CLIENT_ALLOCATIONS_RANGE_END` | Numeric | null | Required, 1024–65535. Visible only when enabled. Nothing checks that start ≤ end. |

**Mail Notifications**: "Toggle which mail notifications should be sent to Users." All are toggles defaulting to true.

| Label | Env key |
|---|---|
| Account Created | `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION` |
| Added to Server | `PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION` |
| Removed from Server | `PANEL_SEND_REMOVED_FROM_SERVER_NOTIFICATION` |
| Server Installed | `PANEL_SEND_INSTALL_NOTIFICATION` |
| Server Reinstalled | `PANEL_SEND_REINSTALL_NOTIFICATION` |

**Connections**: "Timeouts used when making requests." These are the Wings HTTP client timeouts (`panel.guzzle.*`).

| Label | Env key | Default | Validation |
|---|---|---|---|
| Request Timeout (Seconds) | `GUZZLE_TIMEOUT` | 15 | Required, 15–60 |
| Connection Timeout (Seconds) | `GUZZLE_CONNECT_TIMEOUT` | 5 | Required, 5–60 |

**Activity Logs**: "Configure how often old activity logs should be pruned and whether admin activities should be logged."

| Label | Env key | Default | Validation / notes |
|---|---|---|---|
| Prune Age (Days) | `APP_ACTIVITY_PRUNE_DAYS` | 90 | Required, 1–365 |
| Hide admin activities? | `APP_ACTIVITY_HIDE_ADMIN` | false | Toggle. When on, admin entries are hidden from the server activity views and the API; they are still recorded. |

**API**: "Defines the rate limit for the number of requests that can be executed per configured period." There is one fieldset per limiter, each with *Requests Per Period* (suffix "Requests") and *Period Length* (suffix "Minutes"). Every field is required, numeric and at least 1.

| Fieldset | Requests key (default) | Period key (default, minutes) | Scope (helper text) |
|---|---|---|---|
| Client API Ratelimit | `APP_API_CLIENT_RATELIMIT` (256) | `APP_API_CLIENT_RATELIMIT_PERIOD` (1) | Per user, or per IP if there is no user |
| Application API Ratelimit | `APP_API_APPLICATION_RATELIMIT` (256) | `APP_API_APPLICATION_RATELIMIT_PERIOD` (1) | Per user, or per IP if there is no user |
| Password Reset Ratelimit | `APP_API_PASSWORD_RESET_RATELIMIT` (2) | `APP_API_PASSWORD_RESET_RATELIMIT_PERIOD` (1) | Per IP |
| Websocket Ratelimit | `APP_API_WEBSOCKET_RATELIMIT` (5) | `APP_API_WEBSOCKET_RATELIMIT_PERIOD` (1) | Per server |
| Backup and Restore Ratelimit | `APP_API_BACKUP_RESTORE_RATELIMIT` (3) | `APP_API_BACKUP_RESTORE_RATELIMIT_PERIOD` (15) | Per server |
| Database Create Ratelimit | `APP_API_DATABASE_CREATE_RATELIMIT` (2) | `APP_API_DATABASE_CREATE_RATELIMIT_PERIOD` (1) | Per server |
| Subuser Create Ratelimit | `APP_API_SUBUSER_CREATE_RATELIMIT` (10) | `APP_API_SUBUSER_CREATE_RATELIMIT_PERIOD` (15) | Per server |
| File Pull Ratelimit | `APP_API_FILE_PULL_RATELIMIT` (5) | `APP_API_FILE_PULL_RATELIMIT_PERIOD` (10) | Per server |
| Default Ratelimit | `APP_API_DEFAULT_RATELIMIT` (2) | `APP_API_DEFAULT_RATELIMIT_PERIOD` (1) | Per server |

**Servers**: "Settings for Servers"

| Label | Key | Control | Default | Notes |
|---|---|---|---|---|
| Allow Users to edit Descriptions? | `PANEL_EDITABLE_SERVER_DESCRIPTIONS` | Toggle | true | Gates description editing in the server panel and the client API, and whether the description is exposed in `ServerData`. |
| Console Font Upload | *(not env)* `console_font` | File upload: max 1 file, `font/*`, original filename kept | — | Stored on the `public` disk at `fonts/{filename}` when the form is saved (Filament FileUpload dehydration), then removed from the data before the `.env` write. The hint says only `*.ttf` is supported and mono fonts are recommended. Fonts in `storage/app/public/fonts` become choices in users' profile console-font selector and are served from `/storage/fonts/{name}.ttf`. |

**Webhooks**: "Configure how often old webhook logs should be pruned."

| Label | Env key | Default | Validation |
|---|---|---|---|
| Prune Age (Days) | `APP_WEBHOOK_PRUNE_DAYS` | 30 | Required, 1–365 |

### Resources

#### Users (`/admin/users`)

Sources: `app/Filament/Admin/Resources/Users/UserResource.php`, `Pages/{List,Create,View,Edit}User.php` and `RelationManagers/ServersRelationManager.php`.

| Aspect | Detail |
|---|---|
| Pages | List `/admin/users`, Create `/admin/users/create`, View `/admin/users/{id}`, Edit `/admin/users/{id}/edit` |
| Navigation | "Users", group "User" (top-level in `topbar` mode), badge = total user count when > 0. Record title: `username` (global search). |
| Policy (`app/Policies/UserPolicy.php`) | `viewAny` → `viewList user`. `view` → `view user`. `create` → `create user`. `update` → `update user` **and** `canTarget($user)`. `delete` → `delete user` **and** `canTarget`. `deleteAny` → `delete user`. `canTarget` means a non-root admin can never update or delete a Root Admin. |

**Table**:

| Column | Notes |
|---|---|
| Avatar | Shown from the `lg` breakpoint. Circular, using the Filament avatar URL (uploaded avatar, else provider). |
| Username | Searchable, sortable. |
| Email | Searchable, sortable. |
| 2FA | Shown from `lg`. Icon only: QR icon if a TOTP secret is set, mail icon if email MFA is on, lock-open-off otherwise. Tooltip "App", "E-Mail" or "None". |
| Roles | One badge per role name. Placeholder "No Roles". |
| Servers | Count of servers owned. |
| Subusers | Count of subuser memberships. Shown from `sm`. |

- No filters and no explicit default sort. Rows are ordered by primary key [Filament default key sort], 10 per page with options 5, 10, 25, 50 [Filament default].
- Row actions: View (only when the user cannot edit) and Edit.
- Toolbar: bulk **Delete** (`DeleteBulkAction`) and **Create** (user-plus icon).
- A row can be selected for bulk delete only if it is not the current user and owns no servers (`servers_count` is 0).

**Form** (tabs; plugins may add tabs before or after):

*Account* tab:

| Field | Rules / behaviour |
|---|---|
| Username | Required. Unique in `users.username` (ignores the current record). Max 255. The model lower-cases and trims it on save. Model rule: `between:1,255`. |
| Email | Required. Valid email. Unique. Max 255. Lower-cased (CreateUser also lower-cases it before validation). |
| Password | Password input, optional. On create a hint reads: "Providing a user password is optional. New user email will prompt users to create a password the first time they login." On **edit** there is a hint action **Reset Password**, hidden when `mail.default` is `log`. It sends a reset link through the `app` panel's password broker (`Filament\Auth\Notifications\ResetPassword`, URL = app-panel reset URL), fires `PasswordResetLinkSent`, and shows "Password Reset E-Mail Sent" or "Failed to Send Password Reset E-Mail" with the broker status. |
| External ID | Optional. Model: string, max 255, unique. |
| Is managed externally? | Toggle. Hint: when on, the user cannot change their username, email or password inside the panel. |

*Customization* section (collapsible), in the Account tab:

| Field | Rules / behaviour |
|---|---|
| Timezone | Required, searchable select of all PHP timezone identifiers. Default `config('app.timezone')` or `UTC`. Model rule: must be a valid identifier. |
| Language | Required, searchable select from `LanguageService::getAvailableLanguages()`. Default `en`. Model rule: must be a valid ICU locale. |
| Avatar | Visible only if `public/avatars/{id}.png` exists. Shows that file; removing it deletes the file. The value is discarded on save (`unset($data['avatar'])`), so in practice this is a remove-only control. |

*OAuth* section (only for an existing user): one **Unlink {Provider}** button per entry in `users.oauth`, coloured with the provider's brand colour and asking for confirmation. It calls `OAuthService::unlinkUser()`, which removes that key from the `oauth` JSON, refills the form, and shows ":name unlinked". With no linked providers the section shows "No Accounts Linked".

*Roles* tab:

- **Admin Roles** is a checkbox list of every role **except Root Admin**, with no bulk toggle. It is saved with `syncRoles(...)` and hidden when the user being edited is a Root Admin.
- For a Root Admin, the tab instead shows a disabled, checked "Root Admin" option described as "The Root Admin has all permissions.".
- The Root Admin role therefore cannot be granted or revoked from the UI.

*Keys* tab (only for an existing user):

- **API Keys** lists all of the user's keys of any type. Each item is labelled with the key identifier and shows its memo. Items cannot be added. The delete button asks for confirmation, deletes the key immediately (not on Save), and logs the activity `user:api-key.delete` (actor = current admin; subjects = user and key; property `identifier`). Empty state: "No API Keys".
- **SSH Keys** lists the user's SSH keys, labelled by name and showing `SHA256:{fingerprint}`. Delete asks for confirmation and deletes immediately, logging `user:ssh-key.delete` with property `fingerprint`. Empty state: "No SSH Keys".

*Activity* tab (only for an existing user): a read-only list of the user's activity-log entries, newest first (`orderByDesc('timestamp')`), each rendered with `ActivityLog::htmlable()`.

**Create** (`CreateUser`):

1. The header action is Create (Ctrl/Cmd+S).
2. `handleRecordCreation` forces `root_admin = false`, takes `roles` out of the data, calls `App\Services\Users\UserCreationService::handle($data)`, then `syncRoles`.
3. The service, inside a DB transaction:
   - hashes the password if one was given;
   - otherwise sets a random 30-char password and creates a password-reset token;
   - generates a username from the email local part plus 3 random chars if the username is empty;
   - adds a uuid and calls `forceCreate`.
4. After commit, if `panel.email.send_account_created_notification` is set, the service sends the queued `AccountCreated` mail. The mail contains the username and email, and, when no password was given, a button linking to the app-panel reset URL.
5. The redirect after create is the Filament default (view page if allowed, otherwise edit, otherwise index).

**Edit** (`EditUser`):

- Header actions: **Delete** and **Save**.
- Delete is disabled with tooltip "Can't Delete Yourself" for the current user, or "User Has Servers" when the user owns servers.
- `handleRecordUpdate` drops `roles` and `avatar` (roles are saved through the relationship) and calls `UserUpdateService::handle`:
  - hashes the password if non-empty, otherwise leaves it unchanged;
  - `forceFill(...)->saveOrFail()`;
  - fires `PasswordChanged` if the password changed.
- `PasswordChanged` and `User\Deleting` both trigger `RevocationListener`. For every node hosting a server the user owns or is a subuser of, it dispatches `RevokeSftpAccessJob`. That job calls Wings `POST /api/deauthorize-user {user: <uuid>, servers: []}`, which disconnects websocket and SFTP sessions. It retries with backoff on connection errors.

**Delete**: the `User::deleting` hook throws a `DisplayException` if the user owns servers ("exceptions.users.has_servers") or is the current user ("exceptions.users.is_self"). It then fires `Deleting`, which triggers the revocation described above.

**View**: the form is read-only. Header action: Edit.

**Relation manager "Servers"** (owned servers). It is visible when the user has `viewList server` (ServerPolicy).

Header actions:

| Action | Authorised by | Shown when | Effect |
|---|---|---|---|
| Suspend All Servers (warning) | `update server` | The user has at least one server that is not suspended | For each non-suspended server, `SuspensionService::handle($server, SuspendAction::Suspend)`: sets `servers.status = suspended`, then Wings `POST /api/servers/{uuid}/sync`. Servers being transferred are refused with a notification. |
| Unsuspend All Servers | `update server` | The user has at least one suspended server | Sets `status = null` and syncs with Wings in the same way. |

Columns:

| Column | Notes |
|---|---|
| Name | Links to the admin server edit page when the user can update that server. Searchable, sortable. |
| Node | Links to the node edit page. Searchable, sortable. |
| Egg | Links to the egg edit page. Searchable, sortable. |
| Primary Allocation | With `update server`: an inline select of the server's allocations shown as `ip:port`. It writes `servers.allocation_id` directly and is disabled when the server has one allocation or fewer. Without `update server`: read-only text. Placeholder "None". |
| Databases count | Toggleable, hidden by default, sortable. |
| Backups count | Toggleable, hidden by default, sortable. |

- Row actions: **View Console** (opens the server panel console page; authorised by `canAccessTenant`) and **Edit** (admin server edit page).
- Empty state: "No Servers".

#### Roles (`/admin/roles`) and the admin permission model

Sources: `app/Filament/Admin/Resources/Roles/RoleResource.php`, `Pages/*`, `app/Models/Role.php`, `app/Enums/RolePermissionModels.php`, `app/Enums/RolePermissionPrefixes.php`, `app/Policies/*` and `app/Models/User.php`.

**Permission model** (Spatie `laravel-permission`, guard `web`):

- A permission name is `"{prefix} {model}"`, for example `viewList server`, `update settings` or `view panelLog`.
- Roles carry permissions. Users carry roles; there are no direct user permissions in the UI.
- The role named exactly **`Root Admin`** (`Role::ROOT_ADMIN`) bypasses every gate. It is created on demand by `Role::getRootAdmin()`.

`Role::getPermissionList()` builds the full permission set:

| Source | Model key | Permissions |
|---|---|---|
| `RolePermissionModels` with the standard prefixes (`RolePermissionPrefixes`) | `apiKey`, `allocation`, `backupHost`, `databaseHost`, `database`, `egg`, `mount`, `node`, `role`, `server`, `user`, `webhook` | `viewList`, `view`, `create`, `update`, `delete` |
| `MODEL_SPECIFIC_PERMISSIONS` | `egg` | Also `import`, `export` |
| `SPECIAL_PERMISSIONS` | `settings` | `view`, `update` |
| | `health` | `view` |
| | `activityLog` | `seeIps` |
| | `panelLog` | `view` |
| | `plugin` | `viewList`, `create`, `update`, `delete` |
| Plugins (static API) | any | `Role::registerCustomPermissions(['model' => ['perm', …]])` adds arbitrary permissions. `Role::registerCustomDefaultPermissions('model')` adds the five standard prefixes. `Role::registerCustomModelIcon('model', icon)` sets the section icon. Plugins call these from their own service providers. |

How the permissions are enforced:

- **Resources**: the `DefaultAdminPolicies` trait (`app/Policies/DefaultAdminPolicies.php`) maps `viewAny` → `viewList`, `view` → `view`, `create` → `create`, `update` → `update`, and `delete` / `deleteAny` → `delete`. `replicate` → `create`.
- **Settings page**: `view settings` (open, read-only) and `update settings` (edit and save).
- **Health page**: `view health`.
- **Panel Logs**: `view panelLog`.
- **Plugins**: `viewList plugin` (list), `create plugin` (import), `update plugin` (install, update, enable, disable, uninstall, settings, reorder), `delete plugin` (delete).
- **activityLog seeIps**: used outside this part, to show IPs in activity logs.
- **egg import / export**: used by the Eggs resource (part B).

**Node scoping**:

- Roles may be linked to nodes (`node_role` pivot, `Role::nodes()`).
- `User::accessibleNodes()` returns all nodes when the user is a Root Admin, or when **none** of the user's roles has any node link. Otherwise it returns only the nodes linked to the user's roles. If any role is restricted, the restriction applies even when another of the user's roles is unrestricted.
- `User::canTarget($model)`: Root Admin → always true. A User target → false if the target is a Root Admin. A Node target → the node must be in `accessibleNodes()`.
- The Mount, DatabaseHost and BackupHost policies have a `before()` hook that returns **false** if any node linked to the record is not targetable. `ServerPolicy::before` does the same with the server's node.

**Resource configuration**:

| Aspect | Detail |
|---|---|
| Pages | List, Create, View `/{id}`, Edit `/{id}/edit` |
| Navigation | "Roles", group "User" ("Advanced" in `topbar` mode), badge = role count |
| Policy | `RolePolicy` (default admin policies on `role`) |

**Table**:

| Column | Notes |
|---|---|
| Role Name | Sortable. Not searchable. |
| Permissions | Badge with the permission count, or "All" for Root Admin. |
| Nodes | Node-name badges. Placeholder "All". |
| Users | Count. |

- Row actions: View (only when the user cannot edit) and Edit.
- Toolbar: Create and bulk Delete.
- A row can be selected for bulk delete only if it is not Root Admin and has no users.

**Form**:

| Field | Behaviour |
|---|---|
| Role Name | Required. Disabled when the value is `Root Admin`. |
| `guard_name` | Hidden. Default `web`. |
| Permissions | Fieldset with 3 columns, hidden for Root Admin. It holds one collapsible, collapsed section per model key from `getPermissionList()`. The section title comes from `admin/role.models.{model}` or `Str::headline($model)` (for example "Backup Host"), and the icon from `Role::getModelIcon`. A header badge shows the number of checked items. Inside each section is a checkbox list named `{lowercase model}_list`. Option values are `"{perm} {model}"`. Option labels come from `admin/role.permissions_list.*` ("View List", "View", "Create", "Update", "Delete", "Import", "Export", "See IPs") or `Str::headline(perm)`. The list has bulk toggle and is live. On edit or view it is pre-filled with `$record->checkPermissionTo(key)`. |
| (Root Admin only) | Text "The Root Admin has all permissions." |
| Nodes | Multi-select relationship to nodes, searchable by name or FQDN, preloaded. Hint: "Leave empty to allow access to all nodes." Hidden for Root Admin. |

**Saving** (CreateRole and EditRole):

1. Every form key except `name` and `guard_name` is flattened into a unique list of permission names.
2. Each name is passed to `Permission::firstOrCreate([name, guard_name])`, so permissions are created lazily.
3. Then `$role->syncPermissions(...)`.
4. Nodes are saved through the relationship.

**Edit header actions**:

- Delete is disabled for Root Admin (tooltip "Can't delete Root Admin") or when `users_count >= 1` ("In Use"). See Open questions: `users_count` may not be loaded on the edit page.
- Save.

**View header action**: Edit.

#### API Keys (`/admin/api-keys`) – application API keys

Sources: `app/Filament/Admin/Resources/ApiKeys/ApiKeyResource.php`, `Pages/{List,Create}ApiKey.php`, `app/Models/ApiKey.php` and `app/Services/Acl/Api/AdminAcl.php`.

| Aspect | Detail |
|---|---|
| Pages | List `/admin/api-keys` and Create `/admin/api-keys/create` only. Keys have no view or edit page and cannot be modified after creation. |
| Query | `key_type = 2` (`ApiKey::TYPE_APPLICATION`) only. Account keys (type 1) are managed by users in their profile. |
| Navigation | "API Keys", group Advanced, badge = count |
| Policy | `ApiKeyPolicy` (default admin policies on `apiKey`) |

**Table**:

| Column | Notes |
|---|---|
| Key | The **full** secret: `identifier . token` (decrypted). Copyable, with a clipboard icon. |
| Description | `memo`, wrapped, limited to 50 characters. |
| Last Used | Sortable. Placeholder "Never Used". |
| Created | Sortable. |
| Created By | The owner's username. Links to the user edit page when the viewer can update that user. |

- Row action: **Delete**, a plain Eloquent delete with no activity log.
- Toolbar: **Create**.
- Empty state: "No API keys".

**Form**:

| Field | Behaviour |
|---|---|
| Set All Permissions | Toggle buttons: None (0), Read (1), Read & Write (3). Default 0. Live: setting it sets every per-resource toggle to the same value. |
| Permissions | Fieldset with one toggle-button group per resource in `ApiKey::getPermissionList()` (None / Read / Read & Write). Each is required, default 0. The label is the resource name in title case with underscores turned into spaces. |
| Whitelisted IPv4 Addresses | Tags input `allowed_ips`. Helper text: "API keys can be restricted to only work from specific IPv4 addresses…". No format validation; model rule `array` of strings. |
| Description | Textarea `memo`. Required. Model rule: max 500. |

**Resource names** (`ApiKey::DEFAULT_RESOURCE_NAMES` plus `ApiKey::registerCustomResourceName()` from plugins): `server`, `node`, `allocation`, `user`, `egg`, `database_host`, `server_database`, `mount`, `role`, `plugin`, `webhook`. There is no `backup_host` entry.

**Permission levels**:

- They are a bitmask (`AdminAcl`): `NONE=0`, `READ=1`, `WRITE=2`. The UI offers only 0, 1 and 3 (3 = read + write).
- `AdminAcl::can($perm, $action)` returns `($perm & $action) != 0`.
- Permissions are stored as JSON on `api_keys.permissions`, for example `{"server":3,"node":1,...}`.
- Legacy note: the old per-resource columns `r_servers`, `r_nodes`, `r_allocations`, `r_users`, `r_eggs`, `r_database_hosts`, `r_server_databases` and `r_mounts` were converted into this JSON map and dropped by `database/migrations/2024_11_04_185326_revamp_api_keys_permissions.php`. Their keys map in that order to `server`, `node`, `allocation`, `user`, `egg`, `database_host`, `server_database`, `mount`.

**Create** (`CreateApiKey::handleRecordCreation`):

- `identifier` = `ApiKey::generateTokenIdentifier(2)`. That is the prefix `papp_` (account keys use `pacc_`) followed by random characters, 16 characters in total.
- `token` = 32 random characters, stored **encrypted**.
- `user_id` = the current admin.
- `key_type = 2`.
- `permissions` = `{resource: int}` built from the `permissions_*` fields.
- The bearer string used by API clients is `identifier . token` (48 characters). Lookup splits off the first 16 characters as the identifier (`ApiKey::findToken`).

#### Webhooks (`/admin/webhooks`)

Sources: `app/Filament/Admin/Resources/Webhooks/WebhookResource.php`, `Pages/*`, `app/Extensions/Webhooks/{WebhookForm,WebhookPreview,WebhookTypeService}.php`, `app/Extensions/Webhooks/Schemas/*`, `app/Models/WebhookConfiguration.php`, `app/Jobs/ProcessWebhook.php` and `app/Listeners/DispatchWebhooks.php`.

| Aspect | Detail |
|---|---|
| Pages | List, Create, View `/{id}`, Edit `/{id}/edit`. Model: `WebhookConfiguration`, **soft-deleting**. |
| Navigation | "Webhooks", group Advanced, badge = total count (both scopes) |
| Policy | `WebhookConfigurationPolicy` (default admin policies on `webhook`) |

**List page**:

- Tabs, one per `WebhookScope`, each with a count badge:
  - "Global Webhooks" (`global-webhooks`) filters `scope = global`.
  - "Server Webhooks" (`server-webhooks`) filters `scope = server`. Server-scoped webhooks are created by users in the server panel; admins can view and edit them here.
- The header **Create** action is hidden on the Server tab. The table toolbar still has its own Create.

**Table**:

| Column | Notes |
|---|---|
| Name | |
| Description | |
| Type | Icon with tooltip = type label. An unregistered type shows a puzzle-off icon with "Unavailable Type". |
| Server | Server name with a server icon. Placeholder "—". |
| Endpoint | Scheme stripped (shows the part after `://`). Limited to 60 characters, wrapped. |

- Grouping option: by `server.name`.
- Filters (persisted in the session): **Type** (select of registered types) and **Server** (select of all servers by name).
- Row actions:
  - View (only when the user cannot edit).
  - Edit.
  - **Replicate** (icon only, no modal). Copies every attribute except timestamps, appends " Copy {Y-m-d H:i:s}" to the name, and redirects to the replica's edit page.
- Toolbar: Create and bulk Delete (soft delete).
- Empty state: "No Webhooks".

**Form** (tabs, active tab persisted):

*Information* tab:

| Field | Behaviour |
|---|---|
| Name | Required. |
| Server | Select (relationship `server`), always **disabled**. It only displays a server-scoped webhook's server. |
| Description | Required. |
| Type | Toggle buttons from `WebhookForm::typeSelector()`. Options, icons and colours come from the registered types; default `regular`. A stored type with no registered schema is still shown, as "{type} (unavailable)". |
| scope | Hidden. `server` if `server_id` is set, otherwise `global`. |
| Endpoint | Required, with no URL-format rule in the UI. On blur it calls `WebhookTypes::detectFor(new, old, currentType)`. The type is switched to whichever schema `matchesEndpoint()`, falling back to `regular`, but only if the current type is the default or was itself the result of detection for the previous endpoint. |

*Payload* tab: the selected type's `getFormComponents(scope)`.

- **Regular** type (`RegularSchema`):
  - **Headers** key-value editor, default `{"X-Webhook-Event": "{{event}}"}`.
  - Hint action **Reset Headers** (danger) restores that default.
  - On save, spaces in header names are replaced with `-`.
- If the type provides a preview component, the section becomes an aside layout. It shows a Livewire `WebhookPreview` that re-renders on the `webhook-form-changed` event and polls every 15 s. The preview substitutes `{{vars}}` with sample data.
- If the type is not registered, a warning callout appears: "This webhook uses the ':type' type, which is provided by a plugin that is not installed or enabled. It keeps firing with its saved payload, but cannot be edited until that plugin is available again."

*Events* tab: **events** checkbox list. Searchable, bulk toggle, 3 columns, required, live.

- Options come from `WebhookConfiguration::filamentCheckboxList(scope)`.
- **Global scope** uses `allPossibleAdminOnlyEvents()`, with labels from `transformClassName`. For example `eloquent.created: App\Models\User` becomes "created: User", and `App\Events\User\Deleting` becomes "event: User\Deleting". The list is computed by scanning `app/Models` and `app/Events` and filtering by name. With the current code base it is:
  - `eloquent.{created,updated,deleted}` for the models Allocation, ApiKey, DatabaseHost, Egg, EggVariable, Mount, Node, NodeRole, Plugin, Role, User, WebhookConfiguration, Server, ServerTransfer, ServerUserSettings and ServerVariable.
  - `eloquent.{updated,deleted}: App\Models\Webhook`. The `created` event is blacklisted.
  - `eloquent.*: App\Models\HasAccessTokens`, an artefact: a trait file picked up by the model scan.
  - The custom events `App\Events\ActivityLogged`, `App\Events\User\Deleting` and `App\Events\User\PasswordChanged`.
  - Plugins can append events by listening to `global:webhook.events` (passed by reference).
- **Server scope** uses `allPossibleServerEvents()`, a fixed list of activity names. Labels drop the `server:` prefix, show `.` as " → ", and use title case:
  - `server:file.{read,write,rename,copy,compress,decompress,delete,create-directory,uploaded,pull,download}`
  - `server:power.{start,stop,restart,kill}`
  - `server:console.command`
  - `server:startup.{edit,image}`
  - `server:settings.{rename,description,reinstall}`
  - `server:allocation.{notes,primary,create,delete}`
  - `server:schedule.{create,update,execute,delete}`
  - `server:task.{create,update,delete}`
  - `server:backup.{start,delete,download,rename,restore,restore-complete,restore-failed}`
  - `server:database.{create,rotate-password,delete}`
  - `server:subuser.{create,update,delete}`
  - `server:sftp.denied`
  - Plugins extend this list through the `server:webhook.events` event.

**Create** (`CreateWebhookConfiguration`):

- Header actions: Cancel (back arrow) and Create (Ctrl/Cmd+S).
- On mount it shows the info banner "Help: You have to wrap variable name in between {{ }} for example if you want to get the name from the api you can use {{name}}.".
- Before create:
  - `name` is required.
  - `scope` is forced to `global` and `server_id` is removed, so admin-created webhooks are always global.
  - The data goes through the type schema's `mutateFormDataBeforeSave`.
- Redirects to the Edit page.

**Edit** (`EditWebhookConfiguration`):

- Shows the same help banner.
- Fill and save go through the type schema's `mutateFormDataBeforeFill` / `mutateFormDataBeforeSave`.
- Header actions:
  - **Delete** (soft delete).
  - **Test Now** (test-pipe icon). Disabled when no events are selected. Runs `$config->run()`:
    - a global webhook queues `ProcessWebhook` with event `eloquent.created: App\Models\Server` and the large server sample payload from `getWebhookSampleData()`;
    - a server webhook uses event `server:file.write` and `getServerWebhookSampleData()`.
  - **Save**.

**Model side effects**: on save or delete, the cache keys `webhooks.{event}` (global webhooks per event) and `watchedWebhooks` (all events with a global webhook) are rebuilt.

**Delivery** (summary; the dispatch internals may be covered elsewhere):

1. `DispatchWebhooks` listens for Eloquent events, custom events and `ActivityLogged`. It builds `{event, data: model->toArray() | event public props, timestamp: ISO-8601}` and calls `run($eventName, $data)` on each matching global webhook.
2. `ProcessWebhook` (queued, `$tries = 3`) sets `data['event']` to the readable event name (for example "created: Server").
3. It resolves the schema from `WebhookTypeService`. If no schema is registered for the type, it uses `FallbackSchema`.
4. It calls `preparePayload`, `prepareHeaders`, `deliver`, then `wasSuccessful`.
5. It records a `webhooks` row with `payload`, `successful_at` (null on failure), `event` and `endpoint`. These rows are pruned after `APP_WEBHOOK_PRUNE_DAYS`.
6. The delivery is retried only when the schema's `retryAfter()` returns a number of seconds.

What each type sends:

- **Regular**: `POST` JSON whose body is the event data as-is. Headers are the configured key/value pairs, with `{{path}}` placeholders resolved by `data_get` against the event data. A placeholder with no match is replaced by its own name. Timeout: `panel.webhook.timeout` (`APP_WEBHOOK_TIMEOUT`, default 30 s). Success means a 2xx status.
- **Discord**: there is **no Discord schema in core** in this version. Only `regular` is registered (`app/Providers/Extensions/WebhookServiceProvider.php`). Migration `2025_04_09_015500_add_webhook_configurations_type_column.php` still tags existing rows whose host matches `discord(app)?.com` with `type = 'discord'`, and the comments mention Discord tokens and `Retry-After`. That points to a plugin providing the Discord type. Without that plugin, such rows are delivered by `FallbackSchema`: the stored `payload` JSON template, with `{{var}}` replaced recursively in string values (or the raw event data if `payload` is empty), plus the stored headers.

#### Plugins (`/admin/plugins`)

Sources: `app/Filament/Admin/Resources/Plugins/PluginResource.php`, `Pages/ListPlugins.php`, `app/Models/Plugin.php`, `app/Services/Helpers/PluginService.php` and `app/Jobs/Plugin/*`. Plugin internals are documented elsewhere.

| Aspect | Detail |
|---|---|
| Pages | List only. The `Plugin` model is a **Sushi** in-memory model built from the plugins directory and its `plugin.json` manifests, not a regular DB table. It has a string primary key (the plugin id). |
| Navigation | "Plugins", ungrouped, badge = plugin count |
| Policy | `PluginPolicy` (default admin policies on `plugin`; uses `viewList`, `create`, `update`, `delete`) |
| Safe mode | When `PANEL_PLUGIN_SAFE_MODE` is true, the list page shows the warning banner "Safe mode is on, so no plugins are loaded. Unset PANEL_PLUGIN_SAFE_MODE to load them again." |

**Tabs**: "All" plus one tab per `PluginCategory` ("Plugin", "Theme", "Language Pack"), each with a count badge.

**Table**:

- Reorderable by `load_order`. The reorder toggle has the tooltip "Change load order" / "Apply load order" and is authorised by `update plugin`. Saving calls `PluginService::updateLoadOrder($order)`.
- Default sort: `load_order`.

| Column | Notes |
|---|---|
| Name | Shows the description truncated to 80 characters. A versions icon is green, or red with the tooltip "An update for this plugin is available" when the update feed has a newer version. Searchable, sortable. |
| Author | Sortable. |
| Version | Sortable. |
| Category | Badge. Shown only on the All tab. |
| Status | Badge: Not Installed / Disabled / Enabled / Errored / Incompatible / Crashed. Tooltip = `status_message`. Description "⚠ plugin.json does not declare an api_version, assuming 1" when `api_version` is missing. |

**Row actions**:

| Action | Visible when | Effect |
|---|---|---|
| View | The plugin has a README or a `url` | With a README: slide-over rendering it as markdown, plus a "Visit Website" button when `url` is set. Without a README: opens `url` in a new tab. |
| Settings | `status = Enabled` and the plugin defines settings; authorised by `update plugin` | Slide-over form from the plugin (`getSettingsForm`, `getSettingsFormData`); `saveSettings($data)` on submit. |
| *Group:* Install | `status = NotInstalled` | Queues `InstallPlugin`. It enables the plugin unless it is a theme and another theme is already enabled. Notification "Plugin install started in the background"; the result arrives later as a database notification. |
| *Group:* Update | Update feed has a newer version | Queues `UpdatePlugin`. |
| *Group:* Enable | `canEnable()`: Disabled or Crashed, **and** compatible with the panel version | `PluginService::enablePlugin`. Asks for confirmation ("Theme already enabled …") when enabling a theme while another theme is enabled. |
| *Group:* Disable | Enabled or Incompatible | `PluginService::disablePlugin`. |
| *Group:* Delete | NotInstalled or Errored; authorised by `delete plugin`; asks for confirmation | `PluginService::deletePlugin` (removes the files). |
| *Group:* Uninstall | Any installed state; asks for confirmation | Queues `UninstallPlugin`. |

All group actions except Delete are authorised by `update plugin`. Enable, Disable and Delete reload the list on the current tab.

**Header actions** (icon only, authorised by `create plugin`):

- **Import from File**: required zip upload (`application/zip` and variants) → `PluginService::downloadPluginFromFile`.
- **Import from URL**: required URL ending in `.zip` → `downloadPluginFromUrl` (size limit `PANEL_PLUGIN_MAX_IMPORT_SIZE`, default 100 MB).
- For both: if the imported id is already installed, `InstallPlugin` is queued with `reinstall: true`, which keeps its enabled/disabled state. Notifications: "Plugin imported" or "Could not import plugin" with the message. Import errors include "A plugin with that id already exists", "The zip does not contain a valid plugin.json", an invalid id, an id mismatch and a checksum mismatch.

There is **no hub or browse UI** in the admin panel. `PANEL_PLUGIN_HUB_URL` and `PANEL_PLUGIN_HUB_API_KEY` are used only for the update feed.

#### Mounts (`/admin/mounts`)

Sources: `app/Filament/Admin/Resources/Mounts/MountResource.php`, `Pages/*` and `app/Models/Mount.php`.

| Aspect | Detail |
|---|---|
| Pages | List, Create, View `/{id}`, Edit `/{id}/edit` |
| Navigation | "Mounts", group Advanced, badge = count of visible mounts |
| Query scope | Mounts with **no** nodes, or with at least one node in `user()->accessibleNodes()`. |
| Policy | `MountPolicy`: default admin policies on `mount`, plus `before()` denying the record if any linked node is not targetable. |

**Table**:

| Column | Notes |
|---|---|
| Name | Description line `"{source} -> {target}"`. Sortable. |
| Eggs | Badges. Placeholder "All Eggs". |
| Nodes | Badges. Placeholder "All Nodes". |
| Read Only | Badge: "Read Only" (success) or "Writable" (warning). |
| User Mountable | Badge: "User Mountable" (success) or "Admin Only" (warning). |

- Row actions: View (only when the user cannot edit) and Edit.
- Toolbar: Create and bulk Delete.
- Empty state: "No Mounts".

**Form**:

| Field | Rules / default |
|---|---|
| Name | Required, max 64. Helper: "Unique name used to separate this mount from another." Model: string, 2–64 characters, unique. |
| Read Only? | Toggle buttons Writable (false) / Read Only (true). Default false. |
| User Mountable? | Toggle buttons Admin Only (false) / User Mountable (true). Default **true**. Helper: whether users can toggle the mount on or off for their servers. |
| Source | Required, max 255. The host path. Model `not_in`: `/etc/pelican`, `/var/lib/pelican/volumes`, `/srv/daemon-data`. |
| Target | Required, max 255. The path inside the container. Model `not_in`: `/home/container`. |
| Description | Textarea. Model: nullable, max 255. |
| Eggs | Multi-select relationship (polymorphic `mountables`), preloaded. |
| Nodes | Multi-select relationship, limited to accessible nodes, searchable by name or FQDN, preloaded. |

- **Create**: generates `uuid` when missing, then Eloquent create.
- **Edit** header actions: Delete and Save.
- **View** header action: Edit.
- No Wings call is made. Mounts reach Wings through the server configuration.

#### Database Hosts (`/admin/database-hosts`)

Sources: `app/Filament/Admin/Resources/DatabaseHosts/DatabaseHostResource.php`, `Pages/*`, `RelationManagers/DatabasesRelationManager.php`, `app/Services/Databases/Hosts/{HostCreationService,HostUpdateService}.php` and `app/Filament/Components/Actions/RotateDatabasePasswordAction.php`.

| Aspect | Detail |
|---|---|
| Pages | List, Create (**wizard**), View `/{id}`, Edit `/{id}/edit` |
| Navigation | "Database Hosts", group Advanced, badge = count of visible hosts |
| Query scope | Hosts with no linked nodes, or with at least one accessible node. |
| Policy | `DatabaseHostPolicy`: default admin policies on `databaseHost`, plus `before()` denying the record if any linked node is not targetable. |

**Table**:

| Column | Notes |
|---|---|
| Name | |
| Host | |
| Port | |
| Username | |
| Databases | Count. |
| Nodes | Badges. Placeholder "No Nodes". |

- A row can be selected for bulk delete only when it has no databases.
- Row actions: View (only when the user cannot edit) and Edit.
- Toolbar: Create and bulk Delete.
- Empty state: "No Database Hosts".

**Create wizard** (`CreateDatabaseHost`, submit button "Create", Ctrl/Cmd+S):

*Step 1, "Preparations"*:

- Note: "Currently, only MySQL/ MariaDB databases are supported for database hosts!".
- Toggle "Are the panel and the database *not* on the same server?" (not saved). When turned on, it sets the hidden `panel_ip` to `gethostbyname(<APP_URL host>)`. The default is `127.0.0.1`; turning the toggle off does **not** reset it.
- **Username**: required, default `pelicanuser`, max 255.
- **Password**: required, default a random 16-character `Str::password`, revealable, max 255.
- After this step validates, the SQL commands for step 2 are generated.

*Step 2, "Database Setup"*:

- "Database User" fieldset with instructions ("Use `mysql -u root -p` …") and two read-only, copyable commands:
  - `CREATE USER '{user}'@'{panel_ip}' IDENTIFIED BY '{password}';`
  - `GRANT ALL PRIVILEGES ON *.* TO '{user}'@'{panel_ip}' WITH GRANT OPTION;`
- Exit instructions.
- "External Access" text explaining how to set `bind-address=0.0.0.0` in `my.cnf` and open port 3306.

*Step 3, "Panel Setup"*:

| Field | Rules |
|---|---|
| Host | Required, max 255. On blur its value is copied into Display Name. Helper: the address the panel uses to connect. |
| Port | Required, numeric, 0–65535, default 3306. Model rule: `between:1,65535`. |
| Max Databases | Numeric, optional. Placeholder "Unlimited"; blank means unlimited. |
| Display Name | Required, max 60. Shown to end users. |
| Linked Nodes | Multi-select of accessible nodes. Helper: "This setting only defaults to this database host when adding a database to a server on the selected Node." |

**On create**, `HostCreationService::handle` runs in one DB transaction:

1. Creates the host. Model rules: name max 255, host required, port 1–65535, **username max 32**, password nullable; the password is stored encrypted per the model.
2. Syncs `node_ids`.
3. Calls `$host->buildConnection()->getPdo()` to **test the credentials**.

On a `PDOException` the transaction rolls back, the notification "Error connecting to host" with the PDO message is shown, and the action halts.

**Edit form** (a flat section):

- Host (copied into Name on blur), Port, Max Databases, Display Name, Username, and Password (revealable; required only on create; leaving it blank keeps the current one), Linked Nodes.
- `HostUpdateService` updates in a transaction, re-tests the connection with PDO, and shows the same error handling.
- Edit header actions:
  - Delete: disabled when the host has databases (tooltip "Database Host Has Databases"); otherwise a plain Eloquent delete.
  - Save.

**Relation manager "Databases"** (`databases` on the host):

| Column | Notes |
|---|---|
| database | |
| Username | |
| Connections From | `remote`. `%` is shown as "Anywhere ( % )". |
| Server name | Links to the admin server edit page when the user can update the server. |
| Max Connections | 0 or empty is shown as "Unlimited". |
| Created At | |

- Empty state: "No Databases".
- Row action **View** opens a modal with Database, Username, Password (revealable), Connections From, Max Connections, and the JDBC Connection String (revealable).
  - The Password field has the hint action **Rotate** (`RotateDatabasePasswordAction`): it asks for confirmation ("Rotate Password", warning style) and is authorised by `update` on the Database.
  - Rotate calls `DatabaseManagementService::rotatePassword`. Inside a transaction it generates a new random 24-character password with special characters, drops the MySQL user, recreates it, re-grants it on the database, and flushes privileges.
  - It then refreshes the password and JDBC fields, logs activity `server:database.rotate-password` (property `name`), and shows "Password Rotated" or "Password Rotation Failed". The exception text is shown only to admins.
- Row action **Delete**: the default Filament `DeleteAction`, which deletes only the `databases` **row**. It does **not** drop the database or user on the MySQL host. The Servers resource's databases relation manager does drop them, through `DatabaseManagementService::delete`.
- Visibility: the relation manager is shown when the user passes `viewAny` on `Database`. `DatabasePolicy` checks subuser permissions against `Filament::getTenant()`, which is null in the admin panel, so in practice only Root Admins see it (derived from the code, not runtime-tested).

#### Backup Hosts (`/admin/backup-hosts`)

Sources: `app/Filament/Admin/Resources/BackupHosts/BackupHostResource.php`, `Pages/*`, `RelationManagers/BackupsRelationManager.php`, `app/Extensions/BackupAdapter/**`, `app/Models/BackupHost.php` and migration `2026_01_16_081858_create_backup_hosts_table.php`.

| Aspect | Detail |
|---|---|
| Pages | List, Create, View `/{id}`, Edit `/{id}/edit` |
| Navigation | "Backup Hosts" (`trans_choice('admin/backuphost.model_label', 2)`), group Advanced, badge = count |
| Query scope | None: every host is listed. |
| Policy | `BackupHostPolicy`: default admin policies on `backupHost`, plus `before()` denying a record if any linked node is not targetable. |
| Data model | `backup_hosts(id, name, schema, configuration JSON, timestamps)`. Pivot `backup_host_node(node_id UNIQUE, backup_host_id)`: **a node can belong to at most one backup host**. `backups.backup_host_id` is a required FK. The migration seeded one host, "Local" (`wings`) or "Remote" (`s3`, built from the legacy `AWS_*` / `APP_BACKUP_DRIVER` env values), and assigned every existing backup to it. |

**Table**:

| Column | Notes |
|---|---|
| Name | |
| Schema | Badge with the schema id. |
| Backups | Count. |
| Nodes | Badges. Placeholder "All Nodes". |

- Row actions: View (only when the user cannot edit) and Edit.
- Toolbar: **Create** only. There is no bulk delete.
- Empty state: "No Backup Hosts" with the description "All backups will be created locally on the respective node".

**Form**:

| Field | Behaviour |
|---|---|
| Name | Required. Model: max 255. |
| Schema | Required, searchable select with no empty option. Options come from `BackupAdapterService::getMappings()`: `wings` = "Wings" and `s3` = "S3"; plugins may register more. Live on blur; it drives the Configuration section. |
| Linked Nodes | Multi-select of accessible nodes (relationship `nodes`). Picking a node already linked to another host violates the unique pivot constraint (see Open questions). |
| Configuration section | Built by the selected schema's `getConfigurationForm()`. **wings**: the static text "No additional configuration required". **s3**: see the table below. |

S3 configuration fields (stored inside the `configuration` JSON):

| Label | Key | Rules |
|---|---|---|
| Default Region | `configuration.region` | Required |
| Access Key ID | `configuration.key` | Required |
| Secret Access Key | `configuration.secret` | Required; plain text, not masked |
| Bucket | `configuration.bucket` | Required |
| Endpoint | `configuration.endpoint` | Required |
| Use Path Style Endpoint | `configuration.use_path_style_endpoint` | Toggle (boolean) |

Configuration keys imported by the migration but not editable in the UI: `prefix`, `use_accelerate_endpoint`, `storage_class`. `storage_class` is honoured on uploads.

- **Create**: header action Create; a plain Eloquent create.
- **Edit** header actions:
  - **Delete**: hidden when only one backup host exists. Disabled when the host has backups, and in that case its label reads "Backup host has backups".
  - **Save**.
- **View** header action: Edit.

**Relation manager "Backups"** (read-only, no actions):

| Column | Notes |
|---|---|
| Server name | Links to the admin server edit page. Searchable. |
| Name | Searchable. |
| Size | Bytes column. |
| Created At | Relative time, sortable. |
| Status | Badge. |
| Locked | Lock or unlock icon. Shown from `md`. |

As with Databases, `BackupPolicy` checks subuser permissions against a null tenant, so this list is effectively shown only to Root Admins (derived from the code).

**How hosts are used** (`app/Services/Backups/InitiateBackupService.php`): a new backup uses the first backup host linked to the server's node. If there is none, it uses the first host with **no** linked nodes, which acts as the global default. If neither exists, it fails (`firstOrFail`).

Adapter behaviour:

| Adapter | Create | Delete | Download |
|---|---|---|---|
| `wings` (`WingsBackupSchema`) | Wings creates the backup locally (`DaemonBackupRepository::create`). | Through Wings. A 404 from Wings is ignored. | A link to `{node}/download/backup?token=` with a node JWT valid for 15 minutes (scope `BackupDownload`, claims `backup_uuid` and `server_uuid`). |
| `s3` (`S3BackupSchema`) | Wings is told to create the backup. It then requests presigned multipart `UploadPart` URLs from the panel (lifespan `BACKUP_PRESIGNED_URL_LIFESPAN`, default 60 min; part size `BACKUP_MAX_PART_SIZE`; `BACKUP_MAX_CONCURRENT_UPLOADS`). The panel completes or aborts the multipart upload. | The panel calls S3 `DeleteObject` directly. | A presigned `GetObject` URL valid for 5 minutes. |

S3 object key: `{server_uuid}/{backup_uuid}.tar.gz`.

## Admin panel (part B): Nodes, Servers, Eggs, shared components

### Cross-cutting conventions (apply to all three resources)

| Topic | Behaviour | Source |
|---|---|---|
| Panel / URL prefix | Admin panel id `admin`, path `/admin`. Resource slugs are Filament defaults derived from model names: `/admin/nodes`, `/admin/servers`, `/admin/eggs`. | `app/Providers/Filament/AdminPanelProvider.php` |
| Pages per resource | `index` `/`, `create` `/create`, `view` `/{record}`, `edit` `/{record}/edit` (record key = numeric `id`). | `getDefaultPages()` in each `*Resource.php` |
| Navigation | Group "Server" (`admin/dashboard.server`), unless the user's customization `TopNavigation` = `topbar` (then no group). Labels "Nodes" / "Servers" / "Eggs". Nav badge = record count (Nodes and Servers counted through the scoped query; Eggs counts all eggs). | `getNavigationGroup()`, `getNavigationBadge()` |
| Permission model | Spatie-style permission strings `"<prefix> <model>"` with prefixes `viewList`, `view`, `create`, `update`, `delete` (`app/Enums/RolePermissionPrefixes.php`) and models `node`, `server`, `egg`, `allocation`, ... (`app/Enums/RolePermissionModels.php`). Eggs additionally have `import egg` and `export egg` (`Role::MODEL_SPECIFIC_PERMISSIONS`). Users with the `Root Admin` role pass every gate (`Gate::before` in `app/Providers/AppServiceProvider.php`). | `app/Models/Role.php`, `app/Policies/DefaultAdminPolicies.php` |
| Policy mapping | `DefaultAdminPolicies` trait: `viewAny`→`viewList X`, `view`→`view X`, `create`→`create X`, `update`→`update X`, `delete`/`deleteAny`→`delete X`, `replicate`→`create X`. Filament's default `canAccess()` = `viewAny` (Filament default). | `app/Policies/DefaultAdminPolicies.php` |
| Node scoping | A user may be restricted to a subset of nodes through roles (`NodeRole`). `User::accessibleNodes()` returns all nodes for root admins or when none of the user's roles has node restrictions; otherwise only nodes attached to their roles. `NodePolicy::before` and `ServerPolicy::before` deny any record-level ability when `!$user->canTarget($node)`. NodeResource/ServerResource list queries are filtered to accessible nodes. | `app/Models/User.php`, `app/Policies/NodePolicy.php`, `app/Policies/ServerPolicy.php` |
| Row click | Filament default (verified in vendor `ListRecords`): row URL = the first visible of the `view`/`edit` row actions. All three lists hide the View action when the user may edit, so a row click opens Edit if authorized, otherwise View. | `ListNodes.php`, `ListServers.php`, `ListEggs.php` |
| View pages | Read-only rendering of the same tab schema (`$operation === 'view'`); header action "Edit". Relation managers on View pages are read-only (Filament default `isReadOnly()` = page is a `ViewRecord`; verified in vendor). | `ViewNode.php`, `ViewServer.php`, `ViewEgg.php` |
| Save button | Edit pages remove the bottom form buttons (`getFormActions() => []`) and add an icon-only header action `save` with keyboard shortcut `mod+s` (Ctrl/Cmd+S). Egg create page has an equivalent `create` header action with `mod+s`. | `EditNode.php`, `EditServer.php`, `EditEgg.php`, `CreateEgg.php` |
| Tabs in URL | Edit/View/Create tab sets use `persistTabInQueryString()`: query param `?tab=<slug-of-translated-label>::data::tab` (Filament derives the key from the *translated label*; verified in vendor `Tab::setUp`). | e.g. `EditServer.php` |
| Create redirect | No custom `getRedirectUrl()`; Filament default (verified in vendor `CreateRecord::getRedirectUrl`) redirects to the **View** page if the user can view, else Edit. "Create another" is disabled on all three create pages. | `CreateNode.php`, `CreateServer.php`, `CreateEgg.php` |
| Plugin extension points | Pages/relations/tabs/steps/header actions/header widgets can be extended by plugins via traits `CanCustomizePages`, `CanCustomizeRelations`, `CanCustomizeTabs`, `CanCustomizeSteps`, `CanCustomizeHeaderActions`, `CanCustomizeHeaderWidgets` (static registries; tabs can be inserted before/after defaults). | `app/Traits/Filament/*` |
| Units | Memory/disk/upload inputs are in MiB when `config('panel.use_binary_prefix')` is true, else MB (suffix only; stored number is unchanged). | various |
| Global search | Admin panel uses `GlobalSearchModalPlugin`. Eggs are searchable by `name, tags, uuid, id`; Nodes and Servers by `name` (Filament default derived from `$recordTitleAttribute`). | `EggResource::getGloballySearchableAttributes()` |
| Selects | All `Select` / `SelectColumn` are non-native (searchable dropdown) globally. | `app/Providers/Filament/FilamentServiceProvider.php` |
| Editable table columns | `TextInputColumn` / `SelectColumn` / `ToggleColumn` write directly to the row's model on change. Filament checks only `isDisabled()`/`isHidden()`, not policies (verified in vendor `HasColumns::updateTableColumnState`). Model-level validation still runs (models using `HasValidation` validate on `saving` via `app/Observers/ValidationObserver.php`). | — |

---

### Nodes

Files: `app/Filament/Admin/Resources/Nodes/NodeResource.php`, `Pages/{ListNodes,CreateNode,ViewNode,EditNode}.php`, `RelationManagers/{AllocationsRelationManager,ServersRelationManager}.php`, `Widgets/{NodeCpuChart,NodeMemoryChart,NodeStorageChart}.php`, `resources/views/filament/components/node-{cpu,memory,storage}-chart.blade.php`, `app/Livewire/{NodeSystemInformation,NodeClientConnectivity}.php`, `resources/views/livewire/node-client-connectivity.blade.php`, model `app/Models/Node.php`.

#### Routes and access

| URL | Page | Access |
|---|---|---|
| `/admin/nodes` | ListNodes | `viewList node`; rows limited to `accessibleNodes()` |
| `/admin/nodes/create` | CreateNode (2-step wizard) | `create node` |
| `/admin/nodes/{id}` | ViewNode (tabs, read-only) | `view node` + `canTarget(node)` |
| `/admin/nodes/{id}/edit` | EditNode (tabs) | `update node` + `canTarget(node)` |

Delete requires `delete node` (+ canTarget) and is impossible while the node has servers.

#### List `/admin/nodes`

| Column | Source / format | Search | Sort | Visibility |
|---|---|---|---|---|
| UUID | `uuid` | yes | – | hidden (search only) |
| Health | `NodeHealthColumn` (lazy Livewire `NodeSystemInformation`, see below) | – | – | always |
| Reachable | `NodeClientHealthColumn` (lazy Livewire `NodeClientConnectivity`, browser-side check) | – | – | always |
| Name | `name` | yes | yes | always |
| Address | `fqdn` | yes | yes | ≥ md |
| SSL | icon: lock if `scheme === 'https'`, open lock otherwise | – | – | ≥ xl |
| Public | icon eye-check / eye-cancel from `public` | – | – | ≥ lg |
| Servers | `servers_count` (withCount) | – | yes | ≥ sm |

- Default sort: none configured → Filament default key sort (`id` asc). Pagination: Filament default options [5, 10, 25, 50], default 10.
- Filter: `TagsFilter` (select one tag; hidden when no node has tags).
- Row actions: View (only if user cannot edit), Edit.
- Toolbar: Create (icon button).
- Rows are selectable only when `servers_count <= 0`, but no bulk actions are defined (selection has no effect).
- Empty state heading "No Nodes".

#### Create wizard `/admin/nodes/create`

Two steps; arrow icon buttons for next/previous; submit button "Create Node". On submit (`mutateFormDataBeforeCreate`): if `behind_proxy` is false, `daemon_listen := daemon_connect`. Model `creating` hook generates `uuid`, a 64-char random `daemon_token` (stored encrypted) and 16-char `daemon_token_id`. No Wings call on create. Redirect: View page with `?tab=configuration-file::data::tab` (the Configuration File tab; the key is the slug of the English label, so it only matches when the UI language is English). On the View page the config is redacted and Auto Deploy/Reset are hidden; the admin must click Edit to get the real token.

**Step 1 "Basic Settings"** and **Step 2 "Advanced Settings"** use the fields below (same fields also appear in the Edit tabs; differences noted).

#### Node field reference (create + edit)

| Field (label) | Type | Validation | Default (create) | Conditional behaviour |
|---|---|---|---|---|
| `fqdn` ("Domain Name" or "IP Address" depending on value) | text, live (1.5 s debounce), autofocus | required; max 255; model rule `notIn:0.0.0.0,127.0.0.1,localhost`; **prohibited** (error "No valid DNS records were found for the provided domain name.") unless the value is an IP or resolves via DNS (A record, then AAAA; `get_ip_from_hostname()` in `app/helpers.php`) | – | Hint: green "Valid DNS: <ip>" / red "Invalid DNS" for hostnames; when the panel request is HTTPS and an IP is entered, warning hint "Consider using a domain name instead of an ip address". Helper text when panel is HTTPS: "Your panel is currently secured via an SSL certificate and that means your nodes require one too." On change: resolves IP into hidden `ip` (not saved) and sets `name` to the first DNS label if it is not numeric. |
| `ip` | hidden, not saved | – | – | resolved IP; on Edit/View pre-filled in `mutateFormDataBeforeFill` (also sets non-persisted `dns` flag) |
| `daemon_connect` ("Port", or "Connection Port" when proxy) | integer | required, 1–65535 | 8080 | Help text differs for proxy mode (Cloudflare advice: 8443). |
| `daemon_listen` ("Listening Port") | integer | required, 1–65535 | 8080 | Visible only when connection = `https_proxy`. Server-side forced to `daemon_connect` when not behind proxy. |
| `name` ("Display Name") | text | required, max 100 | auto from fqdn | – |
| `connection` ("Communicate over SSL") | toggle buttons: `http` "HTTP", `https` "HTTPS (SSL)", `https_proxy` "HTTPS with (reverse) proxy" | – (not saved, `dehydrated(false)`) | `https` if the panel request is HTTPS else `http` | `http` option disabled when the panel itself is served over HTTPS (helper "Your Panel is using a secure SSL connection, so your Daemon must too."). Selecting sets hidden `scheme` (`http`/`https`), `behind_proxy` (true only for proxy), resets `daemon_connect` to 443 (proxy) or 8080, and `daemon_listen` to 8080. On Edit the initial state is derived from `scheme`+`behind_proxy`. |
| `scheme` | hidden | model: required, in:http,https | `https` if panel HTTPS else `http` | – |
| `behind_proxy` | hidden | boolean | false | – |
| `maintenance_mode` ("Maintenance Mode") | toggle Enabled/Disabled | boolean | false | Hint: users can't access servers on the node while enabled. |
| `public` ("Use for Deployments?") | toggle Yes/No | boolean | true | Used by automatic deployment (API) to pick nodes. |
| `tags` ("Tags") | tags input (free text) | array | [] | Used by `TagsFilter` and deployment tags. |
| `upload_size` ("Upload Limit", MB/MiB) | numeric | required, min 1 (model `int`) | 256 | Max upload size through the web file manager. |
| `daemon_base` ("Daemon Base Directory") | text | required, regex `^([\/][\d\w.\-\/]+)$` | `/var/lib/pelican/volumes` | – |
| `daemon_sftp` ("SFTP Port") | integer | required, 1–65535 | 2022 | – |
| `daemon_sftp_alias` ("SFTP Alias") | text | nullable string | – | "Leave empty to use the Node FQDN." Used in SFTP URLs. |
| `unlimited_mem` ("Memory") | toggle Unlimited/Limited (UI only) | – | derived: `memory == 0` | Toggling resets `memory` and `memory_overallocate` to 0. |
| `memory` ("Memory Limit", MB/MiB) | numeric | required, min 0 | 0 | hidden when unlimited (still submitted). 0 = unlimited. |
| `memory_overallocate` ("Overallocate", %) | numeric | required, −1…100 | 0 | hidden when unlimited. −1 disables the limit check. |
| `unlimited_disk` / `disk` / `disk_overallocate` | same pattern as memory | same | 0 | – |
| `unlimited_cpu` / `cpu` ("CPU Limit", %) / `cpu_overallocate` | same pattern | same | 0 | – |
| `id` ("Node ID"), `uuid` ("Node UUID", copy hint) | read-only text | – | – | Edit/View only. |

Capacity semantics (used by transfers and deployment): `Node::isViable(memory, disk, cpu)` — for each resource, if limit > 0 and overallocate ≥ 0, sum of servers' allocation + requested must be ≤ `limit * (1 + overallocate/100)`.

#### Edit/View tabs `/admin/nodes/{id}[/edit]` (`NodeResource::detailTabs()`)

1. **Overview** (both): Fieldset "Node Information": Wings Version (`systemInformation().version` + "(Latest: x)" from GitHub `pelican/wings` releases, `SoftwareVersionService::latestWingsVersion()`, cached `panel.cdn.cache_time` minutes), CPU Threads (`cpu_count`), Architecture, Kernel (`kernel_version`); "Unknown" fallbacks. Then CPU chart, Memory chart, Storage chart (see Charts).
2. **Basic Settings**: fqdn, daemon_connect, daemon_listen, name, connection (see table).
3. **Advanced Settings**: Node ID, Node UUID, tags, upload_size, daemon_base, daemon_sftp, daemon_sftp_alias, public, maintenance_mode, memory/disk/cpu groups.
4. **Configuration File**:
   - Instructions: "Save this file to your daemon's root directory, named config.yml".
   - Code block labelled `/etc/pelican/config.yml` (YAML, copyable) generated by `Node::getYamlConfiguration()`; on the **View** page `token_id` and `token` are replaced with `********`. Structure (`Node::getConfiguration()`): `debug: false`, `uuid`, `token_id`, `token`, `api: {host: 0.0.0.0, port: daemon_listen, ssl: {enabled: !behind_proxy && scheme==https, cert: /etc/letsencrypt/live/<lower fqdn>/fullchain.pem, key: .../privkey.pem}, upload_limit: upload_size}`, `system: {data: daemon_base, sftp: {bind_port: daemon_sftp}}`, `allowed_mounts: [mount sources attached to node]`, `remote: APP_URL`.
   - **Auto Deploy Command** (Edit only): modal (no submit/cancel buttons) with toggle "Type": Standalone / Docker (default Standalone). On open and on toggle, `NodeAutoDeployService::handle()` (`app/Services/Nodes/NodeAutoDeployService.php`) produces the command shown in a read-only textarea with copy button ("To auto-configure your node run the following command:"):
     - API key: finds the **first existing Application API key** (`key_type = 2`, `ApiKey::TYPE_APPLICATION`) whose `permissions.node` JSON contains `1` (`AdminAcl::READ`), regardless of owner; if none exists, creates one via `KeyCreationService` for the current user with memo "Automatically generated node deployment key." and permissions `{node: READ}`. Token = `identifier . token`.
     - Command: `sudo wings configure --panel-url <APP_URL> --token <token> --node <id>` (Docker variant replaces `sudo` with `docker compose exec -it $(docker ps --filter "name=wings" --format "{{.Names}}")`), plus ` --allow-insecure` when the current panel request is not HTTPS.
   - **Reset Authorization Token** (Edit only, danger, confirmation with description about voiding the old token): `NodeUpdateService::handle($node, [], true)` — generates new `daemon_token` (64) + `daemon_token_id` (16), saves a replicated row in a transaction, then calls Wings `POST /api/update` with the new configuration, authenticating with the old token. If Wings is unreachable the DB change is still committed but a `ConfigurationNotPersistedException` is thrown; the UI shows warning "Error connecting to <node>" / "The configuration could not be automatically updated on Wings, you will need to manually update the configuration file." On success: "The daemon token has been reset." and the record is refreshed so the preview shows the new token.
5. **Diagnostics** (Edit only): Section with toggles "Include Endpoints" (default on; panel URLs not obscured), "Include Logs" (default on), slider "Log Lines" 100–500 step 50 (default 200; visible only when logs included). Header actions:
   - **Pull**: Wings `GET /api/diagnostics?log_lines=&include_endpoints=true|false&include_logs=true|false` (5 s timeout). 404 → warning "The requested diagnostic report could not be found. Make sure wings is up to date and try again." Success → shows response body in a 35-row textarea, section inputs disabled, "Logs Pulled!".
   - **Upload** (after pull): multipart `POST https://logs.pelican.dev` with fields `c` = log text, `e` = `14d`; response JSON `url`; persistent notification "Logs Uploaded" with "View Logs" link (new tab); textarea now shows the URL. Failure → "Logs Upload Failed" with status/body.
   - **Clear** (after pull): resets state.

#### Edit header actions and save

| Action | Behaviour |
|---|---|
| Delete (icon) | Filament `DeleteAction`; disabled with tooltip "Node Has Servers" when the node has servers (model `deleting` hook also throws `HasActiveServersException`). Deletes the DB row; no Wings call. Redirect to list (Filament default). |
| Save (`mod+s`) | Saves fields; `mutateFormDataBeforeSave` sets `daemon_listen := daemon_connect` when not behind proxy. `afterSave`: if any changed attribute other than `updated_at, name, tags, public, maintenance_mode, memory, memory_overallocate, disk, disk_overallocate, cpu, cpu_overallocate`, calls Wings `POST /api/update` with `Node::getConfiguration()` (via `DaemonSystemRepository::update`, sent to the node's *new* connection address). On `ConnectionException`: warning "Error connecting to <node>" + manual-update hint instead of the "Saved" notification. |

View page header: Edit.

#### Relation manager: Allocations (`RelationManagers/AllocationsRelationManager.php`)

Shown on node View/Edit (read-only on View). Tab label is Filament default ("Allocations"; the `setTitle()` method is not a Filament hook). Access: AllocationPolicy (admin context, no tenant) → `viewList/create/update/delete allocation` + `canTarget(node)`.

| Column | Behaviour | Search | Sort |
|---|---|---|---|
| ID | toggleable, hidden by default | – | yes |
| IP | **editable select**; options = node IPs (`Node::ipAddresses()`) not already used by another allocation with the same port, plus current IP; disabled when read-only | yes | yes |
| Port | text | yes | yes |
| Alias (`ip_alias`) | **editable text**, placeholder "No alias" | yes | yes |
| Notes | **editable text**, placeholder "No notes" (model max 256) | – | – |
| Servers (`server.name`) | placeholder "No server"; links to admin Edit Server when user can update it; ≥ md | yes | – |

- Pagination options [10, 20, 50, 100, 200, 500]; bulk-select only on current page; only unassigned (`server_id` null) rows selectable.
- Row action: Delete (only for unassigned allocations; model also blocks deleting assigned ones).
- Toolbar:
  - **Create** (icon "world-plus"; hidden when read-only) — modal:
    - `allocation_ip` "IP Address" (required, `ip` rule): select of node IPs (`Node::ipAddresses()`: Wings `GET /api/system/ips` with 1 s timeouts, cached 1 h under `nodes.{id}.ips`; falls back to the fqdn if it is an IP; always adds `0.0.0.0` and `::`). Hint action "Refresh" clears the IP cache; suffix action "Enter Custom IP" (modal with required IP field). Changing IP clears ports.
    - `allocation_alias` "Alias" (optional; "Optional display name to help you remember what these are.").
    - `allocation_ports` "Ports" (required tags input, placeholder `27015, 27017-27019`, split on Tab/space/comma, disabled until an IP is chosen). Each change is normalised by `CreateServer::retrieveValidPorts()` (see Port parsing).
    - Submit → `AssignmentService::handle($node, $data)` inserts one allocation per IP×port (unassigned, unlocked).
  - **Update IPs** (`UpdateNodeAllocations`, warning, confirmation; requires `update` on the node): select "Old IP Address" (distinct IPs of the node's allocations) and "New IP Address" (node IPs, must differ). Updates every allocation of this node with the old IP to the new IP one by one; result notification "Successfully updated :count of :total allocation(s)" (+ ":count allocation(s) failed to update"); warning when none matched.
  - **Delete bulk** (selected unassigned allocations).

#### Relation manager: Servers (`RelationManagers/ServersRelationManager.php`)

| Column | Behaviour |
|---|---|
| Owner (`user.username`) | link to admin Edit User if authorized; searchable |
| Name | link to Edit Server if authorized; searchable, sortable |
| Egg (`egg.name`) | link to Edit Egg if authorized; sortable |
| Primary Allocation (`allocation.id`) | editable select, but options are only the server's *first* allocation (`take(1)`), disabled when read-only or ≤ 1 allocation; placeholder "None" |
| CPU / Memory / Disk | formatted limits (`Server::formatResource(...Limit)`) |
| Databases / Backups | counts, toggleable hidden by default, sortable |

Row actions: View Console (`ViewConsoleAction`), Edit (→ `/admin/servers/{id}/edit`). No toolbar actions. Empty state "No Servers".

#### Charts (Overview tab)

Each blade view (`node-cpu-chart`, `node-memory-chart`, `node-storage-chart`) embeds a Filament `ChartWidget` Livewire component with the current node. All read `Node::statistics()` = Wings `GET /api/system/utilization` (connect/total timeout 1 s), cached with `cache()->flexible("nodes.{id}.statistics", [5, 30])`; on failure all zeros. Fields: `memory_total, memory_used, swap_total, swap_used, load_average1/5/15, cpu_percent, disk_total, disk_used`.

| Widget | Type | Poll | Data | Heading |
|---|---|---|---|---|
| `NodeCpuChart` | line, filled, max 300px | 5 s | point = `cpu_percent × cpu_count` (threads from `systemInformation()`), rounded 2; x-label `H:i:s` in user timezone; rolling history of 60 points kept in the **session** key `node_stats.{id}.cpu_history` | "CPU - :cpu% of :max%" (max = threads×100) |
| `NodeMemoryChart` | line, filled | 5 s | `memory_used` in GiB/GB; history `node_stats.{id}.memory_history` (60 points) | "Memory - :used of :total" |
| `NodeStorageChart` | pie, max 200px | 360 s | [used, unused] in GiB/GB, labels "Used"/"Unused" | "Storage - :used of :total" (bytes humanised) |

#### Health indicators

- `NodeSystemInformation` (`app/Livewire/NodeSystemInformation.php`, lazy-loaded; placeholder = warning "heart-question" icon): reads `Node::systemInformation()` = Wings `GET /api/system` (3 s connect timeout; response must contain `architecture, cpu_count, kernel_version, os, version` and the Wings `User-Agent` must carry the node's `token_id`, otherwise treated as error), cached `cache()->flexible("nodes.{id}.system_information", [30, 360])`. Success → green heartbeat icon, tooltip = Wings version. Error → red "heart-off" icon, tooltip "Error connecting to node! Check browser console for details.", and the exception text is `console.error`-ed in the browser (cURL resolve/connect errors are shortened). No auto-refresh (code TODO: "poll every 10 secs").
- `NodeClientConnectivity` (`app/Livewire/NodeClientConnectivity.php` + blade, lazy): checks reachability **from the admin's browser**. JS `fetch(<scheme>://<fqdn>:<daemon_connect>, {mode: 'no-cors'})` with 5 s timeout → failure = red "Node is not reachable from your browser". If the node has at least one server, the server mints a 1-minute websocket JWT (`NodeJWTService`, scope Websocket, claims `server_uuid`, `permissions` = the admin's permissions on the first server) and the browser opens `ws(s)://.../api/servers/<uuid>/ws`, sends `{event:"auth", args:[token]}` and waits up to 10 s for `auth success`: success = green "Node is reachable"; failure = warning "Node is reachable, but WebSocket failed. Check reverse proxy config." No servers → green "Node is reachable (WebSocket not tested — no servers)".

---

### Servers

Files: `app/Filament/Admin/Resources/Servers/ServerResource.php`, `Pages/{ListServers,CreateServer,ViewServer,EditServer}.php`, `RelationManagers/{AllocationsRelationManager,DatabasesRelationManager}.php`; services under `app/Services/Servers/**`, `app/Services/Allocations/AssignmentService.php`, `app/Services/Eggs/EggChangerService.php`, `app/Services/Databases/DatabaseManagementService.php`; Wings client `app/Repositories/Daemon/DaemonServerRepository.php`.

#### Routes and access

| URL | Page | Access |
|---|---|---|
| `/admin/servers` | ListServers | `viewList server`; rows limited to servers on accessible nodes |
| `/admin/servers/create` | CreateServer (3-step wizard) | `create server` |
| `/admin/servers/{id}` | ViewServer | `view server` + `canTarget(server.node)` |
| `/admin/servers/{id}/edit` | EditServer | `update server` + `canTarget(server.node)` |

`ServerPolicy::before` also grants subuser-permission abilities to owners/subusers, which is irrelevant for the admin abilities above.

#### List `/admin/servers`

- **Grouping**: default group `node.name` (description = node description truncated to 150). Other groups: `user.username` (description = owner email), `egg.name` (description = egg description truncated to 150). The column matching the active group is hidden.
- Table-wide search enabled.

| Column | Behaviour | Search | Sort |
|---|---|---|---|
| Condition | badge with icon/color of `Server::condition` = DB `status` (`installing`, `install_failed`, `reinstall_failed`, `suspended`, `restoring_backup`) or, when null, the live container state (`created, starting, running, restarting, exited, paused, dead, removing, stopping, offline, missing`) fetched from Wings `GET /api/servers/{uuid}` (1 s timeout, cached 15 s; `missing` when node in maintenance); default "unknown" | – | – |
| UUID | hidden | yes | – |
| Name | | yes (`servers.name LIKE %q%`) | yes |
| Node | link to Edit Node if authorized | yes | yes |
| Egg | link to Edit Egg if authorized | yes | yes |
| Username (owner) | link to Edit User if authorized | yes | yes |
| Primary Allocation | if user has `update server`: **editable select** of the server's allocations (disabled when ≤ 1); changing it writes `servers.allocation_id` directly (no Wings sync). Otherwise read-only text (`allocation.address` or "None"). | – | yes |
| Databases / Backups | counts, toggleable hidden by default | – | yes |

Row actions: View (only if user can't edit), View Console, Edit. Toolbar: Create. Empty state "No Servers". No filters, no bulk actions.

#### Create wizard `/admin/servers/create`

Three steps with next/previous arrow buttons and a "Create Server" submit.

**Step 1 – Information**

| Field | Validation / options | Default | Behaviour |
|---|---|---|---|
| `name` "Name" | required, max 255 | – (auto-set when an egg is chosen: egg kebab name if empty or still equal to previous egg's kebab name) | Suffix "Random" (dice) button sets `<egg-name-kebab>-<random bird word>` (`RandomWordService`). |
| `external_id` "External ID" | unique in `servers`, max 255 | – | – |
| `node_id` | required; accessible nodes; searchable | the session's `last_utilized_node` if still accessible, else the most recently created accessible node | Changing resets `allocation_id`. |
| `owner_id` "Owner" | required; users searchable by username/email, label `username (email)` | current user | Inline "create option" (authorized by `create user`): Username (required, alphanumeric, 3–255), Email (required, email, unique, max 255), Password (optional; help: user will be prompted to create one). Uses `UserCreationService` (random password if blank; sends `AccountCreated` notification). |
| `allocation_id` "Primary Allocation" | **optional**; unassigned allocations (`server_id` null) of the chosen node; searchable by ip/port/alias; disabled until a node is chosen | – | Placeholder "Select Allocation" / "Create New Allocation". Inline create (authorized by `create allocation`): same IP/alias/ports form as the node allocation manager (alias offers datalist of server name and egg name); `AssignmentService::handle(node, data)` creates the allocations and the first new id becomes the primary. Changing clears additional allocations. |
| `allocation_additional` "Additional Allocations" | repeater of selects (button "Add Allocation"); each option = unassigned allocation on the node excluding the primary; an allocation chosen in one row is disabled in sibling rows | – | Disabled until a primary is chosen. Empty rows are dropped on submit. |
| `description` "Description" | textarea | – | – |

**Step 2 – Egg Configuration**

| Field | Behaviour |
|---|---|
| `egg_id` "Name" (egg) | required, searchable. On change: clears `startup` and `image`; loads the egg's variables (sorted by `sort`) into the variables repeater with default values; builds hidden `environment` map `{ENV_NAME: default}`; may auto-set server name (see above). |
| `skip_scripts` "Run Install Script?" | Yes (false) / Skip (true); required; default Yes. |
| `start_on_completion` "Start After Install?" | Yes/No; required; default Yes. Passed to Wings on create. |
| `select_startup` "Startup Command" | visible after an egg is chosen; options = the egg's `startup_commands` (label = display name, value = command) + "Custom Startup"; auto-selects the first (default) command. Selecting copies the command into `startup`. |
| `startup` | required textarea (placeholder "Enter a custom startup command"); editing it re-selects the matching preset or "Custom Startup". |
| `environment` | hidden map, kept in sync with variable inputs. |
| Section "Variables" (collapsible) | One `StartupVariable` per egg variable (2-column grid; no add/delete/reorder). All variables are editable by the admin regardless of `user_editable` (`->disabled(false)`). Input type, required flag and min/max derived from the variable's rules (see StartupVariable). Messages "Select an egg first to show its variables!" / "The selected egg has no variables!". Values are not saved through the relationship; they go through `environment`. |

**Step 3 – Environment Configuration**

| Fieldset / field | Options | Default |
|---|---|---|
| Resource Limits: `unlimited_cpu` CPU Unlimited/Limited → `cpu` "CPU Limit" % (required, min 0; hint "100% equals one CPU thread.") | toggling resets value to 0 | Unlimited, 0 |
| `unlimited_mem` → `memory` "Memory Limit" MB/MiB (required, min 0; hint about Wings overhead) | | Unlimited, 0 |
| `unlimited_disk` → `disk` "Disk Space Limit" (required, min 0) | | Unlimited, 0 |
| Advanced Limits: `io` | hidden | 500 (model rule 0–1000) |
| `cpu_pinning` "CPU Pinning" Disabled/Enabled → `threads` "Pinned Threads" | tags input split on `,`, stored as comma-joined string; required when enabled; placeholder "Add pinned thread, e.g. 0 or 2-4"; model regex `^[0-9-,]+$` | Disabled |
| `swap_support` "Swap Memory": Unlimited / Limited / Disabled → `swap` "Swap Memory Limit" | selecting sets swap −1 / 128 / 0; `swap` (integer, min −1, required) visible only for Limited | Disabled (0) |
| `oom_killer` "OOM Killer" Disabled/Enabled | | Disabled |
| Feature Limits: `allocation_limit` "Allocations", `database_limit` "Databases", `backup_limit` "Backups" | required, numeric, min 0 | 0 |
| Docker Settings: `select_image` "Image Name" | options = egg `docker_images` (label = name, value = URI) + "Custom Image" (value `ghcr.io/custom-image`); auto-selects the first image; selecting copies into `image` | first egg image |
| `image` "Image" | required text (placeholder "Enter a custom image"); typing a non-listed value switches the select to "Custom Image" | |
| `docker_labels` "Container Labels" | key/value (Title → Description) | {} |
| Mounts | `ServerResource::getMountCheckboxList()` (see Mounts tab) | none |

**Submit** (`handleRecordCreation`): drops empty additional allocations, stores `last_utilized_node` in session, calls `ServerCreationService::handle($data)`:
1. Fills `image`/`startup` from the egg if missing.
2. Validates `environment` against each egg variable's rules at admin level (`VariableValidatorService`, all variables).
3. In a transaction: creates the server (unique `uuid` + 8-char `uuid_short`, `status = installing`, limits, `skip_scripts`, `oom_killer`, `docker_labels`, ...), assigns primary + additional allocations (only those still unassigned; sets `server_id`, `is_locked = true`), stores `server_variables`.
4. Wings `POST /api/servers` `{uuid, start_on_completion}`. On `ConnectionException` the server is force-deleted (`ServerDeletionService::withForce()`) and the error re-thrown.
Any exception → danger notification "Could not create Server" with the message; the wizard stays open (`Halt`). No deployment/auto-allocation option exists in this UI (the service's `DeploymentObject` path is API-only).

#### Port entry parsing (`CreateServer::retrieveValidPorts()`, used by all three allocation-create forms)

Runs on every change of the Ports tags input and replaces the tags with the resulting list:
- Each entry is `N` or `A-B` (either order); non-numeric → notification "Invalid Port Range" (entry skipped).
- A range larger than 1000 ports (`AssignmentService::PORT_RANGE_LIMIT`) → "Too many ports at one time!" (skipped).
- Each port must satisfy `1024 < port <= 65535` (note: 1024 itself is rejected here, while `AssignmentService` accepts 1024) → otherwise "Port not in valid range".
- Ports already allocated on that node for the same IP → "Port already in use".
- Result is de-duplicated and sorted ascending; ranges are thus expanded into individual tags.
`AssignmentService::handle()` additionally accepts CIDR IPs (/25–/32, `IPTools\Network`) and `NNNN-NNNNN` range strings, but the UI's IP field enforces a plain IP.

#### Edit/View tabs (`ServerResource::detailTabs()`)

On the Edit page the variables repeater calls `Server::ensureVariablesExist()` (creates missing `server_variables` rows with default values — a DB write on page load); the View page passes `materializeVariables = false`.

**1. Information**

| Field | Behaviour |
|---|---|
| Icon | shows server icon, falling back to egg icon (tooltip when inherited). Edit only: Upload Icon / Delete Icon actions (see shared components; stored under `icons/server/<uuid>.<ext>`). |
| `name` | required, max 255; Random button (edit only). |
| `owner_id` "Owner" | required; users by username/email. Changing it is DB-only (no Wings sync). |
| `condition` "Server Status" | single-option toggle showing current condition (`ServerConditionStateCast`). Hint action "View install log" (edit only): modal with read-only Monaco editor containing Wings `GET /api/servers/{uuid}/install-logs` (`data` field, converted to UTF-8); on connection error warning "Could not connect to Wings to retrieve server install log." |
| `description` | textarea (saved as `''` when empty). |
| `uuid`, `uuid_short` "Short UUID" | read-only, copyable, not saved. |
| `external_id` | unique (ignores own record), max 255. |
| `node_id` "Node" | disabled (change node only via Transfer). |

**2. Environment Configuration** — same Resource Limits / Advanced Limits / Feature Limits / Docker Settings fields as create step 3 (no mounts here), with values derived from the record: unlimited toggles from `== 0`, `cpu_pinning` from filled `threads`, `swap_support` from swap sign (>0 limited, 0 disabled, <0 unlimited), `io` hidden and unchanged, `oom_killer` toggle.

**3. Egg**

| Field / action | Behaviour |
|---|---|
| `egg_id` "Name" | disabled select. Hint action **Change Egg** (edit only): modal "New Egg" (required; all other eggs) + toggle "Keep old variables if possible?" (default on) → `EggChangerService::handle()`: sets `egg_id`, `image` = new egg's first docker image, `startup` = its first startup command; deletes all server variables and recreates them for the new egg, reusing old values matched by `env_variable` when "keep" is on, else defaults. No Wings call. Then full redirect to the edit page on the Egg tab (`?tab=egg::data::tab`). |
| `skip_scripts` "Run Install Script?" | Yes / Skip (required). |
| `select_startup` "Startup Command" | presets from egg + "Custom Startup"; initial = current startup if it matches a preset else Custom. Hint action **Enable Preview / Disable Preview** (`PreviewStartupAction`, edit only). |
| `startup` | required textarea; editing syncs the preset select. |
| Variables | `server_variables` relationship repeater (ordered by egg variable `sort`), one `StartupVariable` per row (`fromRecord()`, always editable by admin); empty values saved as `''`. |

**4. Mounts** — `CheckboxList` bound to the `mounts` relationship (bulk toggle). Offered mounts = all mounts, filtered to those whose node list is empty or contains the server's node **and** whose egg list is empty or contains the server's egg. Each option shows `source -> target`. Helper "No Mounts exist for this Node" when none. Saved by relationship sync on Save.

**5. Actions** (Edit only). Each block = button + explanatory hint.

| Button | Enabled / visible | Effect (service → Wings) | Notifications |
|---|---|---|---|
| **Toggle Install Status** | disabled while suspended | If status is `install_failed`/`reinstall_failed`: modal "Server is in failed state — Do you want to reinstall the server to fix this?" (submit "Reinstall") → `ReinstallServerService` (status `installing`, Wings `POST /api/servers/{uuid}/reinstall`, in a DB transaction). Otherwise no modal → `ToggleInstallService`: flips `status` between `installing` and `null` (installed). DB only. | "Install status toggled" / "Could not toggle install status"; "Reinstall started" / "Could not start reinstall" |
| **Suspend** | visible when not suspended | `SuspensionService::handle(server, Suspend)`: refuses if already in that state or a transfer is in progress; sets `status = suspended`; Wings `POST /api/servers/{uuid}/sync`. | "Server has been suspended" / warning "Server Suspension — Error connecting to <node>" |
| **Unsuspend** | visible when suspended | same service with `Unsuspend` (`status = null`, sync). | "Server has been unsuspended" |
| **Transfer** | disabled if the user has ≤ 1 accessible node or `isInConflictState()` (suspended, node in maintenance, not installed, restoring backup, or transfer in progress) | Modal fields: `node_id` "Node" (accessible nodes except current; default first; required); `allocation_id` "Primary Allocation" (unassigned allocations on target node; required and enabled only if the server currently has a primary); `allocation_additional` "Additional Allocations" (multi; required when server has > 1 allocations; must pick exactly `count − 1`); `backups` checkbox list of the server's Wings-local backups (hidden if none) + warning "Be aware, not transferred backups will be deleted." → `TransferServerService::handle()`: checks target `Node::isViable()` (else `NodeNotViableException`) and `validateTransferState()`; creates `server_transfers` row (old/new node, old/new primary, old/new additional allocation ids) and assigns the new allocations to the server; signs a 15-min JWT (scope ServerTransfer, subject server uuid) for the new node; Wings (old node) `POST /api/servers/{uuid}/transfer` `{url: <newNode>/api/transfers, token: "Bearer <jwt>", backups: [uuids], server: {uuid, start_on_completion: false}}`. Afterwards Wings backups not selected are deleted from the panel DB. | "Transfer started" / "Transfer failed" (+ message). Hint marks the feature experimental. |
| **Reinstall** | disabled while suspended; danger confirmation "Are you sure you want to reinstall this server?" / "!! This can result in unrecoverable data loss !!" | `ReinstallServerService` (see above). | "Reinstall started" / "Could not start reinstall" |

#### Edit header actions

| Action | Behaviour |
|---|---|
| **Delete** (icon; requires `delete server`) | Confirmation → `ServerDeletionService::handle()`: Wings `DELETE /api/servers/{uuid}` (404 tolerated), then in a transaction deletes every server database on its host (`DatabaseManagementService::delete`), deletes non-Wings backups through their backup adapter (Wings-local ones are removed by the daemon), releases allocations (`server_id=null, notes=null, is_locked=false, show_port=true`) and deletes the server row. Redirect to `/admin/servers`. On Wings `ConnectionException`: sets cache flag `servers.{uuid}.canForceDelete` for 5 minutes and shows warning "Server couldn't be safely deleted. You may Force Delete it." |
| **Force delete** (visible only while that flag is set) | Same service `withForce()`: ignores Wings and database-host errors (orphaned DB rows are just deleted). On connection error the flag is cleared. |
| **Console** (icon) | Link to the server panel console `/server/{uuid_short}/console` (`App\Filament\Server\Pages\Console`). |
| **Save** (`mod+s`) | `mutateFormDataBeforeSave`: description defaults to `''`; removes `docker`, `status`, `allocation_id` from the payload. `afterSave`: if anything changed except `updated_at, name, owner_id, condition, description, external_id, tags, cpu_pinning, allocation_limit, database_limit, backup_limit, skip_scripts`, Wings `POST /api/servers/{uuid}/sync`. Connection error → warning "Error connecting to <node> — The configuration could not be automatically synced on Wings, you will need to manually restart the server." |

View page header: Edit.

#### Relation manager: Allocations (`Servers/RelationManagers/AllocationsRelationManager.php`)

| Column | Behaviour |
|---|---|
| IP Address, Port | searchable, sortable |
| Show port (`show_port`) | **toggle column** (writes directly; not disabled on View) |
| Alias (`ip_alias`) | editable text ("No Alias"), searchable, sortable |
| Notes | editable text ("No Notes") |
| Primary | star icon (filled when `id == server.allocation_id`); clicking (when not read-only) sets the server's primary allocation; tooltip "Already Primary"/"Make Primary" |
| Locked? | lock icon from `is_locked`; tooltip "Users won't be able to delete locked allocations" |

Row actions (all require `update allocation` on the record, hidden when read-only): **Make Primary** (updates `servers.allocation_id`), **Lock** / **Unlock** (`is_locked`), **Remove Allocation** (`DissociateAction` → `Server::releaseAllocations([id])`: releases with `RELEASE_ATTRIBUTES` under a row lock; if the primary was removed, picks the lowest-id remaining allocation on the server's node as new primary and logs activity `server:allocation.primary`).
Toolbar: **Dissociate bulk** (same release for selected, current page only); **Create Allocation** (same IP/alias/ports form; hidden `is_locked=true`; `AssignmentService::handle(node, data, server)` creates allocations already assigned to this server and sets the primary if none); **Add Allocation** (`AssociateAction`, multiple, preloaded options = unassigned allocations on the server's node, searchable by ip/port; afterwards marks them locked and sets the first as primary if the server has none). None of these trigger a Wings sync.

#### Relation manager: Databases (`Servers/RelationManagers/DatabasesRelationManager.php`)

Access: `DatabasePolicy` checks subuser permissions against the Filament *tenant*, which is null in the admin panel; therefore effectively only root admins pass (inferred from `app/Policies/DatabasePolicy.php` + Laravel Gate behaviour).

- Table: Database, Username, Connections From (`%` shown as "Anywhere ( % )"), `server.name` (link), Max Connections (0/null → "Unlimited"), Created At (`DateTimeColumn`). Empty state "No Databases exist for this Server".
- Row **View** modal: Database, Username, Password (revealable; hint action Rotate → `RotateDatabasePasswordAction`), Connections From, Max Connections, JDBC Connection String (revealable).
- Row **Delete** (hidden read-only): `DatabaseManagementService::delete()` drops DB and user on the host; success/failure notifications.
- Toolbar **Create Database** (hidden read-only; disabled with tooltip "No Database Hosts" when no database host exists at all):
  - `database_host_id` (required; hosts attached to the server's node; default = the first database host overall, which may not be in the list),
  - `database` "Name" (alpha-dash, shown with prefix `s<serverId>_`; blank → random bird word + 1–420),
  - `remote` "Connections From" (regex `^[\w\-\/.%:]+$`, default `%`).
  - → `DatabaseManagementService::setValidateDatabaseLimit(false)->create()` (admin bypasses the server's database limit). Final name `s<serverId>_<name>` truncated to 48 chars; username `u<serverId>_<10 random>`; 24-char random password. Failure → persistent "Failed to create Database" with message.

#### Action → service → Wings summary (servers)

| UI action | Service | Wings call |
|---|---|---|
| Create server | `ServerCreationService` | `POST /api/servers` |
| Save (relevant fields changed) | — | `POST /api/servers/{uuid}/sync` |
| Suspend / Unsuspend | `SuspensionService` | `POST /api/servers/{uuid}/sync` |
| Toggle install (normal) | `ToggleInstallService` | none |
| Reinstall / toggle on failed install | `ReinstallServerService` | `POST /api/servers/{uuid}/reinstall` |
| Transfer | `TransferServerService` | `POST /api/servers/{uuid}/transfer` (on source node) |
| Delete / Force delete | `ServerDeletionService` | `DELETE /api/servers/{uuid}` |
| View install log | `DaemonServerRepository::getInstallLogs` | `GET /api/servers/{uuid}/install-logs` |
| Condition column/field | `Server::retrieveStatus` | `GET /api/servers/{uuid}` |
| Change egg | `EggChangerService` | none |
| Preview startup | `StartupCommandService` | none |
| Allocation create/assign/remove/primary/lock | `AssignmentService`, `Server::releaseAllocations`, direct updates | none |
| Database create/delete/rotate | `DatabaseManagementService` | none (talks to DB host) |

All Wings requests use `Http::daemon($node)`: base URL `scheme://fqdn:daemon_connect`, `Authorization: Bearer <daemon_token>`, JSON, TLS verification only in production, configurable timeouts (`panel.guzzle.*`), and responses are rejected unless the `User-Agent` header (`Pelican Wings/vX.Y.Z (id:<token_id>)`) matches the node's `daemon_token_id`.

---

### Eggs

Files: `app/Filament/Admin/Resources/Eggs/EggResource.php`, `Pages/{ListEggs,CreateEgg,EditEgg,ViewEgg}.php`, `RelationManagers/ServersRelationManager.php`, model `app/Models/Egg.php`, `app/Models/EggVariable.php`, services `app/Services/Eggs/Sharing/{EggImporterService,EggExporterService}.php`.

#### Routes and access

| URL | Page | Access |
|---|---|---|
| `/admin/eggs` | ListEggs | `viewList egg` |
| `/admin/eggs/create` | CreateEgg (tabs) | `create egg` |
| `/admin/eggs/{id}` | ViewEgg | `view egg` |
| `/admin/eggs/{id}/edit` | EditEgg | `update egg` |
| Import / Update / bulk update | – | `import egg` |
| Export | – | `export egg` to see the button; the download route itself (session-authenticated application API, `app/Http/Requests/Api/Application/ApplicationApiRequest.php`) requires `User::isAdmin()` plus `viewList egg` and `view egg` — not `export egg` |
| Replicate | – | `create egg` (policy `replicate`) |

No node scoping for eggs.

#### List `/admin/eggs`

| Column | Behaviour |
|---|---|
| (icon) | circular image: egg icon URL, or the panel logo (`config('app.logo')` or `pelican.svg`) inlined as a base64 SVG data URI |
| Name | with description underneath (truncated to 120 chars + "..."), wraps; searchable, sortable |
| Servers | `servers_count` |

- Default 25 rows per page (options Filament default [5, 10, 25, 50]); default sort = key (Filament default).
- Filter: `TagsFilter` on `Egg`.
- Row actions: View (only if can't edit); Edit; **Export** (`ExportEggAction`); **Update** (`UpdateEggAction`, visible only when an update was detected); **Replicate** (no modal): copies the egg excluding `author, uuid, update_url, servers_count, created_at, updated_at`, sets `author = current user email`, `name .= ' Copy'`, new UUID, copies all variables to the replica, then redirects to the replica's Edit page.
- Toolbar: **Import** (`ImportEggAction->multiple()`), **Create**, bulk group:
  - **Delete** (`DeleteBulkAction`): eggs with servers are removed from the selection with danger notification "Cannot delete :count egg(s)" / "The following eggs have servers and cannot be deleted: name (N servers)"; halts if nothing remains. Model `deleting` also refuses eggs with servers or with child eggs (eggs that copy config from it).
  - **Update selected** (`UpdateEggBulkAction`): eggs without `update_url` are removed with warning "Cannot update :count egg(s)"; halts if nothing remains.
- Empty state "No Eggs".

#### Create `/admin/eggs/create`

Header action "Create" (`mod+s`); tabs:

**Configuration**

| Field | Type / validation | Default / notes |
|---|---|---|
| `name` "Name" | required, max 255 | help: human-readable identifier |
| `author` "Author" | required, email, max 255 | editable only on create |
| `description` | textarea | – |
| `startup_commands` "Startup Commands" | key/value (Display Name → Command), required, reorderable, "Add Startup Command"; model: array min 1, values required & distinct | First entry is the default. Placeholders `Default` / `java -Xms128M -XX:MaxRAMPercentage=95.0 -jar {{SERVER_JARFILE}}` |
| `file_denylist` "File Denylist" | tags (placeholder `denied-file.txt`) | files end users may not edit |
| `features` "Features" | free tags | – |
| `force_outgoing_ip` "Force Outgoing IP" | toggle | help explains NAT to primary allocation IP and disabling internal networking |
| `script_is_privileged` | hidden | 1 |
| `tags` "Tags" | tags | – |
| `update_url` "Update URL" | URL | must point to raw .json |
| `docker_images` "Docker Images" | key/value (Image Name → Image URI), required, reorderable, "Add Docker Image"; model array min 1 | First is default. Placeholders `Java 21` / `ghcr.io/pelican-eggs/yolks:java_21` |

**Process Management**: `copy_process_from` "Copy Settings From" (`CopyFrom::process()`), `config_stop` "Stop Command" (required on create, max 255; `^C` for SIGINT), `config_startup` "Start Configuration" (JSON textarea, default `{}`), `config_files` "Configuration Files" (JSON, default `{}`), `config_logs` "Log Configuration" (JSON, default `{}`). Model: `config_*` required unless `config_from` set, must be JSON.

**Egg Variables**: repeater (see Variables below), starts empty.

**Install Script**: `copy_script_from` (`CopyFrom::script()`), `script_container` "Script Container" (required, max 255, default `ghcr.io/pelican-eggs/installers:debian`), `script_entry` "Script Entry" (select `bash` / `ash` / `/bin/bash`, required, default `bash`), `script_install` "Install Script" (Monaco editor, shell language, lazy binding).

On create (`handleRecordCreation`): UUID generated if missing; array-valued `config_startup`/`config_logs` JSON-encoded; the payload is written to the application log ("new egg").

#### Edit / View `/admin/eggs/{id}[/edit]` (`EggResource::detailTabs()`)

Header actions (Edit): **Delete** (disabled with tooltip "In Use" when the egg has servers), **Export**, **Import** (single file / single URL; URL pre-filled with this egg's `update_url`; after import the form and Monaco content are refreshed), **Save** (`mod+s`). View: Edit.

Differences from Create:
- Configuration tab adds icon display + Upload Icon / Delete Icon (edit only; stored under `icons/egg/<uuid>.<ext>`), read-only "Egg ID" and "Egg UUID" ("globally unique identifier ... which Wings uses"), **Author disabled** (help: uploading a configuration from a different author changes it).
- Process Management: `config_stop` not required; empty `config_files` saved as `{}`; Copy-From hidden on View.
- Install Script: Monaco editor (not lazy), Copy-From hidden on View.
- Save is a plain Eloquent update (no Wings call; Wings fetches egg config per server on sync/install).

#### Variables tab (create and edit)

Repeater bound to `variables` relationship; grid, collapsible and collapsed by default, item label = variable name, drag-reorder persisted to `sort` (`orderColumn()`), add button "Add New Variable"; items can be deleted (deletes the row).

| Field | Validation / behaviour |
|---|---|
| `name` "Name" | required, max 255, unique per egg (rule `unique` on `egg_variables.name` where `egg_id` = this egg; own row ignored); live (750 ms debounce): auto-fills `env_variable` with `SNAKE_UPPER` of the name |
| `description` | textarea |
| `env_variable` "Environment Variable" | shown as `{{ VALUE }}` with hint; required, alpha-dash, 1–255, unique per egg, **not** one of reserved names `P_SERVER_UUID, P_SERVER_ALLOCATION_LIMIT, SERVER_MEMORY, SERVER_IP, SERVER_PORT, ENV, HOME, USER, STARTUP, MODIFIED_STARTUP, SERVER_UUID, UUID, INTERNAL_IP, HOSTNAME, TERM, LANG, PWD, TZ, TIMEZONE` (`EggVariable::RESERVED_ENV_NAMES`); messages "A variable with this name already exists." / "The environment variable field is required." / "This environment variable is reserved and cannot be used." |
| `default_value` | text (null → `''`) |
| User Permissions: `user_viewable` "Viewable", `user_editable` "Editable" | checkboxes |
| `rules` "Rules" | reorderable tags with suggestions `required, nullable, string, integer, numeric, boolean, alpha, alpha_dash, alpha_num, url, email, regex:, min:, max:, between:, between:1024,65535, in:, in:true,false`; Laravel validation rule strings applied to server variable values |

Missing description/rules/flags are defaulted (`''`, `[]`, `''`) before create/save.

#### Import / export / update

- **Import** (`ImportEggAction`; list: multiple, edit: single). Modal with three tabs:
  - **GitHub**: one sub-tab per category from the cached community egg index (`eggs.index`, built by `p:egg:update-index` daily from `config('panel.cdn.egg_index_url')`; fetched synchronously if the cache is empty), each with a searchable, bulk-toggleable checkbox list (value = download URL, label = egg name, badge = count). Selected eggs are queued as `App\Jobs\InstallEgg` jobs (background `EggImporterService::fromUrl`), notification "background install started" with count.
  - **File**: upload `.json` / `.yaml` / `.yml` (not stored; multiple on list page).
  - **URL**: repeater of URLs (must be URL ending in `.json`, `.yaml`, `.yml`; `github.com/.../blob/...` is rewritten to `raw`). Item label derived from filename after `egg-`.
  - Each file/URL is imported synchronously (`fromFile` / `fromUrl`, 5 s timeout); result notification "Imported :success of :total eggs" with "Imported: ..." / "Failed: ..." (status success/warning/danger).
  - On the Edit page the import is **not** tied to the current egg: the importer matches by the file's `uuid` (creates a new egg if no match).
- **Export** (`ExportEggAction`): modal "How would you like to export <egg>?" with buttons "As .json" / "As .yaml" opening `/api/application/eggs/{id}/export?format=json|yaml` in a new tab (session-cookie authenticated application API, see access table; download name `egg-<kebab-name>.<fmt>`).
- **Update** (`UpdateEggAction`, single): visible only when cache key `eggs.{uuid}.update` is true. That flag is written by the scheduled daily command `p:egg:check-updates` (`app/Console/Commands/Egg/CheckEggUpdatesCommand.php`), which compares an md5 of the local export with the remote `update_url` content (ignoring `exported_at`), and stores the flag for **1 hour**. Confirmation "Are you sure you want to update this egg?" / "If you made any changes to the egg they will be overwritten!" → `EggImporterService::fromUrl(update_url, egg)`; clears the flag; notifications ":egg updated successfully" / "Failed to update :egg".
- **Bulk update** (`UpdateEggBulkAction`): same for each selected egg with an `update_url` (regardless of flag); persistent summary ":count/:total Eggs updated" with Updated/Failed/Skipped lists; deselects afterwards.

#### Egg file format (as far as the UI needs it)

Parsed by `EggImporterService::parse()` from JSON or YAML (by extension/MIME):
- `meta.version` must be one of `PTDL_v1` (legacy: `image`/`images` converted to `docker_images` map, `startup` → `startup_commands: {Default: ...}`), `PTDL_v2`, `PLCN_v1`, `PLCN_v2` (`startup` → `startup_commands`), or current `PLCN_v3` (`Egg::EXPORT_VERSION`). Anything else: "The file format is not recognized."
- Keys consumed: `uuid` (used to find and overwrite an existing egg; generated if absent), `name`, `author`, `description`, `tags`, `features`, `docker_images` (name → URI), `file_denylist`, `startup_commands` (name → command), `meta.update_url`, `config.{files,startup,logs}` (objects stored as pretty JSON strings), `config.stop`, `scripts.installation.{script,container,entrypoint}`, `icon` (or legacy `image`) as `data:image/<ext>;base64,...` saved as the egg icon, `variables[]` (`name, description, env_variable, default_value, user_viewable, user_editable, rules` (array or `|`-separated), `sort`).
- Normalisations: legacy placeholders in `config.files` (`server.build.env.SERVER_IP` → `server.allocations.default.ip`, `SERVER_PORT` → `...default.port`, `SERVER_MEMORY`/`server.build.memory` → `server.build.memory_limit`, `server.build.env.`/`server.build.environment.` → `server.environment.`); YAML boolean defaults become `"true"`/`"false"`; env names upper-cased; reserved env names renamed to `SERVER_<NAME>` and occurrences in startup commands rewritten.
- Import resets `copy_script_from` and `config_from` to null; variables are upserted by `env_variable`, variables not in the file are deleted.
- Export (`EggExporterService`): `_comment`, `meta {version: PLCN_v3, update_url}`, `exported_at`, `name, author, uuid, description, icon (base64 data URI or null), tags, features, docker_images, file_denylist, startup_commands, config {files, startup, logs, stop}` (inherited values resolved from `config_from`), `scripts.installation {script, container, entrypoint}` (resolved from `copy_script_from`), `variables` (all columns except id/egg_id/timestamps).

#### Relation manager: Servers (`Eggs/RelationManagers/ServersRelationManager.php`)

Heading "Servers"; empty state "No Servers are assigned to this Egg". Columns: Owner (link, searchable, sortable), Name (link, searchable, sortable), Node (link, searchable, sortable), Docker Image (badge), Primary Allocation (`allocation.address`, placeholder "None", marked sortable although `address` is an accessor). Row actions: View Console, Edit (admin edit server).

---

### Shared components (`app/Filament/Components/**`)

#### Actions

| Component | Default name | What it does | Authorization | Used in |
|---|---|---|---|---|
| `Actions/ConnectSftpAction` | `connect_sftp` | Green "plug" link button to `Server::getSftpUrl()` = `sftp://<url-encoded username>.<uuid_short>@<sftp_alias or fqdn>:<daemon_sftp>[/dir/]`; optional `->username()` / `->directory()` | subuser permission `file.sftp` on the server (record or tenant) | Server panel `Files/Pages/ListFiles.php`, `Pages/Settings.php` |
| `Actions/DeleteIcon` | `delete_icon` | Icon-only danger button, visible when record has an icon; deletes `<storagePath>/<uuid>.<ext>` for all icon formats on the public disk; "Icon Deleted" | none of its own | Egg config tab, admin Server info tab, server panel Settings |
| `Actions/UploadIcon` | `upload_icon` | Modal with tabs **URL** (debounced validation: http/https, valid URL, host must resolve to a non-private/non-reserved IP — "Local IP Addresses are not allowed"; preview image; on submit fetched with 5 s timeout, no redirects, extension from URL path) and **File** (image ≤ 256 KB, one file, image editor, accepted `image/png`, `image/jpeg`, `image/webp`; written immediately on upload). Stored via `HasIcon::writeIcon()` (normalises jpg/png/webp, removes other formats). Notifications "Icon Updated" / "No Icon Provided" | none of its own | same as DeleteIcon |
| `Actions/ExportEggAction` | `export` | See Eggs → Export | `export egg` | ListEggs row, EditEgg header |
| `Actions/ImportEggAction` | `import` | See Eggs → Import; `->multiple(bool)` builds the schema | `import egg` | ListEggs toolbar, EditEgg header |
| `Actions/UpdateEggAction` | `update` | See Eggs → Update | `import egg`; visible only with update flag | ListEggs row |
| `Actions/UpdateEggBulkAction` | `update` (registered as `exclude_bulk_update`) | See Eggs → Bulk update | `import egg` | ListEggs bulk group |
| `Actions/UpdateNodeAllocations` | `bulk_update_ip` | See Nodes → Allocations → Update IPs; `->nodeRecord($node)` | caller sets `update` on node | Node AllocationsRelationManager |
| `Actions/PreviewStartupAction` | `hint_preview` | Toggles hidden `previewing`; when enabling, replaces the `startup` field content with `StartupCommandService::handle(server, server.startup)` (substitutes `{{SERVER_MEMORY}}`, `{{SERVER_IP}}`, `{{SERVER_PORT}}` from the primary allocation — `127.0.0.1`/`0` if none — and each egg variable's value, or `[hidden]` when not user-viewable); when disabling, restores the saved `server.startup`. Label "Enable Preview"/"Disable Preview" | none | admin Server Egg tab, server panel `Pages/Startup.php` |
| `Actions/RotateDatabasePasswordAction` | `exclude_hint_rotate` | Icon "Rotate" with warning confirmation "Rotate Password" → `DatabaseManagementService::rotatePassword()`, refreshes `password` and `jdbc` form fields, logs activity `server:database.rotate-password`; "Password Rotated" / "Password Rotation Failed" (error text shown only to users who can access the admin panel) | `update` on the Database (DatabasePolicy) | admin DatabaseHosts databases RM, admin Server databases RM, server panel Databases |
| `Actions/ViewConsoleAction` | `view_console` | Terminal icon link to `/server/{uuid_short}/console` for the row's server (or `->server()`, or tenant) | `User::canAccessTenant(server)` (owner, subuser, or `view` server permission) | Node/Egg/User servers RMs, ListServers |
| `Actions/ExportScheduleAction` | `export` | Streams `schedule-<kebab-name>.json` from `ScheduleExporterService` | subuser `schedule.read` on tenant | server panel `Schedules/Pages/EditSchedule.php` |
| `Actions/ImportScheduleAction` | `import` | Tabs File (multiple `.json`) / URL (repeater, `.json`, github blob→raw); `ScheduleImporterService::fromFile/fromUrl` into the tenant server; success/failure notifications listing names | subuser `schedule.create` | server panel `Schedules/ScheduleResource.php` |

#### Forms

| Component | Behaviour | Used in |
|---|---|---|
| `Forms/Actions/CronPresetAction` | Button preset that sets `cron_minute, cron_hour, cron_day_of_month, cron_month, cron_day_of_week` to configured values (`->cron(m,h,dom,mon,dow)`, default `0 0 * * *`); colored success when the form currently equals the preset; disabled on view | server panel ScheduleResource |
| `Forms/Fields/CopyFrom` | Searchable, preloaded Select "Copy Settings From" (placeholder "None") over other eggs. `->process()`: bound to relationship `configFrom` (persists `config_from`, i.e. the egg inherits process config from the chosen egg at export/runtime via `inherit_*` accessors); on change copies `config_stop/startup/files/logs` into the form and also sets `copy_script_from` to the same egg; clearing resets to `''` / `{}`. `->script()`: bound to `scriptFrom` (persists `copy_script_from`); on change copies `script_container`, `script_entry` and pushes `script_install` into the Monaco editor (Livewire `setContent` event); clearing resets to `ghcr.io/pelican-eggs/installers:debian`, `bash`, empty script | Egg create/edit (Process Management, Install Script) |
| `Forms/Fields/MonacoEditor` + `resources/views/filament/components/monaco-editor.blade.php` | Code editor field bound to Livewire state. Loads Monaco 0.49.0 from cdnjs at runtime; 650 px tall; word-wrap; language from `EditorLanguages` enum (default html); theme from `config/monaco-editor.php` (`blackboard` custom theme, `iPlastic` when localStorage theme is light); read-only when the field is disabled; listens to Livewire events `setContent` (replace content) and `setLanguage`. Full-screen toggle code exists but is commented out | Egg install script, Server install-log modal, server panel file editor (`Files/Pages/EditFiles.php`, `ListFiles.php`) |
| `Forms/Fields/StartupVariable` + `resources/views/filament/components/startup-variable.blade.php` | Renders one egg variable: label = variable name, prefix `{{ENV}}`, helper = description, hint icon tooltip = rules joined with `|`, placeholder = default value, validation = the variable's rules, live on blur. Input type from rules: any `in:a,b,...` → select of those values (empty option when not `required`); `boolean` → toggle (bool state cast); `numeric`/`integer` → number input with `min:`/`max:` from rules (number state cast); else text. Required iff rules contain `required`. Data source `->fromForm()` (sibling fields `name, description, env_variable, default_value, rules`, disabled unless `user_editable`) or `->fromRecord()` (from `ServerVariable->variable`). Admin pages override with `->disabled(false)` | admin CreateServer / Server Egg tab, server panel Startup page |

#### State casts

`StateCasts/ServerConditionStateCast`: converts the stored string to `ServerState` or `ContainerStatus` enum (null when blank/unknown) and back to its string value. Used by the admin Server "Server Status" toggle.

#### Table columns

| Column | Behaviour | Used in |
|---|---|---|
| `BytesColumn` | Text column formatting byte counts with `convert_bytes_to_readable` (binary or decimal units per config); empty for 0/null | BackupHosts backups RM, server panel Files/Backups |
| `DateTimeColumn` | Text column with `dateTime()` formatting in the user's timezone (fallback `app.timezone`); `->since()` shows relative time with the absolute time as tooltip | many (databases, backups, activity, API keys, schedules, files) |
| `NodeHealthColumn` | Icon column "Health" rendering lazy `node-system-information` Livewire component per row | ListNodes |
| `NodeClientHealthColumn` | Icon column "Reachable" rendering lazy `node-client-connectivity` per row | ListNodes |
| `ProgressBarColumn` (+ `Concerns/HasProgress`, view `livewire/columns/progress-bar-column.blade.php`) | Progress bar of `state / maxValue` (clamped 0–100%); status danger ≥ danger threshold (default 0.9), warning ≥ warning threshold (default 0.7), else success, with configurable colors; label = `helperLabel` or `N%`; danger state text pulses and is `aria-live` | App panel `Servers/Pages/ListServers.php` (CPU/memory/disk usage) |
| `ServerEntryColumn` (view `livewire/columns/server-entry-column.blade.php`) | Renders the lazy `server-entry` Livewire card for the row's server; carries the same thresholds | App panel ListServers (grid layout) |

#### Filters

`Tables/Filters/TagsFilter` (name `tags`): single searchable, preloaded select of all distinct tags found in the model's `tags` JSON column; filters with `whereJsonContains('tags', tag)`; indicator "Tag: <tag>"; hidden when no record has tags. Requires `->model(Class)`. Used in ListNodes and ListEggs.

---

### Wings endpoints used by these admin screens

| Endpoint | Trigger |
|---|---|
| `GET /api/system` | Node health column, Overview info, CPU chart thread count |
| `GET /api/system/utilization` | CPU / memory / storage charts |
| `GET /api/system/ips` | IP options in allocation create forms, Update IPs, IP select column |
| `GET /api/diagnostics` | Diagnostics → Pull |
| `POST /api/update` | Node save (non-trivial change), Reset token |
| `POST /api/servers` | Server create |
| `GET /api/servers/{uuid}` | Server condition |
| `POST /api/servers/{uuid}/sync` | Server save, suspend, unsuspend |
| `POST /api/servers/{uuid}/reinstall` | Reinstall / toggle install on failed |
| `POST /api/servers/{uuid}/transfer` | Transfer (source node) |
| `DELETE /api/servers/{uuid}` | Delete / force delete |
| `GET /api/servers/{uuid}/install-logs` | View install log |
| Browser → node `GET /` + `WS /api/servers/{uuid}/ws` | Reachable column |

External (non-Wings): `https://logs.pelican.dev` (diagnostics upload), `https://api.github.com/repos/pelican/wings/releases/latest` (latest Wings version), egg index URL and egg `update_url`s, icon URLs.

## Server panel

The server panel is the per-server UI that server owners, subusers and admins use. It is a Filament panel with multi-tenancy: the tenant is a `Server`, resolved by `uuid_short`. Every page and resource below sits under `/server/{uuid_short}/…`.

Notation used below:

- **Perm** is a subuser permission key from `app/Enums/SubuserPermission.php` (for example `file.read`). The [permission model](#permission-model) section explains how a check resolves for owners, admins and subusers.
- **Conflict-blocked** means the page or resource uses `app/Traits/Filament/BlockAccessInConflict.php`. It is hidden from navigation and returns 403 while the server is in a conflict state (see [Server state gating](#server-state-gating)).
- Text marked *(Filament default)* describes framework behaviour that was checked against the vendor sources in `.ref/vendor/filament`, not against Pelican code.

### Panel configuration

Sources: `app/Providers/Filament/ServerPanelProvider.php` and `app/Providers/Filament/PanelProvider.php` (the base class shared by every panel).

| Setting | Value / behaviour |
|---|---|
| Panel id / path | `server` / `/server` |
| Tenancy | `->tenant(Server::class, 'uuid_short')`. The route parameter `{tenant}` is bound through `Server::resolveRouteBinding`, which matches `uuid_short` **or** the full `uuid` (`app/Models/Server.php`). |
| Home URL | The brand/logo link goes to the **app** panel home (the server list), not to a server page. |
| Tenant root | There is no Dashboard page. `/server/{id}` redirects to the first accessible navigation item, which is normally Console. `/server` with no tenant redirects to the user's default tenant, which is the first server from `getTenants()`, or returns 404 if the user has none *(Filament default: `RedirectToTenantController` / `getRedirectUrl`)*. |
| Tenant menu | Enabled *(Filament default)*. The server switcher lists `User::getTenants()` = `accessibleServers()`. |
| Tenant middleware | `app/Http/Middleware/Activity/ServerSubject.php` sets the activity-log **actor** to the current user and the **subject** to the server for every request. `ActivityLogService` also falls back to the Filament tenant as subject. |
| SPA mode | `->spa(fn () => !request()->routeIs('filament.server.pages.console'))`. Every page uses Livewire `wire:navigate` SPA navigation except the Console page: links rendered on the Console page do full page loads. Exception list: `*/oauth/redirect/*`. |
| Persistent render hook | At `PanelsRenderHook::BODY_END`: `@persist("file-upload-manager") @livewire(App\Livewire\FileUploadManager) @endpersist`. The upload queue survives SPA navigation between server pages (see [Upload](#upload-files-and-folders)). |
| Global render hooks | From `app/Providers/Filament/FilamentServiceProvider.php`: `AlertBannerContainer` at `PAGE_START` (the floating alert banners used throughout), a footer, and `resources/js/app.js` (a clipboard polyfill). |
| Notifications | `databaseNotifications()` (the bell icon). |
| Layout | Top or side navigation follows the user customization `top_navigation` (`sidebar` / `topbar` / `mixed`). Max width comes from `config('panel.filament.display-width')` (default `screen-2xl`). |
| Auth | Login page, password reset, MFA (TOTP app with recovery codes, plus email codes). The auth middleware includes `RequireTwoFactorAuthentication`. |
| Discovery | Resources from `app/Filament/Server/Resources`, pages from `app/Filament/Server/Pages`, widgets from `app/Filament/Server/Widgets`. Panel plugins are loaded via `PluginService::loadPanelPlugins`. |

**User menu**

| Item | Target | Visibility |
|---|---|---|
| Profile | `EditProfile` page in the **app** panel | always |
| Server List (`profile.server_list`) | `ListServers` in the app panel | always |
| Admin (`profile.admin`) | admin panel home | `user->canAccessPanel(admin)`, which requires a root admin or a user with a role that has ≥1 permission |
| Logout | *(Filament default)* | always |

### Tenant resolution and access control

`app/Models/User.php`:

- `getTenants()` returns `accessibleServers()`:
  - A user with `viewAny Server` (admin role) gets every server they own, every server they are a subuser of, and every server on an **accessible node**. Root admins get all nodes. A role with no node restrictions also gets all nodes; otherwise only nodes linked to the user's roles.
  - Everyone else gets `directAccessibleServers()`: servers they own or are a subuser of.
- `canAccessTenant(Server)` is true when `canned('view', $server)` (admin "view server" permission via `ServerPolicy`, plus a node-target check), when `owner_id === user.id`, or when the user is a subuser. Otherwise Filament's `IdentifyTenant` returns **404** *(Filament default, verified in `.ref/vendor/filament/filament/src/Http/Middleware/IdentifyTenant.php`)*.
- `canAccessPanel(server)` is always true for authenticated users.

#### Permission model

`User::can($ability, Server)` is overridden. When `$ability` is a `SubuserPermission` or a registered permission string, it calls `hasPermission()`, which caches the result per request in `Context`:

1. If the user can `update` the server (admin with "update server", or root admin via the `Gate::before` in `app/Providers/AppServiceProvider.php`) or is the **owner**, every permission is granted.
2. If the ability is `websocket.connect` and the user can `view` the server (admin with view-only), it is granted.
3. Otherwise the ability must appear in the subuser row's `permissions` JSON array.

Resource-level checks go through policies in `app/Policies/*` that map Filament abilities to subuser permissions on `Filament::getTenant()`:

| Policy | viewAny / view | create | update | delete |
|---|---|---|---|---|
| `FilePolicy` | `file.read` / `file.read-content` | `file.create` | `file.update` | `file.delete` |
| `BackupPolicy` | `backup.read` | `backup.create` | — | `backup.delete` |
| `DatabasePolicy` | `database.read` | `database.create` | `database.update` | `database.delete` |
| `SchedulePolicy` | `schedule.read` | `schedule.create` | `schedule.update` | `schedule.delete` |
| `SubuserPolicy` | `user.read` | `user.create` | `user.update` | `user.delete` |
| `AllocationPolicy` (tenant branch) | `allocation.read` | `allocation.create` | `allocation.update` | `allocation.delete` |
| `ActivityLogPolicy` | `activity.read` | — | — | — |
| `WebhookConfigurationPolicy` | **admin role permissions** `viewList webhook`, `view webhook`, … (`DefaultAdminPolicies`). No subuser permission applies. | `create webhook` | `update webhook` | `delete webhook` |

An action whose `->authorize()` fails is **hidden**, not merely disabled *(Filament default)*.

##### Full subuser permission list

Source: `app/Enums/SubuserPermission.php`. Descriptions come from `lang/en/server/user.php` (`permissions.{group}_{perm}`). The UI groups permissions by the prefix before the dot, one tab per group, with tab titles taken from `{group}_title` and `{group}_desc`. Plugins can add groups or permissions through `Subuser::registerCustomPermissions()`.

| Group (tab) | Key | Description |
|---|---|---|
| *(hidden)* websocket | `websocket.connect` | Access to the websocket. Hidden in the UI and **always appended** when creating or updating a subuser. |
| Control | `control.console` | Send data to the server console |
|  | `control.start` | Start the server |
|  | `control.stop` | Stop the server. **Also gates Kill**: there is no `control.kill` permission, although `lang` has an orphan `control_kill` string. |
|  | `control.restart` | Restart the server |
| File | `file.read` | List directory contents; no file contents and no download |
|  | `file.read-content` | View file contents and download files |
|  | `file.create` | Create files and directories, upload, pull from URL, copy |
|  | `file.update` | Update files and folders (save, rename, move, chmod) |
|  | `file.delete` | Delete files and directories |
|  | `file.archive` | Create archives and decompress |
|  | `file.sftp` | Use SFTP. Also shows the SFTP info on Settings and the "Connect SFTP" buttons. |
| Backup | `backup.read` | View backups |
|  | `backup.create` | Create backups |
|  | `backup.delete` | Delete backups. Also gates rename and lock/unlock, and creating a backup as locked. |
|  | `backup.download` | Download backups |
|  | `backup.restore` | Restore backups |
| Schedule | `schedule.read` | View schedules (also used for export) |
|  | `schedule.create` | Create schedules (also used for import) |
|  | `schedule.update` | Edit schedules, manage tasks, Run Now |
|  | `schedule.delete` | Delete schedules |
| User | `user.read` | View subusers |
|  | `user.create` | Invite subusers |
|  | `user.update` | Edit subuser permissions |
|  | `user.delete` | Remove subusers |
| Database | `database.read` | View databases |
|  | `database.create` | Create databases |
|  | `database.update` | Rotate the password |
|  | `database.delete` | Delete databases |
|  | `database.view-password` | See the password and the JDBC string |
| Allocation | `allocation.read` | View allocations |
|  | `allocation.create` | Add an allocation (auto-assign) |
|  | `allocation.update` | Change the primary allocation, edit notes |
|  | `allocation.delete` | Remove an allocation |
| Activity | `activity.read` | View the activity log |
| Mounts | `mount.read` | View the Mounts page |
|  | `mount.update` | Toggle mounts |
| Startup | `startup.read` | View the Startup page |
|  | `startup.update` | Change the startup command and variables. Also gates the GSL token feature. |
|  | `startup.docker-image` | Change the Docker image. Also gates the Java version feature. |
| Settings | `settings.rename` | Rename the server |
|  | `settings.description` | Change the description |
|  | `settings.reinstall` | Reinstall |
|  | `settings.change-icon` | Upload or delete the server icon |

### Server state gating

`Server::isInConflictState()` is true when **any** of these hold:

- the server is suspended;
- the node is under maintenance;
- the server is not installed (`status` = `installing`, `install_failed` or `reinstall_failed`);
- `status = restoring_backup`;
- a transfer row exists.

`ServerStateConflictException` (`app/Exceptions/Http/Server/ServerStateConflictException.php`) picks one message in this order: suspended → maintenance → "not yet completed its installation" → restoring backup → transferring → generic. Strings are in `lang/en/exceptions.php` under `server.*`.

Effects:

| Area | Behaviour in conflict state |
|---|---|
| Console page | Still accessible. On mount a warning `AlertBanner` with id `server_conflict` shows the exception message. All power buttons are disabled. The command input becomes read-only. The CPU, memory and network charts are hidden. |
| Files, Backups, Schedules, Users, Databases, Network | Conflict-blocked: hidden from navigation, 403 on direct URL *(Filament default for a failing `canAccess`)*. |
| Settings, Startup, Mounts (`ServerFormPage`) | Conflict-blocked. |
| Activity, Webhooks | **Not** blocked. |

### Navigation

Items are ordered by `navigationSort`. Labels come from `lang/en/server/*.php`.

| Sort | Label | URL (under `/server/{uuid_short}`) | Icon (Tabler) | Required to see | Badge | Conflict-blocked |
|---|---|---|---|---|---|---|
| 1 | Console | `/console` | terminal-2 | any tenant user | — | no |
| 2 | Files | `/files/{path?}` | files | `file.read` | — | yes |
| 3 | Backups | `/backups` | file-zip | `backup.read` | `count / backup_limit` (or `count` if the limit is 0). Colour: success, warning at ≥70 %, danger at ≥100 %. | yes |
| 4 | Schedules | `/schedules` | clock | `schedule.read` | — | yes |
| 5 | Users | `/subusers` | users | `user.read` | subuser count | yes |
| 6 | Databases | `/databases` | database | `database.read` | `count / database_limit` | yes |
| 7 | Network | `/allocations` | network | `allocation.read` | — | yes |
| 8 | Activity | `/activities` | stack | `activity.read` | — | no |
| 9 | Mounts | `/mounts` | layers-linked | `mount.read` | — | yes |
| 10 | Startup | `/startup` | player-play | `startup.read` | — | yes |
| 11 | Settings | `/settings` | settings | any tenant user | — | yes |
| 11 | Webhooks | `/webhook` | webhook | admin role `viewList webhook` | count if >0 | no |
| 99 | Open in Admin | admin `EditServer` URL | arrow-back | `canAccessPanel(admin)` **and** `can('view server', tenant)` | — | — |

The badge logic is in `app/Traits/Filament/HasLimitBadge.php`. Resource slugs follow Filament's default naming (`Files\FileResource` → `files`, `Activities\ActivityResource` → `activities`); Webhooks overrides the slug to `webhook`.

### Console page

Sources: `app/Filament/Server/Pages/Console.php`, `resources/views/filament/server/pages/console.blade.php`, `app/Filament/Server/Widgets/*`, `resources/views/filament/components/server-console.blade.php`, `resources/js/console.js`.

**URL** `/server/{id}/console`. There is no permission check on the page itself; the websocket token requires `websocket.connect`.

#### Layout

The page renders a 3-column widget grid. Widgets render in this order; plugins can inject widgets at the positions named in `ConsoleWidgetPosition` (`top`, `above_console`, `below_console`, `bottom`) via `Console::registerCustomWidgets`.

1. *(plugins: top)*
2. **ServerOverview**: six small stat blocks.
3. *(plugins: above_console)*
4. **ServerConsole**: the xterm terminal and command input, full width, not lazy.
5. *(plugins: below_console)*
6. **ServerCpuChart**, **ServerMemoryChart**, **ServerNetworkChart**: one column each, max height 200 px. Each one's `canView()` returns false when the server is in a conflict state or Wings reports `offline`/`missing` at page render.
7. *(plugins: bottom)*

None of the widgets poll; every live value comes from the websocket through JS.

#### Power actions (header button group)

The actions are in `Console::getDefaultHeaderActions()`. Each one dispatches the browser event `setServerState {uuid, state}`. The console JS turns that into the websocket message `{"event":"set state","args":[state]}`. There is **no HTTP call** to the panel; Wings performs the action and reports `server:power.*` activity back through `POST /api/remote/activity`.

| Action | Perm (`authorize`) | Visible when | Enabled when (also requires not in conflict state) | Notes |
|---|---|---|---|---|
| Start | `control.start` | always | `status->isStartable()` | primary colour |
| Restart | `control.restart` | always | `status->isRestartable()` | gray |
| Stop | `control.stop` | `!status->isKillable()` | `status->isStoppable()` | danger |
| Kill | `control.stop` | `status->isKillable()` | `status->isKillable()` | danger. **Requires confirmation.** Tooltip: "This can result in data corruption and/or data loss!" |

Availability by container status, from `app/Enums/ContainerStatus.php` (E = shown and enabled, D = shown and disabled, – = hidden):

| Status | Start | Restart | Stop | Kill |
|---|---|---|---|---|
| created | E | E | – | E |
| starting | D | E | – | E |
| running | D | E | E | – |
| restarting | D | E | – | E |
| exited | E | E | D | – |
| paused | E | E | – | E |
| dead | E | E | – | E |
| removing | E | E | – | E |
| stopping | D | E | – | E |
| offline | E | E | D | – |
| missing | D | D | D | – |

The page's `$status` starts as `offline`. It updates when the JS forwards a websocket `status` event as the Livewire event `console-status`; the page then re-renders the header actions and caches the status in `servers.{uuid}.status` for 15 s.

#### ServerOverview widget

Sources: `app/Filament/Server/Widgets/ServerOverview.php` and `app/Filament/Server/Components/SmallStatBlock.php`. A `SmallStatBlock` is a Filament `Stat` with a custom view `resources/views/filament/components/server-small-data-block.blade.php`; clicking a copyable block copies its value to the clipboard and shows a tooltip.

| Block | Initial value (server-rendered) | Live update (JS, from the `stats`/`status` events) |
|---|---|---|
| Name | server name (copyable) | — |
| Status | `server->condition` label: the DB `status` (installing, suspended, …) if set, else the Wings container state from `GET /api/servers/{uuid}` (cached 15 s) | Only when the DB `status` is null (`statusIsLive`). Shows `"{label} ({uptime})"` with the uptime humanized to 2 parts (e.g. "1d 3h"), or `—` before the first status arrives. |
| Address | `allocation.display_address` (`ip_alias` or IP, with `:port` unless `show_port` is false), copyable, "None" if there is no allocation | — |
| CPU | `— / {cpu}%` or `— / ∞` | `"{cpu_absolute, 2dp} % / limit"`. Shows "Offline" when the state is offline, `—` when unknown. |
| Memory | `— / {memory MiB → readable}` | `bytesToReadable(memory_bytes) + limit` |
| Disk | `— / {disk → readable}` | `bytesToReadable(disk_bytes)`, or `—` when 0 |

Binary vs decimal units follow `config('panel.use_binary_prefix')` (default true: KiB/MiB…, base 1024).

#### ServerConsole widget (xterm)

The widget loads `resources/js/console.js`, which exports `window.Xterm` and `window.ServerStats`.

- **Packages:** `@xterm/xterm` 6, `@xterm/addon-fit`, `@xterm/addon-web-links`, `@xterm/addon-search`, `xterm-addon-search-bar`, `@xterm/addon-webgl`. All five addons are loaded.
- **Terminal options:**

| Option | Value / source |
|---|---|
| `fontSize` | user customization `console_font_size` (default 14; profile field min 1) |
| `fontFamily` | `'{console_font}, monospace'`. `console_font` defaults to `monospace`. Any other value loads `storage/fonts/{font}.ttf` via `@font-face`; the profile lists the files in the public disk's `fonts/` directory. |
| `rows` | `console_rows` (default 30, min 1) |
| `lineHeight` | fixed `1.2` (not user-configurable) |
| `disableStdin` | true (input goes through a separate text field) |
| `cursorStyle` / `cursorInactiveStyle` | `underline`; the cursor colour is transparent |
| `allowTransparency` | true; background `rgba(19,26,32,0.7)` |
| `scrollback` | not set, so the xterm default applies (1000 lines) *(xterm default)* |
| theme | fixed 16-colour palette (red `#E54B4B`, green `#9ECE58`, …) with selection colour `#FAF089` |

- **Key handling** (`attachCustomKeyEventHandler`): Ctrl/Cmd+C copies the selection to the clipboard (and is not sent anywhere). Ctrl/Cmd+F opens the search bar. Esc hides the search bar.
- **Resize:** `fitAddon.fit()` runs immediately, on window `load`, and on window `resize`.
- **Web links** are clickable.
- **Output formatting:**
  - Each websocket line is written with `writeln` after one trailing `\r\n`/`\n`/`\r` is stripped, followed by an ANSI reset.
  - Panel-generated lines are prefixed with `\e[1m\e[33m{slug(app.name)}@{server name} ~ \e[0m`:
    - `Server marked as {state}...` on every `status` event;
    - `Transfer has failed.` on `transfer status` = `failure`;
    - daemon errors, rendered on a red background (`\e[41m`).

#### Wings websocket protocol (as used by the UI)

**Connection URL:** `ServerConsole::getSocket()` builds `str_replace(['https://','http://'],['wss://','ws://'], node.scheme://node.fqdn:node.daemon_connect) . "/api/servers/{server.uuid}/ws"`. It always uses the server's **current** node; unlike the client API `WebsocketController`, it does not redirect to the new node during a transfer.

**Token:** obtained through Livewire. On `socket.onopen` and on `token expiring`/`token expired`, the JS dispatches `token-request`. `ServerConsole::tokenRequest()` calls `getToken()` and dispatches `sendAuthRequest {token}`, and the JS sends `{"event":"auth","args":[token]}`.

`getToken()` throws 403 "You do not have permission to connect to this server's websocket." unless the user has `websocket.connect`. The token is built by `app/Services/Nodes/NodeJWTService.php` (lcobucci/jwt, HS256 keyed with `node.daemon_token`):

| Claim / header | Value |
|---|---|
| `iss` | `config('app.url')` |
| `aud` | node connection address `scheme://fqdn:daemon_connect` |
| `jti` (claim **and** header) | `sha256(user.id . server.uuid)` |
| `iat` | now |
| `nbf` | now − 5 min |
| `exp` | now + **10 min** |
| `user_uuid` | user UUID |
| `server_uuid` | server UUID |
| `permissions` | from `app/Services/Servers/GetUserPermissionsService.php`, see below |
| `scope` | `"websocket"` |
| `unique_id` | `Str::random()` (16 chars) |

`permissions` claim:

- Owner who is not an admin: `["*"]`.
- Admin (`isAdmin()` and can view or update the server) who is the owner or can `update` the server: `["*", "admin.websocket.errors", "admin.websocket.install", "admin.websocket.transfer"]`.
- Admin with view-only access: `["websocket.connect", the three admin.* permissions, …subuser permissions]`.
- Plain subuser: the subuser's `permissions` array.

**Client → Wings messages.** Every message is JSON `{"event": string, "args": [...]}`.

| Event | When | Args |
|---|---|---|
| `auth` | after open and on token refresh | `[jwt]` |
| `send logs` | immediately after `auth success` | `[null]` |
| `set state` | power buttons | `["start" \| "stop" \| "restart" \| "kill"]` |
| `send command` | command input Enter | `[command]` |
| `send stats` | **never sent** by the panel UI | — |

`set state` and `send command` go through `sendAction()`, which queues them in `pendingActions` while the socket is reconnecting or not yet authenticated and flushes them after `auth success`. `auth` and `send logs` are dropped if the socket is not open.

**Wings → client events handled** (the `socket.onmessage` switch):

| Event | Handling |
|---|---|
| `auth success` | Resets the reconnect counter, marks the socket authenticated, sends `send logs`, then flushes queued actions. |
| `console output` | `args[0]` is written to the terminal. |
| `install output` | Same as console output. Wings only sends it to holders of `admin.websocket.install` *(Wings behaviour)*. |
| `install completed` | Sets `statusIsLive = true` and dispatches the Livewire events `refresh-sidebar` and `refresh-topbar` (Filament re-renders navigation) and `removeAlertBanner {id: 'server_conflict'}`. |
| `status` | Writes "Server marked as {state}...", updates `ServerStats` state, re-pushes widget values, and dispatches Livewire `console-status {state}`. Both the Console page and the ServerConsole widget cache the status for 15 s; this unlocks or locks the power buttons and the command input. |
| `stats` | `args[0]` is a JSON string. It is parsed by `ServerStats.push()`; non-object frames are ignored. |
| `transfer status` | Prints "Transfer has failed." only when `args[0] === 'failure'`. |
| `daemon error` | Printed with a red background. |
| `token expiring`, `token expired` | Request a new token and re-send `auth`. |
| `feature match` | `Livewire.dispatch('mount-feature', {data: args[0]})`. See [Feature modals](#feature-modals-console-line-triggers). |

Wings events that the UI **ignores**: `transfer logs`, `daemon message`, `jwt error`, `backup completed`, `backup restore completed`, `install started` *(event names from knowledge of Wings)*.

**Reconnects.**

- On close the JS waits 2 s and reconnects, up to **5** attempts. The counter resets on `auth success`.
- After 5 failures it dispatches `websocket-error`. `ServerConsole::websocketError()` then shows a danger `AlertBanner` "Could not connect to websocket!" / "Check your browser console…". Users who can `update` the node also get an "Open troubleshooting guide" link to `https://pelican.dev/docs/troubleshooting/#wings-connection-issues`.
- On `livewire:navigating` (an SPA navigation away), the socket is closed, pending reconnects are cancelled and listeners are removed.

#### Stats to charts

`window.ServerStats` in `resources/js/console.js`:

- Each `stats` frame becomes a sample with these fields:

  | Sample field | Frame field |
  |---|---|
  | `t` | `Date.now()` |
  | `cpu` | `cpu_absolute` |
  | `memory` | `memory_bytes` |
  | `disk` | `disk_bytes` |
  | `rx` | `network.rx_bytes` |
  | `tx` | `network.tx_bytes` |
  | `uptime` | `uptime` (ms) |

  If the frame has a string `state`, it is recorded as the current state.
- The **history buffer** holds `MAX_SAMPLES = 120` samples. It is persisted to `sessionStorage` under the key `pelican:console-stats:{uuid}` and restored on revisit; only samples from the last 120 s with sane timestamps are kept. Switching servers in the same document clears the buffer.
- **Charts show** the last `period` samples. `period` is the user customization `console_graph_period`: default 30, profile range 10–120, labelled in seconds because Wings sends roughly one frame per second.
- Data is pushed to each chart widget with `Livewire.dispatchTo(widget, 'updateChartData', {data})`, Filament ChartWidget's update hook. The latest values are replayed whenever a lazy widget finishes loading.

| Chart | Series | Y axis |
|---|---|---|
| CPU | `cpu` rounded to 2 dp | min 0, legend hidden |
| Memory | `memory / 1024³` (or `/ 1000³`), 2 dp, so in GiB/GB | min 0 |
| Network | Two series, **Inbound** (rx) and **Outbound** (tx), each the **delta** between consecutive samples (counters are cumulative). Pairs more than 5 s apart (`MAX_SAMPLE_GAP`) are skipped; negative deltas are clamped to 0. The heading shows `Network - ↓{rx total} - ↑{tx total}`. | tick formatter always uses base-1024 units (there is a TODO to honour `use_binary_prefix`) |

X-axis labels are `HH:MM:SS` (`en-GB`, 24 h) in the **user's panel timezone**; the x axis itself is hidden.

#### Command input and history

The input is rendered only when the user has `control.console`.

- It is **read-only** unless `canSendCommand()` holds: `control.console`, not in a conflict state, and the cached container status is `starting` or `running`.
- While blocked the placeholder reads "Server Offline..." and the title explains why.
- **Enter** (`enter()`): if the input is non-empty and sending is allowed, dispatches `sendServerCommand {command}`, which the JS sends as `send command`. The command is prepended to `history` (newest first, **no length cap**), the history index is reset to −1 and the input is cleared. Wings logs `server:console.command`.
- **Up / Down** walk the history (`up()` moves to older entries, up to the last item; `down()` moves to newer entries, down to −1, which gives an empty input).
- History is stored in the **Laravel session** via the Livewire `#[Session(key: 'server.{server.id}.history')]` attribute, so it persists per server across page loads within the session.

#### Feature modals (console line triggers)

Mechanism:

1. An egg lists feature ids in `eggs.features` (a JSON array).
2. `ServerConfigurationStructureService` sends `egg.features = FeatureService::getMappings(egg.features)` to Wings in the server configuration, shaped `{feature_id: [listener strings…]}`.
3. When a console line matches a listener, Wings emits `feature match` with a JSON payload containing `key`. The exact matching rules (case-insensitive substring) are Wings behaviour and not visible in the panel.
4. The JS dispatches the Livewire event `mount-feature`. `Console::mountFeature()` looks up the feature by `key`, re-checks `authorize(user, server)` and mounts (or replaces) the modal action.

All features are registered in `app/Providers/Extensions/FeatureServiceProvider.php`. The Console page pre-caches the actions for the egg's active, authorized features in `boot()`.

| Feature id | Schema | Trigger strings (`getListeners()`) | Who sees it | Modal and action |
|---|---|---|---|---|
| `eula` | `app/Extensions/Features/Schemas/MinecraftEulaSchema.php` | `you need to agree to the eula in order to run the server` | `file.update` | Confirmation "Minecraft EULA" with a link to minecraft.net/eula and an **I Accept** button. Writes `eula.txt` = `eula=true` via Wings `POST /api/servers/{uuid}/files/write?file=eula.txt`, then Wings `POST …/power {action: restart}`. Shows a success or failure notification. |
| `java_version` | `app/Extensions/Features/Schemas/JavaVersionSchema.php` | `java.lang.UnsupportedClassVersionError`; `unsupported major.minor version`; `has been compiled by a more recent version of the java runtime`; `minecraft 1.17 requires running the server with java 16 or above`; `minecraft 1.18 requires running the server with java 17 or above`; `minecraft 1.19 requires running the server with java 17 or above` | `startup.docker-image` | "Unsupported Java Version". A **Docker Image** select lists the egg's `docker_images` (value = image, label = name). It defaults to the current image, is required, must differ from the current image (`notIn`), and is disabled when the current image is not one of the egg's images. On submit: sets `servers.image`, logs `server:startup.image {old,new}`, then Wings power `restart`. |
| `gsl_token` | `app/Extensions/Features/Schemas/GSLTokenSchema.php` | `(gsl token expired)`; `(account not found)` | `startup.update` | "Invalid GSL token". Shows an info line linking to steamcommunity.com/dev/managegameservers and a text input bound to the server variable whose `env_variable = STEAM_ACC`. The input is validated against that variable's `rules`; blank is allowed to clear it. Updates `server_variables.variable_value`, logs `server:startup.edit {variable, old, new}`, then Wings power `restart`. |
| `pid_limit` | `app/Extensions/Features/Schemas/PIDLimitSchema.php` | `pthread_create failed`; `failed to create thread`; `unable to create thread`; `unable to create native thread`; `unable to create new native thread`; `exception in thread "craft async scheduler management thread"` | everyone | Information only. Admins see "Memory or process limit reached..." advising them to raise `container_pid_limit` in the Wings `config.yml` and restart Wings. Users see "Possible resource limit reached..." telling them to contact an admin. The only button is Close; the action does nothing. |
| `steam_disk_space` | `app/Extensions/Features/Schemas/SteamDiskSpaceSchema.php` | `steamcmd needs 250mb of free disk space to update`; `0x202 after update job` | everyone | Information only. "Out of available disk space...", with an admin text (check `df -h`) or a user text (contact an admin). Close only. |

### Files resource

Sources: `app/Filament/Server/Resources/Files/FileResource.php` and its pages. The resource is conflict-blocked and visible with `file.read`. It sets `isScopedToTenant = false`; rows come from Wings, not from the DB.

The `File` model (`app/Models/File.php`) is a Sushi (in-memory SQLite) model:

- `getRows()` calls Wings `GET /api/servers/{uuid}/files/list-directory?directory={path}`, or `GET …/files/search?pattern=&directory=` for search (2 min timeout, cached 1 min per path and term).
- Row fields: `name`, `created_at`, `modified_at` (UTC), `mode`, `mode_bits` (int, e.g. 644), `size`, `is_directory`, `is_file`, `is_symlink`, `mime_type`.
- A `.jar` file whose mime is an archive type is re-typed as `application/java-archive`.
- On error a danger AlertBanner "Could not load files!" appears with the Wings error, or "Node connection failed" for connection errors, and the table is empty.
- **Archive** means `is_file` and the mime type is in `ARCHIVE_MIMES`: rar, x-rar-compressed, x-tar, x-br, x-bzip2, gzip, x-gzip, x-lzip, x-sz, x-xz, 7z, zstd, zip.
- **Editable** (`canEdit()`) means a regular file that is not an archive, not a symlink, not an image (SVG is allowed), and whose mime is not `application/java-archive`, `application/octet-stream` or `inode/directory`.
- **Icons:** folder, zip (archive), photo (image), file-symlink, file.

**Denylisted files:** the egg's `file_denylist` (`inherit_file_denylist`) is sent to Wings in the server configuration; Wings enforces it. The panel UI has no denylist logic of its own; Wings errors surface as notifications or banners.

#### Routes

| Page | Route | Perm |
|---|---|---|
| `ListFiles` | `/server/{id}/files/{path?}` (`path` matches `.*`, default `/`) | resource `canAccess` (`file.read`) |
| `EditFiles` | `/server/{id}/files/edit/{path}`; `getUrl()` appends a trailing `/` | `file.read-content` (abort 403 in `mount`) |
| `SearchFiles` | `/server/{id}/files/search/{searchTerm}?path=` | `file.read` |
| `DownloadFiles` | `/server/{id}/files/download/{path}` | `file.read-content` |

#### ListFiles

Sources: `app/Filament/Server/Resources/Files/Pages/ListFiles.php` and `resources/views/filament/server/pages/list-files.blade.php`.

- **Breadcrumbs:** "Files", then one crumb per path segment, each linking to that directory.
- **Table:**
  - Deferred loading; simple pagination with options 25/50/100/150/200 (default 50).
  - Search box filtering by name within the current directory.
  - Directories first (`orderByDesc('is_directory')`), then the default sort by name.

| Column | Notes |
|---|---|
| Name | searchable, sortable, icon by type |
| Size | bytes, readable; blank for directories; md+; sortable |
| Modified at | relative ("since"); md+; sortable |

- **Row click:** a directory opens `ListFiles` for `path/name`. An editable file opens `EditFiles`, but only with `file.read-content`. Anything else is not clickable.

**Row actions:**

| Action | Perm | Visible | Form | Effect (Wings endpoint) | Activity |
|---|---|---|---|---|---|
| Open | `file.read` | directories | — | navigate | — |
| Edit | `file.read-content` | `canEdit()` | — | navigate to `EditFiles` | — |
| Rename (in group) | `file.update` | all | `name` (required, default = current name) | `PUT /api/servers/{uuid}/files/rename {root: path, files:[{from,to}]}` | `server:file.rename {directory, files, from, to}` |
| Copy | `file.create` | files | — | `POST …/files/copy {location: path/name}` (Wings picks the copy's name *(Wings behaviour)*) | `server:file.copy {file}` |
| Download | `file.read-content` | files | — | opens `DownloadFiles` URL in a new tab | logged by `DownloadFiles` |
| Move | `file.update` | all | `location` (required, live) plus a computed **New Location** preview. Location is relative to the current dir; a trailing `/` means "into this directory, keep the name". | `PUT …/files/rename` | `server:file.rename {directory, files, to, from}` with resolved paths |
| Permissions | `file.update` | all | Three checkbox lists (**Owner**, **Group**, **Public**), each with read/write/execute and bulk toggle, prefilled from the digits of `mode_bits` | `POST …/files/chmod {root, files:[{file, mode:"755"}]}` (mode = owner/group/public octal digits) | **none** |
| Archive | `file.archive` | all | `name` (optional; placeholder `archive-YYYY-MM-DDTHHMMSSZ`), `extension` select: `tar.gz`, `zip`, `tar.bz2`, `tar.xz` | `POST …/files/compress {root, files:[name], name, extension}` (15 min timeout). The response JSON gives the archive name. | `server:file.compress {name, directory, files}` |
| Unarchive | `file.archive` | archives | — | `POST …/files/decompress {root, file}` (15 min timeout) | `server:file.decompress {directory, file}` |
| Delete | `file.delete` | all | Confirmation: "Delete {name} folder\|file" | `POST …/files/delete {root, files:[name]}` | `server:file.delete {directory, files}` |

Rename, Copy, Download, Move, Permissions, Archive and Unarchive are grouped in an ActionGroup ("…" menu). After each mutating action the page reloads itself (`refreshPage()`: redirect to the same path, SPA-aware).

**Bulk actions** (selection):

| Action | Perm | Form | Effect | Activity |
|---|---|---|---|---|
| Move | `file.update` | `location` (directory relative to the current one) plus a preview | `PUT …/files/rename` with `to = location/name` for each file | `server:file.rename {directory, files}` |
| Archive | `file.archive` | name and extension as above | `POST …/files/compress` with all names | `server:file.compress` |
| Delete | `file.delete` | confirmation *(Filament default)* | `POST …/files/delete` | `server:file.delete {directory, files[]}`; notification ":count files were deleted" |

**Toolbar actions:**

| Action | Perm | Form / behaviour | Effect | Activity |
|---|---|---|---|---|
| New file | `file.create` | `name` (required), `lang` (Syntax Highlighting select over `EditorLanguages`, default plaintext; changing it re-highlights Monaco), Monaco editor `editor` | `POST …/files/write?file={path/name}` with the raw body. A Wings 400 becomes `FileExistsException`, which shows the AlertBanner "`{path}` already exists!" and reloads the parent dir. | `server:file.write {file}` |
| New folder | `file.create` | `name` (required) | `POST …/files/create-directory {name, path}`; on 400 the same "already exists" banner | `server:file.create-directory {directory, name}` |
| Upload | `file.create` | Opens the browser file picker; see [Upload](#upload-files-and-folders). The button style follows the user customization `button_style`. | — | — |
| Upload from URL | `file.create` | `url` (required, must be a valid URL) | `POST …/files/pull {url, root: path}` (Wings downloads asynchronously) | `server:file.pull {url, directory}` |
| Nested search | `file.read` | `searchTerm`: required, regex `^[^*]*\*?[^*]*$` (at most one `*`), `minValue(3)` (a numeric min, see open questions), placeholder "ex. *.txt" | redirects to `SearchFiles` with `searchTerm` and `path` | — |
| Connect SFTP | `file.sftp` | link | opens `sftp://{username}.{uuid_short}@{daemon_sftp_alias\|fqdn}:{daemon_sftp}/{current dir}/`. Each path segment is rawurlencoded and the trailing `/` makes clients treat it as a directory. | — |

#### Upload (files and folders)

Sources:

- `app/Livewire/FileUploadManager.php` and `resources/views/livewire/file-upload-manager.blade.php`, mounted once per page and persisted across SPA navigation;
- `resources/views/filament/server/pages/file-upload.blade.php` (browse button and hidden `<input type=file multiple>`);
- `resources/views/filament/server/pages/list-files.blade.php` (full-window drag-and-drop zone with the overlay "Drop files to upload");
- `resources/js/file-uploader.js` (Alpine components `fileUploader`, `fileDropZone`, `fileBrowseButton`);
- `app/Http/Controllers/Api/Client/Servers/FileUploadController.php`.

**Input:**

- Browse: multiple files.
- Drop: files and whole folder trees, walked with `webkitGetAsEntry`/`readEntries`. Relative sub-paths are preserved and **empty directories** are recorded.
- Both sources dispatch the window event `server-file-upload` with `{files:[{file, path, basePath, serverUuid}], directories:[…]}`. `basePath` is the current `ListFiles` path.

**Large-batch guard:** a drop of **≥500 files** asks for confirmation first: "You are about to upload :count files." with **Upload them** and **Cancel** buttons.

**Empty directories** are created up front through the Livewire `createFolder(serverUuid, folderPath, basePath)`, which calls Wings `create-directory` and logs `server:file.create-directory`. Paths containing `..` segments or NUL are rejected with 422. "Already exists" is ignored. Wings creates other parent directories itself.

**Size limit:** Livewire `getUploadSizeLimit(serverUuid)` returns `node.upload_size` (MB, DB default 100) × 1024 × 1024. It is cached per server for the component's lifetime; a failed lookup is not cached. A file over the limit is marked failed immediately with ":name exceeds the upload limit of :limit".

**Authorization:** every Livewire method resolves the server by `uuid` and aborts 403 unless the user has `file.create`.

**Transport:**

- **No chunking**, at most **3 concurrent** uploads.
- For each file:
  1. `GET /api/client/servers/{uuid}/files/upload` (session cookie, `X-Requested-With`). This needs `file.create` (`UploadFileRequest`) and passes the client-API `AuthenticateServerAccess` checks, including the conflict-state block. The response is `{"object":"signed_url","attributes":{"url": "{node}/upload/file?token={JWT}"}}`.
     - JWT scope `file-upload`, claims `server_uuid`, `user_uuid`, `exp` 15 min, same `jti` scheme as above.
     - A **new token is fetched per file**, because Wings rejects reused tokens.
  2. Append `directory=/{basePath}/{relative dir}` (normalized, leading `/`).
  3. XHR `POST` of multipart form data with the single field `files`. Progress and speed are tracked; a 2xx response means success.
- **Errors:** the Wings JSON `{error}`, else "An error occurred while uploading (status)"; "Network error"; "Failed to get an upload token".
- A **401/419** on any panel call stops the whole queue with "Your session expired, reload the page and try again".

**Queue UI** (floating panel):

- Header "Uploading Files" with ":done of :total"; per-file progress, speed and status (`pending`, `uploading`, `complete`, `error`) with the error text.
- Per-row Retry; Cancel all (aborts in-flight XHRs, marks pending files "Cancelled"); Minimize/Restore; Close.
- Esc minimizes while busy, otherwise closes.
- A `beforeunload` guard is active while uploading.

**Finish:**

- Successful files are grouped by (server, directory). Each group is logged through Livewire `logUploadedFiles`, which writes the activity event **`server:files.uploaded`** `{directory, files[]}`.
- The window event `server-files-uploaded` re-renders `ListFiles`.
- Notification: "Files uploaded successfully", "Failed to upload files", or ":failed of :total files failed to upload".
- The panel auto-closes after 1 s when nothing failed. Rows already reported are not logged again when a later drop is appended to the same queue.

#### EditFiles

Sources: `app/Filament/Server/Resources/Files/Pages/EditFiles.php`, `app/Filament/Components/Forms/Fields/MonacoEditor.php`, `resources/views/filament/components/monaco-editor.blade.php`, `config/monaco-editor.php`.

- **Mount:** requires `file.read-content`. Remembers `url()->previous()` for Cancel. Shows an info AlertBanner for special files. The built-in one is `.pelicanignore` ("Any files or directories listed in here will be excluded from backups…"); plugins can add more through `File::registerSpecialFile`.
- **Activity:** `server:file.read {file}` is logged inside `form()`, so it fires whenever the form schema is built.
- **Loading:**
  - Wings `GET /api/servers/{uuid}/files/contents?file={path}`.
  - If `Content-Length` > `config('panel.files.max_edit_size')` (default **4 MiB = 4,194,304 bytes**, env `PANEL_FILES_MAX_EDIT_SIZE`): danger banner "`{name}` is too large! Max is {readable}", then redirect to the parent directory.
  - Wings 404: "not found" banner and redirect.
  - Wings 400 (e.g. a directory): the same "not found" title, banner id `file_is_directory`, and redirect.
  - Connection error: silent redirect (ListFiles shows the banner).
  - Contents are passed through `convert_to_utf8()`.
- **Section title:** "Editing: {path}".
- **Fields:**

| Field | Behaviour |
|---|---|
| Syntax Highlighting (`lang`) | Searchable select over all `EditorLanguages` cases. The default is detected from the file extension, see the mapping below. Changing it dispatches `setLanguage` to Monaco. |
| Editor | Monaco **0.49.0** loaded from cdnjs. Custom theme `blackboard`, font size 15px, `lineNumbersMinChars` 3, automatic layout, word wrap on. `readOnly` follows the field's disabled state, which is **never disabled** here: users without `file.update` can type but cannot save. |

- **Footer actions:**

| Action | Perm | Key binding | Effect |
|---|---|---|---|
| Save & Close | `file.update` | `mod+shift+s` | `POST …/files/write?file={path}` with the editor content as the raw body. Logs `server:file.write {file}`. Notification "File Saved". Redirects to the parent directory. |
| Save | `file.update` | `mod+s` | Same write and log, stays on the page. |
| Cancel | — | — | `history.back()` if a referrer exists, else navigates to the parent listing. |

- **Breadcrumbs:** Files › each directory › file name.

**Language detection** (`EditorLanguages::fromWithAlias(pathinfo(path, EXTENSION))`). Matching is exact and **case-sensitive** on the last extension only.

| Extension(s) | Monaco language |
|---|---|
| `h` | c |
| `cc`, `hpp` | cpp |
| `cs` | csharp |
| `class` | java |
| `htm` | html |
| `js`, `mjs`, `cjs` | javascript |
| `kt`, `kts` | kotlin |
| `md` | markdown |
| `m` | objective-c |
| `pl`, `pm` | perl |
| `php3`, `php4`, `php5`, `phtml` | php |
| `py`, `pyc`, `pyo`, `pyi` | python |
| `rdata`, `rds` | r |
| `rb`, `erb` | ruby |
| `sc` | scala |
| `sh`, `zsh` | shell |
| `ts`, `tsx` | typescript |
| `yml` | yaml |
| any extension equal to a language id | that language |
| anything else (incl. `toml`, `properties`, `cfg`, `conf`, `txt`, `log`, `rs`, no extension) | plaintext |

Language ids that work through the "equal to a language id" row include: abap, apex, azcali, bat, bicep, cameligo, clojure, coffeescript, c, cpp, csharp, csp, css, cypher, dart, dockerfile, ecl, elixir, flow9, fsharp, go, graphql, handlebars, hcl, html, ini, java, javascript, julia, json, kotlin, less, lexon, lua, liquid, m3, markdown, mdx, mips, msdax, mysql, objective-c, pascal, pascaligo, perl, pgsql, php, pla, postiats, powerquery, powershell, proto, pug, python, qsharp, r, razor, redis, redshift, restructuredtext, ruby, rust, sb, scala, scheme, scss, shell, sol, aes, sparql, sql, st, swift, systemverilog, verilog, tcl, twig, typescript, typespec, vb, wgsl, xml, yaml.

#### DownloadFiles

Source: `app/Filament/Server/Resources/Files/Pages/DownloadFiles.php`. The page has no UI; it is reached from the Download row action, which opens it in a new tab.

1. `mount` requires `file.read-content`.
2. It mints a JWT: scope `file-download`, claims `file_path`, `server_uuid`, `user_uuid`, expiry **15 min**.
3. It logs `server:file.download {file}`.
4. It redirects to `{node scheme://fqdn:daemon_connect}/download/file?token={JWT}`.

#### SearchFiles

Source: `app/Filament/Server/Resources/Files/Pages/SearchFiles.php`.

- Title "Nested Search"; breadcrumbs: Files › `Search "term"`.
- Not paginated. Columns: Name (the path returned by Wings), Size, Modified at.
- Results come from Wings `GET …/files/search?pattern={term}&directory={path}`.
- Clicking a directory opens `ListFiles` with `path = name`; clicking an editable file opens `EditFiles`. Unlike `ListFiles`, there is no `file.read-content` check before linking, although the edit page enforces it.
- There are no row actions.

### Backups resource

Sources: `app/Filament/Server/Resources/Backups/BackupResource.php` and `…/Pages/ListBackups.php`.

**URL** `/server/{id}/backups`. Visible with `backup.read`; conflict-blocked. No breadcrumbs. The table does **not** poll, so status changes appear only after a reload.

**Columns:**

| Column | Notes |
|---|---|
| Name | searchable |
| Size | `bytes`, readable |
| Created at | relative, sortable |
| Status | badge: In Progress (`completed_at` null) / Successful / Failed |
| Lock Status | lock or lock-open icon, md+ |

**Header action: Notification Settings** (any user). Two toggles stored per user and per server through `user->updateServerSetting()`:

- Manual Backup Notifications;
- Scheduled Backup Notifications.

Both default to false (`app/Enums/ServerUserSettingKey.php`). They control the notification and email sent when a backup completes or fails. The same toggles appear on Settings.

**Toolbar: Create Backup** (`backup.create`; icon button).

- Disabled, red, with tooltip "Backup Limit Reached" when `backups.count >= backup_limit`. A limit of 0 or null therefore always disables it.
- Form:

| Field | Rules |
|---|---|
| Name | optional; blank becomes `Backup at {Y-m-d H:i:s}` |
| Ignored Files & Directories | textarea, one pattern per line |
| Locked? | toggle; "Prevents this backup from being deleted until explicitly unlocked." Applied only if the user also has `backup.delete`. |

- Service `app/Services/Backups/InitiateBackupService.php`:
  1. **Throttle:** at most `backups.throttles.limit` (default 2) non-failed backups per `period` (default 600 s; env `BACKUP_THROTTLE_LIMIT` / `BACKUP_THROTTLE_PERIOD`). Exceeding it throws a 429 `TooManyRequestsHttpException` with `Retry-After`; the UI shows "Backup Creation Failed" with "… Try again in N seconds.".
  2. **Limit:** fails if non-failed backups ≥ `backup_limit`. There is no rotation for manual backups.
  3. **Backup host:** the first host attached to the server's node, else the first host attached to no nodes.
  4. Inserts the `backups` row: uuid, name, `ignored_files`, `backup_host_id`, `is_locked`, `is_scheduled = false`.
  5. Calls the adapter's `createBackup`. For the Wings adapter that is `POST /api/servers/{uuid}/backup {adapter, uuid, ignore: "\n"-joined}`. The S3 adapter is in `app/Extensions/BackupAdapter/Schemas/S3BackupSchema.php`.
- Activity: `server:backup.start {name, locked}`. Notification "Backup Created Successfully" with ":name created".

**Row actions** (one action group):

| Action | Perm | Visible / enabled | Form | Effect | Activity |
|---|---|---|---|---|---|
| Rename | `backup.delete` | successful only | `name` (required, max 255) | updates the `name` column | `server:backup.rename {old_name, new_name}` |
| Lock / Unlock | `backup.delete` | successful only | — | toggles `is_locked` (via `BackupController::toggleLock`) | `server:backup.lock` / `server:backup.unlock {name}` |
| Download | `backup.download` | successful only | — | Opens the adapter download URL in a new tab. Wings: `{node}/download/backup?token={JWT}` with scope `backup-download`, claims `backup_uuid`, `server_uuid`, `user_uuid`, 15 min. S3: a presigned `GetObject` for `{server_uuid}/{backup_uuid}.tar.gz`, valid 5 min. The URL is minted when the table renders. | **none** in the UI (the client API logs `server:backup.download`) |
| Restore | `backup.restore` | successful only | Info text ("Your server will be stopped…") and a **"Delete all files before restoring backup?"** checkbox (`truncate`) | Fails with a notification if `server.status` is not null, or if the backup is not complete. Otherwise, in one transaction: sets `server.status = restoring_backup`, then Wings `POST /api/servers/{uuid}/backup/{backup_uuid}/restore {adapter, truncate_directory, download_url}`. `download_url` is the adapter download link. Notification "Restoring Backup". | `server:backup.restore {name, truncate}` |
| Delete | `backup.delete` | hidden while in progress; **disabled** when locked and not failed; confirmation "Do you wish to delete :backup?" | — | `DeleteBackupService`: refuses locked successful backups, calls the adapter delete (Wings `DELETE /api/servers/{uuid}/backup/{uuid}`, where a 404 is tolerated), then deletes the row. On a Wings connection failure: "Could not delete backup / Connection to node failed." | `server:backup.delete {name, failed}` |

### Databases resource

Sources: `app/Filament/Server/Resources/Databases/DatabaseResource.php` and `…/Pages/ListDatabases.php`.

**URL** `/server/{id}/databases`. Visible with `database.read`; conflict-blocked. The badge shows `count / database_limit`.

**Columns:** Host (`host.name:port`, badge), Database, Username, Remote, Created at (sortable).

**Toolbar: Create Database** (`database.create` through the policy *(Filament default CreateAction authorization)*).

- Icon button. Disabled and red ("Database limit reached") when `count >= database_limit`, so a limit of 0 or null disables it.
- Form:

| Field | Rules / behaviour |
|---|---|
| Database Host | required select over **the database hosts linked to the server's node** |
| Database Name | optional, shown with the prefix `s{server_id}_`. Blank becomes `Str::random(12)`. The final name is `s{id}_{name}` truncated to 48 chars. Model rules: `alpha_dash`, 3–48. |
| Connections From (`remote`) | default `%`; model rule regex `^[\w\-\/.%:]+$` |

- Service: `DatabaseManagementService::create`.
  1. Requires `panel.client_features.databases.enabled` (default true).
  2. Re-checks the limit.
  3. Username `u{server_id}_{random10}`, password of 24 random characters including specials.
  4. Unique-name check per server.
  5. On the remote host: create database, create user, grant, flush privileges.
- Activity: `server:database.create {name}`. Notifications "Created :database" / "Failed to create :database".

**Row actions:**

- **View** (`database.read`). Modal "Viewing: :database" with read-only fields:

| Field | Behaviour |
|---|---|
| Host | `address()`, copyable |
| Database | copyable |
| Username | copyable |
| Password | revealable and copyable; **hidden without `database.view-password`**. Hint action **Rotate password**, see below. |
| Remote | — |
| Max Connections | "Unlimited" when 0 or null |
| JDBC Connection String | `jdbc:mysql://{username}:{urlencoded password}@{host}:{port}/{database}`; revealable and copyable; hidden without `database.view-password` |

- **Rotate password** (`database.update`; requires confirmation). Generates a new password, then drops and recreates the user, grants and flushes on the remote host. Refreshes the password and JDBC fields. Logs `server:database.rotate-password {name}`. On error the exception text is shown only to admins.
- **Delete** (`database.delete` via the policy; Filament confirmation). `DatabaseManagementService::delete` drops the database and the user on the host and deletes the row. Logs `server:database.delete {name}`.

### Schedules resource

Sources: `app/Filament/Server/Resources/Schedules/ScheduleResource.php`, `…/Pages/{List,Create,View,Edit}Schedule.php`, `…/RelationManagers/TasksRelationManager.php`, `app/Extensions/Tasks/**`.

Visible with `schedule.read`; conflict-blocked.

| Page | URL | Perm |
|---|---|---|
| List | `/server/{id}/schedules` | `schedule.read` |
| Create | `/server/{id}/schedules/create` | `schedule.create` |
| View | `/server/{id}/schedules/{scheduleId}` | `schedule.read` |
| Edit | `/server/{id}/schedules/{scheduleId}/edit` | `schedule.update` |

#### List

**Columns:**

| Column | Notes |
|---|---|
| Name | searchable |
| Cron | `m h dom mon dow`; the description line shows the schedule's `timezone` |
| Status | badge: Inactive (red, `!is_active`) / Processing (amber, `is_processing`) / Active (green) |
| Only When Online | boolean icon, sortable |
| Last Run | relative, sortable; "Never" if unset |
| Next Run | relative, sortable; only shown when Active, else "Never" |

**Row actions:**

- **View**: shown only when the user **cannot** edit.
- **Edit** (`schedule.update`).
- **Delete** (`schedule.delete`; logs `server:schedule.delete {name}`).

**Toolbar:**

- **New Schedule** (`schedule.create`).
- **Import Schedule** (`schedule.create`; `app/Filament/Components/Actions/ImportScheduleAction.php`).
  - Tab **File**: one or more `.json` files (`application/json`), not stored on disk.
  - Tab **URL**: a repeater of URLs, each must be a valid URL ending in `.json`; GitHub `blob` URLs are rewritten to `raw`.
  - Each file or URL goes through `ScheduleImporterService::fromFile` / `fromUrl` (HTTP timeout 5 s). It creates the schedule from the JSON (cron fields default to `0 0 * * *` when absent, timezone default UTC, `next_run_at` computed) and creates its tasks.
  - Results are reported in two notifications, "Import Failed" and "Import Success", each listing names derived from the file name after `schedule-`.

#### Schedule form

Shared by create, edit and view.

| Field | Type / rules | Notes |
|---|---|---|
| Name | text, required (max 255 by model rule) | |
| Only when Server is Online? (`only_when_online`) | toggle, default on | "Only execute this schedule when the server is in a running state." |
| Enable Schedule? (`is_active`) | toggle, default on | hidden on View |
| Status | toggle-buttons, view-only | shows the current status (or "New") |
| Cron section | description: "The cron inputs below use your timezone." plus "Next run in your timezone (tz): **date**", or "Invalid" | |
| Minute / Hour / Day of Month / Month / Day of Week | text, all required, live. Defaults `*/5`, `*`, `*`, `*`, `*`. | Free-form cron syntax, evaluated with `dragonmantank/cron-expression`. |

**Presets** (buttons, hidden on View; the matching preset is highlighted green):

| Preset | Sets (m h dom mon dow) |
|---|---|
| Hourly | `0 * * * *` |
| Daily | `0 0 * * *` |
| Weekly (Monday) | `0 0 * * 1` |
| Weekly (Sunday) | `0 0 * * 0` |
| Monthly | `0 0 1 * *` |
| Every x minutes | modal number 1–60 → `*/x * * * *` |
| Every x hours | 1–24 → `0 */x * * *` |
| Every x days | 1–24 → `0 0 */x * *` |
| Every x months | 1–24 → `0 0 1 */x *` |
| Every x day of week | select Monday(1) … Saturday(6), Sunday(0) → `0 0 * * x` |

The last four presets have no explicit label; see open questions.

**Save logic:**

- **Create:** sets `server_id`, `timezone = user.timezone` (default UTC) and `next_run_at = CronExpression(...).getNextRunDate(now(tz))` stored as UTC. An invalid cron shows "The cron data provided does not evaluate to a valid expression" and halts. Logs `server:schedule.create {name}`. Key binding `mod+s`. "Create another" is disabled.
- **Edit:** recomputes `timezone` and `next_run_at` on every save. Logs `server:schedule.update {name}`.

**Edit page header actions** (the default form buttons are removed):

| Action | Perm | Behaviour |
|---|---|---|
| Delete | `schedule.delete` | logs `server:schedule.delete` |
| Run Now | `schedule.update` | Disabled when the schedule has no tasks or is processing; the tooltip says so. `ProcessScheduleService::handle($schedule, now: true)` sets `is_processing = true` and the next `next_run_at`, and marks the first task queued. If `only_when_online`, it checks the Wings state (5 s timeout) and skips when offline or missing. Otherwise it runs the first `RunTaskJob` synchronously. Logs `server:schedule.execute {name}` and refills the form. |
| Export Schedule | `schedule.read` | Downloads `schedule-{kebab-name}.json`: `{name, is_active, only_when_online, cron_minute, cron_hour, cron_day_of_month, cron_month, cron_day_of_week, timezone, tasks:[{sequence_id, action, payload, time_offset, continue_on_failure}]}` |
| Save Schedule | — | `mod+s` |

**View page header:** an Edit button.

#### Tasks relation manager

The relation manager appears on Edit and on View. It is **read-only on View** *(Filament default `readOnlyRelationManagersOnResourceViewPagesByDefault = true`)*. There is no `TaskPolicy`, so on Edit task create, edit, delete and reorder are allowed for anyone who can open the edit page *(Filament non-strict authorization default)*.

**Table:**

- Reorderable by drag; the reorder writes `sequence_id`. Default sort is `sequence_id`.
- Empty state: "No Tasks".

| Column | Notes |
|---|---|
| Action | task type name |
| Payload | formatted, badge: power → `Ucfirst`; other types → one badge per line. Tooltip shows the payload label; "No Payload" placeholder. |
| Time Offset | `N Second(s)`; "First Task" for the first task; hidden when `queue.default = sync` |
| Continue On Failure | boolean |

**Create Task** (header action):

- Disabled, labelled "Task Limit Reached", when the task count ≥ `panel.client_features.schedules.per_schedule_task_limit` (default 10; env `PANEL_PER_SCHEDULE_TASK_LIMIT`).
- New tasks get `sequence_id = max + 1`.
- Activity `server:task.create {name, action, payload}`.

**Edit** logs `server:task.update`; **Delete** logs `server:task.delete`.

**Task form:**

| Field | Rules |
|---|---|
| Action | Required select over registered task types; default is the first registered (`power`). An option is disabled when `canCreate($schedule)` is false. Changing it resets the payload to the type's default. |
| *(payload fields)* | per type, see below |
| Time Offset | Number 0–900, suffix "Seconds", default 0. Hidden when the queue driver is `sync`, when the schedule has no tasks yet, or when editing the first task. It is the delay before this task runs after the previous one (the model rule allows 0–900). |
| Continue On Failure | toggle |

**Task types** (`app/Extensions/Tasks/Schemas/*`, registered in this order). Plugins can register more through `TaskService::register`.

| Id | Name | Payload field | Default | Can create | Runtime effect |
|---|---|---|---|---|---|
| `power` | Send Power Action | `payload` select, required: Start/Restart/Stop/Kill (`start`/`restart`/`stop`/`kill`) | `restart` | always | Wings `POST /api/servers/{uuid}/power {action}` |
| `command` | Send Command | `payload` text, required, label "Command" | — | always | `Server::send()` sends the command to Wings |
| `backup` | Create Backup | `payload` textarea "Files to Ignore" (one per line, optional) | — | only if `server.backup_limit > 0` | `InitiateBackupService` with `isScheduled = true` and **override = true**: when at the limit it deletes the oldest unlocked successful backup first |
| `delete_files` | Delete Files | `payload` textarea "Files to Delete", one path per line | — | always | For each line: list `dirname(line)` via Wings and match `basename(line)` as a glob (`Str::is`, so `*` works). Then one Wings `POST …/files/delete {root:'/', files}` |

### Users (subusers) resource

Source: `app/Filament/Server/Resources/Subusers/SubuserResource.php`.

**URL** `/server/{id}/subusers`. Visible with `user.read`; conflict-blocked. The badge shows the subuser count. Not paginated.

**Columns:**

| Column | Notes |
|---|---|
| (avatar) | lg+ |
| Username | searchable |
| Email | searchable |
| Permissions | count, excluding hidden permissions (`websocket.connect`) |

**Permission editor** (shared by Invite and Edit): one tab per non-hidden permission group, in the order of the [full permission list](#full-subuser-permission-list) plus any plugin groups.

- Each tab has a section with the group description and icon, and a two-column checkbox list.
- Option label: `Str::headline(perm)`, e.g. "Read Content". Description from lang. Bulk toggle.
- An **Assign all** button checks every permission in every tab.

**Toolbar: Invite User** (`user.create`):

- Form: `email` (required, email) plus the permission tabs.
- Permissions are flattened to `group.perm` and `websocket.connect` is appended.
- `SubuserCreationService`:
  1. Email is lowercased.
  2. **If no user exists with that email**, a new account is created: username = email local part + 3 random chars, random password, a password-reset token generated, and an `AccountCreated` email if `panel.email.send_account_created_notification` is set.
  3. If the user is the owner, it fails with "You cannot add the server owner as a subuser for this server."
  4. If the user is already a subuser, it fails with "A user with that email address is already assigned as a subuser…".
  5. Permissions are **filtered to those the inviting user holds**; `websocket.connect` is always kept.
  6. Upserts the `subusers` row and fires `SubUserAdded`, which sends an email/notification if `send_added_to_server_notification` is set.
- Notification "User Invited!" or "Failed to invite user!" with the message, then redirect to the list.

**Row actions:**

- **Edit User** (`user.update`; hidden on your own row).
  - Modal "Editing :email" with the email disabled and the tabs prefilled.
  - `SubuserUpdateService` filters the new set to permissions the editor holds (plus `websocket.connect`). Permissions the editor lacks are therefore **dropped** from the subuser.
  - Only if the set changed: updates the row and dispatches `RevokeSftpAccessJob`, which calls Wings `POST /api/deauthorize-user {user: uuid, servers:[server uuid]}` with retries.
  - Logs `server:subuser.update {email, old, new, revoked}`.
- **Delete User** (`user.delete`; hidden on your own row).
  - `SubuserDeletionService` deletes the row, fires `SubUserRemoved` (optional email), and dispatches `RevokeSftpAccessJob`.
  - Logs `server:subuser.delete {email, revoked}`.

### Network (allocations) resource

Source: `app/Filament/Server/Resources/Allocations/AllocationResource.php`.

**URL** `/server/{id}/allocations`; title "Network". Visible with `allocation.read`; conflict-blocked. Rows are the allocations whose `server_id` is this server.

**Columns:**

| Column | Notes |
|---|---|
| Address | `ip_alias` if set, else `ip` |
| Port | the port, or "Hidden" when `show_port = false` |
| Notes | Inline text input (sm+). Editable only with `allocation.update`; placeholder "No Notes". Saves directly to `allocations.notes` (model rule max 256). **No activity logged.** |
| Primary | Star icon, filled amber for the primary. Clicking sets `servers.allocation_id` to this allocation, only with `allocation.update`. **No activity logged.** Tooltip "Primary" / "Make Primary". |
| Locked? | lock icon; tooltip "Locked allocations can only be deleted by admins" |

**Row action Delete** (`allocation.delete`):

- Visible only when (the allocation is not locked **or** the user can `update` the node) **and** the server has more than one allocation.
- `Server::releaseAllocations([id], keepOne: true)`. Inside a transaction with a row lock:
  1. Refuses if it would leave no allocation on the node ("You cannot delete the last allocation for this server.").
  2. Resets the allocation to `server_id = null`, `notes = null`, `is_locked = false`, `show_port = true`.
  3. If the primary was removed, picks the lowest-id remaining allocation on the node as the new primary and logs `server:allocation.primary`.
- Logs `server:allocation.delete {allocation}`.

**Toolbar Add Allocation** (`allocation.create`):

- **Hidden** unless `panel.client_features.allocations.enabled` (env `PANEL_CLIENT_ALLOCATIONS_ENABLED`, default **false**).
- Disabled, red, when the server has no primary allocation ("A primary allocation is required before adding more") or when `count >= allocation_limit` ("Allocation limit reached"). A limit of 0 or null disables it.
- `FindAssignableAllocationService`:
  1. Picks a random unassigned allocation on the same node **and the same IP as the primary**, with a port inside [`range_start`, `range_end`] (env `PANEL_CLIENT_ALLOCATIONS_RANGE_START` / `_END`).
  2. If none exists and `create_new` (default true) is set, creates a new allocation on a random free port in that range.
  3. Otherwise fails with "no space".
  4. Assigns it to the server and, if the server had no primary, makes it primary.
- Logs `server:allocation.create {allocation}`.

### Activity resource

Sources: `app/Filament/Server/Resources/Activities/ActivityResource.php` and `…/Pages/ListActivities.php`.

**URL** `/server/{id}/activities`. Visible with `activity.read`. Not conflict-blocked.

**Query:**

- `activity_logs` that have a subject row pointing at this server.
- Excludes the events in `ActivityLog::DISABLED_EVENTS` (`server:file.upload`).
- If `config('activity.hide_admin_activity')` (env `APP_ACTIVITY_HIDE_ADMIN`, default false), also hides events whose actor is a user with any role, unless that user is the owner or a subuser.
- Default sort: timestamp desc. Pagination 25/50 (default 25).

**Columns:**

| Column | Notes |
|---|---|
| Event | HTML label from `lang/en/activity.php` (`activity.{event with ':' → '.'}`) with properties interpolated; `count` drives pluralization. The description line shows the raw event key. The icon reflects the actor: API key → api, user → user, `actor_id` null → desktop ("System"), missing user → user-off. |
| User | Username, plus ` (email)` if the **viewer is an admin** or the actor is the owner or a subuser. "System" or "Deleted User" otherwise. Tooltip shows the IP if the viewer `can('seeIps activityLog')`. Links to the admin EditUser page if the viewer can update that user. |
| Timestamp | relative, sortable |

**Filters:**

- **Event**: searchable select over the distinct events in the result set.
- **User**: searchable select over the distinct actors.

**Row action View:** modal showing the event label, the user (with IP when allowed, and an "Edit" hint link to the admin panel), the timestamp, and **Metadata** (key-value, with properties dot-flattened).

**Event labels (server)** from `lang/en/activity.php` → `server`:

| Event | Label |
|---|---|
| `server:console.command` | Executed "**:command**" on the server |
| `server:power.start` / `stop` / `restart` / `kill` | Started the server / Stopped the server / Restarted the server / Killed the server process |
| `server:backup.download` | Downloaded the **:name** backup |
| `server:backup.delete` | Deleted the **:name** backup |
| `server:backup.restore` | Restored the **:name** backup (deleted files: **:truncate**) |
| `server:backup.restore-complete` / `restore-failed` | Completed / Failed to complete restoration of the **:name** backup |
| `server:backup.start` | Started a new backup **:name** |
| `server:backup.complete` / `fail` | Marked the **:name** backup as complete / failed |
| `server:backup.lock` / `unlock` | Locked / Unlocked the **:name** backup |
| `server:backup.rename` | Renamed backup from "**:old_name**" to "**:new_name**" |
| `server:database.create` / `rotate-password` / `delete` | Created new database **:name** / Password rotated for database **:name** / Deleted database **:name** |
| `server:file.compress` | Compressed **:directory:files** \| Compressed **:count** files in **:directory** |
| `server:file.read` | Viewed the contents of **:file** |
| `server:file.copy` | Created a copy of **:file** |
| `server:file.create-directory` | Created directory **:directory:name** |
| `server:file.decompress` | Decompressed **:file** in **:directory** |
| `server:file.delete` | Deleted **:directory:files** \| Deleted **:count** files in **:directory** |
| `server:file.download` | Downloaded **:file** |
| `server:file.pull` | Downloaded a remote file from **:url** to **:directory** |
| `server:file.rename` | Moved/ Renamed **:from** to **:to** \| Moved/ Renamed **:count** files in **:directory** |
| `server:file.write` | Wrote new content to **:file** |
| `server:file.upload` | Began a file upload (disabled in the list) |
| `server:file.uploaded` | Uploaded **:directory:file** |
| `server:sftp.denied` | Blocked SFTP access due to permissions |
| `server:sftp.create` / `write` / `delete` / `create-directory` / `rename` | Created / Modified the contents of / Deleted / Created directory / Renamed (singular and `:count` plural forms) |
| `server:allocation.create` / `notes` / `primary` / `delete` | Added **:allocation** to the server / Updated the notes for **:allocation** from "**:old**" to "**:new**" / Set **:allocation** as the primary server allocation / Deleted the **:allocation** allocation |
| `server:schedule.create` / `update` / `execute` / `delete` | Created / Updated / Manually executed / Deleted the **:name** schedule |
| `server:task.create` / `update` / `delete` | Created a new / Updated the / Deleted the "**:action**" task for the **:name** schedule |
| `server:settings.rename` | Renamed the server from "**:old**" to "**:new**" |
| `server:settings.description` | Changed the server description from "**:old**" to "**:new**" |
| `server:settings.reinstall` | Reinstalled server |
| `server:startup.edit` | Changed the **:variable** variable from "**:old**" to "**:new**" |
| `server:startup.image` | Updated the Docker Image for the server from **:old** to **:new** |
| `server:startup.command` | Updated the Startup Command for the server from **:old** to **:new** |
| `server:subuser.create` / `update` / `delete` | Added **:email** as a subuser / Updated the subuser permissions for **:email** / Removed **:email** as a subuser |
| `server:mount.update` | Updated the mounts for the server |
| `server:crashed` | Server crashed |

`server:power.*`, `server:console.command` and `server:sftp.*` are produced by Wings and ingested through `POST /api/remote/activity` (`app/Http/Controllers/Api/Remote/ActivityProcessingController.php`); only events prefixed `server:` are accepted.

### ServerFormPage (base for Settings, Startup and Mounts)

Source: `app/Filament/Server/Pages/ServerFormPage.php` with view `resources/views/filament/server/pages/server-form-page.blade.php`.

- An abstract Filament `Page` holding a form with state path `data`, bound to the tenant `Server` as model/record.
- Uses `BlockAccessInConflict` (conflict-blocked) and the header action/widget customization traits.
- `mount()` calls `authorizeAccess()` (a no-op by default) and then `fillForm()`, which fills from `server->attributesToArray()`.
- The view wraps the form in `wire:submit="save"`, but none of the subclasses use a submit button. **Every field saves itself** through `live()` / `afterStateUpdated` hooks.

### Settings page

Source: `app/Filament/Server/Pages/Settings.php`. **URL** `/server/{id}/settings`.

- Visible to **every tenant user**: there is no page-level permission, only per-field checks. Conflict-blocked.
- Title "Settings".

**Section "Server Information"**

| Field | Behaviour | Perm |
|---|---|---|
| Server Name | Text, required. Saves on blur: `servers.name` (model rule 1–255). Notification "Updated Server Name" with `old -> new`. Logs `server:settings.rename {old, new}` if changed. | Disabled without `settings.rename`; the server side aborts 403. |
| Server Description | Textarea, autosize. Saves on blur (`servers.description`). Logs `server:settings.description {old, new}`. **Hidden** when `panel.editable_server_descriptions` is false (env `PANEL_EDITABLE_SERVER_DESCRIPTIONS`, default true). | Disabled without `settings.description` |
| Icon image | Shows `server.icon` or the egg icon; tooltip "Using Egg Icon" when falling back. Hidden if neither exists. | — |
| Upload Icon (icon button) | See the icon upload rows below. | `settings.change-icon` |
| Delete Icon | Visible only when the server has its own icon. Deletes every `icons/server/{uuid}.*` file. | `settings.change-icon` |
| Server UUID | read-only | — |
| Server ID (`uuid_short`) | read-only | — |
| Node Name | read-only | — |

Upload Icon modal (`app/Filament/Components/Actions/UploadIcon.php`) has two tabs:

| Tab | Behaviour |
|---|---|
| **URL** | Must be http/https, a valid URL, and resolve to a **public** IP (private and reserved ranges are rejected). Fetched with a 5 s timeout, 1 s connect timeout, no redirects. The extension is taken from the URL path. |
| **File** | Image upload, max 256 KB, png/jpeg/webp, with image editor. |

The icon is stored on the `public` disk as `icons/server/{uuid}.{png\|jpg\|webp}`; other extensions for the same uuid are removed. No activity is logged.

**Fieldset "Limits"** (read-only displays):

| Limit | Display |
|---|---|
| CPU | `N%` or "Unlimited" |
| Memory | readable bytes of `memory × 2²⁰`, or "Unlimited" |
| Disk Space | same as Memory |
| Backups | `{count} of {limit}` or "No Backups" |
| Databases | `{count} of {limit}` or "No Databases" |
| Allocations | `{count} of {limit}` or "No Additional Allocations" |

**Fieldset "SFTP Information"** (hidden without `file.sftp`):

- Connection: `sftp://{username}.{uuid_short}@{sftp_alias|fqdn}:{daemon_sftp}`, copyable, with a "Connect to SFTP" link action.
- Username: `{panel username}.{uuid_short}`, copyable.
- Password: the text "Your SFTP password is the same as the password you use to access this panel."

There is **no separate "Debug information" box**; the UUID, short ID and node name above cover it.

**Section "Reinstall Server"** (hidden without `settings.reinstall`):

- Explanatory text, and a danger **Reinstall** button with confirmation "Are you sure you want to reinstall the server?" / "Yes, Reinstall".
- `ReinstallServerService` runs in one transaction: sets `servers.status = installing`, then Wings `POST /api/servers/{uuid}/reinstall`.
- Logs `server:settings.reinstall`. Notification "Reinstall Started" and redirect to Console; on failure the notification is "Reinstall Failed" with the message.

**Section "Notifications":** two toggles, Manual Backup Notifications and Scheduled Backup Notifications. They save immediately per user and per server and show "Notification Settings Updated".

### Startup page

Source: `app/Filament/Server/Pages/Startup.php`. **URL** `/server/{id}/startup`. Requires `startup.read` (both `canAccess` and `mount`); conflict-blocked.

| Field | Shown when | Behaviour | Perm |
|---|---|---|---|
| Startup Command (`custom_startup`) | `server.startup` is **not** one of the egg's `startup_commands` values | Read-only text showing "Custom Startup", with a preview hint action | — |
| Startup Command select (`startup_select`) | `server.startup` is one of the egg's `startup_commands` | Options: egg `startup_commands` (`name => command`), shown by name. Saves immediately: `servers.startup = command`; logs `server:startup.command {old: name, new: name}`; notification "Startup Command Updated / Restart the server to use the new startup command." Also exits preview mode. | Disabled without `startup.update` (server aborts 403) |
| Docker Image (`custom_image`) | `server.image` is not one of the egg's `docker_images` | Read-only, shows the image | — |
| Docker Image select (`image`) | `server.image` is one of the egg's `docker_images` | Options: egg `docker_images` (`name => image`). Saves immediately: `servers.image`; logs `server:startup.image {old,new}`; notification "Docker Image Updated / Restart the server to use the new image." | Disabled without `startup.docker-image` |
| Startup textarea | always | Read-only `server.startup`. **Enable/Disable Preview** (hint action on the command field) toggles between the raw command and the result of `StartupCommandService::handle()`. That method substitutes `{{SERVER_MEMORY}}`, `{{SERVER_IP}}`, `{{SERVER_PORT}}` and each `{{ENV}}` with the server value or the default; non-viewable variables show `[hidden]`. | — |
| Server Variables (repeater, grid) | variables with `user_viewable = true`, ordered by `egg_variables.sort` | One `StartupVariable` field per variable (`app/Filament/Components/Forms/Fields/StartupVariable.php`). Missing `server_variables` rows are first created from defaults (`ensureVariablesExist`). The repeater cannot add, delete or reorder. | Repeater disabled without `startup.update`; each field also disabled when `user_editable = false` |

**StartupVariable field:**

- Label: variable name. Prefix: `{{ENV_NAME}}`. Helper text: the description. Hint icon tooltip: the rules joined with `|`. Placeholder: the default value.
- Live on blur.
- The input type is derived from the variable's rules:

| Rule present | Rendered as |
|---|---|
| `in:a,b,c` | Select with those options. An empty option is added unless the rules include `required`. |
| `boolean` | Toggle; the state is cast to bool and saved as `"1"`/`"0"`. |
| `numeric` or `integer` | Number input with `min:`/`max:` taken from the rules. |
| otherwise | Text input. |

The `required` attribute is set when the rules include `required`.

**On update** (`Startup::update`):

1. Aborts 403 unless the user has `startup.update`, the variable belongs to this server, and the variable is `user_viewable`.
2. Silently ignores variables that are not `user_editable`.
3. Validates the value against the variable's Laravel `rules`. On failure it shows the notification "Validation Failed: :variable" with the errors and does not save.
4. Otherwise upserts `server_variables.variable_value` (null becomes `''`).
5. Logs `server:startup.edit {variable: env, old, new}` and shows "Updated: :variable" with `old -> new`.

### Mounts page

Source: `app/Filament/Server/Pages/Mounts.php`. **URL** `/server/{id}/mounts`. Requires `mount.read`; conflict-blocked.

**Checkbox list "Manage the mounts attached to your server:"**

- **Options:** mounts with `user_mountable = true` that are either unrestricted by node or linked to the server's node, **and** either unrestricted by egg or linked to the server's egg.
- **Descriptions:** `source -> target`, with the mount description on a second line. Tags are stripped.
- If no mount qualifies, the helper text reads "There are no user-mountable mounts available for this server."
- Bulk toggle is available. The list is disabled without `mount.update`.
- **Each change saves immediately.** `save()` aborts 403 without `mount.update`, then syncs the `mount_server` pivot through the `mounts` relationship. Note that the checked state is filled from **all** current mounts, including non-user-mountable ones.
- Logs `server:mount.update`. Notification "Mounts updated successfully / Restart your server to apply the new mounts".

### Webhooks resource (server-scoped)

Sources: `app/Filament/Server/Resources/Webhooks/WebhookResource.php` and `…/Pages/*`. The shared form helpers are in `app/Extensions/Webhooks/WebhookForm.php` and `app/Extensions/Webhooks/WebhookTypeService.php`; webhook types and payload schemas are the same as for admin/global webhooks.

- **URLs:**

| Page | URL |
|---|---|
| List | `/server/{id}/webhook` |
| Create | `/server/{id}/webhook/create` |
| View | `/server/{id}/webhook/{webhookId}` |
| Edit | `/server/{id}/webhook/{webhookId}/edit` |

- **Authorization:** `WebhookConfigurationPolicy`, i.e. the **admin role permissions** `viewList webhook`, `view webhook`, `create webhook`, `update webhook`, `delete webhook`. Root admins pass through `Gate::before`. Server owners and subusers without an admin role **cannot** see this resource.
- Not conflict-blocked. Rows are scoped to the tenant through the `server` relation (`server_id`) *(Filament default tenant scoping)*.
- **Badge:** number of webhooks configured for the server, when >0.

**Table:**

| Column | Notes |
|---|---|
| Type | icon and colour of the webhook type; tooltip with the type label, or "unavailable type" |
| Name | — |
| Endpoint | scheme stripped, truncated to 60 chars, wraps |

- **Filter:** Type (select; persisted in the session).
- **Row actions:** View (only when the user cannot edit), Edit, and Replicate (icon button, no modal; copies the row except the timestamps).
- **Bulk action:** Delete.
- **Empty state:** "No Webhooks" with a Create button. The header Create button is hidden while the server has no webhooks.

**Form:**

| Field | Behaviour |
|---|---|
| Type | Toggle buttons over the registered webhook types |
| Endpoint | Required. On blur the type is auto-detected from the URL (`WebhookTypes::detectFor`). |
| Name | Required |
| Payload section | Type-specific components (`WebhookForm::payloadSection(type, WebhookScope::Server)`), with an optional live preview aside that polls every 15 s |
| Events | Required. Searchable checkbox list with bulk toggle, 3 columns. Options come from `WebhookConfiguration::allPossibleServerEvents()`; plugins can extend the list through the `server:webhook.events` event. |

Server events available in the Events list:

- `server:file.read`, `server:file.write`, `server:file.rename`, `server:file.copy`, `server:file.compress`, `server:file.decompress`, `server:file.delete`, `server:file.create-directory`, `server:file.uploaded`, `server:file.pull`, `server:file.download`;
- `server:power.start`, `server:power.stop`, `server:power.restart`, `server:power.kill`;
- `server:console.command`;
- `server:startup.edit`, `server:startup.image`;
- `server:settings.rename`, `server:settings.description`, `server:settings.reinstall`;
- `server:allocation.notes`, `server:allocation.primary`, `server:allocation.create`, `server:allocation.delete`;
- `server:schedule.create`, `server:schedule.update`, `server:schedule.execute`, `server:schedule.delete`;
- `server:task.create`, `server:task.update`, `server:task.delete`;
- `server:backup.start`, `server:backup.delete`, `server:backup.download`, `server:backup.rename`, `server:backup.restore`, `server:backup.restore-complete`, `server:backup.restore-failed`;
- `server:database.create`, `server:database.rotate-password`, `server:database.delete`;
- `server:subuser.create`, `server:subuser.update`, `server:subuser.delete`;
- `server:sftp.denied`.

**Create:** sets `server_id = tenant` and `scope = server`, applies the type's `mutateFormDataBeforeSave`, then redirects to Edit. The Create page shows a "Help" info banner explaining `{{var}}` placeholders. "Create another" is disabled.

**Edit header:**

- Delete.
- **Test Now**: `WebhookConfiguration::run()` sends a test delivery.
- Save.

**Delivery:** `app/Listeners/DispatchWebhooks.php::handleActivityLogged()`. For each server-scoped activity event (resolved through the activity subject or `properties.server.id`), it runs every webhook in `server.webhookConfigurations()` whose `events` JSON contains the event name.

### Plugin extension points (server panel)

These are listed for completeness; they change what appears in the server panel.

- `CanCustomizePages`, `CanCustomizeRelations`, `CanModifyForm`, `CanModifyTable`: plugins can replace pages and relation managers, or modify forms and tables.
- `CanCustomizeHeaderActions` / `CanCustomizeHeaderWidgets`: plugins can add header actions or widgets before or after the defaults (`HeaderActionPosition`).
- `Console::registerCustomWidgets(ConsoleWidgetPosition, …)`.
- `Subuser::registerCustomPermissions(name, perms, translationPrefix, icon, hidden)`.
- `File::registerSpecialFile(name, title, body, check)`: an editor banner for special files.
- `FeatureService::register()` and `TaskService::register()`: new console features and task types.
- `Event::dispatch('server:webhook.events', [&$events])`: additional webhook events.

### Wings endpoints called from the server panel (summary)

All calls go to `node.scheme://fqdn:daemon_connect`, authenticated by the daemon token through `DaemonRepository::getHttpClient()`. JWT links are signed with the node `daemon_token`.

| Endpoint | Used by |
|---|---|
| `GET /api/servers/{uuid}` | status and resources (cached 15 s), schedule "only when online" check |
| `POST /api/servers/{uuid}/power {action}` | feature modals (restart), the `power` task |
| `POST /api/servers/{uuid}/reinstall` | Settings → Reinstall |
| `GET /api/servers/{uuid}/files/list-directory?directory=` | Files list, `delete_files` task |
| `GET /api/servers/{uuid}/files/contents?file=` | Editor |
| `POST /api/servers/{uuid}/files/write?file=` (raw body) | Editor save, New file, EULA feature |
| `POST /api/servers/{uuid}/files/create-directory {name, path}` | New folder, upload of empty folders |
| `PUT /api/servers/{uuid}/files/rename {root, files[{from,to}]}` | Rename, Move, bulk Move |
| `POST /api/servers/{uuid}/files/copy {location}` | Copy |
| `POST /api/servers/{uuid}/files/delete {root, files[]}` | Delete, bulk Delete, `delete_files` task |
| `POST /api/servers/{uuid}/files/compress {root, files, name, extension}` | Archive |
| `POST /api/servers/{uuid}/files/decompress {root, file}` | Unarchive |
| `POST /api/servers/{uuid}/files/chmod {root, files[{file, mode}]}` | Permissions |
| `POST /api/servers/{uuid}/files/pull {url, root}` | Upload from URL |
| `GET /api/servers/{uuid}/files/search?pattern=&directory=` | Nested search |
| `POST /api/servers/{uuid}/backup {adapter, uuid, ignore}` | Create backup (Wings adapter) |
| `POST /api/servers/{uuid}/backup/{backup}/restore {adapter, truncate_directory, download_url}` | Restore |
| `DELETE /api/servers/{uuid}/backup/{backup}` | Delete backup |
| `POST /api/deauthorize-user {user, servers[]}` | Subuser update and delete (SFTP and session revoke) |
| `GET {node}/download/file?token=` (browser) | File download (JWT scope `file-download`) |
| `GET {node}/download/backup?token=` (browser) | Backup download (JWT scope `backup-download`) |
| `POST {node}/upload/file?token=&directory=` (browser, multipart `files`) | Upload (JWT scope `file-upload`) |
| `WS {node}/api/servers/{uuid}/ws` (browser) | Console (JWT scope `websocket`) |

## Installer, API docs, other routes, translations

Section numbering continues from "Cross-cutting, App panel, Auth, Profile".

### 5. Installer

#### 5.1 Access and gating
- **Route:** `GET /installer` (name `installer`, `routes/base.php`). It uses `web` and `auth.session`, and explicitly removes the `auth` and `RedirectIfNotInstalled` middleware. The page is a full-page Livewire component, `app/Livewire/Installer/PanelInstaller.php` (Filament `SimplePage`, view `resources/views/filament/pages/installer.blade.php`, max width `screen-2xl`).
- **Accessible only while `config('app.installed')` is false.** `mount()`, `submit()` and every helper call `abort_if(installed, 404)`.
  - `APP_INSTALLED` defaults to **true** when the key is absent.
  - `.env.example` ships with `APP_INSTALLED=false`.
  - The Docker entrypoint writes `APP_INSTALLED=false` (and generates `APP_KEY`) when it creates a fresh `.env`.
  - Composer `post-install-cmd` copies `.env.example` to `.env`.
- While the panel is not installed, every panel route redirects to `/installer`.
- Install is marked complete by writing `APP_INSTALLED=true` to `.env`.
- **Env writes:** `EnvironmentWriterTrait::writeToEnvironment()` uses `Env::writeVariables($values, .env, overwrite: true)` and then `artisan config:clear`. Each step passes its values through `array_filter`, so null, empty-string and `false` values are **not written**. Write failures show the persistent toast "Could not write to .env file" and halt.

#### 5.2 Page chrome
- **Language select** (top right, prefix "Language", flag icon): required, live, default `en`. Options come from `LanguageService::getAvailableLanguages()`. Changing it calls `app()->setLocale()` for the current request only; it is not persisted.
- **Wizard:**
  - The current step is persisted in the query string (`?step=`). Enter triggers "Next".
  - The final submit button is "Finish", with a loading spinner and disabled while running.
  - Plugins can add steps before or after via `CanCustomizeSteps::registerCustomSteps`.

#### 5.3 Steps
| # | Step key | Fields (state path) | Defaults | Validation / on "Next" |
|---|---|---|---|---|
| 1 | `requirements` (Server Requirements) | Read-only sections. **PHP version** must be ≥ `8.3` (shows the current version). **PHP extensions:** BCMath, cURL, GD, intl, mbstring, MySQL (`pdo_mysql`), SQLite3 (`pdo_sqlite`), XML, Zip (lists any missing). **Folder permissions:** `storage/` and `bootstrap/cache/` octal mode ≥ 755. Each section has a check or X icon. | – | `afterValidation`: if any check fails, danger toast "Some requirements are missing" and the wizard halts. |
| 2 | `environment` | `env_general.APP_NAME` (required), `env_general.APP_URL` (required). Fieldset "Admin User": `user.email` (required, email, placeholder `admin@example.com`), `user.username` (required, placeholder `admin`), `user.password` (required, revealable; **no strength rule**). | `config('app.name')`, `config('app.url')` | Writes `APP_NAME`, `APP_URL`. The user data stays in component state until Finish. |
| 3 | `database` | `env_database.DB_CONNECTION` toggle: SQLite / MariaDB / MySQL / PostgreSQL (help text recommends SQLite). `DB_DATABASE` is labelled "Database Path" for sqlite (placeholder `database.sqlite`, path relative to `database/`) or "Database Name" otherwise. `DB_HOST`, `DB_PORT` (numeric 1–65535), `DB_USERNAME`, `DB_PASSWORD` (optional, revealable). Host, port, username and password are hidden for sqlite. | Driver = `config('database.default')`. Switching driver resets the fields: sqlite → `database.sqlite` and clears host/port/user/pass. mariadb/mysql → host 127.0.0.1, user `pelican`, port 3306, db `panel`. pgsql → port 5432. Values from the current config are used when the selected driver equals the configured default. | Host, port and username are required unless sqlite. A connection test is run for non-sqlite drivers (temporary connection `_panel_install_test`, collation utf8mb4_unicode_ci, strict). A blank password falls back to the configured one when the driver equals the current default. On failure: toast "Database connection failed" with the PDO message, and the wizard halts. Then writes `DB_CONNECTION`, `DB_DATABASE`, `DB_HOST`, `DB_PORT`, `DB_USERNAME`, `DB_PASSWORD`. |
| 4 | `egg` (Eggs) | Builds one tab per egg category, with a count badge. Each tab has a `CheckboxList` `eggs.{category_slug}`: searchable, bulk-toggle, 4 columns, sorted by egg name, values = **download URLs**. If the index is empty, a single tab "No Eggs Available" explains "No eggs are available to install at this time." | none selected | No validation. Nothing is written at this step. The egg index is fetched **when the step schema is built**: `artisan p:egg:update-index` GETs `config('panel.cdn.egg_index_url')` = `PANEL_EGG_INDEX_URL`, default `https://raw.githubusercontent.com/pelican-eggs/pelican-eggs.github.io/refs/heads/main/content/pelican.json` (5s timeout, 1s connect). It parses `nests[].nest_type` and `nests[].Eggs[].{egg.name, download_url}` and caches forever under `eggs.index` = `{category: {download_url: name}}`. |
| 5 | `cache` | `env_cache.CACHE_STORE` toggle: Filesystem (`file`) / Redis (`redis`) (help text recommends Filesystem). When Redis is selected: `REDIS_HOST` (required), `REDIS_PORT` (required), `REDIS_USERNAME` (optional), `REDIS_PASSWORD` (optional, revealable). | `config('cache.default')`. Choosing Redis pre-fills `127.0.0.1` / `6379`. Choosing file clears the Redis fields. | For Redis, a predis `PING` test runs; failure shows the toast "Redis connection failed" and halts. Writes `CACHE_STORE` and the `REDIS_*` values. |
| 6 | `queue` | `env_queue.QUEUE_CONNECTION` toggle: Database / Redis / Sync (Redis is disabled unless cache = redis; help text recommends Database). `done` toggle: "I have done both steps below." Two read-only copyable commands: **crontab** `(sudo crontab -l -u www-data 2>/dev/null; echo "* * * * * php {base}/artisan schedule:run >> /dev/null 2>&1") \| sudo crontab -u www-data -` (note says `www-data` may differ), and **queue service** `sudo php {base}/artisan p:environment:queue-service`. The latter writes `/etc/systemd/system/{name}.service` with `ExecStart=/usr/bin/php {base}/artisan queue:work --tries=3` and runs `systemctl enable --now`. | `config('queue.default')` | `done` must be accepted ("You need to do both steps before continuing!"). **In Docker (`/.dockerenv` exists), the toggle and both commands are hidden and not required.** Writes `QUEUE_CONNECTION`. |
| 7 | `session` | `env_session.SESSION_DRIVER` toggle: Filesystem / Database / Cookie / Redis (Redis is disabled unless cache = redis; help text recommends Filesystem or Database). Hidden `SESSION_SECURE_COOKIE` = `request()->isSecure()`. | `config('session.driver')` (env `SESSION_DRIVER`, default `file`) | Written only on Finish. |

#### 5.4 Finish (`submit`)
Steps run in this order. Any failure shows a persistent danger toast and halts. Earlier writes are not rolled back.
1. `artisan migrate --force --seed`. The seeder creates the `Root Admin` role and runs enabled plugins' seeders. Afterwards it verifies that every migration file has run (`CheckMigrationsTrait`). Failure toast: "Migrations failed".
2. Create the admin user with `UserCreationService::handle(user + root_admin=true)`. The password is hashed, a UUID assigned, and the root-admin role synced. The `AccountCreated` email is queued if `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION` is on. Failure toast: "Could not create admin user".
3. Log the admin in (`remember = true`).
4. Write the `env_session` values (deliberately last among the env writes, to avoid a "page expired" error).
5. Dispatch a queued `InstallEgg` job per selected download URL. Each job has a 15s timeout and uses `EggImporterService::fromUrl`; per-egg failures are only logged. Then a persistent toast reads "Install of :count eggs has been queued and will continue in the background." If dispatching fails: "Failed to install selected eggs. Please import them after the installation via the egg list." (not fatal).
6. Write `APP_INSTALLED=true`.
7. Redirect to `/admin`.

There are also CLI equivalents under `app/Console/Commands/Environment/`: `p:environment:setup`, `p:environment:database`, `p:environment:cache`, `p:environment:queue`, `p:environment:queue-service`, `p:environment:session`, `p:environment:mail` and `p:redis:setup`. They are not part of this UI.

### 6. API documentation pages

#### 6.1 Routes (`routes/docs.php`, mounted under `/docs` with `web` + `auth.session`)
- Scramble's default routes are disabled (`Scramble::ignoreDefaultRoutes()` in `AppServiceProvider::register`).

| Route | Content | Access |
|---|---|---|
| `GET /docs/api` | `resources/views/docs/api-index.blade.php`: a static card with links "Application API" → `/docs/api/application` and "Client API" → `/docs/api/client`, plus the note "You need to be logged in to view the API docs!" | public (no auth gate) |
| `GET /docs/api/application` | Stoplight Elements UI for the `application` API | Scramble middleware `web` + `RestrictedDocsAccess` |
| `GET /docs/api/application.json` | OpenAPI JSON (pretty-printed) | same |
| `GET /docs/api/client` | UI for the `client` API | same |
| `GET /docs/api/client.json` | OpenAPI JSON | same |

- **Access control:**
  - `RestrictedDocsAccess` allows everything when `APP_ENV=local`. Otherwise it requires `Gate::allows('viewApiDocs')`.
  - The gate is defined as `fn () => true` with no nullable user parameter. That means **any authenticated user passes** and **guests get 403** (Laravel denies guests for closures without a nullable user argument). Root admins also pass via `Gate::before`.
- **APIs documented** (`AppServiceProvider::boot`):
  - `Scramble::registerApi('application', ['api_path' => 'api/application', 'info' => ['version' => '1.0']])`
  - `Scramble::registerApi('client', ['api_path' => 'api/client', ...])`
  - Both get a global **HTTP bearer** security scheme.
  - The remote (Wings) API `/api/remote` is **not** documented.
  - There is no `config/scramble.php`, so package defaults apply. The UI title is "{APP_NAME} - API Docs".
- **UI view override** (`resources/views/vendor/scramble/docs.blade.php`):
  - Loads `@stoplight/elements@8.3.4` web components and CSS from unpkg.
  - Theme follows Filament's `localStorage.theme` (light, dark, or system; default dark). Hash router.
  - "Try it" is enabled with credentials policy `include`.
  - It wraps `window.fetch` to copy the `XSRF-TOKEN` cookie into an `X-XSRF-TOKEN` header. Combined with `EnsureStatefulRequests` (any API request carrying the panel session cookie is treated as stateful by Sanctum), a logged-in user can call the APIs from the docs page with their session.
  - The application API still requires `isAdmin()` (`AuthenticateApplicationUser`).
- **Response schema inference** (`app/Extensions/Dedoc/Scramble/PanelResponseTypeInfer.php`):
  - This Scramble `MethodReturnTypeExtension` follows the fluent `PanelResponse` chain (`item|collection → transformWith → addMeta → parseIncludes/limitRecursion → toArray|respond`).
  - It documents the envelope:
    - Single item: `{object: <resourceName>, attributes: {...}}`.
    - Collection, or a paginated item: `{object: "list", data: [{object, attributes}], meta?: {pagination: {total, count, per_page, current_page, total_pages, links: {previous?, next?}}, ...addMeta keys}}`.
  - Attribute shapes come from Scramble's inference of the `ApiResource` Data class's `toArray()`. The fallback is reflection over its public properties: `MapOutputName` gives renames, `Optional` gives optional keys, nullable types give `|null`, and backed enums map to int or string.

### 7. Other UI-relevant routes
| Route | Source | Notes |
|---|---|---|
| `GET /installer` | `routes/base.php` | §5 |
| `GET /auth/login` → `/login` | `routes/auth.php` | named `auth.login`. Used as the redirect target by `OAuthController`. |
| `GET /auth/oauth/redirect/{driver}`, `GET /auth/oauth/callback/{driver}` | `routes/auth.php` | §3.6 |
| Passkey routes | `laravel/passkeys` | §3.5 |
| `GET /up` | `bootstrap/app.php` `withRouting(health: '/up')` | Laravel health check |
| `/docs/api*` | `routes/docs.php` | §6 |
| Filament auth routes per panel: `/login`, `POST /logout`, `/password-reset/request`, `/password-reset/reset`, `/profile` (also prefixed `/admin`, `/server`) | Filament | §3 / §4 |
| `/storage/avatars/{id}.png`, `/storage/fonts/{name}.ttf` | public disk symlink | uploaded avatars and console fonts |

- There is **no** locale-asset route and **no** SFTP "connect" redirect route. SFTP links are built client-side as `sftp://` URLs by `Server::getSftpUrl()` in the server panel.
- The API route groups (`/api/application`, `/api/client`, `/api/remote`) are documented elsewhere.

### 8. Translations and notifications

#### 8.1 Layout of `lang/`
- **Locale directories (32):** `ar, be, bg, cs, da, de, el, en, es, fi, fr, hu, id, it, ja, ko, lt, nl, no, pl, pt, pt_BR, ro, ru, sk, sr, sv, tr, uk, vi, zh_CN, zh_TW`.
  - `en` is the source and has 40 files.
  - Coverage varies. de, es, fr and zh_TW have 40 files; ru 39; ja 38; hu and it 37; nl, pl and pt_BR 36; cs 35; uk and zh_CN 32; ar 27; ro 23; ko 19; tr 15; be and lt 14; sk 13; vi 12; da 10; fi and no 9; bg 8; el, id and sv 7; sr 6; pt 5.
  - Missing keys fall back to `en`.
- **`lang/en` layout** (Laravel PHP array files; keys are referenced as `file.key` or `dir/file.key`):
  - Top level: `activity.php` (activity-log event descriptions, keyed by the event prefix before `:`), `auth.php`, `commands.php` (artisan prompts), `exceptions.php`, `installer.php`, `mail.php`, `notifications.php` (DB notification titles), `passkeys.php`, `profile.php`, `search.php`, `validation.php`.
  - `admin/`: `apikey, backuphost, dashboard, databasehost, egg, health, log, mount, node, plugin, role, server, setting, user, webhook`.
  - `server/`: `activity, backup, console, dashboard, database, feature, file, mount, network, schedule, setting, startup, user`.
  - `command/`: `messages.php`.
- Filament's own UI strings (`filament-panels::`, `filament-actions::`, …) come from Filament's vendor language packs, not from `lang/`. There is no `lang/vendor` override directory.
- **Crowdin** (`crowdin.yml`): source `/lang/en/**/*.php`, translations written to `/lang/%two_letters_code%/**/%original_file_name%` with `preserve_hierarchy: true`. Language mapping: `zh-CN → zh_CN`, `zh-TW → zh_TW`, `pt-BR → pt_BR`. Credentials come from the env vars `CROWDIN_PROJECT_ID` and `CROWDIN_PERSONAL_TOKEN`.

#### 8.2 Supported-locale list (`app/Services/Helpers/LanguageService.php`)
- `getAvailableLanguages()` builds the list from:
  - every **directory** name under `lang/`, plus
  - the locale directories under `plugins/{id}/lang/` of **enabled "language" plugins** (`PluginService::getPluginLanguages()`).
- Each code is mapped to a display name. It first tries `trans('profile.current_language_name', locale: code)`; no locale defines this key, so it falls back to `title_case(Locale::getDisplayName(code, code))`, the native ICU name (e.g. "Deutsch", "Português (Brasil)").
- The result is de-duplicated and sorted by display name.
- It is used by the profile language select, the installer language select, and the admin user form.
- `isLanguageTranslated(code)` returns true only for codes in `TRANSLATED_COMPLETELY = ['en']`. Every other language gets the "not translated yet / Crowdin" helper text in the profile.

#### 8.3 Applying the locale
- `users.language` (string; default `'en'`; model rule: any ICU locale) is applied per request by `LanguageMiddleware`, which runs in the global `web` group and in panel middleware. Guests get `config('app.locale')` (env `APP_LOCALE`, default `en`).
- `fallback_locale` = `en` (`config/app.php`).
- `<html lang>` is the locale with `_` replaced by `-`.
- Emails and DB notifications are rendered in the **recipient's** `language`, not the actor's.
- Number formatting (`format_number`) and chart locales use `user()->language`.
- **Plugin translations** (`PluginService`):
  - For each loaded plugin with a `lang/` directory, a regular plugin gets `translator->addNamespace({plugin_id}, path)`, so its strings are used as `trans('{plugin_id}::file.key')`.
  - A plugin of category *language* gets `translator->addPath(path)`, which merges its files into the global namespace and contributes new locale directories.

#### 8.4 Notifications (`app/Notifications/*`)
All of these are mail notifications. All except `MailTested` are queued (`ShouldQueue`). All are localized to the recipient's language. Each starts with the greeting "Hello {username}!".

| Class | Trigger | Config switch | Content / action button |
|---|---|---|---|
| `AccountCreated` | `UserCreationService::handle`: admin user create, application API, OAuth auto-create, installer | `PANEL_SEND_ACCOUNT_CREATED_NOTIFICATION` (default true) | Account created on {APP_NAME}, plus username and email. If the account was created **without a password**, the button "Setup Your Account" links to the app-panel password reset URL with a fresh token. |
| `AddedToServer` | `SubUserAdded` event (subuser added) | `PANEL_SEND_ADDED_TO_SERVER_NOTIFICATION` (default true) | Server name. Button "Visit Server" → server console. |
| `RemovedFromServer` | `SubUserRemoved` event | `PANEL_SEND_REMOVED_FROM_SERVER_NOTIFICATION` (default true) | Server name. Button "Visit Panel" → `/`. |
| `ServerInstalled` | `Installed` event (Wings install-status callback, `ServerInstallController`) | `PANEL_SEND_INSTALL_NOTIFICATION` for the initial install, `PANEL_SEND_REINSTALL_NOTIFICATION` for reinstalls (both default true) | "Your server has finished installing…", server name. Button "Login and Begin Using" → console. Sent to the server owner. |
| `BackupCompleted` | `BackupCompleted` event (Wings backup status callback) | Per-user, per-server opt-in in `server_user_settings` (`ScheduledBackupNotifications` / `ManualBackupNotifications`, depending on whether the backup was scheduled). Recipients are the owner and subusers. | Success or failure body, backup name, server name. Button "View Backups" → server backups list. |
| `MailTested` | Admin Settings → "test mail" | – | Subject "Panel Test Message" (sent synchronously). |

**Database (bell) notifications sent alongside the emails** (`app/Listeners/Server/*`):
- **Subuser added:** "Added to Server" with an "Open Server" button that marks the notification read.
- **Subuser removed:** "Removed from Server".
- **Install / reinstall:** completed or failed, success or danger status, with an "Open Server" button.
- **Backup completed / failed:** with a "View Backups" button.
- These DB notifications are always sent. Only the emails are gated by the config switches.

**Filament-provided mails:**
- `ResetPassword` (queued).
- The MFA email code (`VerifyEmailAuthentication`).
- No email-change or verify-email mails, because those features are not enabled.

## Open questions

### General (cross-panel)
- Several behaviours are Filament framework defaults (SPA navigation, tenancy redirects, row-click targets, rate limits on login/reset/profile save, notification polling every 30 s, hidden-vs-disabled unauthorized actions). Which of these must be reproduced exactly in the Rust UI, and which are incidental?
- Most persistent configuration lives in `.env` and is rewritten by the Settings page (followed by `config:clear` and `queue:restart`). Should the rewrite keep `.env` as the store or move settings to the database?
- Many strings (error pages, OAuth errors, some package notices) are hard-coded English while the rest is translated; should the rewrite translate everything?

### Cross-cutting, App panel, Auth, Profile, Installer, API docs

- **Profile save rewrites `customization`:**
  - `mutateFormDataBeforeSave` json-encodes a fresh object containing only `console_*`, `dashboard_layout`, `top_navigation`, `button_style` and `redirect_to_admin`. That **drops `servers_per_page`** on every profile save.
  - It stores a JSON **string** into an `array`-cast column, i.e. double-encoded. `getCustomization()` tolerates this.
  - Should the rewrite treat customization as a merged JSON object?
  - It is also unverified whether the model rules (`customization` → `array`, `customization.top_navigation` → `boolean`) actually reject these saves. The values written are the strings `sidebar`/`topbar`/`mixed`, which conflict with that `boolean` rule.
- **Profile password change** has no current-password confirmation, because the custom form omits Filament's `currentPassword` field. Is this intentional, or should the rewrite require it?
- **Profile API-key creation skips server-side checks:** no `API_KEYS_LIMIT`, no IP/CIDR validation and no memo length check, unlike the client API `POST /api/client/account/api-keys`. Should these be unified?
- **OAuth linking** does not check whether the provider account id is already linked to another user. After linking, two users could match the same `oauth->{driver}`, and login would pick the first. Should OAuth ids be unique?
- **OAuth and passkey logins bypass the Filament MFA challenge**, the captcha, the panel access check and `redirect_to_admin`. Is that the desired behaviour for the rewrite? Note that 2FA-required users who only have passkeys get redirected to the profile.
- **OAuth error handling:** `Socialite::driver()->user()` exceptions (invalid state, provider errors) are not caught. A `/auth/oauth/redirect/{unknown}` request null-derefs. Error toasts are hard-coded English.
- **The passkey UI (login button and profile tab) is hidden unless `request()->isSecure()`.** That hides it on `http://localhost`, even though `AllowPasskeyOrigin` explicitly supports loopback HTTP. It also depends on `TRUSTED_PROXIES` when behind TLS termination.
- **The `authenticatePasskey::message` session key** read by the Login page is never written. Is this dead code?
- **`TwoFactorListener` / `ProvidedAuthenticationToken`** is never dispatched, so `auth:token` and `auth:recovery-token` activity is never logged. The `panel.auth.2fa.window/bytes` config is unused, and so is `config/auth.php` `lockout`.
- **`AUTH_DISABLE_PASSWORD_LOGIN`** is env-only, not in Settings. When it is set, the password-reset routes stay active.
- **The password-reset request page** (Filament default) reveals whether an email exists through the broker failure toast. Should the rewrite always answer the same way?
- **Installer:**
  - The egg-index fetch failure path never fires. `UpdateEggIndexCommand` returns exit code 1 instead of throwing, so the installer's catch and its "Failed to update egg index" toast are unreachable; the user just sees "No Eggs Available".
  - `array_filter` drops `SESSION_SECURE_COOKIE=false` and empty values, so existing `.env` values may persist.
  - The admin password has no strength rule.
  - The language choice is not persisted across Livewire requests.
  - The requirements step checks PHP ≥ 8.3, while composer's platform is 8.4 and the target is 8.5.
- **`MaintenanceMiddleware`** references a non-existent `errors.maintenance` view, and its alias `node.maintenance` is unused. Is it dead code?
- **`ServerInstalledListener` sends the "finished installing / ready" email even when the install failed.** The DB notification does distinguish success and failure.
- **The `/docs/api` index page is public** while the spec pages require login (403 for guests outside `APP_ENV=local`). The index links are hard-coded absolute paths (`/docs/api/...`), ignoring any sub-path in APP_URL.
- **`ServerResource::embedServerList()`** and the app panel's `FilamentLogViewerPlugin->authorize(false)` are plugin hooks or leftovers with no core caller. Should the rewrite keep an equivalent extension point?
- **Error pages** (400–503) use hard-coded English strings, and some show raw exception messages (400, 403, 404). Should they be localized or sanitized?

### Admin panel (part A)

- **Environment health card text**: `EnvironmentCheck` calls `trans('admin/health.results.environment.ok')` without the `actual` replacement, so the card probably shows the literal "Ok, Set to :actual". Decide whether to reproduce this or fix it.
- **Health check registration order and labels**: QueueCheck and UsedDiskSpaceCheck messages come from the `spatie/laravel-health` package and are not translated. Verify the exact English text against the vendored package version before copying it.
- **OAuth wizard `_noenv_*` fields**: the wizard's `_noenv_callback` and `_noenv_origin` fields are `dehydrated()` and are pushed into page state through `$set` on enable. I believe Filament's `getState()` prunes them because no main-form component owns those keys, so they never reach `.env`. Not runtime-verified.
- **Captcha enabled flag when unset**: if `CAPTCHA_TURNSTILE_ENABLED` is absent, its hidden field's default is `null`. Saving without toggling it then writes an empty or null value through `Env::writeVariables`. Unclear whether this writes `CAPTCHA_TURNSTILE_ENABLED=` or omits the key.
- **`env()` defaults and cached config**: Settings field defaults call `env()` directly. If the config were cached, those calls return null and the form would show config fallbacks or blanks. `save()` always runs `config:clear`, but loading the page itself does not.
- **Cloudflare preset failure**: "Set to Cloudflare IPs" sets Trusted Proxies to an **empty** list without a notification when the Cloudflare request fails. This is probably unintended.
- **Role delete guard**: on `EditRole`, `users_count` is not explicitly loaded with `withCount`, so the "In Use" guard on Delete may never fire. Deleting a role with users would then detach it from those users. Verify at runtime.
- **Role assignment privilege escalation**: nothing prevents an admin with `update user` from assigning any non-Root role to a user, including roles with more permissions than the admin has. The same applies to an admin with `update role` editing their own role's permissions.
- **Relation managers on Database Hosts and Backup Hosts**: `DatabasePolicy` and `BackupPolicy` evaluate subuser permissions against `Filament::getTenant()`, which is null in the admin panel. That makes these relation managers, and the database Rotate and Delete actions, effectively Root-Admin-only. Confirm whether that is intended.
- **Database row delete does not drop the database**: deleting a database from the Database Host relation manager removes only the panel row, not the MySQL database or user, unlike the Servers resource. Likely a bug; decide whether to replicate it.
- **Database host username limit**: the form allows 255 characters for the username, but the model rule is `max:32`, so long usernames fail model validation with a generic error.
- **Database host wizard `panel_ip`**: turning the "different server" toggle off does not reset `panel_ip` to `127.0.0.1`, because the else-branch of the closure returns a value instead of calling `$set`.
- **Backup host node uniqueness**: linking a node already attached to another backup host violates the `backup_host_node.node_id` unique constraint. The form does not pre-validate this, so the user sees a DB error.
- **Backup host S3 extra keys**: when an S3 host is edited, it is unclear whether extra `configuration` keys (`storage_class`, `prefix`, `use_accelerate_endpoint`) survive the save or are pruned, because they have no form fields. Not verified.
- **Discord webhook type**: no `discord` schema is registered in core; it appears to come from a plugin. The exact Discord payload format (embeds, `Retry-After` handling) could not be documented from this repository.
- **Webhook payload rules in the admin UI**: `getPayloadRules()` from the webhook type schemas is applied only in the Application API request (`StoreWebhookRequest`). The admin forms do not validate payloads, and the Endpoint field has no URL rule.
- **Bogus webhook event entry**: `discoverModels()` uses only the filename. Files in `app/Models/Traits` and `app/Models/Filters` therefore produce entries such as `eloquent.created: App\Models\HasAccessTokens`, and one of them appears in the global event list.
- **Webhooks Create on the Server tab**: the list page hides the header Create action on the "Server Webhooks" tab, but the table toolbar's CreateAction stays visible. Either way, created webhooks are forced to global scope.
- **Users primary-allocation inline select**: it writes `servers.allocation_id` directly through `SelectColumn`, without a Wings sync or activity log. Confirm whether the rewrite should go through a service.
- **Unverified Filament defaults**: these were inferred from the vendored Filament 5 sources and package defaults rather than from Pelican code: redirect after create, pagination options, default key sort, the global-search modal keybinding, and log-viewer per-page options.

### Admin panel (part B)

- Node create redirects (Filament default) to the **View** page, where the config is redacted and Auto Deploy / Reset are hidden; the `?tab=configuration-file::data::tab` key only matches the English label. Should the rewrite land on Edit → Configuration File instead?
- `NodeAutoDeployService` reuses the first application API key with `node: READ` belonging to *any* user (the comment says "for the logged-in user"); `whereJsonContains(..., 1)` also misses keys with other node permission values. Intended?
- Reset token: if Wings is unreachable, the new token is still committed to the DB but the UI shows only a warning and does not refresh the preview. Should the rewrite roll back or clearly show the new token?
- Node Servers relation manager "Primary Allocation" select only lists the first allocation (`take(1)`), so it cannot actually change the primary. Bug or intentional read-only?
- `retrieveValidPorts()` rejects port 1024 (`<= PORT_FLOOR`) while `AssignmentService` accepts it; which bound is authoritative?
- No Wings sync is triggered after allocation changes (primary change, add/remove, lock), owner change, or Change Egg in the admin UI. Does Wings pick these up only on next sync/restart, and should the rewrite sync automatically?
- PreviewStartupAction writes the rendered command into the `startup` textarea; saving while preview is on would persist the rendered (and `[hidden]`) command. Should Save be blocked or the preview kept out of form state?
- The Databases relation manager (and Rotate action) authorize via subuser permissions on the Filament tenant, which is null in the admin panel, so only root admins can see/use it. Is that intended for non-root admins with database permissions?
- Create Database default `database_host_id` = first host overall, which may not be attached to the server's node (not among options).
- `ImportEggAction` on the Edit page does not pass the current egg to the importer; importing a file with a different `uuid` creates a new egg instead of updating the edited one.
- The egg update flag (`eggs.{uuid}.update`) lives 1 hour but is refreshed only daily, so the single "Update" action is visible for ~1 hour per day.
- Egg bulk Delete/Update modify `$records` by reference in `before()`; whether Filament actually honours the filtered collection was not verified (model hooks still block deleting eggs with servers/children).
- Egg variable `unique` checks use `$get('../../id')`, which is null on create, so duplicate names/env vars within a new egg are not caught by the form.
- `allocation_id` is optional in CreateServer; servers can be created without any allocation. Intended?
- Editable table columns (IP/alias/notes/show_port/primary allocation select) bypass policies (only `disabled` checks); the Server allocations `show_port` toggle is not disabled on the read-only View page.
- `Hidden::make('io')` means Block IO weight cannot be changed in the UI (always 500 on create). Should the rewrite expose it?
- `NodeHealthColumn` has a TODO to poll every 10 s; currently it loads once per page view.

### Server panel

- **Upload activity event mismatch.** `FileUploadManager::logUploadedFiles` logs `server:files.uploaded` (plural), but the lang key and the webhook event list use `server:file.uploaded`. The activity label lookup fails, so the raw key is probably shown, and server webhooks subscribed to `server:file.uploaded` never fire for UI uploads. Should the rewrite normalize this to `server:file.uploaded`?
- **Subuser invite never logged.** In `SubuserResource` the `server:subuser.create` activity is built but `->log()` is never called. Is that intentional?
- **Edits can strip permissions.** `SubuserUpdateService` filters the new permission set to what the *editor* holds, so editing a subuser who has permissions the editor lacks silently removes them. Is this the intended behaviour to preserve?
- **No `control.kill` permission.** Kill uses `control.stop` (consistent with Wings), while `lang` has an orphan `control_kill` description. Should a dedicated permission exist?
- **Console ignores transfers.** The UI websocket URL always uses the server's current node, unlike the client-API `WebsocketController`, which switches to the new node after an archived transfer and requires `admin.websocket.transfer` during transfers. The UI also ignores `transfer logs`. Should the rewrite match the API behaviour?
- **Ignored Wings events.** The UI drops `jwt error`, `daemon message`, `backup completed`, `backup restore completed` and `install started`. Backups therefore never refresh live; is that acceptable?
- **Initial power-button state.** The Console page's `$status` starts as `offline` regardless of the real state until Wings emits `status`. We believe Wings sends a status after `auth success`, but the panel does not verify it. Should the rewrite seed it from `retrieveStatus()`?
- **Nested search minimum.** `searchTerm` uses `->minValue(3)`, a numeric minimum, not a length minimum. Was a 3-character minimum intended?
- **Unlabelled cron presets.** "Every x hours/days/months/day of week" have no explicit `->label()`, although the lang keys `every_hour`, `every_day`, … exist, so Filament derives labels from the action names (e.g. "Exclude every x hours"). Probably a bug; confirm the intended labels.
- **Day presets capped at 24.** "Every x days" and "every x months" are limited to 1–24, the same as hours. Was that intended?
- **Repeated `server:file.read`.** It is logged inside `EditFiles::form()`, so it likely fires on every Livewire round-trip (save, language change), not once per open. Confirm the desired semantics.
- **Editor never read-only.** The Monaco editor is never read-only in `EditFiles`. Users without `file.update` can type but have no save button. Should the rewrite render read-only?
- **Unlogged mutations.**
  - Allocation **notes** and **primary** changes made in the UI log no activity, whereas the client API logs `server:allocation.notes` / `server:allocation.primary`.
  - **chmod**, **icon upload/delete** and **backup download** log nothing either.
  - Should the rewrite log these?
- **Backup create limit error not caught.** The Create action catches only `HttpException`. `TooManyBackupsException` (a `DisplayException`) is not caught, so it surfaces as an error if the limit is hit in a race. The `getHeaders()['Retry-After']` access can also fail for other HttpExceptions.
- **Restore gate.** Restore checks only `server.status === null`. It does not check the container state (running) or a pending transfer. Confirm the intended preconditions.
- **Limits of 0 or null.** Backups, databases and allocations all treat a limit of 0 or null as "none allowed" because of `count >= limit`, while the nav badge treats 0 as "no limit" (no denominator). Which semantics should the rewrite use?
- **No `TaskPolicy`.** Task CRUD on a schedule is gated only by being able to open the Edit page (`schedule.update`) under Filament's non-strict authorization. Confirm.
- **Webhook visibility.** Server webhooks require admin role permissions; owners and subusers cannot manage webhooks for their own server. Is this intended, or should a subuser permission exist?
- **Mounts form fill.** The Mounts form is filled with **all** attached mount ids, including non-user-mountable ones that are not options, and saves through `saveRelationships()`. Does saving detach admin-assigned non-user-mountable mounts? This depends on Filament CheckboxList relationship sync semantics and needs verification.
- **GSL token without `STEAM_ACC`.** `GSLTokenSchema::getAction()` assumes a `STEAM_ACC` server variable exists. An egg that enables `gsl_token` without that variable will likely error when the action is built during `Console::boot()`.
- **Unbounded command history.** History lives in the Laravel session under `server.{id}.history` with no cap. Should the rewrite cap it?
- **Upload size default.** The `nodes.upload_size` column defaults to 100 (migration `2016_12_01_173018_add_configurable_upload_limit.php`), but the admin Create Node wizard pre-fills 256 (`CreateNode.php`), so nodes created through the UI get 256 MiB/MB. Which default should the rewrite use for API-created nodes?
