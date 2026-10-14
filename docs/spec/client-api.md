# Client API (`/api/client`) — compatibility contract

Status: reference spec derived from the PHP source in `panel/` (PHP 8.5, Laravel 13.35, Filament 5).
All source paths are relative to `panel/` unless prefixed with `vendor/` (= `.ref/vendor/`, the exact
framework versions the panel ships with). Framework behaviour quoted here was checked against `.ref/vendor`
unless it is listed under "Open questions".

The Client API is what the end-user (non-admin) tooling talks to. It is NOT the Fractal-based API from
Pterodactyl any more: Pelican replaced Fractal with an in-house envelope (`app/Data/Api/*`) that reproduces
Fractal's wire format. There is no `app/Transformers` directory; the "transformers" are the
`app/Data/Api/Client/*Data.php` classes (spatie/laravel-data `Data` subclasses).

---

## 0. Notation used in this document

* `HAS(perm)` — `User::can(perm, $server)` with a `Server` **instance** as the argument
  (`app/Models/User.php:447-456` → `checkPermission` `:428` → `hasPermission` `:409-426`). See §5.3.
* `GATE(perm)` — Laravel Gate check with the server wrapped in an **array** (`$user->can(perm, [$server])`,
  `Gate::authorize(perm, $server)`). Used by include resolvers and the server activity endpoint. See §5.4.
* `ADMIN_UPDATE` — the user passes `canned('update', $server)` (root admin, or a role holding the spatie
  permission `update server` whose node restrictions allow the server's node). See §5.2.
* "pretty JSON" / "compact JSON" — see §6.6 (the original emits both, depending on the error path).
* Types: `int`, `float`, `bool`, `string`, `?T` = `T|null`, `T[]` = JSON array.

---

## 1. Routing and the middleware pipeline

Routes: `routes/api-client.php` (all lines cited below), mounted by `app/Providers/RouteServiceProvider.php:42-52`:

```php
Route::middleware(['api'])->group(function () {
    Route::middleware(['client-api', 'throttle:api.client'])
        ->prefix('/api/client')
        ->scopeBindings()
        ->group(base_path('routes/api-client.php'));
});
```

Groups are defined in `bootstrap/app.php:37-53`. **Important:** `$middleware->api([...])` *appends* to
Laravel's default `api` group (`vendor/laravel/framework/src/Illuminate/Foundation/Configuration/Middleware.php:341-344,
482-528`), whose default content is `[SubstituteBindings]` (statefulApi/apiLimiter are off). The router then
removes duplicates keeping the first occurrence (`Router::uniqueMiddleware`, `SortedMiddleware::sortMiddleware`)
and the priority list is replaced by `[SubstituteBindings]` only (`bootstrap/app.php:68-70`), so no re-ordering
happens. The effective per-request order for every client route is therefore:

| # | Middleware | Source | Effect |
|---|-----------|--------|--------|
| G | global: `ValidatePathEncoding`, `InvokeDeferredCallbacks`, `TrustProxies`, `HandleCors`, `PreventRequestsDuringMaintenance`, `ValidatePostSize`, `TrimStrings`, `ConvertEmptyStringsToNull` | `vendor/.../Foundation/Configuration/Middleware.php:451-475` | input normalisation (§2.4), CORS (§2.6) |
| 1 | `SubstituteBindings` | default `api` group | **route-model binding runs before authentication** (§4) |
| 2 | `App\Http\Middleware\EnsureStatefulRequests` | `api` | decides cookie/session mode (§2.2) |
| 3 | `auth:sanctum` | `api` | 401 if no user (§2.1) |
| 4 | `App\Http\Middleware\Api\IsValidJson` | `api` | 400 on malformed JSON (§2.3) |
| 5 | `App\Http\Middleware\Activity\TrackAPIKey` | `api` | records api_key_id for activity logs |
| 6 | `App\Http\Middleware\Api\AuthenticateIPAccess` | `api` | allowed-IP check (§2.5) |
| 7 | `App\Http\Middleware\Api\Client\RequireClientApiKey` | `client-api` | 403 for application keys (§2.1) |
| 8 | `throttle:api.client` | route group | per-user rate limit (§3) |
| 9a | `AccountSubject` | `/account/*` routes | activity actor+subject = user |
| 9b | `ServerSubject`, `AuthenticateServerAccess`, `ResourceBelongsToServer` | `/servers/{server:uuid}/*` routes (`routes/api-client.php:54`) | §5.1, §4 |
| 10 | `ThrottleRequests:api.client:server-resource:<Case>` | individual routes (`ResourceLimit::*->middleware()`) | per-server resource limit (§3) |
| 10' | `throttle` (no args) | `PUT /account/email` only (`routes/api-client.php:26-28`) | 60/min/user (§3) |
| C | controller; FormRequest `authorize()` **then** `rules()` (`vendor/.../Validation/ValidatesWhenResolvedTrait.php`) | | 403 before 422 |

Exceptions thrown anywhere inside the route pipeline are rendered by `App\Exceptions\Handler` at the layer
where they occur (`vendor/.../Routing/Pipeline.php` `handleException`), so outer middleware (e.g. throttles)
still decorate the resulting error response.

`GET` routes also answer `HEAD`. Trailing slashes are ignored by route matching.

---

## 2. Cross-cutting request handling

### 2.1 Authentication (`auth:sanctum`)

Config: `config/sanctum.php` — guards `['web']`, `expiration => null`. Guard implementation:
`vendor/laravel/sanctum/src/Guard.php` `__invoke`.

Order of attempts:

1. **Session (stateful)** — for each guard in `sanctum.guard` (`web`): if the session guard has a user, the user
   is returned with a `Laravel\Sanctum\TransientToken` as its "current access token". The session is only
   started if step 2 of the pipeline marked the request stateful (§2.2); otherwise this yields nothing.
2. **Bearer token** — `Authorization: Bearer <token>`. `isValidBearerToken`: non-empty; if it contains `|` the part
   before `|` must be all digits and the rest non-empty (ApiKey primary key is `int`). Lookup is
   `App\Models\ApiKey::findToken` (`app/Models/ApiKey.php:197-208`):
   * `identifier = substr(token, 0, 16)`; row = `api_keys WHERE identifier = ?` (any `key_type`);
   * valid iff `decrypt(row.token) === substr(token, 16)` (the `token` column is Laravel-`encrypted`).
   * Then: `expires_at` (if set) must not be past; `config sanctum.expiration` is null → no age limit.
   * Fires `Laravel\Sanctum\Events\TokenAuthenticated`; then **updates `api_keys.last_used_at = now()`** on every
     authenticated request (`trackLastUsedAt = true`). This save runs the model validation observer
     (`app/Observers/ValidationObserver.php`) and fires `eloquent.updated` (webhooks).
3. Neither → `Illuminate\Auth\AuthenticationException('Unauthenticated.')` → **401** (§6.3).

Key format (account keys created by this API, `app/Models/Traits/HasAccessTokens.php:31-44`,
`app/Models/ApiKey.php:212-227`): `identifier = "pacc_" + Str::random(11)` (16 chars), secret
`Str::random(32)`; the bearer value is `identifier . secret` (48 chars). Application keys use prefix `papp_`.

After authentication Laravel calls `Auth::shouldUse('sanctum')` (`vendor/.../Auth/Middleware/Authenticate.php`
`authenticate()`), so the default guard for the rest of the request is `sanctum` (relevant for
`AccountController::updatePassword`, §8.4).

`RequireClientApiKey` (`app/Http/Middleware/Api/Client/RequireClientApiKey.php:16-23`): if the current token is an
`ApiKey` with `key_type === 2` (application) → `AccessDeniedHttpException` **403**,
detail `You are attempting to use an application API key on an endpoint that requires a client API key.`
Session (TransientToken) and account keys (`key_type 1`) pass.

There is **no 2FA enforcement** on the API (2FA middleware is only attached to Filament panels,
`app/Providers/Filament/PanelProvider.php:80`).

### 2.2 `EnsureStatefulRequests` (cookie/session mode)

`app/Http/Middleware/EnsureStatefulRequests.php:18-25` extends Sanctum's
`EnsureFrontendRequestsAreStateful` (`vendor/laravel/sanctum/src/Http/Middleware/EnsureFrontendRequestsAreStateful.php`).

* Always sets `session.http_only = true`, `session.same_site = 'lax'`.
* A request is **stateful** iff
  * Sanctum's `fromFrontend`: `Referer` (or, if absent, `Origin`) with `http(s)://` stripped and a trailing `/`
    added matches `Str::is("<domain>/*")` for any entry in `sanctum.stateful`
    (default: `localhost,localhost:3000,127.0.0.1,127.0.0.1:8000,::1` + host[:port] of `APP_URL`; overridable by
    `SANCTUM_STATEFUL_DOMAINS`), **or**
  * the request carries a cookie named `config('session.cookie')` (default `pelican_session`, `config/session.php`).
* For stateful requests these extra middleware run (in order): attribute `sanctum=true`, `EncryptCookies`,
  `AddQueuedCookiesToResponse`, `StartSession`, `ValidateCsrfToken` (`config/sanctum.php:63-66`).
  * CSRF (`vendor/.../Foundation/Http/Middleware/PreventRequestForgery.php:95-196`): skipped for `GET/HEAD/OPTIONS`;
    passes if header `Sec-Fetch-Site: same-origin`, or token from `_token` input / `X-CSRF-TOKEN` header /
    decrypted `X-XSRF-TOKEN` header equals the session token. Otherwise `TokenMismatchException('CSRF token mismatch.')`
    → rendered as `HttpException` **419** (`Handler::prepareException`).
  * The XSRF-TOKEN cookie is (re)queued on successful responses.
* Consequence: a client that sends the `pelican_session` cookie together with a bearer token on a `POST` must also
  satisfy CSRF, even if it authenticates with the bearer token.

### 2.3 `IsValidJson`

`app/Http/Middleware/Api/IsValidJson.php:17-28`. If `Request::isJson()` (Content-Type contains `/json` or `+json`)
**and** `!empty($request->getContent())` (note PHP `empty`: a body of exactly `0` is treated as empty), the raw body
is `json_decode`d with depth 512 and `JSON_THROW_ON_ERROR`. On failure:
`BadRequestHttpException('The JSON data passed in the request appears to be malformed: ' . json_last_error_msg())`
→ **400** (pretty JSON, §6). Typical messages: `Syntax error`, `Malformed UTF-8 characters, possibly incorrectly encoded`,
`Maximum stack depth exceeded`, `Control character error, possibly incorrectly encoded`, `State mismatch (invalid or malformed JSON)`.
Runs after authentication (step 4), so an unauthenticated malformed request gets 401.

Note: `POST .../files/write` takes the raw body as file content — clients must therefore not send it with a JSON
content type unless the content is valid JSON.

### 2.4 Input normalisation

* `TrimStrings` trims all string inputs (query, form and JSON body), except keys `current_password`, `password`,
  `password_confirmation` (framework default) **and** except whole requests whose path matches
  `/^\/api\/client\/servers\/([a-z0-9-]{36})\/files(\/?$|\/(.)*$)/i`
  (`app/Providers/RouteServiceProvider.php:15,26-28`). Only the 36-char UUID form is matched: file routes addressed via
  the 8-char short identifier **are** trimmed.
* `ConvertEmptyStringsToNull` converts `""` → `null` in query, form and JSON input. Consequences used throughout:
  `required` fails on `""`; `?per_page=` is `null`, `filter[x]=` is `null`, etc.
* `$request->input()` merges body over query (`body + query`), and FormRequest validation sees the merged set, so
  any body field may also be supplied in the query string.

### 2.5 IP allow-list (`AuthenticateIPAccess`)

`app/Http/Middleware/Api/AuthenticateIPAccess.php:23-50`.

* Session requests (TransientToken) and keys whose `allowed_ips` is empty pass.
* Otherwise `$request->ip()` (honours `TRUSTED_PROXIES`, `config/trustedproxy.php`) must be contained in at least one
  entry, each parsed with `IPTools\Range::parse` (single IP or CIDR, v4/v6).
* On failure: activity `auth:ip-blocked` (actor = user, subjects = user + api key, property
  `identifier` = key identifier) is written, then `AccessDeniedHttpException` **403** with detail
  `This IP address (<ip>) does not have permission to access the API using these credentials.`

### 2.6 CORS

`config/cors.php` merged over framework defaults: paths `api/*`, `sanctum/csrf-cookie`; methods
`GET, POST, PUT, PATCH, DELETE, HEAD`; origins from `APP_CORS_ALLOWED_ORIGINS` (comma list); headers `*`;
`supports_credentials = true`; `max_age = 0`; no exposed headers. Preflight `OPTIONS` is answered by `HandleCors`.

### 2.7 Locale

API requests are not localised (`LanguageMiddleware` is only in the `web` group); messages use `app.locale`
(default `en`). Translation files: `lang/en/*.php` overriding `vendor/laravel/framework/src/Illuminate/Translation/lang/en/*.php`.

---

## 3. Rate limiting

Limiter definitions: `app/Providers/RouteServiceProvider.php:64-92`, `app/Enums/ResourceLimit.php`; defaults
`config/http.php`. Implementation `vendor/.../Routing/Middleware/ThrottleRequests.php`; counters live in the default
cache store. Keys are hashed (`shouldHashKeys` default true).

| Limiter | Applied to | Key (cache) | Default limit / window | Env |
|---|---|---|---|---|
| `api.client` | every client route | `md5("api.client" . (user.uuid ?: request.ip))` | 256 / 1 min | `APP_API_CLIENT_RATELIMIT`, `_PERIOD` |
| `api.client:server-resource:Websocket` | `GET /servers/{s}/websocket` | `md5(name . server.uuid)` (shared by **all users** of the server) | 5 / 1 min | `APP_API_WEBSOCKET_RATELIMIT[_PERIOD]` |
| `...:AllocationCreate` | `POST /servers/{s}/network/allocations` | per server | 2 / 1 min (`default`) | `APP_API_DEFAULT_RATELIMIT[_PERIOD]` |
| `...:BackupRestore` | `POST /servers/{s}/backups/{b}/restore` | per server | 3 / 15 min | `APP_API_BACKUP_RESTORE_RATELIMIT[_PERIOD]` |
| `...:DatabaseCreate` | `POST /servers/{s}/databases` | per server | 2 / 1 min | `APP_API_DATABASE_CREATE_RATELIMIT[_PERIOD]` |
| `...:ScheduleCreate` | `POST /servers/{s}/schedules` | per server | 2 / 1 min (`default`) | `APP_API_DEFAULT_RATELIMIT[_PERIOD]` |
| `...:SubuserCreate` | `POST /servers/{s}/users` | per server | 10 / 15 min | `APP_API_SUBUSER_CREATE_RATELIMIT[_PERIOD]` |
| `...:FilePull` | `POST /servers/{s}/files/pull` | per server | 5 / 10 min | `APP_API_FILE_PULL_RATELIMIT[_PERIOD]` |
| `throttle` (bare) | `PUT /account/email` | `sha1(user.id)` (unprefixed; shared with any other bare `throttle` route) | 60 / 1 min | — |
| email-change limiter (in controller) | `PUT /account/email` | `user:update-email:{uuid}` (unhashed) | 3 per 24 h, hit only on actual change | — |

(`<name>` above = `api.client:server-resource:<EnumCaseName>`, e.g. `api.client:server-resource:Websocket`;
`Limit::perMinutes(p, n)` = window `60*p` s.)

Algorithm per limiter: if `attempts >= max` → throw; else `hit(key, decay)` **before** running the rest of the
pipeline (failed requests count too), then add headers to the response.

Success-response headers (added by each limiter, inner first):
`X-RateLimit-Limit: <max>`, `X-RateLimit-Remaining: <max - attempts>`. When two limiters apply, the outer one does
**not** overwrite the headers if the existing `X-RateLimit-Remaining` is `<=` its own remaining value
(`getHeaders` early return) — i.e. the most restrictive limiter's pair is reported.

429 response: `Illuminate\Http\Exceptions\ThrottleRequestsException('Too Many Attempts.')` with headers
`X-RateLimit-Limit`, `X-RateLimit-Remaining: 0`, `Retry-After: <seconds>`, `X-RateLimit-Reset: <unix ts>`; body
(pretty JSON):
```json
{"errors":[{"code":"ThrottleRequestsException","status":"429","detail":"Too Many Attempts."}]}
```
Requests rejected before step 8 (binding 404, 401, JSON 400, IP 403, application-key 403) carry no rate-limit
headers and are not counted.

Other 429s: `TooManyRequestsHttpException` (`code` `TooManyRequestsHttpException`) from the email-change limiter
(no `Retry-After`) and from backup creation throttling (`Retry-After` set), see §8.

---

## 4. Route-model binding (runs before auth!)

`scopeBindings()` is enabled for the whole client group, so every child parameter is resolved through its parent
(`Model::resolveChildRouteBinding` → `$parent->{plural(camel(child))}()->where(field, value)->first()`).

| Parameter | Resolution | Source |
|---|---|---|
| `{server:uuid}` | `servers WHERE uuid_short = ? OR uuid = ?` → `firstOrFail` (accepts the 8-char identifier **or** full UUID) | `app/Models/Server.php:458-464` |
| `{database}` | `$server->databases()->where('id', ?)` | default scoped |
| `{schedule}` | `$server->schedules()->where('id', ?)` | default scoped |
| `{task}` | parent is `{schedule}`: `$schedule->tasks()->where('id', ?)` | default scoped |
| `{allocation}` | `$server->allocations()->where('id', ?)` | default scoped |
| `{backup:uuid}` | `$server->backups()->where('uuid', ?)` | default scoped |
| `{user:uuid}` | `$server->subusers()->whereRelation('user','uuid',?)->firstOrFail()->user` (only users that are subusers) | `app/Models/Server.php:466-472` |
| `{identifier}`, `{fingerprint}` | plain strings | — |

Not found → `ModelNotFoundException` → rendered as `NotFoundHttpException` **404** with detail
`The requested resource could not be found on the server.` (§6.2). Because binding is step 1, this 404 is
returned **even for unauthenticated requests**, and before the throttle counts.
Non-numeric ids for integer keys are passed to the DB as-is (MySQL/SQLite coerce → 404; PostgreSQL may raise
a query error → 500; see Open questions).

`ResourceBelongsToServer` (`app/Http/Middleware/Api/Client/Server/ResourceBelongsToServer.php:29-82`) re-checks every
bound model: `Allocation|Backup|Database|Schedule|Subuser` must have `server_id == server.id`; `User` must be a subuser
(the `Subuser` row is stored in request attribute `subuser`, consumed by `SubuserController`); `Task` must belong to the
bound schedule which belongs to the server; any other model type → `InvalidArgumentException` (500). Mismatch →
`NotFoundHttpException('The requested resource was not found for this server.')` 404. With scoped bindings this
is defence-in-depth only.

---

## 5. Server access, state checks and permissions

### 5.1 `AuthenticateServerAccess`

`app/Http/Middleware/Api/Client/Server/AuthenticateServerAccess.php:32-61`. For every `/servers/{server}` route:

1. If `user.id != server.owner_id` and `user->cannot('update server', $server)` and the user is not in
   `server.subusers` → `NotFoundHttpException(trans('exceptions.api.resource_not_found'))` **404**,
   detail `The requested resource does not exist on this server.` (`lang/en/exceptions.php:57`).
   `'update server'` is evaluated through the Gate: root admin (`Gate::before`, `app/Providers/AppServiceProvider.php:105`)
   or a role with spatie permission `update server` (spatie's `Gate::before`, so node restrictions are **not** applied here).
2. `$server->validateCurrentState()` (`app/Models/Server.php:474-489`): conflict iff
   `status == suspended` OR `node.maintenance_mode` OR `!isInstalled()` (status `installing`, `install_failed`,
   `reinstall_failed`) OR `status == restoring_backup` OR an unfinished transfer exists
   (`server_transfers.successful IS NULL`, latest by id). On conflict:
   * route `api:client:server.view` (`GET /servers/{s}`) → allowed anyway;
   * routes `api:client:server.ws` (websocket) and `api:client:server.files.*` → allowed if the user passes
     `'update server'` (as in 1);
   * otherwise rethrow `App\Exceptions\Http\Server\ServerStateConflictException` (extends `ConflictHttpException`) **409**.
     Detail chosen by the **first** matching condition (`app/Exceptions/Http/Server/ServerStateConflictException.php`,
     `lang/en/exceptions.php:62-70`):

     | condition (in order) | detail |
     |---|---|
     | suspended | `This server is currently suspended and the functionality requested is unavailable.` |
     | node maintenance | `The node of this server is currently under maintenance and the functionality requested is unavailable.` |
     | not installed (installing / install_failed / reinstall_failed) | `This server has not yet completed its installation process, please try again later.` |
     | restoring backup | `This server is currently restoring from a backup, please try again later.` |
     | transfer in progress | `This server is currently being transferred to a new machine, please try again later.` |
     | (fallback) | `This server is currently in an unsupported state, please try again later.` |

### 5.2 Permission strings

`app/Enums/SubuserPermission.php`, flattened by `Subuser::allPermissionKeys()` (`app/Models/Subuser.php:113-156`) in
this exact order (plugins may append custom groups/keys via `Subuser::registerCustomPermissions`):

```
websocket.connect
control.console control.start control.stop control.restart
file.read file.read-content file.create file.update file.delete file.archive file.sftp
backup.read backup.create backup.delete backup.download backup.restore
schedule.read schedule.create schedule.update schedule.delete
user.read user.create user.update user.delete
database.read database.create database.update database.delete database.view-password
allocation.read allocation.create allocation.update allocation.delete
activity.read
mount.read mount.update
startup.read startup.update startup.docker-image
settings.rename settings.description settings.reinstall settings.change-icon
```
(`mount.*`, `settings.change-icon`, `file.sftp` have no client-API route; `websocket.connect` is "hidden" in the UI.)

`Subuser::doesPermissionExist($p)` = `$p` contains `.` and is in the list above.

### 5.3 `HAS(perm)` — `User::can($perm, Server $server)`

`app/Models/User.php:409-456`. When the second argument is a `Server` instance and the ability is a
`SubuserPermission` or an existing permission string, the result is (memoised per request in `Context` under
`users.{uid}.servers.{sid}.{perm}`):

1. `ADMIN_UPDATE` (`canned('update', $server)`) → true. This Gate call resolves to: root admin → true; else
   `ServerPolicy::before` (`app/Policies/ServerPolicy.php:18-45`) → `false` if the user cannot target the server's node
   (role node restrictions), else `ServerPolicy::update` → `can('update server', $server)` → spatie role permission.
2. `server.owner_id == user.id` → true.
3. `perm == 'websocket.connect'` and `canned('view', $server)` (root admin / role permission `view server`) → true.
4. Subuser row exists and `perm ∈ subuser.permissions` → true; else false.

Root admins always pass (via 1). Abilities that are not subuser permissions fall through to the normal Gate.

### 5.4 `GATE(perm)` — Gate with array arguments

Used by `IncludeContext::allowsClient` (`app/Data/Api/IncludeContext.php:55-58`, passes `[$server]`) and by
`Gate::authorize(SubuserPermission::ActivityRead, $server)`. `User::can` does not intercept (argument is not a
`Server` instance), so the Gate runs:

1. `Gate::before`: root admin → true; spatie `checkPermissionTo(perm)` (no such permission → null).
2. `ServerPolicy::before($user, $perm, $server)`: owner → true; subuser holding `perm` → true; user cannot target
   node → false; else null.
3. Policy method (`__call` no-op) → null → **deny**.

Difference to `HAS`: a non-root admin role (`update server`/`view server`) that is neither owner nor subuser gets
**false** from `GATE` but true from `HAS`.

### 5.5 FormRequest authorisation

`app/Http/Requests/Api/Client/ClientApiRequest.php:14-30`: if the request class has a `permission()` method, it
returns `HAS(permission())` against the route's `server` (false if no server param); otherwise true. A false
result → `AuthorizationException` → `AccessDeniedHttpException` **403**, detail `This action is unauthorized.`
(pretty JSON). Authorization runs **before** validation, so missing permission wins over 422.

### 5.6 `GetUserPermissionsService`

`app/Services/Servers/GetUserPermissionsService.php:19-51` — returns the permission list for websocket JWTs and
`GET /servers/{s}` meta:

* `isOwner = user.id == server.owner_id`;
  `isAdmin = user->isAdmin() && (can('view',$server) || can('update',$server))` where `isAdmin()` = root admin OR
  (≥1 role AND ≥1 permission across roles) (`app/Models/User.php:465-471`).
* owner and not admin → `["*"]`
* admin and (owner or `can('update', $server)`) → `["*","admin.websocket.errors","admin.websocket.install","admin.websocket.transfer"]`
* admin otherwise → `array_unique(["websocket.connect","admin.websocket.errors","admin.websocket.install","admin.websocket.transfer", ...subuser.permissions])`
  (PHP `array_unique` keeps keys; if a removed duplicate is not the last element the JSON becomes an **object**
  `{"0":..,"1":..,...}` — see Open questions)
* otherwise → `subuser.permissions ?? []` (or `[]` if not a subuser).

---

## 6. Error format (`app/Exceptions/Handler.php`)

### 6.1 When JSON is produced

Laravel only renders JSON errors when `$request->expectsJson()`
(`vendor/.../Foundation/Exceptions/Handler.php:923-928`; no `shouldRenderJsonWhen` is configured,
`bootstrap/app.php:76`): `Accept` whose first type contains `/json` or `+json`, **or** `X-Requested-With: XMLHttpRequest`
with an `Accept` of `*/*`/absent. Without that:
* `AuthenticationException` → `302` redirect to the Filament login (`Handler::unauthenticated`, `:375-382`);
* `ValidationException` → `302` redirect back with flashed errors;
* everything else → HTML error page (Laravel/Filament views).
Success responses are JSON regardless of `Accept`. The Rust port must implement the JSON path exactly; the
non-JSON path is listed in Open questions.

### 6.2 Generic error object

`Handler::exceptionToArray` (`app/Exceptions/Handler.php:308-349`). Body is always `{"errors":[ <one object> ]}`:

```json
{"errors":[{"code":"<class_basename>","status":"<HTTP status as STRING>","detail":"<message>"}]}
```
* `code` = short class name of the exception **after** Laravel's `prepareException` mapping
  (`vendor/.../Foundation/Exceptions/Handler.php:769-783`): `ModelNotFoundException` → `NotFoundHttpException`;
  `AuthorizationException` → `AccessDeniedHttpException` (or `HttpException` if it carries a status);
  `TokenMismatchException` → `HttpException` (419); `BackedEnumCaseNotFoundException` → `NotFoundHttpException`.
* `status` = `strval(getStatusCode())` if the exception has that method; else `401` for
  `AuthenticationException`, `403` for `AuthorizationException`, `422` for `ValidationException`, else `"500"`.
* `detail` = `getMessage()` if the exception implements `HttpExceptionInterface` or is one of the three mapped
  classes above; otherwise the fixed string
  `An unexpected error was encountered while processing this request, please try again.`
  HttpExceptions constructed without a message have `detail: ""`.
* If the exception (or its `previous`) is a `ModelNotFoundException`, detail is replaced by
  `The requested resource could not be found on the server.`
* The HTTP status of the response = `getStatusCode()` for `HttpExceptionInterface`, else 500; headers from
  `getHeaders()` (e.g. `Retry-After`).

Common codes produced by the client API: `NotFoundHttpException` (404), `AccessDeniedHttpException` (403),
`HttpForbiddenException` (403, `app/Exceptions/Http/HttpForbiddenException.php`), `ServerStateConflictException`
(409), `BadRequestHttpException` (400), `ConflictHttpException`, `ThrottleRequestsException` (429),
`TooManyRequestsHttpException` (429), `InvalidFilterQuery`/`InvalidSortQuery` (400), `DisplayException` and its
subclasses (400), `DataValidationException` (500, detail = full message, see below), `HttpException` (419),
`MethodNotAllowedHttpException` (405: `The <M> method is not supported for route <uri>. Supported methods: <list>.`),
and 500s such as `RequestException`, `ConnectionException`, `TypeError`, `InvalidArgumentException`.

`App\Exceptions\Model\DataValidationException` (`app/Exceptions/Model/DataValidationException.php`) — thrown by the
model validation observer when a model fails its `$validationRules` on save — implements `HttpExceptionInterface`
with status **500**, so its detail is exposed:
`Could not save App\Models\<Class>[<key>]: failed to validate data: <errors as JSON>`.

### 6.3 Unauthenticated (401)

`Handler::unauthenticated` (`:375-382`): `new JsonResponse(exceptionToArray($e), 401)` →
`{"errors":[{"code":"AuthenticationException","status":"401","detail":"Unauthenticated."}]}` (compact JSON).

### 6.4 Validation errors (422)

`Handler::invalidJson` (`app/Exceptions/Handler.php:261-296`). One error object **per message**, flattened across
fields in the order of `$validator->errors()` (rule-definition order of fields, then message order):

```json
{"errors":[
  {"code":"ValidationException","status":"422","detail":"The name field is required.",
   "meta":{"source_field":"name","rule":"required"}}
]}
```
* `meta.source_field` = the attribute key with dots, e.g. `allowed_ips.0`, `files.1.to`.
* `meta.rule` = `Str::snake(<FailedRuleName>)` of the n-th failed rule for that field (`Required` → `required`,
  `AlphaDash` → `alpha_dash`, `RequiredUnless` → `required_unless`, `Unique` → `unique`, rule objects such as
  `Rule::in` → `in`); custom rule classes have `App\_rules\_` replaced by `p_`. Errors added manually in
  `withValidator()->after()` hooks have **no** failed-rule entry → `meta.rule` becomes `""`.
  (Lookup key: field with `.`→`_`, then `.<index>`.)
* There is **no** `source` object in non-debug mode (Pterodactyl's `source.field` does not exist here).
* HTTP status = `$exception->status` (422). Compact JSON.
* Messages: `lang/en/validation.php` (Pelican's own, older wording, e.g. max → `The :attribute may not be greater
  than :max characters.`) falling back to the framework file for missing keys (`required` →
  `The :attribute field is required.`, `in` → `The selected :attribute is invalid.`, `boolean` →
  `The :attribute field must be true or false.`, `unique` → `The :attribute has already been taken.`, `present` →
  `The :attribute field must be present.`, `required_unless` → `The :attribute field is required unless :other is in :values.`,
  `alpha_dash` → `The :attribute may only contain letters, numbers, and dashes.` (app file wins)).
  `:attribute` is the key with `_` → space (e.g. `allowed ips`), confirmed by
  `tests/Integration/Api/Client/ApiKeyControllerTest.php:95,131-150`.

### 6.5 Debug mode (`APP_DEBUG=true`)

`exceptionToArray` merges, for every error object:
`detail` = raw `getMessage()` (no generic 500 text, no ModelNotFound rewrite — e.g.
`No query results for model [App\Models\Server] <id>`), `source: {line: int, file: "<path relative to base>"}`,
`meta: {trace: [frames without args], previous: [[frames]...]}`. For validation errors `meta` additionally gets
`source_field` and `rule` (merged after `trace`/`previous`). Key order: `code, status, detail, source, meta`.

### 6.6 Encoding of error bodies (byte-level)

| Path | Encoder | Flags |
|---|---|---|
| `DisplayException` and subclasses (render method, `app/Exceptions/DisplayException.php:60-77`) | `response()->json()` | `0` (compact, `\/` escaped, `\uXXXX`) |
| `AuthenticationException` | `new JsonResponse` | `0` |
| `ValidationException` | `response()->json` | `0` |
| everything else (all `HttpException`s, 500s) | `prepareJsonResponse` (`vendor/.../Foundation/Exceptions/Handler.php:1118-1126`) | `JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES` (4-space indent, `\n`, slashes unescaped, unicode still `\uXXXX`) |

Example (pretty path, exact bytes):
```
{
    "errors": [
        {
            "code": "NotFoundHttpException",
            "status": "404",
            "detail": "The requested resource could not be found on the server."
        }
    ]
}
```

Before rendering, `Handler::render` rolls back any open DB transaction (`:237-253`).

---

## 7. Response conventions

### 7.1 JSON encoding of success bodies

Controllers return PHP arrays or `JsonResponse`s; both are encoded by `Illuminate\Http\JsonResponse` with
**flags 0** (`vendor/.../Http/JsonResponse.php:27-32`): `/` → `\/`, non-ASCII → `\uXXXX`, key order = insertion
order, empty PHP arrays → `[]`, PHP floats with `serialize_precision=-1` (whole floats keep `.0`, e.g. `1.0`).
`Content-Type: application/json`. Status 200 unless stated; create endpoints also return **200** (not 201).

`204` responses (`new JsonResponse([], 204)`, `new Response('', 204)`, `new JsonResponse(null, 204)`) have an **empty
body and no Content-Type** (`Symfony Response::prepare`). `202`/`403` responses built with `new JsonResponse([], ...)`
have body `[]`.

### 7.2 Envelope

`app/Data/Api/Envelope.php`, `app/Data/Api/PanelResponse.php`, `app/Data/Api/IncludeContext.php`.

* Item: `{"object": "<resource name>", "attributes": {...}}` (+ `"meta": {...}` if any meta).
* Collection: `{"object": "list", "data": [ {"object": "<name>", "attributes": {...}}, ... ]}` (+ `"meta"`).
* Null item (`PanelResponse` with null data, or include resolvers returning null):
  `{"object": "null_resource", "attributes": null}`.
* Raw item (database password include): `{"object":"database_password","attributes":{"password":"..."}}`.
* Includes nest under `attributes.relationships` (key added **last**, only when at least one include resolved):
  `"relationships": {"<include>": <item|list|null_resource>}`.
* Meta: `addMeta()` blocks are unioned with first-key-wins; for paginated collections `meta.pagination` is appended
  **after** any controller meta.

### 7.3 Includes (`?include=`)

`ApplicationApiController::__construct` (`app/Http/Controllers/Api/Application/ApplicationApiController.php:21-35`):
* source: `$request->input('include', [])` (query or body); string → `explode(',')`; array accepted as-is;
  each item trimmed, empties dropped; recursion limit **2**.
* `PanelResponse::requestedIncludes` (`app/Data/Api/PanelResponse.php:136-153`): strips any `:params` suffix, cuts each
  dot path to 2 segments, and implies all parents (`a.b` → `a`, `a.b`).
* Endpoints may force includes via `parseIncludes([...])` (merged with the request's).
* `Envelope::attributes` (`app/Data/Api/Envelope.php:62-88`): names = `$defaultIncludes` (always) followed by each
  `$availableIncludes` entry (in declaration order) that was requested at the current path. Unknown include names
  are **silently ignored** (no 400).
* Per-resource includes are listed in §9. No client resource has nested includes deeper than one level.

### 7.4 Pagination

Paginated endpoints: `GET /api/client`, `GET /account/activity`, `GET /servers/{s}/activity`, `GET /servers/{s}/backups`.

* `page`: `$request->input('page')`; used if `filter_var(INT)` and `>= 1`, else 1
  (`vendor/.../Pagination/PaginationState.php:19-27`). Pages beyond the last return `data: []` with
  `current_page` = requested page.
* `per_page`: endpoint-specific `min(<raw string>, <max>)` using PHP loose comparison (see each route). The value is
  passed to Eloquent `paginate()`: a falsy value (`0`, `null`, `""`→null) falls back to the model default **15**;
  `LengthAwarePaginator` casts to `int`. Negative values disable the SQL `LIMIT` (all rows) and are echoed back as
  `per_page`; non-numeric strings compare greater than the max and therefore yield the max. (These quirks are
  documented for completeness; tests should mostly use sane values.)
* `meta.pagination` (`app/Data/Api/PanelResponse.php:161-182`), key order:
  ```json
  "pagination": {"total": int, "count": int, "per_page": int, "current_page": int, "total_pages": int,
                 "links": {"previous": "<url>", "next": "<url>"}}
  ```
  `total_pages = max(ceil(total / max(per_page,1)), 1)`. `links.previous` only if `current_page > 1`;
  `links.next` only if `current_page < total_pages`; when neither exists `links` is the empty **object** `{}`.
* Link URL = `Request::url()` (scheme+host+path of the current request, trailing `/` trimmed, query removed;
  honours trusted proxies — not `APP_URL`) + `?` + `http_build_query(appended_query + ["page" => n], RFC3986)`.
  `appends($request->query())` is used by the server list and both activity endpoints (all current query params,
  except `page`, are kept in their original order, `page` is always last). The backups list does **not** append, so its
  links are `<url>?page=N` only. Brackets are percent-encoded (`filter%5Bevent%5D=...`). In the JSON body `/` is
  escaped (`http:\/\/...`).

### 7.5 Filters and sorts (spatie/laravel-query-builder)

`vendor/spatie/laravel-query-builder` with default config (`config/query-builder.php` in the package; no app override).

* `filter[<name>]=<value>` read via `$request->input('filter')` (query or body). If `filter` is a plain string it is
  ignored. Values `"true"`/`"false"` become booleans; string values containing `,` are split into arrays.
* Unknown filter names → `Spatie\QueryBuilder\Exceptions\InvalidFilterQuery` **400** (pretty JSON), detail
  ``Requested filter(s) `<names, comma-space>` are not allowed. Allowed filter(s) are `<allowed>`.``
* Partial filter (string names in `allowedFilters`): `LOWER(<table>.<col>) LIKE ?` with `%<lowercased, LIKE-escaped value>%`
  (adds `ESCAPE '\'` on SQLite/SQLSRV); array values → OR of partial matches (empty items ignored); null value → no-op.
* `sort=<a>,-<b>`: only when `allowedSorts()` is called. Unknown sort → `InvalidSortQuery` **400**, detail
  ``Requested sort(s) `<names>` is not allowed. Allowed sort(s) are `<allowed>`.`` Leading `-` = descending.
  Endpoints that don't call `allowedSorts` ignore `sort` entirely.
* Without `sort`, list order is whatever the DB returns (no `ORDER BY`) — see Open questions.

### 7.6 Dates

* Most resources: `Carbon::toAtomString()` = PHP `DATE_ATOM` `Y-m-d\TH:i:sP`, e.g. `2025-01-31T12:34:56+00:00`
  (never `Z`, no fractional seconds), in the model's timezone = `app.timezone` = `UTC` (`config/app.php`).
* `UserData`/`SubuserData` use `ApiResource::formatTimestamp` (`app/Data/Api/ApiResource.php:55-60`): the Carbon value is
  stringified (`Y-m-d H:i:s`), re-parsed in the default timezone, converted to `UTC`, `toAtomString()` → same format.
* `FileObjectData` parses the Wings-provided strings with `Carbon::parse` and keeps **their** offset
  (`Z` → `+00:00`); an absent value parses as "now".
* JWT time claims are integer Unix timestamps (`app/Extensions/Lcobucci/JWT/Encoding/TimestampDates.php`).

### 7.7 Activity log conventions

`app/Services/Activity/ActivityLogService.php`, `app/Services/Activity/ActivityLogTargetableService.php`.

* Every entry: `event`, `ip = Request::ip()`, `api_key_id` (set by `TrackAPIKey` when a bearer `ApiKey` is used, else
  null), `actor` = request user (set by `AccountSubject`/`ServerSubject`), `timestamp = now()`, `properties` (JSON,
  insertion order), `description` null.
* Subjects (`activity_log_subjects`): the default subject (user for `/account/*`, server for `/servers/*`) **plus**
  any explicit `subject(...)` models (deduplicated).
* `->log()` saves immediately; `->transaction(cb)` runs `cb` in a DB transaction and saves the log only if it succeeds.
* Each save dispatches `App\Events\ActivityLogged` (→ `DispatchWebhooks`). In non-production envs a failure to save
  the log is rethrown; in production it is logged and swallowed.
* Model writes also fire `eloquent.created/updated/deleted` events consumed by `App\Listeners\DispatchWebhooks`
  (`app/Providers/EventServiceProvider.php:14-20`), as do all `App\Events\*` events.

### 7.8 Wings (daemon) calls

`Http::daemon($node)` macro (`app/Providers/AppServiceProvider.php:68-78`): `Accept: application/json`, JSON body,
`Authorization: Bearer <decrypt(node.daemon_token)>`, TLS verification only when `APP_ENV=production`,
timeout `GUZZLE_TIMEOUT` (15 s), connect timeout `GUZZLE_CONNECT_TIMEOUT` (5 s), base URL
`node.getConnectionAddress()` = `{scheme}://{fqdn}:{daemon_connect}` (`app/Models/Node.php:219-222`).

`DaemonRepository::getHttpClient()` (`app/Repositories/Daemon/DaemonRepository.php:48-67`) adds
`throwIf(enforceValidNodeToken)`: on a non-2xx Wings response, if the `User-Agent` header is empty, or matches
`/^Pelican Wings\/v(?:\d+\.\d+\.\d+|develop) \(id:(\w*)\)$/` with an id different from `node.daemon_token_id`, a
`ConnectionException("<uri> does not match node token_id !")` is thrown; otherwise
`Illuminate\Http\Client\RequestException` (its `code` = Wings HTTP status). Both render as generic **500**
(`code` `ConnectionException` / `RequestException`). Connection failures → `ConnectionException` 500.
`Server::send()` (console commands) uses the bare macro **without** `throwIf`, so Wings HTTP errors are ignored there.

---

## 8. Endpoints

Paths are relative to `/api/client`. "Server routes" (`/servers/{server}/...`) always include §5.1 checks.
Unless noted, the request needs no body.

### 8.1 Base

#### `GET /` — list servers
`routes/api-client.php:19`, `app/Http/Controllers/Api/Client/ClientController.php:35-78`. Request `GetServersRequest` (always authorised).

Query:
* `type`: `admin` | `admin-all` | `owner` | anything else (default).
* `filter[uuid]`, `filter[name]`, `filter[description]`, `filter[external_id]` — partial filters on `servers.<col>`.
* `filter[*]` — `App\Models\Filters\MultiFieldServerFilter` (`app/Models/Filters/MultiFieldServerFilter.php`):
  * value matching `/^(?:[0-9]{1,3}\.){3}[0-9]{1,3}(\:\d{1,5})?$/` or `/^:\d{1,5}$/` → `SELECT servers.* JOIN allocations ON allocations.server_id = servers.id`
    with `allocations.ip = <ip>` (and `allocations.port LIKE '<port>%'` when `ip:port`), or only
    `allocations.port LIKE '<port>%'` for `:port`; `GROUP BY servers.id`;
  * otherwise `(servers.uuid = v OR servers.uuid LIKE 'v%' OR servers.uuid_short = v OR servers.external_id = v OR LOWER(servers.name) LIKE '%v%')`
    (no lower-casing of `v` itself; `v` containing `,` becomes an array → TypeError 500).
  * Any other `filter[...]` key → 400 `InvalidFilterQuery` (allowed list rendered as `uuid, name, description, external_id, *`).
* `include` — `egg`, `subusers` (eager loaded); `per_page` (default `"50"`, max 100: `min(query('per_page','50'), 100)`); `page`. `sort` ignored.

Scope:
* `admin`/`admin-all`: non-root-admins get an always-empty result (`whereRaw('1 = 2')`); root admins get all servers
  (`admin-all`) or all servers they are **not** owner/subuser of (`admin`).
* `owner`: `servers.owner_id = user.id`.
* default: servers the user owns or is a subuser of (`User::directAccessibleServers`, `app/Models/User.php:324-332`).
No explicit ordering.

Response 200: list of `server` (§9.1) with default includes `allocations`, `variables` + requested ones, and
`meta.pagination` (query appended).

#### `GET /permissions`
`routes/api-client.php:20`, `ClientController.php:87-95`. No permission. Response 200 (not via the envelope helper):
```json
{"object":"system_permissions","attributes":{"permissions":["websocket.connect","control.console", ...]}}
```
List exactly as in §5.2 (flat array of strings, plus plugin permissions).

### 8.2 Account (`/account`, middleware `AccountSubject`)

#### `GET /account`
`AccountController.php:42-47`. Response: item `user` (§9.10).

#### `PUT /account/username`
`AccountController.php:54-66`; `app/Http/Requests/Api/Client/Account/UpdateUsernameRequest.php`.
1. authorize: `Hash::check(input('password'), user.password)` else `InvalidPasswordProvidedException`
   (DisplayException) **400** `The password provided was invalid for this account.`; then `!user.is_managed_externally`
   else 403 `This action is unauthorized.`
2. rules: `username`: `required`, `between:1,255`, `unique:users,username` ignoring the current user (`app/Models/User.php:188`, `HasValidation::getRulesForUpdate`).
3. `UserUpdateService::handle(user, validated)` (`app/Services/Users/UserUpdateService.php`): `forceFill` + `saveOrFail`
   (mutator lower-cases and trims; model validation observer re-validates the whole user → `DataValidationException` 500 on failure).
4. If `original_username !== input('username')` (raw input, so a case-only change still logs): activity
   `user:account.username-changed` `{old, new}`.
Response **204**.

#### `PUT /account/email`
Extra middleware `throttle` (60/min). `AccountController.php:73-95`; `UpdateEmailRequest`.
1. authorize: same password check (`input('password')`) and managed-externally check.
2. rules: `email`: `required`, `email`, `between:1,255`, `unique:users,email` ignoring self.
3. If `RateLimiter::tooManyAttempts("user:update-email:{uuid}", 3)` → `TooManyRequestsHttpException` **429**
   `Your email address has been changed too many times today. Please try again later.` (no Retry-After).
4. If `mb_strtolower(original) !== mb_strtolower(validated email)`: `RateLimiter::hit(key, 86400)`; update (email
   lower-cased by mutator); activity `user:account.email-changed` `{old, new: raw input}`.
Response **204**.

#### `PUT /account/password`
`AccountController.php:105-123`; `UpdatePasswordRequest`.
1. authorize: `Hash::check(input('current_password'), user.password)` else 400 (same message); managed-externally → 403.
2. rules: `password`: `required`, `string`, `confirmed` (needs `password_confirmation`), `min:8`.
3. Activity `user:account.password-changed` (no properties) wrapping `UserUpdateService::handle(user, ['password'=>…])`
   (bcrypt hash; dispatches `App\Events\User\PasswordChanged` → `RevocationListener` queues one
   `RevokeSftpAccessJob(user.uuid, node)` per node hosting a server the user owns/subuses → Wings
   `POST /api/deauthorize-user {"user": "<uuid>", "servers": []}`).
4. `$this->manager->guard()` is the Sanctum `RequestGuard` (default guard switched in §2.1), never a `SessionGuard`,
   so `logoutOtherDevices` is effectively never called.
Response **204**.

#### `GET /account/activity`
`app/Http/Controllers/Api/Client/ActivityLogController.php:20-33`. Request `ClientApiRequest` (no permission).
Query: activity logs where the user is a **subject** (`activity_log_subjects`, `app/Models/User.php:297`),
excluding events in `ActivityLog::DISABLED_EVENTS = ['server:file.upload']`; `filter[event]` partial
(`LOWER(activity_logs.event) LIKE`); `sort=timestamp|-timestamp`; eager `actor`; `per_page` default `"25"`, max 100;
query appended to links. Response: list of `activity_log` (§9.9) + `meta.pagination`; `?include=actor` supported.

#### `GET /account/api-keys`
`ApiKeyController.php:22-27`. All keys with `key_type = 1` of the user (no ordering). Response list of `api_key` (§9.11).

#### `POST /account/api-keys`
`ApiKeyController.php:38-56`; `StoreApiKeyRequest` (`app/Http/Requests/Api/Client/Account/StoreApiKeyRequest.php`).
Rules: `description`: `required`, `nullable`, `string`, `max:500` (so effectively required non-empty);
`allowed_ips`: `array`, `max:50`; `allowed_ips.*`: `string`. After-hook: for every `allowed_ips[i]`, `Range::parse(ip)->valid()`
must be true, else error on key `allowed_ips.<i>`: `"<ip>" is not a valid IP address or CIDR range.` (meta.rule `""`).
Logic:
1. If `user.apiKeys.count >= config('panel.api.key_limit')` (25) → `DisplayException` **400**
   `You have reached the account limit for number of API keys.`
2. Create `api_keys` row: `user_id`, `key_type=1`, `identifier` (`pacc_`+11), `token` = encrypt(32-char secret),
   `memo` = description (`?? ''`), `allowed_ips` = input or `[]`.
3. Activity `user:api-key.create` `{identifier}`, subjects user + key.
Response 200:
```json
{"object":"api_key","attributes":{...§9.11...},"meta":{"secret_token":"<32-char secret only>"}}
```
The bearer token is `attributes.identifier + meta.secret_token` (confirmed by
`tests/Integration/Api/Client/ApiKeyControllerTest.php:71-74`).

#### `DELETE /account/api-keys/{identifier}`
`ApiKeyController.php:63-78`. Finds the user's key with `key_type=1` and `identifier` (`firstOrFail` → 404). Activity
`user:api-key.delete` `{identifier}` (subject: user only). Hard delete. Response **204**. Deleting the key used for the
request is allowed.

#### `GET /account/ssh-keys`
`SSHKeyController.php:21-26`. Non-deleted keys (model uses `SoftDeletes`). List of `ssh_key` (§9.12).

#### `POST /account/ssh-keys`
`SSHKeyController.php:35-51`; `StoreSSHKeyRequest`.
Rules: `name` `required|string`; `public_key` `required|string` (`app/Models/UserSSHKey.php` rules). After-hook
(runs even if rules failed): `phpseclib3\Crypt\PublicKeyLoader::loadPublicKey(input('public_key'))`:
* load failure → error `public_key`: `The public key provided is not valid.` (stops the hook)
* DSA → `DSA keys are not supported.`
* RSA with length < 2048 → `RSA keys must be at least 2048 bytes in length.`
* fingerprint already on a non-deleted key of this user → `The public key provided already exists on your account.`
* A missing/null `public_key` makes `loadPublicKey(null)` throw `TypeError` → **500** (not 422).
Logic: create `user_ssh_keys` `{user_id, name, public_key = key->toString('PKCS8'), fingerprint = key->getFingerprint('sha256')}`.
`public_key` format: `-----BEGIN PUBLIC KEY-----\r\n` + base64 DER in 64-char lines each ending `\r\n` +
`-----END PUBLIC KEY-----`. `fingerprint` = base64(SHA-256 of the OpenSSH wire blob) with the single trailing `=`
removed (43 chars, no `SHA256:` prefix). Activity `user:ssh-key.create` `{fingerprint}`, subjects user + key.
Response 200 item `ssh_key`.

#### `DELETE /account/ssh-keys/{fingerprint}`
`SSHKeyController.php:58-73`. `firstOrFail` by fingerprint among the user's non-deleted keys (404). Activity
`user:ssh-key.delete` `{fingerprint}`, subjects user + key. Soft delete (`deleted_at`). Response **204**.

### 8.3 Server: core

All routes below: `ServerSubject`, `AuthenticateServerAccess`, `ResourceBelongsToServer` (§5.1, §4).

#### `GET /servers/{server}`
`routes/api-client.php:55` (name `api:client:server.view`, allowed in conflict states),
`ServerController.php:27-36`. Request `GetServerRequest` (always authorised).
Response 200: item `server` (§9.1) with default includes (+ `egg`, `subusers` if requested) and
```json
"meta": {"is_server_owner": bool, "user_permissions": [string...]}
```
(`user_permissions` per §5.6).

#### `GET /servers/{server}/websocket`
`routes/api-client.php:56-58` (name `api:client:server.ws`), extra throttle `Websocket`. `WebsocketController.php:38-74`.
1. `HAS(websocket.connect)` else `HttpForbiddenException` **403** `You do not have permission to connect to this server's websocket.`
2. `permissions = GetUserPermissionsService(server, user)`.
3. `node = server.node`; if a transfer is in progress: require `admin.websocket.transfer ∈ permissions` else 403
   `You do not have permission to view server transfer logs.`; if `transfer.archived` → `node = transfer.newNode`.
4. JWT (`app/Services/Nodes/NodeJWTService.php:72-108`), HS256 keyed with `decrypt(node.daemon_token)`:
   * header (order): `typ: "JWT"`, `alg: "HS256"`, `jti: <id>`;
   * claims (order): `iss` = `config('app.url')`, `aud` = `[node.getConnectionAddress()]` (**array**), `jti` =
     `sha256_hex(user.id . server.uuid)`, `iat` = now, `nbf` = now − 5 min, `exp` = now + 10 min, `server_uuid`,
     `permissions` (array from step 2), `user_uuid`, `scope` = `"websocket"`, `unique_id` = `Str::random(16)`.
     JSON encoded with `JSON_UNESCAPED_SLASHES|JSON_UNESCAPED_UNICODE`; dates as Unix ints.
5. `socket` = `node.getConnectionAddress()` with `https://`→`wss://`, `http://`→`ws://`, + `/api/servers/{server.uuid}/ws`.
Response 200 (not an envelope):
```json
{"data":{"token":"<jwt>","socket":"wss:\/\/node.example.com:8080\/api\/servers\/<uuid>\/ws"}}
```
No activity log.

#### `GET /servers/{server}/resources`
`ResourceUtilizationController.php:37-47`. `GetServerRequest` (no permission). Cached 20 s under cache key
`resources:{server.uuid}`: `DaemonServerRepository::getDetails()` → Wings `GET /api/servers/{uuid}` with **1 s**
connect/request timeout. Any Wings error is swallowed and yields `{"state": "missing"}`
(`app/Repositories/Daemon/DaemonServerRepository.php:20-45`). Response item `stats` (§9.14).

#### `GET /servers/{server}/activity`
`app/Http/Controllers/Api/Client/Servers/ActivityLogController.php:27-45`.
`Gate::authorize(activity.read, $server)` = `GATE(activity.read)` → 403 `This action is unauthorized.`
Query: logs having a subject `(subject_type='server', subject_id=server.id)`, excluding disabled events; if
`config('activity.hide_admin_activity')` (env `APP_ACTIVITY_HIDE_ADMIN`, default false) hide entries whose actor
is a user holding any role and who is neither owner nor subuser (`ActivityLog::scopeHideAdminActivity`,
`app/Models/ActivityLog.php:130-146`). `sort=timestamp|-timestamp`, `filter[event]` partial, eager `actor`,
`per_page` default `"25"` max 100, query appended. Response list of `activity_log` + `meta.pagination`.

#### `POST /servers/{server}/command`
`CommandController.php:26-45`; `SendCommandRequest`: permission `control.console`; `command` `required|string|min:1`.
Wings `POST /api/servers/{uuid}/commands {"commands": ["<command>"]}` via `Server::send` (`app/Models/Server.php:513-518`),
**no throwIf**: Wings HTTP errors are ignored (the `502 → 'Server must be online in order to send commands.'` branch is
unreachable because Laravel's client never throws `BadResponseException`); only connection failures propagate (500).
Activity `server:console.command` `{command}`. Response **204** (`new Response('', 204)`).

#### `POST /servers/{server}/power`
`PowerController.php:32-41`; `SendPowerRequest` (`app/Http/Requests/Api/Client/Servers/SendPowerRequest.php`):
permission from **raw input** `signal`: `start`→`control.start`; `stop`/`kill`→`control.stop`; `restart`→`control.restart`;
anything else→`websocket.connect` (then fails validation). Rule `signal` `required|string|in:start,stop,restart,kill`.
Wings `DaemonServerRepository::power(signal)` → `POST /api/servers/{uuid}/power {"action": "<signal>"}` (throwIf → 500 on error).
Activity `server:power.<signal>` (lower-cased, no properties). Response **204**.

### 8.4 Server: databases

#### `GET /servers/{server}/databases`
`DatabaseController.php:43-48`; `GetDatabasesRequest` (`database.read`). Response list of `server_database` (§9.6);
`?include=password` honoured per §9.6.

#### `POST /servers/{server}/databases`
Throttle `DatabaseCreate`. `DatabaseController.php:61-75`; `StoreDatabaseRequest` (`database.create`).
Rules: `database`: `required`, `alpha_dash`, `min:3`, `max:48`, unique in `databases` where `server_id = server.id`
and `database = "s{server.id}_" + substr(input, 0, 48 - len("s{id}_"))` (custom message `database.unique`:
`The database name you have selected is already in use by this server.`); `remote`: `required`, `string`,
`regex:/^[\w\-\/.%:]+$/` (`app/Models/Database.php:74`).
Logic (`app/Services/Databases/DeployServerDatabaseService.php`, `DatabaseManagementService.php`), in a DB transaction
that first `SELECT ... FOR UPDATE`s the server row:
1. No database hosts at all → `NoSuitableDatabaseHostException` **400** `No database host was found that meets the requirements for this server.`
2. Hosts linked to the node (`database_host_node`); none and `PANEL_CLIENT_DATABASES_ALLOW_RANDOM=false` → same 400.
   Host = random node host, or random of all hosts.
3. `PANEL_CLIENT_DATABASES_ENABLED=false` → `DatabaseClientFeatureNotEnabledException` (plain `PanelException`) → **500** generic.
4. `database_limit` not null and `count >= limit` → `TooManyDatabasesException` **400**
   `Operation aborted: creating a new database would put this server over the defined limit.` (null = unlimited, 0 = none allowed).
5. Duplicate `(server_id, database)` → `DuplicateDatabaseNameException` **400** `A database with that name already exists for this server.`
6. Insert `databases` row: `server_id, database_host_id, database (prefixed), remote, username = "u{server.id}_" + random(10), password = encrypt(random 24 chars with 2–6 of !@=.+^)`.
7. On the DB host: `CREATE DATABASE IF NOT EXISTS`, `CREATE USER ... IDENTIFIED BY ...`, `GRANT ... ON db.* TO user@remote`, `FLUSH PRIVILEGES`.
8. Activity `server:database.create` `{name: <prefixed name>}`, subjects server + database.
Response 200 item `server_database` with the `password` include forced (still subject to `GATE(database.view-password)`).

#### `POST /servers/{server}/databases/{database}/rotate-password`
`DatabaseController.php:87-98`; `RotatePasswordRequest` (`database.update`). Activity
`server:database.rotate-password` `{name}` (subjects server + database) wrapping: new random password, update row,
`DROP USER IF EXISTS`, `CREATE USER`, `GRANT`, `FLUSH PRIVILEGES`. Response 200 item (refreshed) with forced `password` include.

#### `DELETE /servers/{server}/databases/{database}`
`DatabaseController.php:105-110`; `DeleteDatabaseRequest` (`database.delete`). In a transaction: `DROP DATABASE IF EXISTS`,
`DROP USER IF EXISTS`, `FLUSH PRIVILEGES`, activity `server:database.delete` `{name}`, delete row. Response **204** (empty).

### 8.5 Server: files

Route names `api:client:server.files.*` (admins with `update server` bypass state conflicts, §5.1). The 36-char
UUID form disables `TrimStrings` (§2.4). All Wings calls use `DaemonFileRepository` (`app/Repositories/Daemon/DaemonFileRepository.php`).

| Route | Permission | Validation | Wings call (method path, payload) | Activity | Response |
|---|---|---|---|---|---|
| `GET /files/list` | `file.read` | `directory` `sometimes|nullable|string` | `GET /api/servers/{uuid}/files/list-directory?directory=<directory ?? "/">` | — | list of `file_object` (§9.13) built from the Wings JSON array |
| `GET /files/contents` | `file.read-content` | `file` `required|string` | `GET /api/servers/{uuid}/files/contents?file=<file>` | `server:file.read` `{file}` | 200 raw body, `Content-Type: text/plain; charset=utf-8` |
| `GET /files/download` | `file.read-content` | `file` `required|string` | none (JWT only) | `server:file.download` `{file}` | `{"object":"signed_url","attributes":{"url":"<node>/download/file?token=<jwt>"}}` |
| `PUT /files/rename` | `file.update` | `root` `required|nullable|string`; `files` `required|array`; `files.*` `array`; `files.*.to`, `files.*.from` `required|string` | `PUT /api/servers/{uuid}/files/rename {"root": root, "files": [{from,to}...as sent]}` | `server:file.rename` `{directory: root, files, to: files[0].to, from: files[0].from}` | 204 |
| `POST /files/copy` | `file.create` | `location` `required|string` | `POST .../files/copy {"location": location}` | `server:file.copy` `{file: location}` | 204 |
| `POST /files/write` | `file.create` | `file` `required|string` (query) | `POST .../files/write?file=<file>` with the **raw request body** as body | `server:file.write` `{file}` | 204 |
| `POST /files/compress` | `file.archive` | `root` `sometimes|nullable|string`; `files` `required|array`; `files.*` `string`; `name` `sometimes|nullable|string`; `extension` `sometimes|in:zip,tgz,tar.gz,txz,tar.xz,tbz2,tar.bz2` | `POST .../files/compress {"root": root ?? "/", "files": files, "name": name ?? "", "extension": extension ?? ""}` (timeout 900 s) | `server:file.compress` `{name: <wings response name>, directory: root, files}` | 200 item `file_object` from the Wings JSON |
| `POST /files/decompress` | `file.create` | `root` `sometimes|nullable|string`; `file` `required|string` | `POST .../files/decompress {"root": root ?? "/", "file": file}` (timeout 900 s; PHP `set_time_limit(300)`) | `server:file.decompress` `{directory: root, file}` | 204 |
| `POST /files/delete` | `file.delete` | `root` `required|nullable|string`; `files` `required|array`; `files.*` `string` | `POST .../files/delete {"root": root, "files": files}` | `server:file.delete` `{directory: root, files}` | 204 |
| `POST /files/create-folder` | `file.create` | `root` `sometimes|nullable|string`; `name` `required|string` | `POST .../files/create-directory {"name": name, "path": root}` (`root` defaults to `"/"` only when **absent**; explicit `null`/`""` → `TypeError` 500) | `server:file.create-directory` `{name, directory: raw root}` | 204 |
| `POST /files/chmod` | `file.update` | `root` `required|nullable|string`; `files` `required|array`; `files.*.file` `required|string`; `files.*.mode` `required|numeric` | `POST .../files/chmod {"root": root, "files": files as sent}` | **none** | 204 |
| `POST /files/pull` (throttle `FilePull`) | `file.create` | `url` `required|string|url`; `directory` `nullable|string`; `filename` `nullable|string`; `use_header` `boolean`; `foreground` `boolean` | `POST .../files/pull` with `{"url", "root": directory ?? "/", "file_name"?, "use_header"?, "foreground"?}` — optional keys only if present & non-null, values passed through as sent | `server:file.pull` `{directory, url}` | 204 |
| `GET /files/upload` | `file.create` | — | none (JWT only) | — | `{"object":"signed_url","attributes":{"url":"<node>/upload/file?token=<jwt>"}}` |

Notes:
* `required|nullable` behaves as `required` (null and `""` fail), so `root` must be a non-empty string for rename/delete/chmod.
* `contents`: Wings 400 → `FileNotEditableException`, 404 → `Illuminate\Contracts\Filesystem\FileNotFoundException`,
  other errors → `RequestException` — all plain exceptions → **500** generic. If the Wings `Content-Length` header is
  greater than `PANEL_FILES_MAX_EDIT_SIZE` (default 4 MiB) → `FileSizeTooLargeException` (DisplayException) **400**
  `The file you are attempting to open is too large to view in the file editor.` (no check if the header is absent).
  The body is passed through `convert_to_utf8` (`app/helpers.php:146-160`): valid UTF-8 unchanged; UTF-16 with BOM
  converted; otherwise treated as ISO-8859-1.
* `write`/`create-folder`: Wings 400 → `FileExistsException` → 500 generic.
* Signed URLs: `<node>` = `server.node.getConnectionAddress()` (not transfer-aware). JWT as in §8.3 websocket but:
  `exp` = now + 15 min; download claims `file_path` = `rawurldecode(file)`, `server_uuid`, then `user_uuid`,
  `scope: "file-download"`, `unique_id`; upload claims `server_uuid`, `user_uuid`, `scope: "file-upload"`, `unique_id`.
  `jti` = `sha256_hex(user.id . server.uuid)`.
* Upload activity (`server:file.upload`) is reported by Wings via the remote API and hidden from activity lists.

### 8.6 Server: schedules

Permission requests extend `ViewScheduleRequest` (`app/Http/Requests/Api/Client/Servers/Schedules/ViewScheduleRequest.php:17-35`),
which also throws `NotFoundHttpException('The requested resource does not exist on the system.')` 404 if the bound
schedule/task does not belong to the server/schedule (unreachable with scoped bindings).

Common schedule body rules (`StoreScheduleRequest`, from `app/Models/Schedule.php:109-123`):
`name` `required|string|max:255`; `is_active` `filled|boolean`; `only_when_online` `boolean`;
`minute`, `hour`, `day_of_month`, `month`, `day_of_week` each `required|string`.
Next run: `CronExpression("<minute> <hour> <dom> <month> <dow>")->getNextRunDate(now(<tz>))` converted to UTC
(`app/Helpers/Utilities.php:41-47`); any exception → `DisplayException` **400**
`The cron data provided does not evaluate to a valid expression.`

#### `GET /schedules`
`ScheduleController.php:46-53`; `ViewScheduleRequest` (`schedule.read`). List of `server_schedule` (§9.7) with tasks.

#### `POST /schedules` (throttle `ScheduleCreate`)
`ScheduleController.php:65-89`; `StoreScheduleRequest` (`schedule.create`). Insert `schedules`:
`server_id, name, cron_* , is_active = (bool) input (absent → false!), only_when_online = (bool) input, next_run_at`
(timezone column defaults `UTC`, `is_processing` false). Activity `server:schedule.create` `{name}`, subjects server + schedule.
Response 200 item `server_schedule` (tasks `[]`).

#### `GET /schedules/{schedule}`
`ScheduleController.php:98-107`; `ViewScheduleRequest`. Item `server_schedule` with tasks.

#### `POST /schedules/{schedule}` — update
`ScheduleController.php:119-151`; `UpdateScheduleRequest` (`schedule.update`, same rules as store).
Updates the same fields (next run computed in `schedule.timezone`); if `is_active` changes, also sets `is_processing = false`.
Activity `server:schedule.update` `{name: <new name>, active: bool}`. Response 200 refreshed item.

#### `POST /schedules/{schedule}/execute`
`ScheduleController.php:161-168`; `TriggerScheduleRequest` (`schedule.update`).
`ProcessScheduleService::handle(schedule, now: true)` (`app/Services/Schedules/ProcessScheduleService.php`):
* no tasks → `DisplayException` **400** `Cannot process schedule for task execution: no tasks are registered.`
* transaction: `is_processing = true`, `next_run_at = next cron date`; first task (lowest `sequence_id`) `is_queued = true`.
* if `only_when_online`: Wings `GET /api/servers/{uuid}` (5 s timeout); state offline/missing or error → mark task not
  queued, schedule `is_processing=false, last_run_at=now` and return (still 202).
* otherwise runs the first task **synchronously** (`RunTaskJob` via `dispatchNow`; it performs the task's Wings action and
  queues the following tasks with their `time_offset` delays). Exceptions propagate (500 etc.).
Activity `server:schedule.execute` `{name}`. Response **202**, body `[]`.

#### `DELETE /schedules/{schedule}`
`ScheduleController.php:175-182`; `DeleteScheduleRequest` (`schedule.delete`). Delete row (tasks via DB cascade), then
activity `server:schedule.delete` `{name}` (subjects server + schedule). Response **204**.

#### `POST /schedules/{schedule}/tasks`
`ScheduleTaskController.php:46-95`; `StoreTaskRequest` (`schedule.update`). Rules: `action` `required|in:command,power,backup,delete_files`;
`payload` `required_unless:action,backup|string|nullable`; `time_offset` `required|numeric|min:0|max:900`;
`sequence_id` `sometimes|required|numeric|min:1`; `continue_on_failure` `sometimes|required|boolean`.
1. `schedule.tasks.count >= PANEL_PER_SCHEDULE_TASK_LIMIT` (10) → `ServiceLimitExceededException` **400**
   `Schedules may not have more than <limit> tasks associated with them. Creating this task would put this schedule over the limit.`
2. `server.backup_limit === 0 && action == 'backup'` → `HttpForbiddenException` **403**
   `A backup task cannot be created when the server's backup limit is set to 0.`
3. Transaction: `next = max(sequence_id) + 1` (1 if none); `req = (int) sequence_id` (default `next`, clamped ≥ 1); if `req < next`,
   increment `sequence_id` of tasks with `sequence_id >= req` and use `req`; insert task `{schedule_id, sequence_id, action,
   payload = input ?? '', time_offset, continue_on_failure = boolean(input)}` (`is_queued` false). Touches `schedules.updated_at`.
4. Activity `server:task.create` `{name: schedule.name, action, payload}`, subjects server + schedule + task.
Response 200 item `schedule_task` (§9.8).

#### `POST /schedules/{schedule}/tasks/{task}`
`ScheduleTaskController.php:106-149`; `StoreTaskRequest` (`schedule.update`). 404 (`NotFoundHttpException`, empty detail) if
task/schedule/server mismatch; same backup-limit 403. Transaction: `seq = (int) input sequence_id ?? task.sequence_id`, clamped ≥ 1;
moving up: increment tasks in `[seq, old)`; moving down: decrement tasks in `(old, seq]`; update task `{sequence_id, action,
payload ?? '', time_offset, continue_on_failure}`. Activity `server:task.update` `{name, action, payload}`. Response 200 refreshed item.

#### `DELETE /schedules/{schedule}/tasks/{task}`
`ScheduleTaskController.php:159-173`; `ClientApiRequest` (no FormRequest permission). 404 (empty detail) on mismatch;
`HAS(schedule.delete)` else `HttpForbiddenException` **403** `You do not have permission to perform this action.`
Decrement `sequence_id` of later tasks, delete task. Activity `server:task.delete` `{name}`. Response **204**.

### 8.7 Server: network allocations

#### `GET /network/allocations`
`NetworkAllocationController.php:41-46`; `GetNetworkRequest` (`allocation.read`). List of `allocation` (§9.2), all
allocations of the server (no ordering).

#### `POST /network/allocations` (throttle `AllocationCreate`)
`NetworkAllocationController.php:108-125`; `NewAllocationRequest` (`allocation.create`), no body.
Activity `server:allocation.create` wraps (transaction):
1. `server.allocations.count >= server.allocation_limit` → `DisplayException` **400**
   `Cannot assign additional allocations to this server: limit has been reached.` (PHP: `n >= null` is always true, so a
   null limit always fails; 0 too).
2. `FindAssignableAllocationService::handle` (`app/Services/Allocations/FindAssignableAllocationService.php`):
   `PANEL_CLIENT_ALLOCATIONS_ENABLED` false → `AutoAllocationNotEnabledException` 400 `Server auto-allocation is not enabled for this instance.`;
   no primary allocation → 400 `A primary allocation is required before adding more`; range from
   `PANEL_CLIENT_ALLOCATIONS_RANGE_START/END` (must be integerish, else webmozart `InvalidArgumentException` 500);
   pick random unassigned allocation on the node with the primary's IP and port in range (`FOR UPDATE`); if none and
   `CREATE_NEW` is false → `NoAutoAllocationSpaceAvailableException` 400 `Cannot assign additional allocation: no more space available on node.`;
   else create a new allocation on a random free port (same 400 if no range/ports); set `server_id`.
3. Activity properties `{allocation: "<alias or ip>:<port>"}` (IPv6 bracketed), subjects server + allocation.
Response 200 item `allocation`.

#### `POST /network/allocations/{allocation}` — notes
`NetworkAllocationController.php:57-73`; `UpdateAllocationRequest` (`allocation.update`). Rule `notes`:
`nullable|string|max:256|present`. `forceFill(notes)->save()`. If changed: activity `server:allocation.notes`
`{allocation: address, old, new}`. Response 200 item `allocation`.

#### `POST /network/allocations/{allocation}/primary`
`NetworkAllocationController.php:84-97`; `SetPrimaryAllocationRequest` (`allocation.update`, no rules).
`servers.allocation_id = allocation.id`; activity `server:allocation.primary` `{allocation: address}`. Response 200 item
`allocation` (`is_default` true).

#### `DELETE /network/allocations/{allocation}`
`NetworkAllocationController.php:134-148`; `DeleteAllocationRequest` (`allocation.delete`).
1. `empty(server.allocation_limit)` (null or 0) → `DisplayException` 400 `You cannot delete allocations for this server: no allocation limit is set.`
2. `Server::releaseAllocations([id], keepOne: true)` (`app/Models/Server.php:258-284`), transaction: lock server row; if no
   other allocation on the server's node → `DisplayException` 400 `You cannot delete the last allocation for this server.`;
   update allocation `server_id=null, notes=null, is_locked=false, show_port=true`; if it was the primary, the
   lowest-id remaining allocation on the node becomes primary and activity `server:allocation.primary` `{allocation}`
   (subjects server + new primary) is logged.
3. Activity `server:allocation.delete` `{allocation}`. Response **204**.

### 8.8 Server: subusers (`/users`)

`SubuserRequest::authorize` (`app/Http/Requests/Api/Client/Servers/Subusers/SubuserRequest.php:23-71`), after the
permission check: a route `{user}` equal to the requester → false (403); for `POST` requests containing `permissions`,
unless `ADMIN_UPDATE` or owner, every requested permission must be in the requester's §5.6 list, else
`HttpForbiddenException` **403** `Cannot assign permissions to a subuser that your account does not actively possess.`
"Cleaned" permissions (`SubuserController::getCleanedPermissions`, `:143-151`): input ∩ `allPermissionKeys()`, then
`websocket.connect` appended, unique. Services further keep only permissions the requester `HAS` (plus
`websocket.connect`) and **sort** them alphabetically.

#### `GET /users`
`SubuserController.php:48-53`; `GetSubuserRequest` (`user.read`). List of `server_subuser` (§9.4).

#### `POST /users` (throttle `SubuserCreate`)
`SubuserController.php:83-98`; `StoreSubuserRequest` (`user.create`). Rules: `email` `required|email|between:1,255`;
`permissions` `required|array`; `permissions.*` `string`.
`SubuserCreationService::handle` (`app/Services/Subusers/SubuserCreationService.php`), transaction:
* user by email (exact match) not found → create user (`UserCreationService`): `username = <local part> + random(3)`,
  random password, `uuid`, then a password-reset token and (if `send_account_created_notification`) `AccountCreated`
  notification;
* found and is owner → `UserIsServerOwnerException` 400 `You cannot add the server owner as a subuser for this server.`;
* already a subuser → `ServerSubuserExistsException` 400 `A user with that email address is already assigned as a subuser for this server.`;
* `updateOrCreate` subuser `{user_id, server_id}` with permissions; event `App\Events\Server\SubUserAdded`
  (→ database notification + optional `AddedToServer` mail).
Activity `server:subuser.create` `{email: raw input, permissions: stored list}`, subjects server + user.
Response 200 item `server_subuser`.

#### `GET /users/{user}`
`SubuserController.php:62-69`; `GetSubuserRequest` (`user.read`). Item `server_subuser`.

#### `POST /users/{user}`
`SubuserController.php:109-119`; `UpdateSubuserRequest` (`user.update`). Rules: `permissions` `required|array`, `permissions.*` `string`.
`SubuserUpdateService` (`app/Services/Subusers/SubuserUpdateService.php`): compute cleaned+sorted list; if different from the
stored (sorted) list: in a transaction update permissions, queue `RevokeSftpAccessJob(user.uuid, server)` (→ Wings
`POST /api/deauthorize-user {"user": uuid, "servers": [server.uuid]}`) and log `server:subuser.update`
`{email, old: [...sorted], new: [...], revoked: true}` (subjects server + user). No log if unchanged.
Response 200 refreshed item.

#### `DELETE /users/{user}`
`SubuserController.php:126-134`; `DeleteSubuserRequest` (`user.delete`). `SubuserDeletionService`
(`app/Services/Subusers/SubuserDeletionService.php`): activity `server:subuser.delete` `{email, revoked: true}` (subjects server + user)
wrapping: delete subuser, event `SubUserRemoved` (→ notification + optional mail), queue `RevokeSftpAccessJob(user.uuid, server)`.
Response **204**.

### 8.9 Server: backups

Backup routes use plain `Request` objects with explicit `HAS` checks (→ `AuthorizationException` 403
`This action is unauthorized.`) except where a FormRequest is listed.

#### `GET /backups`
`BackupController.php:49-61`. `HAS(backup.read)`. `per_page`: `min(query('per_page') ?? 20, 50)`; **no** query
appending in links. Response list of `backup` (§9.5) with meta (in order) `backup_count` (=count of backups with
`completed_at IS NULL OR is_successful = true`) then `pagination`.

#### `POST /backups`
`BackupController.php:72-103`; `StoreBackupRequest` (`backup.create`). Rules: `name` `nullable|string|max:255`;
`is_locked` `nullable|boolean`; `ignored` `nullable|string`.
1. `ignored_files` = `explode("\n", ignored ?? '')` minus empty strings (no trimming of `\r`).
2. `is_locked` applied only if `HAS(backup.delete)`, else false.
3. Activity `server:backup.start` wraps a transaction that locks the server row and calls `InitiateBackupService::handle`
   (`app/Services/Backups/InitiateBackupService.php`):
   * throttle: backups (non-failed) created in the last `BACKUP_THROTTLE_PERIOD` s (600) `>= BACKUP_THROTTLE_LIMIT` (2) →
     `TooManyRequestsHttpException` **429** `Only <limit> backups may be generated within a <period> second span of time.`
     with `Retry-After`;
   * `!backup_limit` (null/0) or non-failed count `>= backup_limit` → `TooManyBackupsException` **400**
     `Cannot create a new backup, this server has reached its limit of <n> backups.` (`%d` of null = 0);
   * backup host = first host linked to the node, else first host linked to no node (`firstOrFail` → 404 if none);
     unknown adapter → plain `Exception` 500;
   * insert `backups` `{server_id, uuid (v4), name = trim(name) ?: "Backup at <Y-m-d H:i:s>", ignored_files, backup_host_id, is_locked, is_scheduled=false}`;
   * adapter `createBackup`: Wings `POST /api/servers/{uuid}/backup {"adapter": "<wings|s3>", "uuid": "<backup uuid>", "ignore": "<ignored joined by \n>"}`.
   * activity properties `{name, locked: boolean(input is_locked)}` (raw request flag), subjects server + backup.
Response 200 item `backup`.

#### `GET /backups/{backup}`
`BackupController.php:139-146`. `HAS(backup.read)`. Item `backup`.

#### `GET /backups/{backup}/download`
`BackupController.php:180-197`. `HAS(backup.download)`. Unknown adapter → `BadRequestHttpException` 400
`The backup requested references an unknown disk driver type and cannot be downloaded.`
URL: Wings adapter → `<node>/download/backup?token=<jwt>` (JWT exp +15 min, claims `backup_uuid`, `server_uuid`,
`user_uuid`, `scope: "backup-download"`, `unique_id`; `app/Extensions/BackupAdapter/Schemas/WingsBackupSchema.php`);
S3 adapter → presigned `GetObject` URL for `<server_uuid>/<backup_uuid>.tar.gz`, 5 min.
Activity `server:backup.download` `{name}`. Response `{"object":"signed_url","attributes":{"url":"..."}}`.
(No check that the backup completed.)

#### `PUT /backups/{backup}/rename`
`BackupController.php:209-226`; `RenameBackupRequest` — permission **`backup.delete`**; rule `name` `required|string|max:255`.
Update name; if changed activity `server:backup.rename` `{old_name, new_name}`. Response 200 item.

#### `POST /backups/{backup}/lock`
`BackupController.php:115-128`. `HAS(backup.delete)`. Toggles `is_locked`; activity `server:backup.unlock` (if it was
locked) or `server:backup.lock` `{name}`. Response 200 item (new state).

#### `POST /backups/{backup}/restore` (throttle `BackupRestore`)
`BackupController.php:241-264`; `RestoreBackupRequest` (`backup.restore`); rule `truncate` `required|boolean`.
1. `server.status` not null → `BadRequestHttpException` 400 `This server is not currently in a state that allows for a backup to be restored.`
2. `!is_successful && completed_at IS NULL` → 400 `This backup cannot be restored at this time: not completed or failed.`
   (completed-but-failed backups pass this check).
3. Activity `server:backup.restore` `{name, truncate: raw input}` wraps a transaction: download URL (as above),
   `servers.status = 'restoring_backup'`, Wings `POST /api/servers/{uuid}/backup/{backup_uuid}/restore
   {"adapter": "<schema>", "truncate_directory": bool, "download_url": "<url or ''>"}`.
Response **204**.

#### `DELETE /backups/{backup}`
`BackupController.php:156-168`. `HAS(backup.delete)`. `DeleteBackupService` (`app/Services/Backups/DeleteBackupService.php`):
locked AND successful AND completed → `BackupLockedException` 400 `Cannot delete a backup that is marked as locked.`;
transaction: adapter delete (Wings `DELETE /api/servers/{uuid}/backup/{backup_uuid}`, Wings 404 ignored; S3 `DeleteObject`);
errors ignored for unsuccessful backups; delete row. Activity `server:backup.delete` `{name, failed: !is_successful}`.
Response **204**.

### 8.10 Server: startup

#### `GET /startup`
`StartupController.php:37-51`; `GetStartupRequest` (`startup.read`).
Response list of `egg_variable` (§9.3) for `user_viewable` variables ordered by `egg_variables.sort`, meta (in order):
```json
"meta": {"startup_command": "<rendered>", "docker_images": {"<display name>": "<image>", ...}, "raw_startup_command": "<server.startup>"}
```
`startup_command` = `StartupCommandService::handle(server)` (`app/Services/Servers/StartupCommandService.php`): replaces
`{{SERVER_MEMORY}}`, `{{SERVER_IP}}` (primary ip or `127.0.0.1`), `{{SERVER_PORT}}` (or `0`) and `{{ENV}}` for every egg
variable with `server_value ?? default_value` if viewable, else `[hidden]`. `docker_images` is the egg's map (JSON object).

#### `PUT /startup/variable`
`StartupController.php:63-106`; `UpdateStartupVariableRequest` (`startup.update`); rules `key` `required|string`, `value` `present`.
1. Variable by `env_variable = key` (with server value joined); missing or not `user_viewable` → `BadRequestHttpException` 400
   `The environment variable you are trying to edit does not exist.`; not `user_editable` → 400
   `The environment variable you are trying to edit is read-only.`
2. Validate `value` against the variable's `rules` array → 422 with `source_field: "value"`.
3. `server_variables` `updateOrCreate({server_id, variable_id}, {variable_value: value ?? ''})`.
4. If `original server_value !== raw value` (strict, so non-string JSON values always count as changed): activity
   `server:startup.edit` `{variable, old, new: value ?? ''}`, subjects server + egg variable.
Response 200 item `egg_variable` (server_value = raw input, coerced to string) with meta `{startup_command, raw_startup_command}`.

### 8.11 Server: settings

#### `POST /settings/rename`
`SettingsController.php:36-50`; `RenameServerRequest` (`settings.rename`); `name` `required|string|min:1|max:255`.
Update; if changed activity `server:settings.rename` `{old, new}`. **204**.

#### `POST /settings/description`
`SettingsController.php:58-75`; `DescriptionServerRequest` (`settings.description`); `description` `string|nullable`.
If `PANEL_EDITABLE_SERVER_DESCRIPTIONS` is false → **403** with body `[]` (not the error envelope). Else
`description = input ?? ''`; if changed activity `server:settings.description` `{old, new: raw input}`. **204**.

#### `POST /settings/reinstall`
`SettingsController.php:84-91`; `ReinstallServerRequest` (`settings.reinstall`). `ReinstallServerService`
(`app/Services/Servers/ReinstallServerService.php`), transaction: `status = 'installing'`, Wings
`POST /api/servers/{uuid}/reinstall`. Activity `server:reinstall`. Response **202**, body `[]`.

#### `PUT /settings/docker-image`
`SettingsController.php:100-114`; `SetDockerImageRequest` (`startup.docker-image`); `docker_image` `required|string|in:<values of egg.docker_images>`.
If the current `server.image` is not one of the egg images → `BadRequestHttpException` 400
`This server's Docker image has been manually set by an administrator and cannot be updated.` Update image; if changed activity
`server:startup.image` `{old, new}`. **204**.

Server saves also trigger `Server::booted` `saved` hook: if the owner is also a subuser, that subuser is removed
via `SubuserDeletionService` (`app/Models/Server.php:219-228`).

---

## 9. Resources ("transformers")

All in `app/Data/Api/Client/`. Base `app/Data/Api/ApiResource.php` (spatie/laravel-data `Data`). Serialisation:
properties in declaration order (`vendor/spatie/laravel-data/src/Resolvers/TransformedDataResolver.php:55-90`), `null` kept,
`Optional` values **omitted**, `#[MapOutputName]` renames, backed enums → value, arrays passed through. Resource name =
`object` value. Typed constructor properties coerce scalars (non-strict mode) and throw `TypeError` (500) for
incompatible values.

### 9.1 `ServerData` — `server`
`ServerData.php`. availableIncludes `['egg','subusers']`, defaultIncludes `['allocations','variables']`.

| # | key | type | source |
|---|---|---|---|
| 1 | `server_owner` | bool | `request.user.id === server.owner_id` |
| 2 | `identifier` | string | `uuid_short` |
| 3 | `internal_id` | int | `id` |
| 4 | `uuid` | string | |
| 5 | `name` | string | |
| 6 | `node` | string | node name |
| 7 | `is_node_under_maintenance` | bool | `node.maintenance_mode` |
| 8 | `sftp_details` | `{ip: string (node.fqdn), alias: ?string (daemon_sftp_alias), port: int (daemon_sftp)}` | |
| 9 | `description` | ?string | **omitted** when `PANEL_EDITABLE_SERVER_DESCRIPTIONS=false` |
| 10 | `limits` | `{memory:int, swap:int, disk:int, io:int, cpu:int, threads:?string, oom_disabled:bool (= !oom_killer, deprecated), oom_killer:bool}` | |
| 11 | `invocation` | string | `StartupCommandService::handle(server, hideAllValues: !HAS(startup.read))` — all values `[hidden]` without the permission |
| 12 | `docker_image` | string | `image` |
| 13 | `egg_features` | ?string[] | egg `features`, or the `config_from` parent's features when own features are null (`app/Models/Egg.php:295-302`) |
| 14 | `feature_limits` | `{databases:?int, allocations:?int, backups:?int}` | `database_limit`, `allocation_limit`, `backup_limit` |
| 15 | `status` | ?string | `installing`, `install_failed`, `reinstall_failed`, `suspended`, `restoring_backup`, or null |
| 16 | `is_suspended` | bool | deprecated |
| 17 | `is_installing` | bool | `!isInstalled()` (true for installing and both failed states), deprecated |
| 18 | `is_transferring` | bool | unfinished transfer exists |
| 19 | `relationships` | object | see below |

Includes:
* `allocations` (default): if `GATE(allocation.read)` → list of all server allocations (§9.2); otherwise a list
  containing only a **clone of the primary allocation with `notes = null`** (a server without a primary allocation
  makes this throw → 500).
* `variables` (default): if `GATE(startup.read)` → list of `egg_variable` for viewable variables (relation order, not
  sorted); else `null_resource`.
* `egg` (on request): item `egg` (§9.15).
* `subusers` (on request): if `GATE(user.read)` → list of `server_subuser`; else `null_resource`.

### 9.2 `AllocationData` — `allocation`
`id:int, ip:string, ip_alias:?string, port:int, notes:?string, is_default:bool` (`server.allocation_id === allocation.id`,
using the allocation's (re)loaded `server` relation).

### 9.3 `EggVariableData` — `egg_variable`
`name:string, description:?string, env_variable:string, default_value:?string, server_value:?string, is_editable:bool (user_editable), rules:string (rules array joined with "|")`.
Throws `BadMethodCallException` (500) if the variable is not `user_viewable`. `server_value` comes from the
`server_variables` left join (null when no row).

### 9.4 `SubuserData` — `server_subuser`
`uuid, username, email, language:?string, image, admin:bool, root_admin:bool, 2fa_enabled:bool, created_at, updated_at, permissions:string[]` —
first ten fields identical to `UserData` of the subuser's user (§9.10), `permissions` = stored list.

### 9.5 `BackupData` — `backup`
`uuid:string, is_successful:bool, is_locked:bool, is_scheduled:bool, name:string, ignored_files:string[], checksum:?string, bytes:int, created_at:string, completed_at:?string`.

### 9.6 `DatabaseData` — `server_database`
`id:int, host:{address:string (database_hosts.host), port:int}, name:string (databases.database), username:string, connections_from:?string (remote), max_connections:?int`.
availableIncludes `['password']`: if `GATE(database.view-password)` (server = `database.server`) →
`{"object":"database_password","attributes":{"password":"<decrypted>"}}`, else `null_resource`.

### 9.7 `ScheduleData` — `server_schedule`
`id:int, name:string, cron:{day_of_week, day_of_month, month, hour, minute} (strings, this key order), is_active:bool, is_processing:bool, only_when_online:bool, last_run_at:?string, next_run_at:?string, created_at:string, updated_at:string`.
available & default include `tasks`: list of `schedule_task` (relation order, no explicit `ORDER BY`). `timezone` is not exposed.

### 9.8 `TaskData` — `schedule_task`
`id:int, sequence_id:int, action:string, payload:?string, time_offset:int, is_queued:bool, continue_on_failure:bool, created_at:string, updated_at:string`.

### 9.9 `ActivityLogData` — `activity_log`
`ActivityLogData.php:19-67`, availableIncludes `['actor']`.

| key | type | source |
|---|---|---|
| `id` | string | `sha1((string) activity_logs.id)` |
| `event` | string | |
| `is_api` | bool | `api_key_id IS NOT NULL` |
| `ip` | ?string | shown only if the actor is the requesting user or the requester has the Gate ability `seeIps activityLog` (root admin or role permission); else null |
| `description` | ?string | |
| `properties` | object or `[]` | `ActivityLog::wrapProperties()` (below) |
| `has_additional_metadata` | bool | `ActivityLog::hasAdditionalMetadata()` (below) |
| `timestamp` | string | ATOM |

`wrapProperties` (`app/Models/ActivityLog.php:224-259`): empty → `[]` (JSON **array**). Otherwise for each stored key in order:
* scalar/null → string with HTML tags stripped (`true`→`"1"`, `false`/`null`→`""`, numbers → decimal string); for key
  `directory` the value is first normalised to `str_replace('//','/', '/' . trim(v,'/') . '/')`;
* array whose first element is an array (legacy) → `"<key>_count": count`;
* other array → `"<key>": <first element stringified>, "<key>_count": count`.
Then if exactly one `*_count` key exists it is removed and `"count": <value>` is appended at the end.

`hasAdditionalMetadata` (`:267-288`): false if no properties; else translate `activity.<event with ':' → '.'>`
(`lang/en/activity.php`, `trans_choice` with `count` or 1), collect placeholders matching
`/:(?<key>[\w.-]+\w)(?:[^\w:]?|$)/`, and return true iff any **stored** property key is not in
placeholders ∪ `{ip, useragent, using_sftp}`.

Include `actor`: item `user` (§9.10) when the actor is a `User`, else `null_resource`.

### 9.10 `UserData` — `user`
`uuid:string, username:string, email:string, language:?string, image:string ("https://gravatar.com/avatar/" + md5(lowercase email), deprecated), admin:bool (= root admin, deprecated), root_admin:bool, 2fa_enabled:bool (mfa_app_secret filled; email-2FA not reflected), created_at:string, updated_at:string` (§7.6).

### 9.11 `ApiKeyData` — `api_key`
`identifier:string, description:?string (memo), allowed_ips:string[], last_used_at:?string, created_at:string`.

### 9.12 `UserSSHKeyData` — `ssh_key`
`name:string, fingerprint:string, public_key:string, created_at:string`.

### 9.13 `FileObjectData` — `file_object`
Built from a Wings file object (array):
`name:?string (name), mode:?string (mode), mode_bits:any (mode_bits, passed through), size:?int (size), is_file:bool (file, default true), is_symlink:bool (symlink, default false), mimetype:string (mime, default "application/octet-stream"), created_at:string (Carbon::parse(created)), modified_at:string (Carbon::parse(modified))`.

### 9.14 `StatsData` — `stats`
`current_state:string (state, default "stopped"; "missing" when Wings unreachable), is_suspended:bool (is_suspended, default false), resources:{memory_bytes, cpu_absolute, disk_bytes, network_rx_bytes (utilization.network.rx_bytes), network_tx_bytes, uptime}` — each resource defaults to `0`, values passed through from Wings JSON (ints stay ints, floats like `cpu_absolute` keep PHP float formatting). A Wings `state: null` would throw `TypeError` → 500.

### 9.15 `EggData` — `egg`
`uuid:string, name:string`.

### 9.16 Non-envelope bodies
* `system_permissions` (§8.1), `signed_url` (`{"object":"signed_url","attributes":{"url":string}}`), websocket `{"data":{token,socket}}`.

---

## 10. Activity event catalogue (client API)

| Event | Properties (order) | Extra subjects |
|---|---|---|
| `auth:ip-blocked` | identifier | user, api key (actor = user) |
| `user:account.username-changed` | old, new | — |
| `user:account.email-changed` | old, new | — |
| `user:account.password-changed` | — | — |
| `user:api-key.create` | identifier | api key |
| `user:api-key.delete` | identifier | — |
| `user:ssh-key.create` | fingerprint | ssh key |
| `user:ssh-key.delete` | fingerprint | ssh key |
| `server:console.command` | command | — |
| `server:power.{start,stop,restart,kill}` | — | — |
| `server:database.create` | name | database |
| `server:database.rotate-password` | name | database |
| `server:database.delete` | name | database |
| `server:file.read` / `server:file.download` / `server:file.write` | file | — |
| `server:file.create-directory` | name, directory | — |
| `server:file.rename` | directory, files, to, from | — |
| `server:file.copy` | file | — |
| `server:file.compress` | name, directory, files | — |
| `server:file.decompress` | directory, file | — |
| `server:file.delete` | directory, files | — |
| `server:file.pull` | directory, url | — |
| `server:schedule.create` | name | schedule |
| `server:schedule.update` | name, active | schedule |
| `server:schedule.execute` | name | schedule |
| `server:schedule.delete` | name | schedule |
| `server:task.create` / `server:task.update` | name, action, payload | schedule, task |
| `server:task.delete` | name | schedule, task |
| `server:allocation.create` | allocation | allocation |
| `server:allocation.notes` | allocation, old, new | allocation |
| `server:allocation.primary` | allocation | allocation |
| `server:allocation.delete` | allocation | allocation |
| `server:subuser.create` | email, permissions | user |
| `server:subuser.update` | email, old, new, revoked | user |
| `server:subuser.delete` | email, revoked | user |
| `server:backup.start` | name, locked | backup |
| `server:backup.lock` / `server:backup.unlock` | name | backup |
| `server:backup.download` | name | backup |
| `server:backup.rename` | old_name, new_name | backup |
| `server:backup.restore` | name, truncate | backup |
| `server:backup.delete` | name, failed | backup |
| `server:startup.edit` | variable, old, new | egg variable |
| `server:startup.image` | old, new | — |
| `server:settings.rename` | old, new | — |
| `server:settings.description` | old, new | — |
| `server:reinstall` | — | — |

Default subject (server or user) is always attached as well (§7.7).

---

## 11. Open questions / uncertain framework behaviour

1. **Non-JSON clients.** Without `Accept: application/json` errors become 302 redirects or HTML pages (Filament/Laravel
   error views). Byte-compatibility of that path is out of scope here; decide whether the Rust port replicates it or always
   returns JSON (difftests should always send `Accept: application/json`).
2. **Unordered lists.** Server list (no sort), activity (no `sort`), backups, databases, allocations, subusers, schedules,
   tasks, api/ssh keys are fetched without `ORDER BY`; MySQL/MariaDB/PostgreSQL/SQLite may return different orders.
   Difftests must sort or the Rust port must mimic each engine's natural order (usually PK order for simple scans).
3. **DB-driver typing.** Columns without casts (`database_hosts.port`, `nodes.daemon_sftp`, `allocations.id`, `servers.id`,
   `servers.threads`, …) are emitted with whatever type PDO returns (PHP 8.1+ returns native ints for MySQL/SQLite/PG
   integer columns). Verify per engine in difftests.
4. **Non-numeric route ids** (`/schedules/abc`): MySQL/SQLite coerce → 404; PostgreSQL raises `invalid input syntax` →
   `QueryException` 500. Not verified.
5. **`user_permissions` / websocket `permissions` as JSON object** when `array_unique` removes a non-final duplicate
   (§5.6). Only reachable for non-root admins who are also subusers with plugin permissions sorting after `websocket.connect`.
6. **Debug-mode traces** (§6.5) cannot be reproduced byte-for-byte; treat `APP_DEBUG=true` as unsupported for difftests.
7. **Validation messages** for nested attributes and custom rule objects rely on Laravel's attribute humanisation and
   message selection (`size`/`between` variants by type). The common cases are listed in §6.4; exotic combinations
   (numeric strings vs `between` on `username`, arrays on string rules) were not exhaustively verified.
8. **PHP deprecations/TypeErrors as behaviour**: `trim(null)`, `explode(',', null)`, `str_replace(..., null)` are
   deprecation-only (logged, not thrown) in PHP 8.5; `TypeError`s noted in the text (create-folder with null root,
   SSH key with missing `public_key`, server without primary allocation, non-string filter values) produce generic 500s.
   Whether the Rust port must reproduce 500s or may return 422 needs a decision.
9. **`CommandController` 502 branch** is dead code (Laravel HTTP client never throws `BadResponseException`); real Wings
   errors are ignored and the endpoint returns 204 — confirm by difftest against a mocked Wings.
10. **`throttle` (bare) key sharing**: the `PUT /account/email` limiter key is `sha1(user_id)` with no prefix and would be
    shared with any other bare `throttle` route (none in the client API; check web/auth routes).
11. **Activity log failure handling** depends on `APP_ENV` (`production` swallows save errors).
12. **Plugins** can add subuser permissions, routes and include-affecting behaviour; not covered here (see `plugins.md`).
13. **Wings payload pass-through**: `chmod` `mode`, `pull` `use_header`/`foreground` are forwarded with the JSON type the
    client sent (string vs number vs bool). The Rust port must not normalise them.
14. **`per_page` loose comparisons** (§7.4) — negative/non-numeric values are reproduced from PHP 8 comparison rules; low
    priority but cheap to mimic.
15. **CSRF/origin** for cookie-authenticated requests uses Laravel 13's `PreventRequestForgery` (Sec-Fetch-Site support);
    `originOnly`/`allowSameSite` statics are left at defaults (not changed by the app) — assumed false.
