# Panel ⇄ Wings contract (Remote API, daemon calls, JWTs, node config)

Status: reverse-engineered from the PHP panel (Pelican, Laravel 13.35.0, PHP 8.5) and cross-checked against the Wings Go sources. **This document is the contract** for the Rust rewrite and for the differential tests. When it says "PHP does X", the Rust panel has to do X, quirks included, unless a quirk is explicitly called out as "fix allowed".

Citation roots (all paths relative to `/Users/tazik/Desktop/Code/claude/`):

| Prefix | Meaning |
|---|---|
| `panel/…` | `pelican/panel/…`, the original PHP panel (read-only) |
| `vendor/…` | `pelican/.ref/vendor/…`, the exact vendored framework sources (Laravel 13.35.0 `laravel/framework/src/Illuminate/Foundation/Application.php:49`, lcobucci/jwt 5.6.0, symfony/yaml 7.4.20, guzzle 8.2.0, phpseclib 4.0.2) |
| `wings/…` | `_ref/wings/…`, the Wings (Go) checkout. It shows what Wings sends and expects. Wings must keep working **unchanged**. |

The notation `file:N` or `file:N-M` gives line numbers.

---

## 0. TL;DR: compatibility-critical facts

1. Remote API auth is `Authorization: Bearer <token_id>.<token>`. The id is looked up in the DB, which is **case-insensitive on MySQL collations**. The secret is compared with `hash_equals` against the **decrypted** `nodes.daemon_token` (Laravel `encrypted` cast, AES-256-CBC). An unknown token id gives **404**, not 401 or 403 (§1.3).
2. Route-model binding (`SubstituteBindings`) runs **before** `DaemonAuthenticate`. An unknown server UUID gives 404 even with no or invalid credentials (§1.2).
3. `{server:uuid}` binding matches **`uuid` OR `uuid_short`** and is **not scoped to the node**. Each controller does its own node check and returns 403 (§1.4).
4. Errors are JSON:API-ish `{"errors":[{code,status(string!),detail}]}`. They are **pretty-printed** (`JSON_PRETTY_PRINT|JSON_UNESCAPED_SLASHES`) for HttpExceptions and **compact** (escaped slashes) for validation errors and `DisplayException`s (§1.5).
5. `/activity` **discards the Wings timestamp**. The `ActivityLog::creating` hook overwrites `timestamp` with `now()` (§2.4).
6. SFTP throttle key = `lower(<server part of username>) + "|" + <Wings node IP>`. It is per-server/per-node, not per-user: 5 failures in 60 s give a 429 for everyone on that server (§2.1).
7. `egg.features` in the server config is built from **all registered feature schemas when `eggs.features` IS NULL**. It is `[]` (JSON array, not `{}`) when the egg has `features: []` (§3.4).
8. The `GET /servers` list goes through Laravel's `JsonResource::filter()`, which **re-indexes every nested array whose keys are all numeric**. The single-server endpoint does not. Byte-level output can therefore differ between the two endpoints for weird data (§3.9).
9. JWTs: HS256, key = decrypted node token, custom claims formatter. `aud` is a **JSON array**, `iat/nbf/exp` are integer seconds, `nbf = now-300`, there is a `jti` header, `scope` is a space-joined claim and `unique_id` is a 16-char random string (§5).
10. The panel validates **Wings' response `User-Agent`** (`Pelican Wings/vX.Y.Z (id:<token_id>)`), but only on **non-2xx** responses (plus `/api/system`). A mismatch becomes `ConnectionException` (§4.2).
11. Wings failures surface to panel API clients as **HTTP 500** (`ConnectionException` / `RequestException`). The old Pterodactyl `DaemonConnectionException`/502 mapping **no longer exists** (§4.5).

---

## 1. Transport layer common to `/api/remote/*`

### 1.1 Route registration

- Routes file: `panel/routes/api-remote.php:1-28`. It is mounted with `Route::middleware('daemon')->prefix('/api/remote')->scopeBindings()` (`panel/app/Providers/RouteServiceProvider.php:54-57`).
- Middleware group `daemon` = `[SubstituteBindings, DaemonAuthenticate]` (`panel/bootstrap/app.php:55-58`). The global priority list is replaced by `[SubstituteBindings]` (`panel/bootstrap/app.php:68-70`), so bindings always run first.
- The remote routes are **not** in the `api` group (`panel/bootstrap/app.php:37-43`). That means **no** `auth:sanctum`, **no** `IsValidJson`, **no** `TrackAPIKey`, **no** `AuthenticateIPAccess` and **no rate limiting** (the `throttle:*` middleware is only on application/client groups, `RouteServiceProvider.php:43-51`).
- Global middleware still applies (Laravel default list, `vendor/laravel/framework/src/Illuminate/Foundation/Configuration/Middleware.php:451-473`):
  - `ValidatePathEncoding`: malformed UTF-8 path gives 400 `MalformedUrlException`, detail `Malformed URL.`.
  - `InvokeDeferredCallbacks`.
  - `TrustProxies`: `TRUSTED_PROXIES` env, `panel/config/trustedproxy.php`. It decides what `$request->ip()` returns, which matters for the SFTP throttle key and activity IPs.
  - `HandleCors`.
  - `PreventRequestsDuringMaintenance`: `php artisan down` gives **503** `{"errors":[{"code":"HttpException","status":"503","detail":"Service Unavailable"}]}` (`vendor/…/Foundation/Http/Middleware/PreventRequestsDuringMaintenance.php:102-107`).
  - `ValidatePostSize`.
  - `TrimStrings`: trims every string input except keys `current_password`, `password`, `password_confirmation`; `vendor/…/Foundation/Http/Middleware/TrimStrings.php:16-20`. The skip regex in `RouteServiceProvider.php:15,26-28` only concerns client file routes.
  - `ConvertEmptyStringsToNull`: `""` becomes `null` recursively, in query and in JSON body.
- Final route table (Laravel strips the trailing slash, and every GET route also answers HEAD):

| # | Method | Path | Controller |
|---|---|---|---|
| R1 | POST | `/api/remote/sftp/auth` | `SftpAuthenticationController` (invokable) |
| R2 | GET | `/api/remote/servers` | `ServerDetailsController@list` |
| R3 | POST | `/api/remote/servers/reset` | `ServerDetailsController@resetState` |
| R4 | POST | `/api/remote/activity` | `ActivityProcessingController` |
| R5 | GET | `/api/remote/servers/{server:uuid}` | `ServerDetailsController@__invoke` |
| R6 | GET | `/api/remote/servers/{server:uuid}/install` | `ServerInstallController@index` |
| R7 | POST | `/api/remote/servers/{server:uuid}/install` | `ServerInstallController@store` |
| R8 | POST | `/api/remote/servers/{server:uuid}/transfer/failure` | `ServerTransferController@failure` |
| R9 | POST | `/api/remote/servers/{server:uuid}/transfer/success` | `ServerTransferController@success` |
| R10 | POST | `/api/remote/servers/{server:uuid}/container/status` | `ServerContainersController@status` |
| R11 | GET | `/api/remote/backups/{backup}` | `BackupRemoteUploadController` (invokable) |
| R12 | POST | `/api/remote/backups/{backup}` | `BackupStatusController@index` |
| R13 | POST | `/api/remote/backups/{backup}/restore` | `BackupStatusController@restore` |

Notes on routing:

- `GET /api/remote/servers/reset` matches R5 with uuid `reset`. It returns a 404 binding failure.
- Unknown path: 404 `NotFoundHttpException`, detail `The route api/remote/xyz could not be found.` (`vendor/…/Routing/AbstractRouteCollection.php:45`).
- Wrong method: 405 `MethodNotAllowedHttpException`, detail `The POST method is not supported for route api/remote/servers. Supported methods: GET, HEAD.` (`AbstractRouteCollection.php:133`). Both of these happen **before** auth.
- Wings also has a client method for `POST /servers/{uuid}/archive` (`wings/remote/servers.go:104-111`). The panel has **no such route**, so it would get a 404. Wings never calls it in this version, so do **not** add it.

### 1.2 Request pipeline order (observable precedence of errors)

1. Global middleware (above).
2. Route match: 404 or 405.
3. `SubstituteBindings`: resolves `{server:uuid}` (§1.4) and gives **404 before any auth**.
4. `DaemonAuthenticate`: 401, 400, 404 or 403 (§1.3).
5. Controller dependency resolution. FormRequests (`ActivityEventRequest`, `InstallationDataRequest`, `ReportBackupCompleteRequest`, `SftpAuthenticationFormRequest`) validate here and give **422**.
6. Controller body.

Example: an unauthenticated `GET /api/remote/servers/<existing uuid>` gives 401. The same call with `<nonexistent>` gives 404.

### 1.3 `DaemonAuthenticate` (`panel/app/Http/Middleware/Api/Daemon/DaemonAuthenticate.php:28-50`)

1. Skip list `['daemon.configuration']` (`:19-21`, `:30-32`). No remote route has a name, so it never matches. Rust: ignore it.
2. `$bearer = $request->bearerToken()` (`vendor/…/Http/Concerns/InteractsWithInput.php:58-69`). Exact algorithm:
   - `h = header('Authorization', '')`
   - `pos = strripos(h, 'Bearer ')`, i.e. the **last** case-insensitive occurrence of `"Bearer "`.
   - If not found, the token is `null`.
   - Otherwise `t = substr(h, pos+7)`. If `t` contains `,`, `t` = the part before the first `,`.
   - So `"Bearer "` alone yields `""`, which is not null.
3. If the token is `null`, throw `HttpException(401, 'Access to this endpoint must include an Authorization header.', null, ['WWW-Authenticate' => 'Bearer'])` (`:34`). The response gets header `WWW-Authenticate: Bearer`, and `code` is `"HttpException"`.
4. `$parts = explode('.', $bearer)`. If `count($parts) !== 2 || empty($parts[0]) || empty($parts[1])`, throw `BadRequestHttpException('The Authorization header provided was not in a valid format.')`, which is a **400** (`:36-38`). PHP `empty()` also treats `"0"` as empty. The token cannot contain more than one `.`.
5. `Node::query()->where('daemon_token_id', $parts[0])->firstOrFail()` (`:41`). If no row matches, `ModelNotFoundException` is rendered as **404** `{"code":"NotFoundHttpException","status":"404","detail":"The requested resource could not be found on the server."}` (§1.5).
   - Comparison semantics are the DB's: MySQL/MariaDB `utf8mb4_unicode_ci` is case-insensitive and PAD SPACE (trailing spaces ignored). PostgreSQL and SQLite are case-sensitive.
   - Column `daemon_token_id` is `varchar(16)` with a unique index (`panel/database/migrations/2020_04_10_141024_store_node_tokens_as_encrypted_value.php`).
6. `hash_equals((string) $node->daemon_token, $parts[1])` (`:43`).
   - This is **constant-time** for the secret. The DB lookup of the id is not.
   - `daemon_token` is decrypted on access via the `encrypted` cast (`panel/app/Models/Node.php:193`, §8). A decryption failure (rotated `APP_KEY` without `APP_PREVIOUS_KEYS`) throws `DecryptException`, which gives a **500**.
   - A NULL token compares as `""`, which fails.
7. On success: `$request->attributes->set('node', $node)` and continue (`:44-46`).
8. Otherwise: `AccessDeniedHttpException('You are not authorized to access this resource.')`, a **403** (`:49`).

What is **not** checked: node `maintenance_mode` (never checked by the remote API, except indirectly in SFTP auth through `validateCurrentState`, §2.1), IP allow-lists, the Wings User-Agent and the `Accept` header.

What Wings sends on every remote request (`wings/remote/http.go:117-120`):

```
User-Agent: Pelican Wings/v<version> (id:<token_id>)
Accept: application/json
Content-Type: application/json
Authorization: Bearer <token_id>.<token>
```

`Accept: application/json` makes `$request->expectsJson()` true (`vendor/…/Http/Concerns/InteractsWithContentTypes.php:24-38`), so all errors render as JSON.

Wings retries 5xx with exponential backoff (max 30 s). It does **not** retry 4xx (`wings/remote/http.go:156-229`). Wings reads only `errors[0].code/status/detail`, where `status` must be a JSON **string** (`wings/remote/errors.go:14-19`, `wings/remote/http.go:284-305`).

### 1.4 Route-model binding for `{server:uuid}`

- `Server::resolveRouteBinding($value, 'uuid')` runs `where('uuid_short', $value)->orWhere('uuid', $value)->firstOrFail()` (`panel/app/Models/Server.php:458-464`).
  - **Both** the 36-char UUID and the 8-char short UUID are accepted.
  - It is **not** restricted to the authenticated node. `scopeBindings()` has no effect because there is no parent parameter.
  - If nothing matches, the response is a 404 (§1.5).
  - `Server::$with = ['allocation']` is eager loaded (`Server.php:162`).
- `{backup}` routes declare `{backup:uuid}`, but the controllers type-hint `string $backup`. Laravel only binds parameters type-hinted with `UrlRoutable`, so **no implicit binding happens**. The controllers look the backup up themselves with `Backup::where('uuid', $backup)->firstOrFail()`. That lookup is not node-scoped and the controllers then check ownership manually.

What happens for a server that belongs to a different node:

| Endpoint | Behaviour |
|---|---|
| R5 details | 403, unless the server is being transferred and the node is the old or new node |
| R6/R7 install | 403 |
| R8/R9 transfer | **409 if the server has no active transfer (checked first)**, else 403 |
| R10 status | 403 |
| R1 SFTP | 403 (counts as a failed login attempt) |
| R4 activity | the entry is silently dropped |
| R11–R13 backups | 404 if the uuid is unknown, else 403 |

All 403s use `HttpForbiddenException('Requesting node does not have permission to access this server.')` (`panel/app/Exceptions/Http/HttpForbiddenException.php:14-17`). Its `code` is `"HttpForbiddenException"`.

### 1.5 Error response format

The handler is `panel/app/Exceptions/Handler.php`. The Laravel base is `vendor/laravel/framework/src/Illuminate/Foundation/Exceptions/Handler.php`.

Pipeline (`vendor/…/Foundation/Exceptions/Handler.php:705-735`):

1. If any DB transaction is open, it is fully rolled back (`panel/app/Exceptions/Handler.php:237-253`).
2. `mapException`.
3. If the exception has a `render()` method, use it. Only `DisplayException` subclasses have one.
4. `prepareException` (`:769-790`):
   - `ModelNotFoundException` becomes `NotFoundHttpException(msg, previous)`.
   - `AuthorizationException` becomes `AccessDeniedHttpException`.
   - and so on.
5. Renderable callbacks. The panel's `crashedPlugin` callback returns null for JSON requests (`panel/app/Exceptions/Handler.php:118-138`).
6. Then:
   - `ValidationException` goes to `invalidJson()` (compact).
   - `AuthenticationException` goes to `unauthenticated`.
   - Anything else goes to `prepareJsonResponse` (`vendor/…/Handler.php:1118-1125`), which uses `JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES`, the status from `getStatusCode()` (or 500), and the headers from `getHeaders()`.

Array shape. `Handler::exceptionToArray` (`panel/app/Exceptions/Handler.php:308-349`) builds:

```json
{"errors":[{"code":"<class basename>","status":"<status as STRING>","detail":"<message>"}]}
```

- `code` = `class_basename($e)` of the (possibly converted) exception.
- `status` = `strval(getStatusCode())`. If the exception has no such method, the map `{AuthenticationException:401, AuthorizationException:403, ValidationException:422}` is used, otherwise `"500"`.
- `detail`:
  - `getMessage()` if the exception implements `HttpExceptionInterface` or is in the map above.
  - Otherwise the fixed string `An unexpected error was encountered while processing this request, please try again.`
  - If `$e` or `$e->getPrevious()` is a `ModelNotFoundException`, detail is forced to `The requested resource could not be found on the server.`.
- When `APP_DEBUG=true`:
  - `detail` = raw `getMessage()`.
  - Added: `source: {line:int, file:"<path minus base path>"}`.
  - Added: `meta: {trace:[…frames without args…], previous:[…]}`.
  - Key order: `code, status, detail, source, meta`.

Exact pretty-print example (PHP `JSON_PRETTY_PRINT`: 4-space indent, `\n`, `": "`, no trailing newline):

```
{
    "errors": [
        {
            "code": "HttpForbiddenException",
            "status": "403",
            "detail": "Requesting node does not have permission to access this server."
        }
    ]
}
```

- `Content-Type: application/json`.
- Extra headers from the exception, e.g. `WWW-Authenticate: Bearer` (401) or `Retry-After: <n>` (429, `vendor/symfony/http-kernel/Exception/TooManyRequestsHttpException.php`).

Validation errors (`panel/app/Exceptions/Handler.php:261-296`) use `response()->json([...], 422)` with **default flags 0**: compact output, `/` escaped as `\/`, non-ASCII escaped as `\uXXXX`.

```json
{"errors":[{"code":"ValidationException","status":"422","detail":"The username field is required.","meta":{"source_field":"username","rule":"required"}}]}
```

- There is one element per **message**, flattened in attribute order.
- `meta.rule` = `Str::snake(<failed rule class name>)`, e.g. `required`, `required_if`, `uuid`, `boolean`, `in`, `ip`, `array`, `string`, `numeric`, `present`. It is looked up as `failed()[str_replace('.', '_', field)][index]`. The prefix `App\_rules\_` is replaced by `p_`.
- `meta.source_field` is the dotted attribute, e.g. `data.0.server`.
- In debug mode `meta` also contains `trace`/`previous` before `source_field`/`rule`, and `source` is present.

Message strings come from the panel's `panel/lang/en/validation.php`, merged over the framework defaults with `array_replace_recursive` (`vendor/…/Translation/FileLoader.php:129-139`):

| rule | message |
|---|---|
| required | `The :attribute field is required.` (framework) |
| array | `The :attribute must be an array.` (panel) |
| string | `The :attribute must be a string.` (panel) |
| uuid | `The :attribute field must be a valid UUID.` (framework) |
| ip | `The :attribute must be a valid IP address.` (panel) |
| boolean | `The :attribute field must be true or false.` (framework) |
| in | `The selected :attribute is invalid.` (framework) |
| numeric | `The :attribute must be a number.` (panel) |
| present | `The :attribute field must be present.` (framework) |
| required_if | `The :attribute field is required when :other is :value.` (framework) |

`:attribute` = `str_replace('_', ' ', Str::snake(attr))`, e.g. `checksum_type` becomes `checksum type` and `data.0.server` stays `data.0.server`.

`DisplayException` subclasses render through their own `render()` (`panel/app/Exceptions/DisplayException.php:60-77`). For JSON requests that is `response()->json(Handler::toArray($e), 400)`: compact, status 400.

#### 1.5.1 JSON encoding of success bodies

- `new JsonResponse($data)` uses Illuminate `JsonResponse` with `$options = 0` (`vendor/…/Http/JsonResponse.php:27-31`). That gives PHP `json_encode($data, 0)`: `\/` escaped, unicode as `\uXXXX`, no pretty print.
- PHP arrays with sequential `0..n-1` keys encode as JSON arrays. Any other key set encodes as an object. The empty array encodes as `[]`.
- `stdClass` encodes as an object (`{}` when empty).
- 204 responses have **no body and no Content-Type**. The controller passes `new JsonResponse([], 204)`, but `Response::prepare()` strips the body and the type for `isEmpty()` statuses (`vendor/symfony/http-foundation/Response.php:244-253`, invoked from `vendor/…/Routing/Router.php:930-960`).
- A controller returning `void` produces **200, empty body, `Content-Type: text/html; charset=utf-8`** (`Router.php:950-953` plus `prepare()`).

### 1.6 Input normalisation

- JSON bodies: `$request->input()` reads `json()` when `Content-Type` contains `/json` or `+json`. Otherwise it reads form data. An invalid or empty JSON body becomes `[]` (`IsValidJson` is not applied).
- Wings sends `null` as the body for `POST /servers/reset` and `/transfer/*` (`wings/remote/servers.go:62-70,113-123`). That becomes `[]`.
- `TrimStrings` and `ConvertEmptyStringsToNull` run before validation (§1.1). So `"ip": ""` becomes `null` and `"password": ""` becomes `null` (which then fails `required`). `username` is trimmed. `password` is **not** trimmed.
- `$request->boolean(k)` = `filter_var(v, FILTER_VALIDATE_BOOLEAN)`: `true/"1"/"true"/"on"/"yes"` are true, everything else is false (`vendor/…/Support/Traits/InteractsWithData.php:276-279`).
- The `boolean` validation rule only accepts `true,false,0,1,"0","1"` (strict `in_array`), see `vendor/…/Validation/Concerns/ValidatesAttributes.php:536-545`. The strings `"true"`/`"false"` **fail** it.
- PHP notices are fatal. Laravel's `HandleExceptions` turns warnings (e.g. "Undefined array key") into `ErrorException`, which gives a 500. Deprecations (e.g. passing null to `strrev`) are only logged.

---

## 2. Endpoint reference (Wings → Panel)

All successful JSON bodies use flags 0 (§1.5.1). "Activity log" means rows in `activity_logs` plus `activity_log_subjects`, followed by `Event::dispatch(new ActivityLogged($log))`, which feeds webhooks (`panel/app/Providers/EventServiceProvider.php:14-20`, `panel/app/Listeners/DispatchWebhooks.php`).

Every Eloquent `save()` on a model with `HasValidation` first runs model validation (`panel/app/Observers/ValidationObserver.php:12-19`). A failure throws `DataValidationException`, which gives **500**. `DataValidationException` implements `HttpExceptionInterface` with status 500, so the *message is exposed*. The `detail` string value is `Could not save App\Models\<Model>[<id>]: failed to validate data: <MessageBag::toJson()>` (`panel/app/Exceptions/Model/DataValidationException.php:17-43`). It is rendered pretty-printed, so in the raw body the backslashes appear as `\\` and the embedded JSON's quotes as `\"`.

Activity rows written through the `Activity` facade (`panel/app/Services/Activity/ActivityLogService.php:198-257`) get:

- `ip = Request::ip()`. On remote routes that is **the Wings node IP**, not the SFTP or console user's IP.
- `actor = null`, unless set explicitly. There is no session or sanctum user on remote routes.
- `api_key_id = null`.
- `properties = {}`, then the merged properties, stored as JSON of a PHP array.
- `timestamp = now()` (§2.4).

In non-production `APP_ENV`, `log()` rethrows save errors. In production it swallows them (`ActivityLogService.php:138-157`).

### 2.1 R1 `POST /api/remote/sftp/auth`

Controller: `panel/app/Http/Controllers/Api/Remote/SftpAuthenticationController.php:32-79`. FormRequest: `panel/app/Http/Requests/Api/Remote/SftpAuthenticationFormRequest.php:22-29`.

Wings request body (`wings/remote/types.go:75-86`):

```json
{"type":"password"|"public_key","username":"<user>.<server>","password":"<password or authorized_keys line>","ip":"<client ip>","session_id":"<base64>","client_version":"<base64>"}
```

The panel ignores `ip`, `session_id` and `client_version`.

Validation:

- `type`: `nullable|in:password,public_key`
- `username`: `required|string`
- `password`: `required|string`

Failure gives 422. Wings treats **any 4xx** as "invalid credentials" (`wings/remote/servers.go:131-147`).

Algorithm:

1. **Username parse** (`:110-120`):
   - `parts = explode('.', strrev(username), 2)`.
   - `server = strrev(parts[0])`: everything after the **last** `.`.
   - `user = strrev(parts[1] ?? null)`: everything before the last `.`. With no dot it is `""`.
   - Dots inside the username are therefore allowed.
2. If `empty(server)` (`""` or `"0"`): **400** `BadRequestHttpException('No valid server identifier was included in the request.')` (`:35`). There is no throttle increment.
3. **Throttle check** (`:37-41`, trait `vendor/laravel/ui/auth-backend/ThrottlesLogins.php`):
   - Key (`:155-160`): `strtolower(strrev(explode('.', strrev(username))[0]) . '|' . $request->ip())`, i.e. `lower(server_part) + "|" + wings_ip`.
   - Limiter: `Illuminate\Cache\RateLimiter` on the default cache store (`vendor/…/Cache/RateLimiter.php:128-180`).
     - The stored key is `cleanRateLimiterKey(key)` (htmlentities, with `&xacute;` collapsed to `x`).
     - Counter at `key`, window start at `key:timer` (value = unix time when available).
     - A fixed window opens at the first hit.
     - `maxAttempts = 5`, `decay = 60 s` (trait defaults, `ThrottlesLogins.php:110-123`).
   - If `attempts >= 5` and the timer exists: **429**. `TooManyRequestsHttpException($seconds, "Too many login attempts for this account, please try again in $seconds seconds.")` with `seconds = max(0, timer - now)` and header `Retry-After: <seconds>`.
   - If `attempts >= 5` but the timer has expired, the attempts are reset.
4. **User lookup** (`:98-103`): `User::where('username', lower(trim(user)))->first()`. If missing, *reject with increment*.
5. **Server lookup** (`:85-93`): `Server::where(uuid = s OR uuid_short = s) AND node_id = <auth node id>`. If missing (including servers on another node), *reject with increment*.
6. **Credential check**:
   - `type !== 'public_key'` (including null or missing type): `password_verify(password, users.password)`. On failure:
     - Activity `auth:sftp.fail`, subject = user, property `{"method":"password"}`, actor null, ip = Wings IP (`:46-51`).
     - Then *reject with increment*.
   - `type === 'public_key'` (`:52-70`):
     - Parse with `PublicKeyLoader::loadPublicKey(trim(password))` (phpseclib4).
     - If that throws `NoKeyLoadedException` (also thrown when a *private* key is given), the key is `null`.
     - Lookup: `user_ssh_keys` rows of the user with `deleted_at IS NULL` (SoftDeletes, `panel/app/Models/UserSSHKey.php:46`) and `fingerprint = <sha256 fingerprint>`.
     - Fingerprint = `base64(sha256(OpenSSH binary public-key blob))` with **the last character removed**: 43 chars, no `=` padding, no `SHA256:` prefix (`vendor/phpseclib/phpseclib/phpseclib/Crypt/Common/Traits/Fingerprint.php:38-52`).
     - Failure: *reject*, but the increment happens **only if the key could not be parsed** (`reject($request, is_null($key))`). A valid but unknown key does not count. Nothing is logged.
7. **Access check** (`:137-150`):
   - If `!user->can('update server', server) && server.owner_id !== user.id`:
     - perms = GetUserPermissionsService (§2.1.1).
     - If `'file.sftp'` is not in perms: activity `server:sftp.denied` (actor = user, subject = server), then **403** `HttpForbiddenException('You do not have permission to access SFTP for this server.')`.
     - This 403 has **no** throttle increment.
   - Then `$server->validateCurrentState()` (`panel/app/Models/Server.php:474-489`). If the server is suspended, the node is in maintenance, it is not installed (installing / install_failed / reinstall_failed), it is restoring a backup, or it has an active transfer, the response is **409** `ServerStateConflictException`.
   - The 409 detail is picked in this order (`panel/app/Exceptions/Http/Server/ServerStateConflictException.php:16-32`, `panel/lang/en/exceptions.php:63-69`):
     - suspended: `This server is currently suspended and the functionality requested is unavailable.`
     - maintenance: `The node of this server is currently under maintenance and the functionality requested is unavailable.`
     - not installed: `This server has not yet completed its installation process, please try again later.`
     - restoring: `This server is currently restoring from a backup, please try again later.`
     - transferring: `This server is currently being transferred to a new machine, please try again later.`
     - fallback: `This server is currently in an unsupported state, please try again later.`
8. **Success**: 200 with `{"user":"<user uuid>","server":"<server uuid (full)>","permissions":<GetUserPermissionsService result>}`. Key order is exactly this. The login attempts are **never cleared** on success.

"Reject" (`:125-132`): optionally `RateLimiter::hit(key, 60)`, then **403** `HttpForbiddenException('Authorization credentials were not correct, please try again.')`.

DB writes: `activity_logs`/`activity_log_subjects` only (`auth:sftp.fail`, `server:sftp.denied`) and cache keys for the limiter.

`can('update server', $server)` is evaluated by Laravel's Gate:

- `Gate::before` grants root admins (`panel/app/Providers/AppServiceProvider.php:105`).
- spatie's `Gate::before` grants a role permission named `update server` (`vendor/spatie/laravel-permission/src/PermissionRegistrar.php:119-130`).
- `ServerPolicy::before` denies if the user cannot target the node (`panel/app/Policies/ServerPolicy.php:18-45`).

Reproduce it with the shared authorization spec.

#### 2.1.1 Permission list (`panel/app/Services/Servers/GetUserPermissionsService.php:19-51`)

- `isOwner = user.id === server.owner_id`.
- `isAdmin = user.isAdmin() && (can('view', server) || can('update', server))`.
- If `isOwner && !isAdmin`: `["*"]`.
- If `isAdmin && (isOwner || can('update', server))`: `["*","admin.websocket.errors","admin.websocket.install","admin.websocket.transfer"]`.
- Otherwise `sub = subusers row(server_id, user_id)` and `subPerms = sub ? (sub.permissions ?? []) : []`.
- If `isAdmin`: `array_unique(array_merge(["websocket.connect"], [the 3 admin perms], subPerms))`.
  - **Quirk:** `array_unique` keeps keys. Subusers always store `websocket.connect` (`panel/app/Http/Controllers/Api/Client/Servers/SubuserController.php:143-151`), so for an admin who is also a subuser the merged list has a gap.
  - The result is then encoded as a **JSON object** `{"0":"websocket.connect","1":…,"5":"control.console",…}`.
  - Wings expects `[]string` (`wings/remote/types.go:88-92`, `wings/router/tokens/websocket.go:84-90`), so SFTP login and websocket auth break for such users.
  - Rust must reproduce the PHP output byte-for-byte (differential tests). Whether to fix it is a product decision, see Open Questions.
- Else: `subPerms` as stored, a JSON list.

### 2.2 R2 `GET /api/remote/servers`

Controller: `panel/app/Http/Controllers/Api/Remote/Servers/ServerDetailsController.php:59-74`.

Wings request: `GET /api/remote/servers?page=<n>&per_page=<limit>` with `page` starting at `0` (`wings/remote/servers.go:25-53,195-213`). The default limit is `remote_query.boot_servers_per_page = 50`.

Algorithm:

```
perPage = (int) $request->input('per_page', 50)   // query or body; "" → null → (int)null = 0
servers = Server::with(['allocations','egg','mounts'])->where('node_id', node.id)->paginate(perPage)
```

Laravel `paginate` (`vendor/…/Database/Eloquent/Builder.php:1167-1183`):

- `page = request.input('page')` if `FILTER_VALIDATE_INT` and `>= 1`, else 1 (`vendor/…/Pagination/PaginationState.php:19-27`). Wings' `page=0` therefore means page 1.
- `total = SELECT COUNT(*) …`.
- `perPage = perPage ?: 15`, so `per_page=0`, `""` or garbage yields **15**.
- If total > 0: `LIMIT perPage OFFSET (page-1)*perPage`.
- **No `ORDER BY`**, so the order is whatever the DB returns (in practice `servers.id` ascending).
- Negative per_page: `limit()` ignores negatives, so all rows come back, `last_page = 1` and `per_page` echoes the negative value.

Response, 200, wrapped by `ServerConfigurationCollection` (`panel/app/Http/Resources/Daemon/ServerConfigurationCollection.php:21-36`). It is serialised by `PaginatedResourceResponse` (`vendor/…/Http/Resources/Json/PaginatedResourceResponse.php:15-100`) with flags 0:

```json
{
  "data": [ {"uuid":"<uuid>","settings":{…§3…},"process_configuration":{…§3.6…}}, … ],
  "links": {"first":"<path>?page=1","last":"<path>?page=<last>","prev":null|"<path>?page=<n-1>","next":null|"<path>?page=<n+1>"},
  "meta": {
    "current_page": <int>,
    "from": <int|null>,
    "last_page": <int>,
    "links": [ {"url":<string|null>,"label":"&laquo; Previous","page":<int|null>,"active":false},
               {"url":"<path>?page=1","label":"1","page":1,"active":true}, …,
               {"url":<string|null>,"label":"Next &raquo;","page":<int|null>,"active":false} ],
    "path": "<path>",
    "per_page": <int>,
    "to": <int|null>,
    "total": <int>
  }
}
```

Details:

- Key order: `data, links, meta`. That follows from `array_merge_recursive(wrap(data), paginationInformation)` (`ResourceResponse.php:60-73`).
- `meta` key order = `LengthAwarePaginator::toArray()` minus `data`/`*_page_url` (`vendor/…/Pagination/LengthAwarePaginator.php:206-222`).
- `<path>` = `$request->url()`: scheme and host as seen by Laravel (TrustProxies), no query string, no trailing slash. Example: `http://panel.example.com/api/remote/servers`. Page URLs are `path + "?page=N"`. `per_page` is **not** carried over.
- `"/"` is escaped as `\/`, so the URLs appear as `http:\/\/…`.
- Labels `&laquo; Previous` / `Next &raquo;` come from the framework's `pagination.php` lang file (`vendor/…/Translation/lang/en/pagination.php`). The `&` is not escaped (flags 0).
- `meta.links` follows `UrlWindow` (onEachSide = 3):
  - If `last_page < 14`, every page is listed.
  - Otherwise first 2, `…`, a window, `…`, last 2. The ellipsis entry is `{"url":null,"label":"...","active":false}` and has **no `page` key** (`LengthAwarePaginator.php:113-140`, `vendor/…/Pagination/UrlWindow.php`).
- `from`/`to` are `null` when the page is empty. `total = 0` gives `last_page = 1`, `current_page = 1` and `data = []`.
- `per_page` is always a JSON integer. The comment in the source says a stringified value crashes Wings.
- Wings reads only `data[].uuid/settings/process_configuration` and `meta.current_page`/`last_page` (`wings/remote/types.go:31-38,65-69`).

Side effects: none. `allocation` is eager-loaded through `Server::$with`. Each server lazily loads `variables` (N+1, it cannot be eager-loaded) and `egg.configFrom`/`egg.scriptFrom` when inheritance applies.

The per-element `settings` differ subtly from R5, see §3.9.

### 2.3 R3 `POST /api/remote/servers/reset`

Controller: `ServerDetailsController.php:84-133`. Wings calls it once after booting all servers (`wings/cmd/root.go:339`) with body `null`.

1. `servers` = this node's servers with `status = 'restoring_backup'`, each eager-loading `activity` (morphToMany via `activity_log_subjects`, subject_type `server`) filtered to `event = 'server:backup.restore-started'`, ordered by `timestamp DESC`.
2. In one DB transaction:
   - For each such server:
     - `activity = first()`. Skip if there is none.
     - Find that log's subject row with `subject_type = 'backup'` and load the Backup. If the backup no longer exists, `$backup->name` on null throws, giving **500** and a rollback.
     - Write activity `server:backup.restore-failed` with subjects `[server, backup]` and property `{"name": backup.name}`.
   - `UPDATE servers SET status = NULL, updated_at = now() WHERE node_id = ? AND status IN ('installing','restoring_backup')`. This is a mass update through the Eloquent builder, so `updated_at` is set and no model events fire.
3. Response **204**, empty.

Note: nothing in this codebase ever writes `server:backup.restore-started` (the client restore logs `server:backup.restore`, `panel/app/Http/Controllers/Api/Client/Servers/BackupController.php:250-252`). In practice step 2's loop logs nothing and only the mass update matters.

### 2.4 R4 `POST /api/remote/activity`

Controller: `panel/app/Http/Controllers/Api/Remote/ActivityProcessingController.php:20-85`. FormRequest: `panel/app/Http/Requests/Api/Remote/ActivityEventRequest.php:18-56`.

Wings body (`wings/remote/servers.go:184-191`, `wings/internal/models/activity.go`):

```json
{"data":[{"user":"<uuid>"|null,"server":"<uuid>","event":"server:…","metadata":{…},"ip":"<ip>","timestamp":"2025-01-01T12:00:00.123456789Z"}]}
```

Validation:

```
data               required|array          (empty [] fails "required")
data.*             array
data.*.user        sometimes|nullable|uuid
data.*.server      required|uuid
data.*.event       required|string
data.*.metadata    present|nullable|array  (key must exist; JSON object or list or null)
data.*.ip          sometimes|nullable|ip
data.*.timestamp   required|string
```

Algorithm:

1. `servers = node.servers WHERE uuid IN (unique data[*].server)`, keyed by uuid. This uses only the full `uuid` (no `uuid_short`) and only **this node's** servers.
2. `users = users WHERE uuid IN (unique non-empty data[*].user)`, keyed by uuid. There is **no** check that the user is related to the server.
3. For each datum, in order:
   - Skip it (silently) if the server is not in `servers` or `event` does not start with `server:`.
   - Timestamp parsing:
     - `ts = preg_replace('/(\.\d+)Z$/', 'Z', timestamp)`, which strips fractional seconds only when followed by `Z`.
     - `Carbon::createFromFormat(RFC3339 = 'Y-m-d\TH:i:sP', ts, 'UTC')`.
     - On exception: log a warning, `when = now()`, and `metadata = array_merge(metadata ?? [], ['original_timestamp' => <raw timestamp>])`.
   - `log = {ip: empty(ip) ? '127.0.0.1' : ip, event, properties: metadata ?? [], timestamp: when}`.
   - If a user was found: `actor_id = user.id` and `actor_type = 'user'` (morph map, `panel/app/Providers/AppServiceProvider.php:127-140`).
   - The datum is accessed as `$datum['user']`. If the key is missing, PHP raises a warning that becomes an `ErrorException`, i.e. a **500**. Wings always sends the key (`JsonNullString`).
4. For each server, then for each log in input order:
   - `ActivityLog::forceCreate(log)`.
   - **The model's `creating` hook unconditionally sets `timestamp = Carbon::now()`** (`panel/app/Models/ActivityLog.php:157-158`). The parsed Wings timestamp is therefore **always discarded**. Only `properties.original_timestamp` survives, and only on parse failure.
   - Model validation (`event required|string`, `ip required|string`, `properties array`) runs.
   - Then `activity_log_subjects (activity_log_id, subject_id = server.id, subject_type = 'server')`.
   - `Event::dispatch(new ActivityLogged($log))`.
   - There is **no wrapping transaction**. A failure midway leaves the earlier rows committed.
5. Response: **200, empty body, `Content-Type: text/html; charset=utf-8`**, because the controller returns `void` (§1.5.1).

`properties` encoding:

- `json_encode` of the PHP array decoded from the request.
- The JSON object `{}` becomes PHP `[]`, which is stored as `"[]"`.
- Objects whose keys are exactly `"0".."n-1"` become lists.
- Slashes and unicode are escaped (flags 0, the `collection` cast).

DB columns written: `activity_logs(event, ip, description NULL, actor_type, actor_id, api_key_id NULL, properties, timestamp)`.

### 2.5 R5 `GET /api/remote/servers/{server}`

Controller: `ServerDetailsController.php:37-54`.

1. `transfer = server.transfer`, the latest `server_transfers` row with `successful IS NULL` (`panel/app/Models/Server.php:423-426`).
2. `valid = transfer ? (node.id == transfer.old_node || node.id == transfer.new_node) : node.id == server.node_id`. If not valid: 403.
3. 200 with `{"settings": <§3.1>, "process_configuration": <§3.6>}`. Flags 0, key order as shown.

During a transfer, both nodes receive the configuration computed from the **current** `servers.node_id` (still the old node). So `allocations.mappings` only includes allocations on the old node, and `default` is the old primary allocation (§3.3).

### 2.6 R6 `GET /api/remote/servers/{server}/install`

Controller: `panel/app/Http/Controllers/Api/Remote/Servers/ServerInstallController.php:21-32`.

1. If the server's node is not the authenticated node: 403.
2. Response 200:

```json
{"container_image":<egg.copy_script_container>,"entrypoint":<egg.copy_script_entry>,"script":<egg.copy_script_install|null>}
```

The `copy_script_*` accessors (`panel/app/Models/Egg.php:206-241`): if the egg's own `script_*` value is non-empty, or `copy_script_from` is empty, use the egg's own value. Otherwise use the `scriptFrom` egg's value.

- PHP `empty()` semantics apply, so `""` and `"0"` count as empty.
- `script` may be `null`, which Wings reads as `""`.
- The script text is JSON-encoded with `\/` and `\uXXXX` escapes, and newlines become `\n`.

### 2.7 R7 `POST /api/remote/servers/{server}/install`

Controller: `ServerInstallController.php:39-67`. FormRequest: `panel/app/Http/Requests/Api/Remote/InstallationDataRequest.php:17-23`.

Wings body: `{"successful":bool,"reinstall":bool}` (`wings/remote/types.go:173-176`).

Validation:

- `successful`: `present|boolean`. A JSON `null` fails `boolean`.
- `reinstall`: `sometimes|boolean`.

Algorithm:

1. Node check: 403.
2. Compute the status:
   - `status = null`.
   - If not successful: `status = reinstall ? 'reinstall_failed' : 'install_failed'`.
   - If the **current** status is `suspended`: `status = 'suspended'`, which overrides everything.
3. `prev = installed_at`, then `status = <status>`, `installed_at = now()` (**set even on failure**), and `save()`.
   - The save writes `servers.status`, `servers.installed_at` and `servers.updated_at`.
   - It fires Server validation, `eloquent.updated` webhooks, and the `saved` hook, which deletes a subuser row for the owner if one exists (`Server.php:221-227`). That deletion logs `server:subuser.delete` and queues `RevokeSftpAccessJob`, i.e. Wings call S9 (`panel/app/Services/Subusers/SubuserDeletionService.php:15-28`).
4. `event(new Installed(server, successful, initialInstall = prev === null))`. The listener `panel/app/Listeners/Server/ServerInstalledListener.php:13-36` sends a database notification and, depending on `panel.email.send_install_notification` / `send_reinstall_notification`, an email. Wildcard webhooks also fire.
5. Response **204**.

### 2.8 R8 `POST /api/remote/servers/{server}/transfer/failure`

Controller: `panel/app/Http/Controllers/Api/Remote/Servers/ServerTransferController.php:35-57`.

1. `transfer = server.transfer`. If null: **409** `ConflictHttpException('Server is not being transferred.')`. This is checked *before* the node check.
2. If node is neither `transfer.newNode` nor `transfer.oldNode`: 403.
3. In a transaction:
   - `server_transfers.successful = 0` (`saveOrFail`, which also sets `updated_at`).
   - If `new_allocation || new_additional_allocations`: `UPDATE allocations SET server_id=NULL, notes=NULL, is_locked=0, show_port=1, updated_at=now WHERE id IN ([new_allocation] + new_additional_allocations)` (`Allocation::RELEASE_ATTRIBUTES`, `panel/app/Models/Allocation.php:70-75`).
   - `servers.status` is **not** touched.
4. **204**.

### 2.9 R9 `POST /api/remote/servers/{server}/transfer/success`

Controller: `ServerTransferController.php:64-109`.

1. `transfer = server.transfer`. If null: 409 (same message as R8).
2. Only the **new** node is allowed, else 403.
3. In a transaction:
   - `data = {}`.
   - If `old_allocation || old_additional_allocations`: release (as in R8) `[old_allocation] + old_additional_allocations`, and set `data.allocation_id = transfer.new_allocation`.
   - `data.node_id = transfer.new_node`.
   - `server.update(data)`, which runs model validation including `allocation_id unique:servers`.
   - `server = server.fresh()`, then `server.transfer.update(successful = 1)`.
4. After the commit, call Wings: `DELETE /api/servers/{uuid}` on the **old node** (§4.4 S4).
   - Only `Illuminate\Http\Client\ConnectionException` is caught (logged as a warning).
   - A non-2xx answer from the old node (e.g. 404) raises `RequestException`, which gives a **500 to the new node after the DB has already committed**. Wings then retries the call, and the retries end in 409 because no active transfer exists any more.
5. **204**.

### 2.10 R10 `POST /api/remote/servers/{server}/container/status`

Controller: `panel/app/Http/Controllers/Api/Remote/Servers/ServerContainersController.php:17-26`.

Wings body: `{"data":{"previous_state":"<s>","new_state":"<s>"}}` (`wings/remote/servers.go:216-223`, `wings/remote/types.go:178-181`).

1. Node check: 403.
2. `status = ContainerStatus::tryFrom($request->json('data.new_state')) ?? ContainerStatus::Missing`. The valid values are in `panel/app/Enums/ContainerStatus.php:12-25`: `created, starting, running, restarting, exited, paused, dead, removing, stopping, offline, missing`. Anything else, including a missing value, becomes `missing`.
3. `cache.put("servers.<uuid>.status", <enum>, now + 1h)`. This is the same key `Server::retrieveStatus()` remembers for 15 s (`Server.php:520-532`).
4. **200**, body `[]`, `Content-Type: application/json`.

There is no DB write and no validation.

### 2.11 R11 `GET /api/remote/backups/{backup}?size=<bytes>`

Controller: `panel/app/Http/Controllers/Api/Remote/Backups/BackupRemoteUploadController.php:40-68`. Wings call: `wings/remote/servers.go:149-160`.

1. `size = (int) query('size')`, using PHP int cast semantics (`"12abc"` gives 12, `"1e3"` gives 1000). If `empty(size)` (0 or missing): **400** `A non-empty "size" query parameter must be provided.` This happens *before* the backup lookup.
2. `Backup::where('uuid', backup)->firstOrFail()`: 404 if missing.
3. If `backup.server.node_id !== node.id`: 403.
4. If `completed_at` is not null: **409** `ConflictHttpException('This backup is already in a completed state.')`.
5. `schema = BackupAdapterService.get(backup.backupHost.schema)`. If it is not `S3BackupSchema`: **400** `The configured backup schema is not an S3 compatible.`
6. `S3BackupSchema::getUploadParts(backup, size)` (`panel/app/Extensions/BackupAdapter/Schemas/S3BackupSchema.php:104-154`):
   - `expires = now + config('backups.presigned_url_lifespan')` minutes. The default is 60 (`BACKUP_PRESIGNED_URL_LIFESPAN`, `panel/config/backups.php`).
   - `params = {Bucket: cfg.bucket, Key: "<server uuid>/<backup uuid>.tar.gz", ContentType: "application/x-gzip", [StorageClass: cfg.storage_class if set]}`.
   - The S3 client is built from `backup_hosts.configuration` (JSON) plus `version: latest`, with `credentials = {key, secret, token}` when both key and secret are present (`:24-34`).
   - `CreateMultipartUpload(params)`, then `params.UploadId = result.UploadId`.
   - `partSize = config('backups.max_part_size')` (`BACKUP_MAX_PART_SIZE`, default 134217728). If it is `< 5 MiB` or `> 5 GiB`, use 134217728.
   - For `i = 0; i < size / partSize; i++` (float division, so the count is `ceil(size/partSize)` for positive sizes): presigned URL for `UploadPart(params + PartNumber = i+1)`, expiry `expires` (AWS SigV4 query presign).
   - `backups.upload_id = UploadId` (Eloquent update, so `updated_at` is set).
   - Return `{"parts":[<url>,…],"part_size":<int>,"max_concurrent_uploads":<int>}`. `max_concurrent_uploads` comes from `BACKUP_MAX_CONCURRENT_UPLOADS` (default 10). This Wings version ignores the field (`wings/remote/types.go:155-158`).
7. 200 with that body. URLs contain `\/` escapes (flags 0).

An exception from the S3 SDK (`S3Exception` etc.) gives a 500 with the generic detail.

### 2.12 R12 `POST /api/remote/backups/{backup}`

Controller: `panel/app/Http/Controllers/Api/Remote/Backups/BackupStatusController.php:33-80`. FormRequest: `panel/app/Http/Requests/Api/Remote/ReportBackupCompleteRequest.php:10-21`. It has no `authorize()`, so it is treated as authorized.

Wings body (`wings/remote/types.go:160-171`):

```json
{"checksum":"<hex>","checksum_type":"sha1","size":<int64>,"successful":<bool>,"parts":[{"etag":"…","part_number":1}]|null}
```

Validation:

```
successful      required|boolean
checksum        nullable|string|required_if:successful,true
checksum_type   nullable|string|required_if:successful,true
size            nullable|numeric|required_if:successful,true
parts           nullable|array
parts.*.etag        required|string
parts.*.part_number required|numeric
```

Note that Wings sends `checksum:""` on failure. That becomes `null`, which passes `nullable`.

Algorithm:

1. Backup by uuid: 404.
2. Server node check: 403.
3. If `is_successful` is already true: **400** `Cannot update the status of a backup that is already marked as completed.` A *failed* (completed) backup **can** be reported again.
4. `action = boolean(successful) ? 'server:backup.complete' : 'server:backup.fail'`. Activity subjects are `[backup, server]` with property `{"name": backup.name}`.
5. In a transaction (`ActivityLogService::transaction`, `panel/app/Services/Activity/ActivityLogService.php:175-184`):
   - Update the backup:

     ```
     is_successful = successful
     is_locked     = successful ? is_locked : false
     checksum      = successful ? checksum_type . ':' . checksum : null   // e.g. "sha1:9f86…"
     bytes         = successful ? input('size') : 0
     completed_at  = now()
     (updated_at)
     ```

   - If the schema is S3, call `completeMultipartUpload(backup, successful, input('parts'))` (`S3BackupSchema.php:163-209`):
     - If `upload_id` is empty: return if `!successful`, otherwise throw `Exception('Cannot complete backup request: no upload_id present on model.')`, which gives **500** and a rollback.
     - If `!successful`: `AbortMultipartUpload`.
     - Otherwise `CompleteMultipartUpload` with `Parts = parts ? [{ETag: etag, PartNumber: part_number}…] : ListParts(...)['Parts']`.
   - Then the activity row is inserted (still inside the transaction).
6. `event(new BackupCompleted(backup))`. `panel/app/Listeners/Server/BackupCompletedListener.php:16-60` sends database notifications and mail to the owner and subusers that opted in. Webhooks also fire.
7. **204**.

### 2.13 R13 `POST /api/remote/backups/{backup}/restore`

Controller: `BackupStatusController.php:92-108`. There is no FormRequest.

Wings body: `{"successful":bool}` (`wings/remote/servers.go:174-182`).

1. Backup by uuid: 404.
2. If `backup.server.node` is not the authenticated node: 403.
3. `servers.status = NULL` via Eloquent `update`, unconditionally. It does not check that the server was restoring.
4. Activity:
   - Event `'server:backup.restore-complete'` if `boolean(successful)`, else **`'server.backup.restore-failed'`**.
   - That second string is a typo in the source: `.` instead of `:`. Reproduce it exactly.
   - Subjects `[backup, server]`, property `{"name":…}`.
5. **204**.

### 2.14 Summary of status codes per endpoint

| Endpoint | 200/204 | Possible errors (in precedence order after auth) |
|---|---|---|
| R1 | 200 JSON | 422, 400, 429, 403, 403(sftp perm), 409 |
| R2 | 200 JSON | none |
| R3 | 204 | 500 on missing backup |
| R4 | 200 empty text/html | 422, 500 |
| R5 | 200 JSON | 404 (binding, pre-auth), 403, 500 (§3 edge cases) |
| R6 | 200 JSON | 404, 403 |
| R7 | 204 | 404, 422, 403, 500 (validation) |
| R8 | 204 | 404, 409, 403 |
| R9 | 204 | 404, 409, 403, 500 (old node delete failed) |
| R10 | 200 `[]` | 404, 403 |
| R11 | 200 JSON | 400(size), 404, 403, 409, 400(schema), 500 |
| R12 | 204 | 422, 404, 403, 400, 500 |
| R13 | 204 | 404, 403 |

All of the above can additionally be preceded by the auth errors 401, 400, 404, 403 and 500 (decrypt).

---

## 3. Server configuration document (`settings` + `process_configuration`)

Built by `ServerConfigurationStructureService::handle()` (`panel/app/Services/Servers/ServerConfigurationStructureService.php:22-126`) and `EggConfigurationService::handle()` (`panel/app/Services/Eggs/EggConfigurationService.php:26-38`). The same code is used by R2 (list), R5 (single) and `BuildModificationService` (to decide whether to sync). The source says "DO NOT MODIFY THIS FUNCTION" (`:16-17`).

`handle($server, $override)`: if `$override` is non-empty, it reloads the server with `fresh()` and applies the overrides with `setAttribute`. No remote caller passes overrides.

### 3.1 `settings` field list (exact key order)

| # | Key | Value / source | JSON type |
|---|---|---|---|
| 1 | `id` | `servers.id` | int |
| 2 | `uuid` | `servers.uuid` | string |
| 3 | `meta` | `{"name": servers.name, "description": servers.description}` | object; description is string or **null** |
| 4 | `suspended` | `servers.status === 'suspended'` | bool |
| 5 | `environment` | §3.2 | object |
| 6 | `invocation` | `servers.startup`, raw, **placeholders not substituted** (Wings substitutes) | string |
| 7 | `skip_egg_scripts` | `servers.skip_scripts` (boolean cast) | bool |
| 8 | `build` | object, key order: `memory_limit, swap, io_weight, cpu_limit, threads, disk_space, oom_killer` | object |
| 8a | `build.memory_limit` | `(int) round(use_binary_prefix ? memory : memory / 1.048576)` | int |
| 8b | `build.swap` | same formula on `swap` (-1 stays -1; `round(-1/1.048576) = -1`) | int |
| 8c | `build.io_weight` | `servers.io` | int |
| 8d | `build.cpu_limit` | `servers.cpu` | int |
| 8e | `build.threads` | `servers.threads` | string or null |
| 8f | `build.disk_space` | same formula on `disk` | int |
| 8g | `build.oom_killer` | `servers.oom_killer` | bool |
| 9 | `container` | `{"image": servers.image, "requires_rebuild": false}` | object |
| 10 | `allocations` | `{"force_outgoing_ip": egg.force_outgoing_ip, "default": {"ip": …, "port": …}, "mappings": §3.3}` | object |
| 10a | `allocations.default.ip` | `server.allocation.ip ?? '127.0.0.1'`. Uses the **`ip` column, not `ip_alias`**. The allocation comes from `servers.allocation_id`, regardless of node. | string |
| 10b | `allocations.default.port` | `server.allocation.port ?? 0` | int |
| 11 | `egg` | `{"id": eggs.uuid, "file_denylist": egg.inherit_file_denylist, "features": §3.4}` | object |
| 12 | `labels` | **only present if** `!empty(servers.docker_labels)`: the decoded JSON of `docker_labels` (array cast) | object (or array, see §3.9) |
| 13 | `mounts` | **only present if** the server has ≥1 mount: `[{"source", "target", "read_only": bool}, …]` | array |

Notes:

- `use_binary_prefix` = `config('panel.use_binary_prefix')`, env `PANEL_USE_BINARY_PREFIX`, default `true` (`panel/config/panel.php`). PHP `round()` rounds half away from zero.
- Mounts come from `servers` → `mountables(mountable_type='server')` → `mounts`, in DB order (no ORDER BY) (`panel/app/Models/Server.php:436-439`). Mounts attached to eggs or nodes are **not** included here.
- `labels` and `mounts` are omitted entirely (key missing, not `null` and not `[]`) when empty.
- Wings fields not sent: `crash_detection_enabled` (Wings uses its own config default, `wings/server/server.go:298-304`) and `container.requires_rebuild` (ignored).
- `egg.force_outgoing_ip` is boolean cast. A DB NULL would give `null`.

### 3.2 `environment` (`panel/app/Services/Servers/EnvironmentService.php:37-69`)

1. Start from `server.variables`. That is `egg_variables` for the server's `egg_id`, LEFT JOIN `server_variables` on `variable_id` AND `server_id = <this server>`, selecting `egg_variables.*`, `server_variables.variable_value AS server_value` (`panel/app/Models/Server.php:365-377`).
   - **No ORDER BY.** The order is whatever the DB returns, effectively `egg_variables.id`. Note that the `sort` column is ignored here.
2. Map each row to `env_variable => (server_value ?? default_value)`. Only `NULL` falls back to the default; an empty string `""` is kept.
3. Then `put('STARTUP', servers.startup)` and `put('P_SERVER_UUID', servers.uuid)`. `put` overwrites in place if the key already exists. These names are reserved (`EggVariable::RESERVED_ENV_NAMES`, `panel/app/Models/EggVariable.php`), so in practice they are appended at the end in this order.
4. Then any plugin-registered keys (`setEnvironmentKey`). There are none in core.

Values are strings, or null when both values are NULL. Example:

```json
{"SERVER_JARFILE":"server.jar","MC_VERSION":"latest","STARTUP":"java -Xms128M -jar {{SERVER_JARFILE}}","P_SERVER_UUID":"9b2f…"}
```

The panel adds **no other** `P_SERVER_*` or `SERVER_*` variables. Wings adds `SERVER_MEMORY`, `SERVER_IP`, `SERVER_PORT`, `TZ` and so on by itself.

### 3.3 `allocations.mappings` (`panel/app/Models/Server.php:235-247`)

```php
$allocations = $this->allocations->where('node_id', $this->node_id);   // only allocations on the server's CURRENT node_id
if (!$this->allocation || $allocations->isEmpty()) return ['' => []];   // → JSON {"":[]}
return $allocations->groupBy('ip')->map(fn ($i) => $i->pluck('port'))->toArray();
```

- Output: `{"<ip>":[<port int>, …], …}`.
- IP keys appear in first-seen order and ports in row order. `server.allocations` is `hasMany` without ORDER BY, so in practice that is `allocations.id`.
- Uses the `ip` column, not `ip_alias`.
- When the server has no primary allocation **or** no allocations on its node, the result is the magic `{"":[]}` (an object with an empty-string key). It is never `[]` or `{}`.

### 3.4 `egg.features` (`panel/app/Extensions/Features/FeatureService.php:27-51`)

`getMappings($server->egg->features)` uses the egg's **own** `features` column, **not** `inherit_features`.

- Registered schemas, in registration order (`panel/app/Providers/Extensions/FeatureServiceProvider.php:17-28`), with listeners from each schema's `getListeners()`:
  1. `gsl_token`: `["(gsl token expired)","(account not found)"]`
  2. `java_version`: `["java.lang.UnsupportedClassVersionError","unsupported major.minor version","has been compiled by a more recent version of the java runtime","minecraft 1.17 requires running the server with java 16 or above","minecraft 1.18 requires running the server with java 17 or above","minecraft 1.19 requires running the server with java 17 or above"]`
  3. `eula`: `["you need to agree to the eula in order to run the server"]`
  4. `pid_limit`: `["pthread_create failed","failed to create thread","unable to create thread","unable to create native thread","unable to create new native thread","exception in thread \"craft async scheduler management thread\""]`
  5. `steam_disk_space`: `["steamcmd needs 250mb of free disk space to update","0x202 after update job"]`
- Sources: `panel/app/Extensions/Features/Schemas/*.php`.
- `collect($schemas)->only($features)` (`vendor/…/Collections/Collection.php:988-1001`):
  - `features IS NULL`: `only(null)` returns **all 5** schemas.
  - `features = ["eula","java_version"]`: `array_intersect_key`, so the order is **registration order** (`java_version` before `eula`). Unknown ids are dropped.
  - `features = []`, or only unknown ids: an empty PHP array, encoded as **`[]`** (JSON array).
- Wings tolerates `[]` here: a failed unmarshal leaves `Features = nil` (`wings/server/configuration.go:24-37`).

### 3.5 `egg.file_denylist`

`Egg::getInheritFileDenylistAttribute()` (`panel/app/Models/Egg.php:310-317`):

- If `config_from IS NULL`: the egg's own `file_denylist` (array cast).
- Else: **the parent egg's** `file_denylist`. The child's own list is ignored even if set, unlike the other `inherit_*` accessors.
- Value: a JSON list, `null`, or, if the stored JSON is an object, an object. See §3.9.

### 3.6 `process_configuration` (`panel/app/Services/Eggs/EggConfigurationService.php:26-132`)

```json
{"startup":{"done":[…],"user_interaction":[],"strip_ansi":<bool>},"stop":{"type":"command"|"signal","value":"…"},"configs":[…]}
```

`startup` (`:46-55`):

- `s = json_decode(egg.inherit_config_startup, true)`.
- If `inherit_config_startup` is NULL, `json_decode(null)` returns null, which is passed to an `array`-typed parameter: `TypeError`, **500**. The same happens for invalid JSON or JSON that decodes to a scalar.
- `done = s['done']`. A string becomes `[string]`. An array is kept as-is. Missing gives `null`, emitted as JSON `null`.
- `user_interaction` is always `[]`.
- `strip_ansi = s['strip_ansi'] ?? false`. The value is kept with whatever JSON type it had.

`inherit_*` accessors (`Egg.php:244-307`): if `config_X IS NOT NULL` or `config_from IS NULL`, use the egg's own value; otherwise use `configFrom`'s value. This is a single level, not recursive.

`stop` (`:66-81`):

- `stop = egg.inherit_config_stop`. NULL is a `TypeError`, **500**.
- If it does **not** start with `^`: `{"type":"command","value":stop}`. This includes `""`.
- Otherwise `{"type":"signal","value":strtoupper(substr(stop,1))}`, e.g. `^C` gives `C` and `^sigkill` gives `SIGKILL`.

`configs`: §3.7.

### 3.7 `configs`: config-file parser entries and placeholder replacement

Input: `files = json_decode(egg.inherit_config_files ?? '{}')`, **non-assoc** (objects become `stdClass`).

- If the result is neither an object nor an array (invalid JSON, scalar, null), `configs = []`.
- Otherwise `structure = ServerConfigurationStructureService::handle($server)`. This is the full `settings` array from §3.1, recomputed.

For each `file => data` pair, in JSON order (`:96-129`):

1. Skip unless `data` is an object **and** `isset(data->find)` (a null `find` also skips).
2. `append = array_merge((array) data, ['file' => file, 'replace' => []])`.
   - Key order: the original object keys in order, then `file`, then `replace`. Pre-existing `file`/`replace` keys are overwritten **in their original position**.
   - Nested values other than `find` stay `stdClass` and encode as objects.
   - If `files` was a JSON *array*, `file` is the integer index.
3. For each `find => replace` in `iterate(data->find, structure)`:
   - If `replace` is an object: for each `match => replaceWith` add `{"match": find, "if_value": match, "replace_with": replaceWith}`.
   - Else add `{"match": find, "replace_with": replace}`.
   - Key orders are exactly as shown.
   - If `data->find` is a scalar (e.g. a string), `foreach` over a non-iterable raises a PHP warning, which becomes `ErrorException`, **500**.
4. `unset(append['find'])` and push `append`.

Typical output:

```json
{"parser":"properties","file":"server.properties","replace":[{"match":"server-ip","replace_with":"0.0.0.0"},{"match":"server-port","replace_with":"25565"}]}
```

`iterate(value)` (`:194-221`):

- Non-iterable scalars are returned unchanged.
- Arrays are copied; objects are shallow-`clone`d.
- For each child: iterable or object children are recursed; leaves go through `matchAndReplaceKeys`.
- Only **values** are processed. Keys (`match`, `if_value`) are never substituted.

`matchAndReplaceKeys(value)` (`:137-185`):

```
keys = all regex matches of /{{(?<key>[\w.-]*)}}/ in (string)value   // \w = [A-Za-z0-9_]
for key in keys (in match order, duplicates included):
    if not starts_with(key, 'server.' | 'env.' | 'config.'): continue        // e.g. {{SERVER_PORT}} left verbatim
    if value is not a string: continue                                       // ints/bools/null untouched
    if starts_with(key, 'config.'): continue                                  // left for Wings
    if starts_with(key, 'server.'):
        v = Arr::get(structure, key without 'server.', '')                   // dot-path into §3.1 array
    else (env.):
        v = Arr::get(structure, 'environment.' + key without 'env.', '')
    value = str_replace('{{' + key + '}}', v, value)                          // all occurrences
```

- `Arr::get` semantics:
  - An exact key match is tried first (keys never contain dots here).
  - Then the path is walked segment by segment, and a missing segment gives `''`.
  - A path through a list uses numeric indexes (`mounts.0.source`).
- Stringification of `v` by `str_replace`:
  - int → decimal
  - `true` → `"1"`
  - `false` / `null` → `""`
  - array (e.g. `{{server.build}}`) → **`TypeError`, 500**. `str_replace` with a string search and an array replace is a fatal TypeError in PHP 8.
- Because leaves pass through `str_replace`, any leaf that contained a `server.`/`env.` placeholder becomes a **string**, even `"25565"`.
- Supported paths in practice:
  - `server.allocations.default.ip`, `server.allocations.default.port`
  - `server.build.memory_limit`
  - `server.environment.<VAR>`, `env.<VAR>`
  - `server.uuid`
  - Legacy `server.build.default.port` resolves to `''`. Migrations rewrote it (`panel/database/migrations/2024_06_05_220135_update_egg_config_variables.php`, `2024_06_08_020904_refix_egg_variables.php`) and the egg importer converts on import (`panel/app/Services/Eggs/Sharing/EggImporterService.php:24-31`).

Wings-side expectations (`wings/parser/parser.go:116-220`): each config needs `file` and `parser` keys. `create_file` defaults to true. Each replacement needs a string `match`, an optional string `if_value`, and `replace_with` of any JSON type.

### 3.8 Full example (R5 response, flags 0)

```json
{"settings":{"id":7,"uuid":"0b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11","meta":{"name":"Survival","description":""},"suspended":false,"environment":{"SERVER_JARFILE":"server.jar","STARTUP":"java -Xms128M -XX:MaxRAMPercentage=95.0 -jar {{SERVER_JARFILE}}","P_SERVER_UUID":"0b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11"},"invocation":"java -Xms128M -XX:MaxRAMPercentage=95.0 -jar {{SERVER_JARFILE}}","skip_egg_scripts":false,"build":{"memory_limit":2048,"swap":0,"io_weight":500,"cpu_limit":200,"threads":null,"disk_space":10240,"oom_killer":false},"container":{"image":"ghcr.io\/parkervcp\/yolks:java_21","requires_rebuild":false},"allocations":{"force_outgoing_ip":false,"default":{"ip":"0.0.0.0","port":25565},"mappings":{"0.0.0.0":[25565,25566]}},"egg":{"id":"5b1d4a0c-…","file_denylist":[],"features":{"java_version":["…"],"eula":["you need to agree to the eula in order to run the server"]}}},"process_configuration":{"startup":{"done":[")! For help, type "],"user_interaction":[],"strip_ansi":false},"stop":{"type":"command","value":"stop"},"configs":[{"parser":"properties","file":"server.properties","replace":[{"match":"server-ip","replace_with":""},{"match":"server-port","replace_with":"25565"},{"match":"query.port","replace_with":"25565"}]}]}}
```

### 3.9 List endpoint vs single endpoint: `JsonResource::filter()` re-indexing

- R5 encodes the PHP arrays directly.
- R2 passes each `{uuid, settings, process_configuration}` through `JsonResource::resolve()` → `filter()` → `removeMissingValues()` (`vendor/…/Http/Resources/Json/JsonResource.php:120-132`, `vendor/…/Http/Resources/ConditionallyLoadsAttributes.php:25-111`). That function **recurses into every PHP array (not `stdClass`)** and, if *all* keys of an array are numeric (`is_numeric`), replaces it with `array_values()`.

Consequences:

- Arrays with non-sequential numeric keys become JSON lists in R2 but stay JSON objects in R5. Examples:
  - `docker_labels` stored as `{"1":"a","5":"b"}`.
  - `file_denylist` stored as an object (the egg importer saves `Collection::filter()` output, which can keep holes, `EggImporterService.php:250`).
  - Environment maps whose keys are all numeric. This is impossible, because STARTUP is always present.
- Arrays with mixed or string keys are unchanged. `{"":[]}` is unchanged (`''` is not numeric).
- Lists are unchanged.
- `stdClass` values inside `configs` (nested egg JSON objects) are **not** touched.

Rust: implement R2 by applying the same normalisation (§10.2) to the R5 value tree, but only to nodes that were PHP arrays. Objects that came from `json_decode` without `assoc` must be left alone.

---

## 4. Panel → Wings HTTP calls

### 4.1 HTTP client defaults (`Http::daemon` macro, `panel/app/Providers/AppServiceProvider.php:68-78`)

```php
Http::acceptJson()->asJson()->withToken($node->daemon_token)->withHeaders($headers)
    ->withOptions(['verify' => (bool) $app->environment('production')])
    ->timeout(config('panel.guzzle.timeout'))->connectTimeout(config('panel.guzzle.connect_timeout'))
    ->baseUrl($node->getConnectionAddress())
```

Base URL:

- `"{scheme}://{fqdn}:{daemon_connect}"` (`panel/app/Models/Node.php:219-222`).
- Request URL = `rtrim(base,'/') . '/' . ltrim(path,'/')` (`vendor/…/Http/Client/PendingRequest.php:1057-1060`).

Headers on every request:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <decrypted nodes.daemon_token>`. This is **only the secret, not `id.secret`**. Wings compares it in constant time with its `token` (`wings/router/middleware/middleware.go:168-185`). |
| `Accept` | `application/json` |
| `Content-Type` | `application/json`. Sent on **every** request, including GET/DELETE without a body. `withBody()` callers keep `application/json` too (see `files/write`). |
| `User-Agent` | `GuzzleHttp/8` (Guzzle default, `vendor/guzzlehttp/guzzle/src/Utils.php:249-252`, `ClientInterface.php:27`). The panel sets no UA. |

Timeouts:

- `timeout = env GUZZLE_TIMEOUT` (default **15 s**).
- `connect_timeout = env GUZZLE_CONNECT_TIMEOUT` (default **5 s**) (`panel/config/panel.php` `guzzle`).
- Per-call overrides are listed in §4.4.
- Laravel's constructor defaults (`PendingRequest.php:262-275`) are `connect_timeout=10`, `timeout=30`, `http_errors=false` and `crypto_method=TLSv1.2+`. The macro overrides only the two timeouts; `http_errors=false` and TLS ≥ 1.2 remain.

TLS: certificate verification is enabled **only when `APP_ENV=production`**.

Body encoding:

- `post($url, $data = [])` sends `json => $data`. Guzzle `json_encode($data, JSON_THROW_ON_ERROR)` uses flags 0, so `/` is escaped as `\/` and unicode as `\uXXXX` (`vendor/guzzlehttp/guzzle/src/Client.php:1569-1580`).
- **A POST with no data sends the body `[]`** (`isset([])` is true).
- `delete($url)` with no data sends no body (`PendingRequest.php:972-977`).
- `get($url, $query)` sends query params via Guzzle `query`. That is `http_build_query(…, PHP_QUERY_RFC3986)`, so spaces become `%20`. Booleans are passed as the strings `'true'`/`'false'` where relevant.

Retries: none.

### 4.2 Error handling inside the repositories (`panel/app/Repositories/Daemon/DaemonRepository.php:48-67`)

`getHttpClient()` = `Http::daemon($node)->throwIf(fn ($r) => $this->enforceValidNodeToken($r))`. Laravel semantics (`vendor/…/Http/Client/PendingRequest.php:795-802,1086-1120`):

- **2xx**: returned as-is. The throw callback is not evaluated, so **the User-Agent is not checked on success**.
- **Non-2xx**: `enforceValidNodeToken($response)` runs:
  - Let `ua` = the response `User-Agent` header.
  - Throw `Illuminate\Http\Client\ConnectionException("<effective uri> does not match node token_id !")` if `ua` is empty, OR if `ua` matches `^Pelican Wings\/v(?:\d+\.\d+\.\d+|develop) \(id:(\w*)\)$` but group 1 `!== node.daemon_token_id`.
  - A UA that is present but does not match the regex passes.
  - Otherwise the callback returns true and `$response->throw()` raises `Illuminate\Http\Client\RequestException`, whose `code` is the HTTP status (`vendor/…/Http/Client/RequestException.php:43-50`).
  - Only 4xx/5xx throw. 3xx does not throw (Guzzle follows redirects by default).
- Transport failures (DNS, connect refused, timeout, TLS) throw `ConnectionException` with message `"cURL error N: …"` and code 0 (`PendingRequest.php:2134-2170`).
- Wings sets the response UA on all *authenticated* routes (`wings/router/middleware/middleware.go:174`).

Calls that bypass the repository and use `Http::daemon(...)` **without `throwIf`** (non-2xx is silently ignored; only transport errors throw):

- `Server::send()` (commands)
- `TransferServerService::notify()`
- `Node::statistics()`
- `Node::ipAddresses()`
- `PruneImagesCommand`

### 4.3 Response User-Agent check on `GET /api/system`

`DaemonSystemRepository::getSystemInformation()` (`panel/app/Repositories/Daemon/DaemonSystemRepository.php:18-29`) also calls `Response::throwIf(callback)` (`vendor/…/Http/Client/Response.php:374-377`), which always runs the callback. So even on 2xx:

- The UA check above runs.
- `throw_unless(json has keys ['architecture','cpu_count','kernel_version','os','version'], ConnectionException("<uri> is not Pelican Wings !"))`.

### 4.4 Catalogue of calls

S = `DaemonServerRepository`, F = `DaemonFileRepository`, B = `DaemonBackupRepository`, Y = `DaemonSystemRepository` (all in `panel/app/Repositories/Daemon/`). "rep" means the request goes through `getHttpClient` with the throwIf behaviour of §4.2.

#### Server lifecycle

| # | Method & path on Wings | Body / query (exact keys, order) | Timeouts | Source | Main callers / error handling |
|---|---|---|---|---|---|
| S1 | `GET /api/servers/{uuid}` | none | connect=**1 s**, timeout=**1 s** (param `$timeout`, default 1) | `DaemonServerRepository.php:20-45` (rep, plus an explicit `->throw()`) | **Never throws**, except for one edge: a 2xx with a non-JSON body makes `json()` return null, which violates the `array` return type and raises a `TypeError` (not caught, so 500). Otherwise it returns `->json()` on 2xx. On `RequestException`, a Cloudflare 502 without cache status triggers a Filament notification. Any other `Exception` is `report()`ed. The fallback is `{"state":"missing"}`. Callers: `Server::retrieveStatus/retrieveResources` (cache 15 s, `panel/app/Models/Server.php:520-549`); client `GET /api/client/servers/{s}/resources` (cache key `resources:<uuid>` 20 s, `panel/app/Http/Controllers/Api/Client/Servers/ResourceUtilizationController.php:38-46`); `ProcessScheduleService` with timeout 5 (`panel/app/Services/Schedules/ProcessScheduleService.php:43`). Fields consumed: `state`, `is_suspended`, `utilization.{memory_bytes,cpu_absolute,disk_bytes,network.rx_bytes,network.tx_bytes,uptime}` (`panel/app/Data/Api/Client/StatsData.php:27-41`). |
| S2 | `POST /api/servers` | `{"uuid":<server uuid>,"start_on_completion":<bool>}` | default | `:52-58` (rep) | `ServerCreationService` (`panel/app/Services/Servers/ServerCreationService.php:122-129`). On `ConnectionException` the server is force-deleted and the exception rethrown. `start_on_completion = data['start_on_completion'] ?? false`. |
| S3 | `POST /api/servers/{uuid}/sync` | `[]` | default | `:65-68` (rep) | Suspension (`SuspensionService.php:51`, errors propagate); `BuildModificationService.php:67-72` (only if `build` is non-empty, which is always true; `ConnectionException` is logged and swallowed); admin EditServer after save (`EditServer.php:159`). |
| S4 | `DELETE /api/servers/{uuid}` | none | default | `:75-78` (rep) | `ServerDeletionService.php:49-62`: `ConnectionException` is rethrown unless forced (the 404 code check never matches because ConnectionException code is 0); any other `Exception` (incl. `RequestException` 404/500) is `report()`ed and deletion continues. Transfer success R9 (§2.9) targets the old node. |
| S5 | `POST /api/servers/{uuid}/reinstall` | `[]` | default | `:85-88` (rep) | `ReinstallServerService.php:26-33`: inside a transaction that first sets `status='installing'`. Any exception rolls back the status. |
| S6 | `POST /api/servers/{uuid}/archive` | `[]` | default | `:96-99` | **Dead code**. There are no callers and Wings has no such route (`wings/router/router.go:82-127`). |
| S7 | `DELETE /api/servers/{uuid}/transfer` (old node), then `DELETE /api/transfers/{uuid}` (new node) | none | default | `:106-122` (rep) | Application API `POST /api/application/servers/{id}/transfer/cancel` (`panel/app/Http/Controllers/Api/Application/Servers/ServerManagementController.php:115-132`). It sets `transfer.successful = true` **before** calling (quirk), and errors propagate as 500. |
| S8 | `POST /api/servers/{uuid}/ws/deny` | `{"jtis":[md5(user_id . server_uuid)]}` | default | `:134-140` | **Deprecated, unused**. Its md5 no longer matches the sha256 jti, see §5. |
| S9 | `POST /api/deauthorize-user` | `{"user":<user uuid>,"servers":[<server uuid>]}`, or `"servers":[]` for node-wide | default | `:147-153` (rep) | `RevokeSftpAccessJob` (queued, unique, 3 tries; on `ConnectionException` it calls `release(attempts*10 s)`) (`panel/app/Jobs/RevokeSftpAccessJob.php:40-54`). Dispatched on subuser delete/update, owner change, user delete and password change (`panel/app/Listeners/RevocationListener.php`). |
| S10 | `GET /api/servers/{uuid}/install-logs` | none | default | `:155-161` (rep plus `->throw()`) | Returns `json('data')` (a string). Admin ServerResource only. |
| S11 | `POST /api/servers/{uuid}/power` | `{"action":"start"|"stop"|"restart"|"kill"}` | default | `:168-173` (rep) | Client `POST /power` (`PowerController.php:32-41`, errors give 500); schedules (`PowerActionSchema.php:22`); Filament; `BulkPowerActionCommand`. |
| S12 | `POST /api/servers/{uuid}/commands` | `{"commands":[<string>,…]}` | default | `panel/app/Models/Server.php:513-518` (**no throwIf**) | Client `POST /command` (`CommandController.php:26-45`). A **non-2xx Wings answer (e.g. 502 when the server is offline) is ignored**: the client gets 204 and activity `server:console.command` is logged. The `BadResponseException` → 502 branch is unreachable because `http_errors=false`. A transport error gives 500. |

#### Files

| # | Method & path on Wings | Body / query (exact keys, order) | Timeouts | Source | Main callers / error handling |
|---|---|---|---|---|---|
| F1 | `GET /api/servers/{uuid}/files/contents?file=<path>` | query | default | `DaemonFileRepository.php:24-44` (rep) | Wings 400 → `FileNotEditableException` (plain Exception, client gets 500); 404 → `Illuminate\Contracts\Filesystem\FileNotFoundException` (500). If `max` is given and `Content-Length > max` (`panel.files.max_edit_size`, default 4 MiB): `FileSizeTooLargeException` (DisplayException, **400** compact JSON). The client API returns the body as `text/plain; charset=utf-8` after `convert_to_utf8` (`FileController.php:70-83`). |
| F2 | `POST /api/servers/{uuid}/files/write?file=<path>` | **raw** request body (the client's raw content), `Content-Type: application/json` (from `withBody` default) | default | `:53-65` (rep) | Wings 400 → `FileExistsException` (500). |
| F3 | `GET /api/servers/{uuid}/files/list-directory?directory=<path>` | query | default | `:74-79` (rep) | Returns json. The client API maps each entry via `FileObjectData` (`name, mode, mode_bits, size, file, symlink, mime, created, modified`, `panel/app/Data/Api/Client/FileObjectData.php:31-44`). |
| F4 | `POST /api/servers/{uuid}/files/create-directory` | `{"name":…,"path":…}` | default | `:87-101` (rep) | Wings 400 → `FileExistsException`. |
| F5 | `PUT /api/servers/{uuid}/files/rename` | `{"root":<root ?? "/">,"files":[{"from","to"}…]}` | default | `:110-118` (rep) | |
| F6 | `POST /api/servers/{uuid}/files/copy` | `{"location":…}` | default | `:125-130` (rep) | |
| F7 | `POST /api/servers/{uuid}/files/delete` | `{"root":<root ?? "/">,"files":[…]}` | default | `:139-147` (rep) | |
| F8 | `POST /api/servers/{uuid}/files/compress` | `{"root":<?? "/">,"files":[…],"name":<?? "">,"extension":<?? "">}` | **timeout 900 s** | `:157-171` (rep) | Returns a json file object. |
| F9 | `POST /api/servers/{uuid}/files/decompress` | `{"root":<?? "/">,"file":…}` | **900 s** | `:178-190` (rep) | |
| F10 | `POST /api/servers/{uuid}/files/chmod` | `{"root":<?? "/">,"files":[{"file","mode"}…]}` | default | `:199-207` (rep) | |
| F11 | `POST /api/servers/{uuid}/files/pull` | `{"url","root":<?? "/">,"file_name"?,"use_header"?,"foreground"?}`. Null-valued keys are **removed**. Key order is as listed. | default | `:216-227` (rep) | |
| F12 | `GET /api/servers/{uuid}/files/search?pattern=…&directory=<?? "/">` | query | **timeout 120 s** | `:236-248` (rep) | |

#### Backups

| # | Method & path on Wings | Body / query (exact keys, order) | Timeouts | Source | Main callers / error handling |
|---|---|---|---|---|---|
| B1 | `POST /api/servers/{uuid}/backup` | `{"adapter":<backup_hosts.schema: "wings"/"s3">,"uuid":<backup uuid>,"ignore":implode("\n", ignored_files)}` | default | `DaemonBackupRepository.php:16-25` (rep) | `InitiateBackupService` inside the transaction that inserts the backup row. Any error rolls the row back (`panel/app/Services/Backups/InitiateBackupService.php:131-145`). |
| B2 | `POST /api/servers/{uuid}/backup/{backup}/restore` | `{"adapter":…,"truncate_directory":<bool>,"download_url":<url ?? "">}` | default | `:32-41` (rep) | Client restore: inside a transaction after setting `status='restoring_backup'`. `download_url` comes from §5 (wings) or an S3 presigned GET (5 min) (`BackupController.php:241-263`). |
| B3 | `DELETE /api/servers/{uuid}/backup/{backup}` | none | default | `:48-51` (rep) | `WingsBackupSchema::deleteBackup` swallows exceptions whose `getCode() === 404` (a RequestException 404) and rethrows the rest (`WingsBackupSchema.php:31-42`). `DeleteBackupService` only rethrows if `backup.is_successful`, and deletes the row otherwise (`DeleteBackupService.php:22-45`). |

#### System and transfers

| # | Method & path on Wings | Body / query (exact keys, order) | Timeouts | Source | Main callers / error handling |
|---|---|---|---|---|---|
| Y1 | `GET /api/system` | none | **connect 3 s**, timeout default | `DaemonSystemRepository.php:18-29` | §4.3. `Node::systemInformation()` caches it (`cache()->flexible("nodes.<id>.system_information",[30,360])`) and converts exceptions to `{"exception": msg}`, with cURL message clean-up (`Node.php:369-390`). |
| Y2 | `GET /api/diagnostics?log_lines=<int>&include_endpoints=true|false&include_logs=true|false` | query | **timeout 5 s** | `:37-46` (rep) | Admin node page. The body is plain text. |
| Y3 | `POST /api/update` | `Node::getConfiguration()` of the **updated** node (§6). Request goes to the **new fqdn** but with the **old** scheme, port and token. | default | `:55-58` (rep); `panel/app/Services/Nodes/NodeUpdateService.php:30-67` | `ConnectionException`: the DB update is committed and `ConfigurationNotPersistedException` (DisplayException, 400) is thrown. `RequestException`: the transaction is rolled back and the error propagates (500). |
| Y4 | `GET /api/system/utilization` | none | connect 1 s, timeout 1 s | `Node.php:415-432` (no throwIf) | Cached `[5,30]`. If the response lacks `memory_total`, defaults are used. |
| Y5 | `GET /api/system/ips` | none | 1 s / 1 s | `Node.php:435-457` (no throwIf) | Reads `ip_addresses`, keeps valid IPs, always appends `0.0.0.0` and `::`. Cached 1 h. |
| Y6 | `DELETE /api/system/docker/image/prune` | none | connect 5 s, timeout 30 s | `panel/app/Console/Commands/Maintenance/PruneImagesCommand.php:36-58` | CLI only. Reads `ImagesDeleted` and `SpaceReclaimed`. |
| T1 | `POST /api/servers/{uuid}/transfer` (to the **old** node) | `{"url":"<new node address>/api/transfers","token":"Bearer <transfer JWT>","backups":[<uuid>…],"server":{"uuid":<uuid>,"start_on_completion":false}}` | default | `panel/app/Services/Servers/TransferServerService.php:33-53` (**no throwIf**) | `backups` = only the requested uuids of this server whose backup host schema is `wings`. Called **after** the transfer row is committed. A Wings non-2xx is ignored; a transport error propagates, leaving the transfer row in place. Wings expects this shape (`wings/router/router_server_transfer.go:20-25`). |

### 4.5 How Wings failures surface on the panel's own APIs

There is **no `DaemonConnectionException`** in this codebase (the Pterodactyl 502 mapping is gone).

- `ConnectionException` and `RequestException` extend `HttpClientException` → `Exception`, so they are not HttpExceptions. If uncaught, the panel API answers **500**:

  ```json
  {"errors":[{"code":"ConnectionException"|"RequestException","status":"500","detail":"An unexpected error was encountered while processing this request, please try again."}]}
  ```

  This is pretty-printed (§1.5). With `APP_DEBUG` the detail is the raw message, e.g. `HTTP request returned status code 502:\n<body summary>\n` (`vendor/…/Http/Client/RequestException.php:91-112`).
- Exceptions thrown inside DB transactions roll them back (Handler `render()` also rolls back any open transaction).
- Special mappings are listed in the tables above: F1 400 and F1 too-large, F2/F4 400, S1 never fails, S12 ignores status, B3 404 tolerated, Y3.

---

## 5. JWTs the panel signs for Wings

### 5.1 Builder (`panel/app/Services/Nodes/NodeJWTService.php:72-108`)

The library is `lcobucci/jwt` 5.6.0. Setup and validation:

- `Configuration::forSymmetricSigner(new Sha256(), InMemory::plainText($node->daemon_token))`.
- The key is the **decrypted** node token: 64 ASCII chars for new nodes, see §8.
- `InMemory::plainText` rejects an empty key.
- `Hmac::sign` throws `InvalidKeyProvided` when the key is shorter than **256 bits (32 bytes)** (`vendor/lcobucci/jwt/src/Signer/Hmac.php:14-24`, `Hmac/Sha256.php:20-23`). That surfaces as a 500. Nodes migrated from old Pterodactyl had 20-char tokens (`panel/database/migrations/2020_04_10_141024_…php`: `substr(secret, 16)` of a 36-char secret) and **cannot get JWTs** until their token is reset.
- A scope must be set (`Assert::notEmpty`), otherwise `InvalidArgumentException` → 500.

Claims are built in this order (`setClaim` appends new keys):

```
builder(new TimestampDates())                      // custom ClaimsFormatter
  ->issuedBy(config('app.url'))                    // "iss"
  ->permittedFor($node->getConnectionAddress())    // "aud" (stored as array)
  ->identifiedBy($identifier)                      // "jti"
  ->withHeader('jti', $identifier)                 // header "jti"
  ->issuedAt(now())                                // "iat"
  ->canOnlyBeUsedAfter(now()->subMinutes(5))       // "nbf"
  ->expiresAt($this->expiresAt)                    // "exp"
  [->relatedTo($subject)->withHeader('sub', $subject)]   // "sub" claim + header, only if subject set
  ->withClaim(k, v) for each setClaims() entry, in given order
  [->withClaim('user_uuid', $user->uuid)]          // only if setUser() was called
  ->withClaim('scope', implode(' ', scopes))       // all current callers pass exactly one scope
  ->withClaim('unique_id', Str::random())          // 16 chars [A-Za-z0-9]
```

Field details:

- **`$identifier = hash('sha256', $identifiedBy)`**: 64 lowercase hex chars.
- `TimestampDates` (`panel/app/Extensions/Lcobucci/JWT/Encoding/TimestampDates.php:17-29`) replaces `iat`, `nbf` and `exp` with `DateTimeImmutable::getTimestamp()`, i.e. **integer seconds** (microseconds truncated).
- It does **not** apply lcobucci's `UnifyAudience`, so **`aud` stays a JSON array** with one element. Compare `ChainedFormatter::default()`, which would unify it into a string (`vendor/lcobucci/jwt/src/Configuration.php` `builder()`).
- `exp` is computed by the caller **before** `handle()`. `iat` and `nbf` call `now()` separately, so `exp - iat` can be the nominal value or one second less.
- `Str::random(16)`: base64 of `random_bytes` with `/`, `+` and `=` removed, truncated to 16 (`vendor/…/Support/Str.php:1158-1175`).

Encoding (`vendor/lcobucci/jwt/src/Token/Builder.php:143-166`, `Encoding/JoseEncoder.php:23-30`):

- **Header**: `{"typ":"JWT","alg":"HS256","jti":"<hex>"}`, plus `,"sub":"<uuid>"` for transfer tokens. Key order is `typ, alg, jti, sub`.
- JSON flags are `JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE` (compact, `/` not escaped).
- base64url without padding (`SodiumBase64Polyfill`, URL-safe, no padding).
- Signature: `HMAC-SHA256(key, b64(header) + "." + b64(payload))`, base64url without padding.

### 5.2 Token kinds

| Kind | Issued by | Node (signing key and `aud`) | `identifiedBy` (pre-hash) | TTL | Custom claims (order) | `user_uuid` | `scope` | `sub` |
|---|---|---|---|---|---|---|---|---|
| Websocket | client `GET /api/client/servers/{s}/websocket` (`panel/app/Http/Controllers/Api/Client/Servers/WebsocketController.php:38-74`); Filament console widget (`panel/app/Filament/Server/Widgets/ServerConsole.php:50-66`, 10 min); admin node connectivity check (`panel/app/Livewire/NodeClientConnectivity.php:35-58`, **1 min**, first server of the node) | `server.node`. If the server has an active transfer with `archived = true`, `transfer.newNode` instead. `archived` is never set by this panel. | `"{user.id}{server.uuid}"`, e.g. `"1" + uuid` | 10 min | `server_uuid`, `permissions` (§2.1.1 list, possibly the object quirk) | yes | `websocket` | – |
| File download | client `GET …/files/download?file=` (`FileController.php:93-121`); Filament `DownloadFiles` (`panel/app/Filament/Server/Resources/Files/Pages/DownloadFiles.php:34-56`) | `server.node` | same | 15 min | `file_path`, `server_uuid` | yes | `file-download` | – |
| File upload | client `GET …/files/upload` (`FileUploadController.php:32-59`) | `server.node` | same | 15 min | `server_uuid` | yes | `file-upload` | – |
| Backup download | `WingsBackupSchema::getDownloadLink` (`panel/app/Extensions/BackupAdapter/Schemas/WingsBackupSchema.php:44-57`), used by client `GET …/backups/{b}/download` **and** by restore (B2 `download_url`) | `backup.server.node` | `"{user.id}{server.uuid}"` | 15 min | `backup_uuid`, `server_uuid` | yes | `backup-download` | – |
| Transfer | `TransferServerService::handle` (`panel/app/Services/Servers/TransferServerService.php:108-113`) | **`transfer.newNode`** (destination) | `server.uuid` | 15 min | – | **no** | `transfer` | `server.uuid` (claim **and** header) |

Websocket permissions: when the server has an active transfer, the user must hold `admin.websocket.transfer`, else 403 `You do not have permission to view server transfer logs.` (`WebsocketController.php:46-54`).

`file_path` handling:

- Client API: `rawurldecode($request->get('file'))`. Laravel already decoded the query string once, so the value is **decoded twice** (e.g. `?file=a%2520b` yields `a b`).
- Filament: the raw route parameter, not decoded again.

Scope enum: `panel/app/Enums/NodeJwtScope.php:7-11`: `backup-download, file-download, file-upload, transfer, websocket`. These match `wings/router/tokens/token.go:9-15`.

What Wings verifies:

- The HS256 signature with its own token.
- `exp` only (`wings/router/tokens/parser.go:20-30`). `nbf` and `aud` are not checked.
- `scope` must contain the required value.
- `iat` must be after Wings boot and after any deny time (`wings/router/tokens/websocket.go:52-80`).
- `unique_id` is single-use for download and upload tokens.
- Websocket `permissions` must be a JSON array of strings (`wings/router/tokens/websocket.go:84-90`).

### 5.3 URLs returned to clients

`addr = node.getConnectionAddress()` = `{scheme}://{fqdn}:{daemon_connect}` (fqdn and scheme as stored).

| Kind | URL | Client API response body (flags 0, so `/` → `\/`) |
|---|---|---|
| Websocket | `str_replace(['https://','http://'], ['wss://','ws://'], addr) . "/api/servers/{server.uuid}/ws"`. The token is separate. | `{"data":{"token":"<jwt>","socket":"wss:\/\/node.example.com:8080\/api\/servers\/<uuid>\/ws"}}` |
| File download | `addr . "/download/file?token=" . jwt` | `{"object":"signed_url","attributes":{"url":"…"}}` |
| File upload | `addr . "/upload/file?token=" . jwt` | same envelope |
| Backup download | `addr . "/download/backup?token=" . jwt`. For S3 backups it is instead an AWS presigned `GetObject` URL with a 5 min expiry. The command also passes `ContentType: application/x-gzip`, which is not a GetObject input member (`S3BackupSchema.php:56-70`). | same envelope (`BackupController.php:180-197`) |
| Transfer | `newNode.addr . "/api/transfers"`, sent to the old node as `url`, with `token = "Bearer " . jwt` (§4.4 T1) | n/a |

The JWT is appended to the URL unescaped. Its alphabet is URL-safe.

### 5.4 Test vector

Computed with Python, mirroring the PHP encoding rules above. It was not generated by PHP itself; re-verify once a PHP runtime is available.

- key = `0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01`
- user.id = 1
- server uuid = `0b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11`

```
jti     = sha256("10b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11") = 27946d473a27d6dc6f5a7bd8b23516902ae2ef28912a95c6e7ee1962312774be
header  = {"typ":"JWT","alg":"HS256","jti":"27946d473a27d6dc6f5a7bd8b23516902ae2ef28912a95c6e7ee1962312774be"}
payload = {"iss":"https://panel.example.com","aud":["https://node.example.com:8080"],"jti":"27946d473a27d6dc6f5a7bd8b23516902ae2ef28912a95c6e7ee1962312774be","iat":1760000000,"nbf":1759999700,"exp":1760000600,"server_uuid":"0b5c6a0e-3f0e-4c1e-9d55-6c1d7c0a2d11","permissions":["*"],"user_uuid":"5d3c9a1b-7e2f-4b8a-9c0d-1e2f3a4b5c6d","scope":"websocket","unique_id":"AbCdEfGhIjKlMnOp"}
token   = eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImp0aSI6IjI3OTQ2ZDQ3M2EyN2Q2ZGM2ZjVhN2JkOGIyMzUxNjkwMmFlMmVmMjg5MTJhOTVjNmU3ZWUxOTYyMzEyNzc0YmUifQ.eyJpc3MiOiJodHRwczovL3BhbmVsLmV4YW1wbGUuY29tIiwiYXVkIjpbImh0dHBzOi8vbm9kZS5leGFtcGxlLmNvbTo4MDgwIl0sImp0aSI6IjI3OTQ2ZDQ3M2EyN2Q2ZGM2ZjVhN2JkOGIyMzUxNjkwMmFlMmVmMjg5MTJhOTVjNmU3ZWUxOTYyMzEyNzc0YmUiLCJpYXQiOjE3NjAwMDAwMDAsIm5iZiI6MTc1OTk5OTcwMCwiZXhwIjoxNzYwMDAwNjAwLCJzZXJ2ZXJfdXVpZCI6IjBiNWM2YTBlLTNmMGUtNGMxZS05ZDU1LTZjMWQ3YzBhMmQxMSIsInBlcm1pc3Npb25zIjpbIioiXSwidXNlcl91dWlkIjoiNWQzYzlhMWItN2UyZi00YjhhLTljMGQtMWUyZjNhNGI1YzZkIiwic2NvcGUiOiJ3ZWJzb2NrZXQiLCJ1bmlxdWVfaWQiOiJBYkNkRWZHaElqS2xNbk9wIn0.lOQBrookVmRHAOSo1A2TDgYUWo9BQ3xj-Val2j74xzE
```

---

## 6. Node configuration (`Node::getConfiguration`, `panel/app/Models/Node.php:246-290`)

### 6.1 Structure (exact key order)

```php
[
  'debug'    => false,
  'uuid'     => nodes.uuid,                              // string|null (legacy rows)
  'token_id' => $redacted ? '********' : nodes.daemon_token_id,
  'token'    => $redacted ? '********' : decrypt(nodes.daemon_token),   // plaintext secret
  'api' => [
    'host' => '0.0.0.0',
    'port' => nodes.daemon_listen,                       // int cast
    'ssl'  => [
      'enabled' => !nodes.behind_proxy && nodes.scheme === 'https',
      'cert'    => '/etc/letsencrypt/live/' . strtolower(nodes.fqdn) . '/fullchain.pem',
      'key'     => '/etc/letsencrypt/live/' . strtolower(nodes.fqdn) . '/privkey.pem',
    ],
    'upload_limit' => nodes.upload_size,                 // no cast in model; int from PDO
  ],
  'system' => [
    'data' => nodes.daemon_base,
    'sftp' => ['bind_port' => nodes.daemon_sftp],        // int cast
  ],
  'allowed_mounts' => node.mounts->pluck('source'),      // list<string>, DB order, mountables(mountable_type='node')
  'remote' => (string) config('app.url'),
]
```

`Str::lower` is `mb_strtolower`.

Consumers:

1. **Application API** `GET /api/application/nodes/{id}/configuration` (`panel/routes/api-application.php:40`, `panel/app/Http/Controllers/Api/Application/Nodes/NodeConfigurationController.php:21-24`):
   - `new JsonResponse(getConfiguration())`, flags **0**, so `"remote":"https:\/\/panel.example.com"` and the paths are escaped too.
   - Never redacted.
   - Used by `wings configure --panel-url --token --node`. The command line is generated by `panel/app/Services/Nodes/NodeAutoDeployService.php:27-58`: `sudo wings configure --panel-url <app.url> --token <identifier+token> --node <id>[ --allow-insecure]`, or the docker-compose variant.
2. **`POST /api/update`** to Wings (§4.4 Y3): Guzzle JSON, flags 0, not redacted.
3. **Admin UI** "Configuration File" tab: `getYamlConfiguration($operation === 'view')`, so it is **redacted on the view page** and plaintext on the edit page (`panel/app/Filament/Admin/Resources/Nodes/NodeResource.php:621-628`).
4. **CLI** `p:node:configuration {id|uuid} --format=yaml|yml|json` (`panel/app/Console/Commands/Node/NodeConfigurationCommand.php:16-43`):
   - JSON uses `JSON_UNESCAPED_SLASHES|JSON_PRETTY_PRINT` (+ `JSON_THROW_ON_ERROR`) and is not redacted.
   - YAML is not redacted.
   - A newline is written after either.

### 6.2 YAML formatting

`Yaml::dump($cfg, inline: 4, indent: 2, flags: DUMP_EMPTY_ARRAY_AS_SEQUENCE)` uses symfony/yaml 7.4.20 (`vendor/symfony/yaml/Yaml.php:99-104`, `Dumper.php:52-150`, `Inline.php:115-245`, `Escaper.php:55-90`).

Layout:

- Two-space indentation per level. `key: value` lines, every line terminated by `\n`, so the output ends with `\n`.
- Nested maps are block style down to depth 3; deeper levels would be inline `{ … }`, which never happens here.
- A non-empty `allowed_mounts` is a block sequence, with `  - <item>` lines indented 2 under the key.
- An empty `allowed_mounts` is written as `allowed_mounts: []`.

Scalars:

- `true` / `false`.
- `null` is written as `null` (e.g. `uuid: null`).
- Ints are bare.
- Strings follow `Inline::dump`, checked in order:
  1. A numeric string (PHP `is_numeric`, no `\f\n\r\t\v`) → `'…'`.
  2. `""` → `''`.
  3. Non-UTF-8 → `!!binary`.
  4. Contains a control char, `\x7f`, U+0085, U+00A0, U+2028 or U+2029 → double quotes with escapes.
  5. Lower-cases to one of `null ~ true false y n yes no on off`, **or** contains any of `` \s ' " : { } [ ] , & * # ? ``, **or** starts with one of `` - ? | < > = ! % @ ` `` or a Unicode space separator → single quotes (`'` doubled). If the value contains `'` and the double-quoted form is shorter, double quotes are used instead.
  6. Matches `^[0-9]+[_0-9]*$`, `^[+-]?0o[0-7_]++$`, `^0x[0-9a-fA-F_]++$` or the YAML timestamp regex → single quotes.
  7. Otherwise bare.
- Practical consequences:
  - `remote` (contains `:`) is always `'https://…'`.
  - The redacted token is `'********'` (`*`).
  - `0.0.0.0`, UUIDs and plain paths are bare.
  - A random token/token_id that happens to be all digits, or like `1e123…` (numeric), would be single-quoted. That is astronomically rare but the rule must be implemented.

Example (non-redacted, no mounts):

```yaml
debug: false
uuid: 5f0c1a7e-2b0e-4a8e-9a43-0d6f3f1c2b9a
token_id: AbCdEfGh12345678
token: 0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01
api:
  host: 0.0.0.0
  port: 8080
  ssl:
    enabled: true
    cert: /etc/letsencrypt/live/node.example.com/fullchain.pem
    key: /etc/letsencrypt/live/node.example.com/privkey.pem
  upload_limit: 256
system:
  data: /var/lib/pelican/volumes
  sftp:
    bind_port: 2022
allowed_mounts: []
remote: 'https://panel.example.com'
```

With mounts:

```yaml
allowed_mounts:
  - /mnt/shared
  - /srv/data
```

JSON (application API, flags 0):

```json
{"debug":false,"uuid":"5f0c1a7e-2b0e-4a8e-9a43-0d6f3f1c2b9a","token_id":"AbCdEfGh12345678","token":"0123…01","api":{"host":"0.0.0.0","port":8080,"ssl":{"enabled":true,"cert":"\/etc\/letsencrypt\/live\/node.example.com\/fullchain.pem","key":"\/etc\/letsencrypt\/live\/node.example.com\/privkey.pem"},"upload_limit":256},"system":{"data":"\/var\/lib\/pelican\/volumes","sftp":{"bind_port":2022}},"allowed_mounts":[],"remote":"https:\/\/panel.example.com"}
```

### 6.3 Token generation and reset

- On node create (`Node.php:201-209`): `uuid = Str::uuid()`, `daemon_token = Str::random(64)` (stored encrypted), `daemon_token_id = Str::random(16)`.
- On reset (`NodeUpdateService.php:34-37`): both are regenerated with the same lengths.
- Defaults (`Node.php:167-182`): `daemon_base=/var/lib/pelican/volumes`, `daemon_sftp=2022`, `daemon_listen=8080`, `daemon_connect=8080`, `public=true`, `behind_proxy=false`, `maintenance_mode=false`.

---

## 7. State machines relevant to Wings

### 7.1 `servers.status` (`panel/app/Enums/ServerState.php:12-16`; NULL = normal)

Values: `installing`, `install_failed`, `reinstall_failed`, `suspended`, `restoring_backup`.

| From | Event | To | Where |
|---|---|---|---|
| (new) | create | `installing` | `ServerCreationService.php:152`, model default `Server.php:153-157` |
| any | admin reinstall | `installing` (then Wings `/reinstall`) | `ReinstallServerService.php:26-33` |
| `suspended` | Wings install callback (any result) | stays `suspended` | R7 |
| not suspended | Wings install success | NULL | R7 |
| not suspended | Wings install failure, `reinstall=true` | `reinstall_failed` | R7 |
| not suspended | Wings install failure, `reinstall=false` or missing | `install_failed` | R7 |
| any | suspend | `suspended` (then Wings `/sync`) | `SuspensionService.php:44-51`; refused while transferring |
| `suspended` | unsuspend | NULL (then `/sync`) | same |
| NULL only | client/Filament restore | `restoring_backup` (then Wings restore) | `BackupController.php:244-260`, `panel/app/Filament/Server/Resources/Backups/BackupResource.php:189-214` |
| any | Wings restore callback | NULL | R13 |
| `installing` / `restoring_backup` (this node) | Wings boot reset | NULL | R3 |
| installed ↔ `installing` | admin "toggle install" | flips | `panel/app/Services/Servers/ToggleInstallService.php:14` |

Notes:

- `installed_at` is set to `now()` on **every** R7 call, success or failure.
- The derived predicates `isInstalled`, `isSuspended`, `isInConflictState` are at `Server.php:286-299,474-489`.
- "Not installed" means `installing`, `install_failed` or `reinstall_failed`.

### 7.2 Container status (not persisted)

- `ContainerStatus` values: §2.10.
- Sources: the R10 push (cache 1 h) and the S1 poll (cache 15 s). Both use the same cache key `servers.<uuid>.status`.
- Node in maintenance means `missing` (`Server.php:520-532`).

### 7.3 `server_transfers` (`panel/app/Models/ServerTransfer.php`)

Columns: `server_id, old_node, new_node, old_allocation, new_allocation, old_additional_allocations JSON, new_additional_allocations JSON, successful (NULL = in progress, 1 = success, 0 = failure), archived (bool, default 0, never written by this panel)`.

1. **Start** (`TransferServerService.php:64-117`):
   - Viability check.
   - `validateTransferState` (409 if not installed, restoring, or already transferring).
   - In a transaction:
     - Insert the row with `successful = NULL`.
     - If the server has a primary allocation, also set `old_allocation`, `new_allocation`, `old_additional_allocations` (ids ≠ primary) and `new_additional_allocations`, and reserve the new allocations (`UPDATE allocations SET server_id = <server> WHERE id IN (…) AND currently unassigned on the target node`).
   - Then sign the transfer JWT and call T1 on the old node.
2. **Active transfer** = latest row with `successful IS NULL` (`Server.php:423-426`). While it exists:
   - `isInConflictState` is true (SFTP 409 etc.).
   - Both nodes may fetch R5.
   - Websocket tokens require `admin.websocket.transfer`.
3. **Failure** (R8, either node): `successful = 0` and the new allocations are released. Server `node_id`/`allocation_id` are unchanged.
4. **Success** (R9, new node only): the old allocations are released, `allocation_id = new_allocation` (only if old allocations existed), `node_id = new_node`, `successful = 1`, then `DELETE /api/servers/{uuid}` on the old node.
5. **Cancel** (application API): `successful = 1` (**sic**), then S7 to both nodes. Allocations are **not** released.

### 7.4 Backups (`panel/app/Models/Backup.php`)

Derived status (`Backup.php:105-110`):

- `completed_at IS NULL`: in progress.
- `is_successful`: successful.
- Otherwise: failed.

Lifecycle:

- Created by `InitiateBackupService` with `is_successful = false`, `bytes = 0`, `checksum = NULL`, `upload_id = NULL`, `completed_at = NULL`, then B1.
- S3: R11 sets `upload_id`.
- R12 sets `completed_at`, `is_successful`, `checksum`, `bytes` and possibly `is_locked = false`. It is allowed again only while `is_successful = false`.
- Backups not completed within `backups.prune_age` minutes (default 360) are failed by a scheduled job. That is outside the remote API.

---

## 8. `nodes.daemon_token` encryption (Laravel `encrypted` cast)

Decrypt is `Encrypter::decrypt($payload, unserialize: false)` and encrypt is `encrypt($value, serialize: false)` (`vendor/…/Database/Eloquent/Concerns/HasAttributes.php:1456-1471`, `vendor/…/Encryption/Encrypter.php:104-231`):

- Cipher `AES-256-CBC` (`vendor/laravel/framework/config/app.php:127`).
- Key = `APP_KEY` with its `base64:` prefix decoded (32 bytes). Decryption also tries every key in `APP_PREVIOUS_KEYS` (comma-separated).
- Stored value = `base64( json_encode({"iv": b64(iv16), "value": b64(ciphertext PKCS7), "mac": hex(HMAC-SHA256(key, iv_b64 . value_b64)), "tag": ""}, JSON_UNESCAPED_SLASHES) )`. The key order is exactly `iv, value, mac, tag`. `openssl_encrypt` with options 0 already returns base64 for `value`.
- Validation:
  - The payload must be an array with string `iv`, `value` and `mac`, and `tag` a string if present.
  - `base64_decode(iv)` must be 16 bytes.
  - The MAC is compared with `hash_equals` for each key; the first key that verifies is used.
  - Any failure throws `DecryptException` (`The payload is invalid.` / `The MAC is invalid.` / `Could not decrypt the data.`).
- The plaintext is used raw: no PHP `serialize` framing. Legacy rows were re-encrypted unserialized by `panel/database/migrations/2024_05_31_204646_fix_old_encrypted_values.php`.

---

## 9. Activity events written by the remote API (for activity/webhook specs)

| Event | Written by | Subjects (order) | Properties | Actor |
|---|---|---|---|---|
| `auth:sftp.fail` | R1, bad password | user | `{"method":"password"}` | none |
| `server:sftp.denied` | R1, missing `file.sftp` | server | `{}` | user |
| `server:*` (any, from Wings) | R4 | server | Wings `metadata` (+ `original_timestamp`) | user if the uuid matched |
| `server:backup.restore-failed` | R3 | server, backup | `{"name"}` | none |
| `server:backup.complete` / `server:backup.fail` | R12 | backup, server | `{"name"}` | none |
| `server:backup.restore-complete` / **`server.backup.restore-failed`** (typo) | R13 | backup, server | `{"name"}` | none |

---

## 10. Implementation notes for Rust

### 10.1 PHP value and encoding semantics to emulate

- `json_encode(…, 0)`:
  - Escapes `/` as `\/`.
  - Escapes non-ASCII as `\uXXXX`, with UTF-16 surrogate pairs for astral characters.
  - Does **not** escape `<`, `>`, `&` or `'`.
  - Never pretty-prints.
  - Arrays: sequential `0..n-1` keys give a JSON list; otherwise a JSON object, with integer keys written as strings.
- `JSON_PRETTY_PRINT` (error bodies): 4 spaces, `\n`, `": "`. Empty arrays print as `[]` and empty objects as `{}`, without inner newlines.
- PHP arrays turn numeric-string keys (`"123"`, but not `"0123"` or `"1.5"`) into integer keys. `json_decode(…, true)` therefore loses the `{}`/`[]` distinction for empty objects, and for objects keyed `"0".."n-1"`.
- Request JSON decoding uses `json_decode($body, true)` (assoc) for `$request->input()`/`json()`. `EggConfigurationService` decodes egg JSON **non-assoc** (objects preserved). The Rust code must keep that distinction:
  - egg `config_files`: keep JSON objects as objects, including empty ones;
  - egg `config_startup` and model `array` casts: assoc semantics.

### 10.2 R2 normalisation (re-indexing)

For each list element, walk only the nodes that originated from PHP arrays:

- the whole `settings` tree;
- in `process_configuration`: the outer maps, `startup.done`, the `configs` list, each config's top-level map, `replace` lists and their element maps, and `replace_with` values that were JSON lists.

For each such node, if every key is numeric (integers, or numeric strings per PHP `is_numeric`), replace it by its values in order.

Nodes that were `stdClass` (non-assoc decoded egg JSON objects) are left untouched. In practice the only realistic differences come from `labels`, `file_denylist` and `startup.done` stored with non-sequential numeric keys.

### 10.3 Suggested differential test cases

- Auth error matrix:
  - no header
  - `Bearer`
  - `Bearer x`
  - `Bearer a.b.c`
  - `Bearer 0.x`
  - unknown id
  - wrong secret
  - mixed-case id on MySQL
  - comma after the token
  - unknown server UUID without auth (expect 404)
- SFTP:
  - lockout sequence of 5 bad attempts, then a good one → 429 with a decreasing `Retry-After`
  - valid key that is not registered: no increment
  - garbage key: increment
  - username without a dot, username ending with a dot
  - `uuid_short` vs full uuid
  - an admin who is also a subuser → object-encoded permissions
- Server config:
  - egg with `features` NULL, `[]`, unordered lists
  - `config_from` inheritance, including file_denylist inheritance
  - config files with nested objects, `if_value` maps, list-valued finds, `{{config.*}}`, `{{env.X}}`, unknown keys
  - server with no allocation → `{"":[]}`
  - docker_labels / mounts empty vs non-empty
  - `use_binary_prefix` false rounding
- Pagination: `per_page` 0, negative, absent, `""`; `page` 0, -1, beyond the last page; total 0; more than 14 pages (`links` window).
- Activity: timestamps with and without fractional seconds and `Z`, invalid timestamps (`original_timestamp`), empty metadata `{}` → `"[]"`, unknown server, non-`server:` event.
- Transfer success where the old node returns 404 on delete → 500 after commit.

---

## 11. Open questions / uncertain framework behaviour

1. **`array_unique` permission quirk** (§2.1.1). PHP emits a JSON object for admins who are also subusers, and Wings rejects that for both SFTP and websocket. The decision is pending (`docs/decisions`): reproduce it byte-for-byte (strict compatibility) or emit a list. Emitting a list only *fixes* Wings behaviour and cannot break it.
2. **Activity timestamp discard** (§2.4). It looks unintended (the controller parses the timestamp carefully), but the PHP behaviour is that DB `timestamp = now()`. Confirm whether differential tests should compare `timestamp` with tolerance, or whether Rust may store the Wings time.
3. **DB collation**. Token-id, username, uuid and uuid_short lookups inherit the DB's case and trailing-space semantics: MySQL `*_ci`/PAD SPACE vs PostgreSQL/SQLite exact. The Rust implementation should issue the same SQL equality and let the DB decide. Do not normalise case in code.
4. **Laravel event discovery**. `BackupCompletedListener` and `ServerInstalledListener` are not in `EventServiceProvider::$listen`. They are assumed to be registered by Laravel 11+ automatic listener discovery (`Application::configure()->withEvents()`). Not verified at runtime.
5. **AWS SDK presigned URLs** (R11). The exact query parameter set and order (`X-Amz-*`, whether unknown params such as `ContentType`/`StorageClass` on `UploadPart` are silently dropped) depends on aws-sdk-php internals that were not audited here. Wings only uses the URLs opaquely, so byte equality is not required, only SigV4 validity.
6. **PHP 8.5 deprecations**. `strrev(null)` in SFTP username parsing and `ContainerStatus::tryFrom(null)` in R10 are assumed to be *deprecations* (logged, not thrown) under PHP 8.5, as in 8.1–8.4. If 8.5 turned them into `TypeError`s, those paths would be 500 instead of 403 / `missing`.
7. **`upload_size` type**. It has no cast in the model, so the JSON/YAML type depends on the PDO driver returning native ints. That is true for pdo_mysql with emulation off (Laravel default), pgsql, and sqlite on PHP ≥ 8.1. Assumed int.
8. **JWT test vector** (§5.4). Computed in Python from the documented rules, not produced by PHP. Re-verify with a real panel instance.
9. **Wings `Accept`/UA handling**. Verified against `_ref/wings` (`remote/http.go:117-120`, `router/middleware/middleware.go:174`). If production Wings differ from this checkout (e.g. older Pterodactyl-era Wings sending `application/vnd.pterodactyl.v1+json`), `expectsJson()` still holds because of `+json`.
10. **`server:backup.restore-started`**. Never written anywhere in this codebase, so R3's audit-log branch is effectively dead. Keep it for plugin parity.
11. **Exception rendering in `APP_DEBUG`**. Trace frame contents (`meta.trace`) are PHP-specific and cannot be matched. Differential tests should run with `APP_DEBUG=false`.
12. **`Str::lower` on fqdn** uses `mb_strtolower` with full Unicode case mapping. IDN fqdns are assumed to be stored in punycode.
