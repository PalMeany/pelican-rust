# Application (admin) API — compatibility specification

Source of truth: the PHP panel at `panel/` (PHP 8.5, Laravel 13.35.0, Filament 5, spatie/laravel-query-builder 7.3.5, spatie/laravel-data 4.23.0, spatie/laravel-permission 8.3.0, laravel/sanctum 4.3.3). Framework behaviour below was checked against the vendored sources in `.ref/vendor` unless marked **[unverified]**.

All paths in citations are relative to `panel/`. `file:N` means line N, `file:N-M` a range.

> **Important difference from upstream Pelican/Pterodactyl:** this panel has **no Fractal transformers**. The wire format is produced by `App\Data\Api\*` "ApiResource" classes (spatie/laravel-data) and a hand-written envelope (`app/Data/Api/Envelope.php`, `app/Data/Api/PanelResponse.php`, `app/Data/Api/IncludeContext.php`) that reproduces the old Fractal `PanelSerializer` output. "Transformer" in this document means one of these Data classes.

---

## 0. Conventions used in this document

* `{x}` in a path is a route parameter. `{user:id}` means Laravel implicit model binding on column `id`.
* "perm `user:R`" means the API key must have the READ bit for ACL resource `user`; `W` means the WRITE bit (see §1.6).
* "→ 404 (binding)" means the model lookup failed during route-model binding (§1.3, §1.12).
* JSON examples show key order exactly as the PHP panel emits it. Types: `int`, `string`, `bool`, `?T` (nullable), `object`, `list`.
* "DisplayException-400" means an exception class extending `App\Exceptions\DisplayException`, rendered as HTTP 400 (§1.12.4).

---

## 1. Cross-cutting behaviour

### 1.1 Route registration

* Routes: `routes/api-application.php` (all lines cited per endpoint in §3).
* Mounted at prefix `/api/application` with middleware `['api']` → nested `['application-api', 'throttle:api.application']`, and `->scopeBindings()` (`app/Providers/RouteServiceProvider.php:41-46`).
* Laravel route matching happens **before** any route middleware: an unknown path returns 404 `NotFoundHttpException` ("The route api/application/xyz could not be found.") and a known path with the wrong verb returns 405 `MethodNotAllowedHttpException` ("The GET method is not supported for route api/application/users/1/roles/assign. Supported methods: PATCH.") even for unauthenticated requests (`vendor/laravel/framework/src/Illuminate/Routing/AbstractRouteCollection.php:30-47,128-139`). GET routes also answer HEAD.
* There are no route-parameter regex constraints. Non-numeric ids simply fail the binding query → 404.

### 1.2 Effective middleware pipeline (order matters)

Global middleware (Laravel 13 default list, `vendor/.../Foundation/Configuration/Middleware.php:451-475`), in order:
`ValidatePathEncoding`, `InvokeDeferredCallbacks`, `TrustProxies`, `HandleCors`, `PreventRequestsDuringMaintenance`, `ValidatePostSize`, `TrimStrings`, `ConvertEmptyStringsToNull`.

* `TrimStrings`: every string in the query string and in the JSON body (or form body) is trimmed, **except** keys named `password`, `password_confirmation`, `current_password` (`vendor/.../Foundation/Http/Middleware/TrimStrings.php:16-20`, `TransformsRequest.php:30-39`).
* `ConvertEmptyStringsToNull`: every `""` (after trimming) becomes `null`, in query and JSON body. Consequences: `?per_page=` → default page size; `"description": "  "` → `null`.
* CORS: `config/cors.php` (paths `/api/application`, `/api/application/*`; methods GET/POST/PUT/PATCH/DELETE/HEAD; origins from `APP_CORS_ALLOWED_ORIGINS`; `supports_credentials=true`).

Route middleware. `bootstrap/app.php:39-50` *appends* to the default `api` group, whose Laravel-13 default is `[SubstituteBindings]` (`vendor/.../Configuration/Middleware.php:495-499,524-528`). Duplicates are removed keeping the first occurrence (`vendor/.../Routing/Router.php:1472-1487`). `$middleware->priority([SubstituteBindings::class])` (`bootstrap/app.php:68-70`) **replaces** the priority list, so nothing gets reordered. The resulting order is:

| # | Middleware | Effect |
|---|---|---|
| 1 | `SubstituteBindings` | Route model binding. **Runs before authentication**, so an unauthenticated request for a missing id gets **404**, and for an existing id gets 401. |
| 2 | `EnsureStatefulRequests` | Sanctum "stateful" detection (§1.3.3). |
| 3 | `Authenticate:sanctum` | 401 if no user (§1.3). |
| 4 | `IsValidJson` | 400 on malformed JSON body (§1.12.6). |
| 5 | `TrackAPIKey` | Stores the api_key id for activity logs (`app/Http/Middleware/Activity/TrackAPIKey.php:18-27`). |
| 6 | `AuthenticateIPAccess` | 403 if the key's `allowed_ips` doesn't match (§1.4). |
| 7 | `AuthenticateApplicationUser` | 403 if the user isn't an admin (§1.5). |
| 8 | `ThrottleRequests:api.application` | Rate limit (§1.7). |
| — | Controller constructor | Parses `include` (§1.9). (The controller is actually instantiated during middleware gathering, which has no observable effect.) |
| — | FormRequest `authorize()` | 403 if the ACL check fails (§1.6). |
| — | FormRequest validation | 422 (§1.12.3). |
| — | Controller action | |

The status codes therefore take precedence in this order: **404/405 (route) → 404 (binding) → 401 → 400 (bad JSON) → 403 (IP) → 403 (admin) → 429 → 403 (ACL) → 422 → action errors.**

A FormRequest's `prepareForValidation()` runs before `authorize()` (`vendor/.../Validation/ValidatesWhenResolvedTrait.php`, `validateResolved()`), which matters only for `UpdateWebhookRequest` (§3.9).

### 1.3 Authentication

#### 1.3.1 Bearer API keys (the normal case)

* Header: `Authorization: Bearer <token>`. The token is 48 characters: a 16-char **identifier** (prefix `papp_` for application keys or `pacc_` for account/client keys, then random characters, `app/Models/ApiKey.php:213-228`), followed by the 32-char secret.
* Sanctum guard (`vendor/laravel/sanctum/src/Guard.php:30-61`):
  1. It first tries the `web` session guard (§1.3.3). If that yields a user, the token is a `TransientToken`.
  2. Otherwise it reads the bearer token. If the token contains `|`, the part before the `|` must be all digits (`isValidBearerToken`, `Guard.php:97-110`). Pelican tokens never contain `|`.
  3. `ApiKey::findToken` (`app/Models/ApiKey.php:197-208`): `identifier = first 16 chars`, looked up by `api_keys.identifier`; the rest must equal the **decrypted** `token` column (`token` uses the `encrypted` cast, `ApiKey.php:132`). The comparison is a plain `===`, not constant-time.
  4. The key is valid if `expires_at` is null or in the future (`config/sanctum.php:50` sets `expiration=null`). The owning user must use the `HasApiTokens` trait, which `User` does through `HasAccessTokens`.
  5. On success it fires the `TokenAuthenticated` event, then runs `forceFill(['last_used_at' => now()])->save()` on the key. Every authenticated request therefore writes to the DB, runs model validation on the ApiKey (`ValidationObserver`), and fires `eloquent.updated: App\Models\ApiKey`, which can trigger webhooks (§1.13).
* **Any key type works** at this layer. Account keys (`key_type=1`) and application keys (`key_type=2`) both authenticate; what they may then do differs (§1.6).
* On failure: `AuthenticationException("Unauthenticated.")` → **401** (§1.12.1).

#### 1.3.2 Key model (`api_keys`)

Columns used: `id`, `user_id`, `key_type` (0 none, 1 account, 2 application), `identifier` (16 chars, unique), `token` (encrypted), `permissions` (JSON object `{resource: int}`), `allowed_ips` (JSON list of IPs/CIDRs), `memo`, `last_used_at`, `expires_at`, timestamps (`app/Models/ApiKey.php:83-136`). Validation rules: `permissions.*` is an integer 0–3 (`ApiKey.php:110-122`).

#### 1.3.3 Stateful (cookie) path — `EnsureStatefulRequests`

`app/Http/Middleware/EnsureStatefulRequests.php:18-25` plus Sanctum `EnsureFrontendRequestsAreStateful::handle`. A request counts as "frontend" when either:
* its `Referer` (or, failing that, `Origin`) host matches `config('sanctum.stateful')` (default `localhost,localhost:3000,127.0.0.1,127.0.0.1:8000,::1` plus the APP_URL host and port, `config/sanctum.php:19-23`), or
* it carries the panel session cookie.

Frontend requests run through `EncryptCookies`, `AddQueuedCookiesToResponse`, `StartSession`, `ValidateCsrfToken` (`config/sanctum.php:63-66`). **Non-GET requests therefore need a CSRF token. Without one they fail with 419 `TokenMismatchException`, even when they also carry a valid bearer token.** A logged-in session user authenticates with a `TransientToken`.

Recommendation for the Rust port: support bearer keys only. Document the stateful path as unsupported, or emulate the 419 for Referer/Origin-matching non-GET requests if byte-for-byte parity is needed (see Open Questions).

### 1.4 IP allow-list — `AuthenticateIPAccess`

`app/Http/Middleware/Api/AuthenticateIPAccess.php:23-50`
* `TransientToken` requests, and keys with empty `allowed_ips`, pass.
* Otherwise `$request->ip()` (after TrustProxies, `config/trustedproxy.php`) must be inside at least one entry, parsed with `IPTools\Range::parse` (single IPs and CIDR ranges).
* On mismatch it writes an activity log `auth:ip-blocked` (actor and subject = user, subject = key, property `identifier`), then returns **403** `AccessDeniedHttpException`: `This IP address (<ip>) does not have permission to access the API using these credentials.`

### 1.5 Admin gate — `AuthenticateApplicationUser`

`app/Http/Middleware/Api/Application/AuthenticateApplicationUser.php:16-23`: if there is no user, or `!$user->isAdmin()`, the response is **403** `AccessDeniedHttpException`: `This account does not have permission to access the API.`

`User::isAdmin()` (`app/Models/User.php:470-473`) is true when:
* the user has the role named `Root Admin` (`isRootAdmin`, `User.php:465-468`; spatie `hasRole`), **or**
* the user has at least one role **and** at least one permission (`getAllPermissions()->count() >= 1`).

### 1.6 Per-endpoint authorization (ACL)

Every endpoint's FormRequest extends `ApplicationApiRequest` (`app/Http/Requests/Api/Application/ApplicationApiRequest.php:15-55`) and declares `$resource` and `$permission`.

```
AdminAcl::NONE=0, READ=1, WRITE=2        (app/Services/Acl/Api/AdminAcl.php:13-17)
can(p, action)  = (p & action) != 0      (AdminAcl.php:22-29)
check(key, res, action) = can(key.permissions[res] ?? 0, action)   (AdminAcl.php:35-38, ApiKey.php:155-158)
```

* The permission value is a **bitmask**: 1 = read, 2 = write, 3 = read+write. **WRITE (2) does not imply READ**. A key with `"server": 2` can POST but cannot GET.
* `authorize()` runs in this order:
  1. If the token is a `TransientToken` (session): READ needs `can('viewList <res>') && can('view <res>')`, WRITE needs `can('update <res>')`, both via the spatie permission Gate; root admins pass through `Gate::before` (`app/Providers/AppServiceProvider.php:101`).
  2. If the user is a root admin **and** `key_type == ACCOUNT (1)`, it returns `true` (a full bypass).
  3. Otherwise `AdminAcl::check($token, $resource, $permission)`. This applies to application keys **even for root admins**. Account keys of non-root users have an empty `permissions` object and are always denied.
* On denial: `AuthorizationException("This action is unauthorized.")` → **403** `AccessDeniedHttpException` (§1.12.2).

ACL resource names (`app/Models/ApiKey.php:160-172`, plus any registered by plugins via `registerCustomResourceName`):

| ACL key | Constant |
|---|---|
| `server` | `Server::RESOURCE_NAME` |
| `node` | `Node::RESOURCE_NAME` |
| `allocation` | `Allocation::RESOURCE_NAME` |
| `user` | `User::RESOURCE_NAME` |
| `egg` | `Egg::RESOURCE_NAME` |
| `database_host` | `DatabaseHost::RESOURCE_NAME` |
| `server_database` | `Database::RESOURCE_NAME` |
| `mount` | `Mount::RESOURCE_NAME` |
| `role` | `Role::RESOURCE_NAME` |
| `plugin` | `Plugin::RESOURCE_NAME` |
| `webhook` | `WebhookConfiguration::RESOURCE_NAME` |

Per-include checks are separate (§1.9).

### 1.7 Rate limiting

`app/Providers/RouteServiceProvider.php:77-84`: limiter `api.application` = `Limit::perMinutes(config('http.rate_limit.application_period')=1, config('http.rate_limit.application')=256)`, keyed by the user's `uuid` (IP fallback) (`config/http.php:17-18`, env `APP_API_APPLICATION_RATELIMIT_PERIOD` / `APP_API_APPLICATION_RATELIMIT`).
* The limiter runs **after** auth, so 401/403 responses from middleware don't count and carry no rate-limit headers.
* Every response that passes the throttle (including 403/422/404/500 rendered later in the pipeline) gets `X-RateLimit-Limit` and `X-RateLimit-Remaining` headers.
* When exceeded: **429** `ThrottleRequestsException("Too Many Attempts.")`, with headers `Retry-After`, `X-RateLimit-Reset`, `X-RateLimit-Limit`, `X-RateLimit-Remaining: 0` (`vendor/.../Routing/Middleware/ThrottleRequests.php:244-315`).

### 1.8 Response envelopes

Implemented in `app/Data/Api/PanelResponse.php`, `Envelope.php` and `IncludeContext.php`. Controllers return either `array` (Laravel wraps it with `new JsonResponse($array)`, status 200) or `->respond($status)`.

**Item** (`Envelope.php:19-29`):
```json
{"object": "<resource name>", "attributes": { ...transformer fields..., "relationships": {...} }}
```
* `relationships` is present **only** if at least one include was resolved. It is always the **last** key inside `attributes` (`Envelope.php:62-88`).
* An optional top-level `meta` comes after `attributes` (`PanelResponse.php:100-122`).

**Collection** (`Envelope.php:36-52`):
```json
{"object": "list", "data": [ {"object": "...", "attributes": {...}}, ... ]}
```
A collection that isn't paginated has **no** `meta` key at all.

**Paginated collection** (`PanelResponse.php:106-108,161-182`):
```json
{
  "object": "list",
  "data": [ ... ],
  "meta": {
    "pagination": {
      "total": 123,
      "count": 50,
      "per_page": 50,
      "current_page": 1,
      "total_pages": 3,
      "links": {"previous": "<url>", "next": "<url>"}
    }
  }
}
```
* `links` holds `previous` only if `current_page > 1` and `next` only if `current_page < total_pages`; `previous` is inserted first. With no links it is the **empty JSON object `{}`**, never `[]` (`PanelResponse.php:179`).
* `count` is the number of items on this page. `per_page` is `(int)` of the effective page size. `total_pages = max(ceil(total / max(per_page,1)), 1)` (`vendor/.../Pagination/LengthAwarePaginator.php:51-65`).
* Link URLs are `{scheme://host of THE REQUEST}{path}?page=N`, built by `Paginator::currentPathResolver` → `$request->url()` (`vendor/.../Pagination/PaginationState.php:17`). **Only `page` is in the query string. `per_page`, `filter[...]`, `sort` and `include` are NOT carried over** (no `appends()` / `withQueryString()` is called). For the URL builder, see `AbstractPaginator::url()` (`AbstractPaginator.php:182-201`).

**Null resource** (`IncludeContext.php:107-113`):
```json
{"object": "null_resource", "attributes": null}
```

**Raw item** (used only for the database `password` include, `IncludeContext.php:96-102`):
```json
{"object": "database_password", "attributes": {"password": "<plaintext>"}}
```

**`meta.resource`** (store endpoints only, where noted): an absolute URL from `route(...)`. URL generation is forced to the origin `config('app.url')` (`URL::useOrigin(APP_URL)`, `AppServiceProvider.php:61-62`), and to https when APP_URL starts with https. So `meta.resource` uses **APP_URL**, while pagination links use the **request host**.

### 1.9 Includes (`?include=`)

* Parsed in the `ApplicationApiController` constructor (`app/Http/Controllers/Api/Application/ApplicationApiController.php:21-35`): `$request->input('include', [])`, read from **query string or JSON body**. A string is split on `,`; an array (`include[]=a`) is used as-is. Each value is trimmed and empties are dropped. Then `limitRecursion(2)` is applied.
* Normalisation (`PanelResponse.php:136-153`):
  * any `:params` suffix is stripped (`servers:limit(3)` → `servers`);
  * dot-paths are cut to **2 segments** (`a.b.c` → `a.b`);
  * every parent path is implied (`a.b` ⇒ `a` and `a.b`);
  * duplicates are removed;
  * matching is case-sensitive and exact.
* Includes apply to **every** endpoint that returns a transformer, including POST/PATCH responses.
* Only names in the transformer's `$availableIncludes` are honoured. Unknown names are silently ignored (no error).
* Resolution order is the order of `$availableIncludes`, **not** the request order (`Envelope.php:68-81`). So `relationships` keys follow the declaration order listed in §2.
* Each include resolver does its own permission check through `IncludeContext::allowsAdmin($resource)` (`IncludeContext.php:37-49`):
  * the token must be an `ApiKey` with `key_type` ∈ {1 account, 2 application}, otherwise **false**. Session/TransientToken requests therefore **always** get `null_resource` for permission-checked includes;
  * account key → allowed iff the user is a root admin (any resource);
  * application key → `AdminAcl::check(key, resource, READ)`.
* Denied include → `{"object":"null_resource","attributes":null}` in place of the relationship. This is **not** an error.
* To-one includes whose related row is missing also give `null_resource` (`Envelope::item(null)` → `IncludeContext::null()`).
* To-many includes are an unpaginated `{"object":"list","data":[...]}` with no meta.
* Nested includes (`servers.node`) are resolved inside each nested item, with their own permission checks.

### 1.10 Listing: pagination, filters, sorts (spatie/laravel-query-builder 7.3.5)

There is no `config/query-builder.php` in the panel, so package defaults apply: parameter names `filter`, `sort`, delimiter `,`, filter-value splitting on, invalid filter/sort **throws**.

**Page size**: `->paginate($request->query('per_page') ?? DEFAULT)` (exception: webhooks, §3.9). Eloquent `Builder::paginate` (`vendor/.../Eloquent/Builder.php:1167-1183`):
* `per_page` is read from the **query string only**. After `ConvertEmptyStringsToNull`, an empty value means DEFAULT.
* The value is used raw: `perPage ?: Model::$perPage(=15)`. `per_page=0` and other falsy values (`"0"`) become **15**, not the endpoint default.
* `offset = max(0, (int)((page-1)*per_page))`; `limit` is set only if `per_page >= 0` (`vendor/.../Query/Builder.php:3214-3249,3274-3277`). A **negative** `per_page` therefore means *no limit* (all rows returned), and `meta.per_page` reports the negative number. A non-numeric `per_page` (e.g. `abc`) makes PHP 8 throw a `TypeError` on `(page-1)*"abc"` → **500**. A leading-numeric value like `10abc` raises a PHP warning, which becomes an `ErrorException` → **500** **[unverified exact message]**. There is **no upper bound** on per_page (except for webhooks).
* `page` comes from `$request->input('page')` (query or JSON body). Anything that isn't `FILTER_VALIDATE_INT` and `>= 1` becomes 1 (`PaginationState.php:19-27`). Pages past the end return `data: []` with `current_page` equal to the requested page.
* If `total = 0`, no rows query is run: `data: []`, `total_pages: 1`, `links: {}`.
* With no `sort`, there is **no ORDER BY**, so the order is the DB's natural order (in practice ascending PK).

**Filters** (`vendor/spatie/laravel-query-builder/src/Concerns/FiltersQuery.php`, `QueryBuilderRequest.php:117-148`, `AllowedFilter.php`):
* Syntax `?filter[name]=value`.
* A requested filter name that isn't allowed → **400** `InvalidFilterQuery`: ``Requested filter(s) `x, y` are not allowed. Allowed filter(s) are `a, b`.``
* `?filter=scalar` is ignored.
* Values `"true"`/`"false"` become booleans.
* A value containing `,` is split into a list.
* An empty value (null after ConvertEmptyStringsToNull) means the filter is skipped.
* **Partial** (default for string names): `LOWER(<table>.<col>) LIKE '%<lowercased, LIKE-escaped value>%'`, plus `ESCAPE '\'` on sqlite/sqlsrv. A list value becomes OR-ed LIKEs. Booleans are stringified (`true` → `"1"`).
* **Exact**: `<table>.<col> = value`, or `IN (...)` for lists.
* Filters are applied in the order they are allowed, and combined with AND.

**Sorts** (`Concerns/SortsQuery.php`):
* `?sort=id,-name` (leading `-` means DESC); multiple sorts are applied in request order.
* An unknown sort → **400** `InvalidSortQuery`: ``Requested sort(s) `x` is not allowed. Allowed sort(s) are `a, b`.``
* `allowedFilters` runs before `allowedSorts`, so a bad filter is reported first.
* Endpoints that don't call `allowedSorts` ignore `sort` entirely. Endpoints that don't call `allowedFilters` ignore `filter` entirely.

Per-endpoint matrix:

| Endpoint | Default per_page | Filters | Sorts |
|---|---|---|---|
| GET /users | 50 | partial: `email`, `uuid`, `username`, `external_id` | `id`, `uuid` |
| GET /servers | 50 | partial: `uuid`, `uuid_short`, `name`, `description`, `image`, `external_id` | `id`, `uuid` |
| GET /nodes | 50 | partial: `uuid`, `name`, `fqdn`, `daemon_token_id` | `id`, `uuid`, `memory`, `disk`, `cpu` |
| GET /nodes/{node}/allocations | 50 | exact `ip`, exact `port`, partial `ip_alias`, callback `server_id` (§3.2.8) | *(none — `sort` ignored)* |
| GET /database-hosts | **10** | partial: `name`, `host` | `id`, `name`, `host` |
| GET /mounts | 50 | partial: `uuid`, `name` | `id`, `uuid` |
| GET /roles | **10** | partial: `id` (LIKE on the id!), `name` | `id`, `name` |
| GET /plugins | **10** | partial: `id`, `name`, `author`, `category` | `id`, `name`, `author`, `category` |
| GET /webhooks | 50 (validated 1–100) | partial: `name`, `description`, `endpoint`, `type`, `scope`, `server_id` | `id`, `name`, `type`, `created_at` |
| GET /eggs, /nodes/deployable, /servers/{s}/databases, /mounts/{m}/eggs|nodes|servers | *not paginated* | — | — |

### 1.11 Serialization details

**Timestamps.** Every timestamp in a transformer is ISO-8601 with an offset and no fractional seconds. The app timezone is hard-wired to `UTC` (`config/app.php`), so the value is always `YYYY-MM-DDTHH:MM:SS+00:00`. Two code paths produce the same string:
* `ApiResource::formatTimestamp` (`app/Data/Api/ApiResource.php:55-60`) is used by User, Server, Node and Subuser. It takes a `string` parameter: the Carbon value is coerced through `__toString()` (`Y-m-d H:i:s`), re-parsed, set to UTC and printed with `toAtomString()`. A **null** timestamp here causes a TypeError → 500.
* `$carbon->toAtomString()` directly is used by Database, DatabaseHost, Role, Permission and Webhook (the webhook path is null-safe and yields `null`).

**Exception — Eloquent `toArray()` resources.** `EggVariableData`, `ServerVariableData` and `MountData`, and every nested Eloquent date in `NodeData` other than `created_at`/`updated_at`, are serialized by Laravel's `serializeDate` → Carbon `toJSON()` → **`YYYY-MM-DDTHH:MM:SS.000000Z`** (microseconds plus `Z`) (`vendor/.../Eloquent/Concerns/HasAttributes.php:1659-1664`). Egg variables use this format for their `created_at`/`updated_at`.

**JSON encoding flags (byte-level only, semantically irrelevant):**
* Normal responses use `Illuminate\Http\JsonResponse` with options `0`, so `/` is escaped as `\/` and non-ASCII as `\uXXXX`.
* Error responses rendered by `prepareJsonResponse` (404/403/405/400-HttpException/429/500) use `JSON_PRETTY_PRINT|JSON_UNESCAPED_SLASHES` (`vendor/.../Foundation/Exceptions/Handler.php:1118-1126`).
* 401, 422 and DisplayException responses use options `0` (not pretty).

**Empty PHP arrays** serialize as `[]` (e.g. `tags: []`, `headers: []`, egg `config.files` when empty, see §2.5). PHP associative arrays become objects. The pagination `links` value is the only forced `{}`.

**Enums** (`ServerState`, `PluginStatus`, `PluginCategory`, `WebhookScope`) serialize as their string `value`.

**204 responses.** Controllers return `new JsonResponse([], 204)`, `response('', 204)` or `new Response('', 204)`. Symfony strips the body and the `Content-Type` for 204, so the result is always an empty body.

### 1.12 Error format

Produced by `app/Exceptions/Handler.php` on top of `Illuminate\Foundation\Exceptions\Handler::render` (`vendor/.../Foundation/Exceptions/Handler.php:705-733`). The custom `Handler::render` first rolls back any open DB transaction (`app/Exceptions/Handler.php:237-253`).

Generic error object (`Handler::exceptionToArray`, `app/Exceptions/Handler.php:308-349`):
```json
{"errors": [{"code": "<class basename>", "status": "<HTTP status as STRING>", "detail": "<message>"}]}
```
* `code` = `class_basename($e)` **after** Laravel's `prepareException` mapping (`vendor/.../Handler.php:769-785`): `ModelNotFoundException` → `NotFoundHttpException`, `AuthorizationException` → `AccessDeniedHttpException`, `TokenMismatchException` → `HttpException` (419).
* `status` = `strval($e->getStatusCode())` if the method exists, else from the map {AuthenticationException:401, AuthorizationException:403, ValidationException:422}, else `"500"`.
* `detail` = `$e->getMessage()` if the exception implements `HttpExceptionInterface` or is in that map. Otherwise it is the fixed text `An unexpected error was encountered while processing this request, please try again.`.
* If the exception, **or its previous exception**, is a `ModelNotFoundException`, `detail` becomes `The requested resource could not be found on the server.`.
* With `APP_DEBUG=true`, `detail` becomes the raw message, and `source:{line,file}` and `meta:{trace:[...], previous:[...]}` are added. Production parity should assume debug **off**.

**JSON vs HTML.** Every JSON rendering below happens only if `$request->expectsJson()` (an `Accept` header whose first type contains `/json` or `+json`, or an XHR-with-accept-any request). Without it:
* 401 → 302 redirect to the Filament login;
* 422 → 302 back with flashed errors;
* DisplayException → 302 back;
* other errors → HTML error pages.

Nothing in this panel forces JSON for `/api/*`. **The Rust port should always answer JSON (assume clients send `Accept: application/json`) and document this divergence.**

#### 1.12.1 401
`Handler::unauthenticated` (`app/Exceptions/Handler.php:375-382`), not pretty-printed:
```json
{"errors":[{"code":"AuthenticationException","status":"401","detail":"Unauthenticated."}]}
```

#### 1.12.2 403
* ACL failure: `{"errors":[{"code":"AccessDeniedHttpException","status":"403","detail":"This action is unauthorized."}]}`
* Admin gate: same `code`/`status`, detail `This account does not have permission to access the API.`
* IP gate: same `code`/`status`, detail `This IP address (1.2.3.4) does not have permission to access the API using these credentials.`
* Special case: `DELETE /users/{id}` on a root admin returns **403 with body `[]`** (§3.1.8).

#### 1.12.3 422 (validation)
`Handler::invalidJson` (`app/Exceptions/Handler.php:261-296`). Each failing rule message is its own error object:
```json
{"errors":[
  {"code":"ValidationException","status":"422","detail":"The name field is required.","meta":{"source_field":"name","rule":"required"}},
  {"code":"ValidationException","status":"422","detail":"The limits.memory must be a number.","meta":{"source_field":"limits.memory","rule":"numeric"}}
]}
```
* `source_field` is the dotted attribute key (wildcards expanded, e.g. `roles.0`).
* `rule` is `Str::snake(<RuleName>)` of the n-th failed rule for that field (`Required` → `required`, `AlphaDash` → `alpha_dash`, `RequiredWith` → `required_with`). Rule objects that stringify (`In`, `Unique`, `Exists`, `NotIn`) report their string rule name. Closure/class rules report a snake-cased FQCN (e.g. `illuminate\_validation\_closure_validation_rule`); the `App\_rules\_` → `p_` replacement can never match because the snake-cased string starts with lowercase `app` **[minor]**. For errors added in `after()` hooks (webhooks), `rule` is `null`.
* Errors are ordered by field (rule-definition order), then by message.
* Messages come from `lang/en/validation.php`, with Laravel's framework defaults filling missing keys **[unverified merge]**. Custom attribute names are listed per endpoint.
* Service-level `ValidationException`s (egg variable validation, §3.3.4) use the same format, with `source_field` `environment.<ENV_NAME>` and the attribute display name `<Variable Name> variable` (`lang/en/validation.php:97-98`).

#### 1.12.4 400
* `DisplayException` subclasses (`app/Exceptions/DisplayException.php:42-77`) render **themselves**: `response()->json(Handler::toArray($e), 400)`, not pretty. Example:
  `{"errors":[{"code":"HasActiveServersException","status":"400","detail":"A node must have no servers linked to it in order to be deleted."}]}`
* `BadRequestHttpException` (malformed JSON), `InvalidFilterQuery` and `InvalidSortQuery` are pretty-printed; the shape is identical.

#### 1.12.5 404
* Binding miss or `firstOrFail`/`findOrFail`: `{"errors":[{"code":"NotFoundHttpException","status":"404","detail":"The requested resource could not be found on the server."}]}`
* Unknown route: same code, detail `The route api/application/foo could not be found.`

#### 1.12.6 Others
* Malformed JSON (`IsValidJson`, `app/Http/Middleware/Api/IsValidJson.php:17-28`): this applies only when the `Content-Type` is JSON (`isJson()`) and the body is non-empty. Response: **400** `BadRequestHttpException`, detail `The JSON data passed in the request appears to be malformed: <json_last_error_msg, e.g. "Syntax error">`.
* 405 `MethodNotAllowedHttpException` includes an `Allow` header.
* 406: either `NodeNotViableException` (NotAcceptableHttpException), detail `The node <name> does not have enough free resources to accept this server.`, or a bare `new Response('', 406)` with an empty body (transfer cancel).
* 409: `ConflictHttpException` / `ServerStateConflictException` (messages in `lang/en/exceptions.php:60-68`).
* 429: see §1.7. Body `{"errors":[{"code":"ThrottleRequestsException","status":"429","detail":"Too Many Attempts."}]}`.
* 500: any other exception, e.g. `PanelException`, `QueryException`, `ConnectionException`, `RequestException`, `TypeError`, `ErrorException`, `InvalidArgumentException` (spatie `RoleAlreadyExists`), `JsonException`. The detail is always the generic text. **`DataValidationException`** (model-level validation failure on save, `app/Exceptions/Model/DataValidationException.php`) is the exception: it is `HttpExceptionInterface` with status 500, so its `detail` **is** the message `Could not save App\Models\X[<id>]: failed to validate data: {"field":["msg"]}`.

### 1.13 Implicit side effects common to all writes

* **Model validation on save.** Models using `HasValidation` (User, Node, Server, Egg, EggVariable, Allocation, Database, DatabaseHost, Mount, ServerVariable, Subuser, ApiKey, …) re-validate all attributes against `$validationRules` in a `saving` observer (`app/Traits/HasValidation.php:17,87-107`, `app/Observers/ValidationObserver.php:12-19`). A failure is a `DataValidationException` → 500 (§1.12.6).
* **Webhooks.** `app/Providers/EventServiceProvider.php` routes `eloquent.created*`, `eloquent.updated*`, `eloquent.deleted*`, `App\Events\*` and `ActivityLogged` to `App\Listeners\DispatchWebhooks`. For every model create/update/delete, global-scope webhook configurations subscribed to `eloquent.<action>: App\Models\<Model>` get a queued `ProcessWebhook` job (`app/Listeners/DispatchWebhooks.php`). This includes the `ApiKey` `last_used_at` update on every request.
* **Activity logs** are written only where noted (server database create/delete, IP block, allocation primary change).
* **Transactions.** The Handler rolls back all open transactions when an exception is rendered (`app/Exceptions/Handler.php:248-250`).

### 1.14 Wings (daemon) HTTP client conventions

* `Http::daemon($node)` (`app/Providers/AppServiceProvider.php:65-75`) sends JSON (`Accept` and `Content-Type: application/json`) with `Authorization: Bearer <node.daemon_token>` (decrypted) to base URL `{scheme}://{fqdn}:{daemon_connect}`. Timeout `GUZZLE_TIMEOUT` = 15 s, connect timeout 5 s; TLS verification only in production.
* `DaemonRepository::getHttpClient()` (`app/Repositories/Daemon/DaemonRepository.php:48-67`) adds `throwIf(callback)`. On a **non-2xx** response the callback checks the `User-Agent` header: if it is empty, or matches `^Pelican Wings/v(x.y.z|develop) (id:<id>)$` with `<id> ≠ node.daemon_token_id`, it throws `ConnectionException("<uri> does not match node token_id !")`. Otherwise `RequestException` is thrown. 2xx responses are never checked. Connection failures throw `ConnectionException`.
* The distinction matters because several services catch **only** `ConnectionException`.

---

## 2. Transformers (wire resources)

All live in `app/Data/Api/Application/`. Base class: `app/Data/Api/ApiResource.php`. `toArray()` emits constructor properties in declaration order, with `#[MapOutputName]` applied. NodeData, MountData, EggVariableData and ServerVariableData override `toArray()` to return an Eloquent `toArray()`.

### 2.1 UserData — object `user`
`app/Data/Api/Application/UserData.php`

| # | key | type | source |
|---|---|---|---|
| 1 | `id` | int | |
| 2 | `external_id` | ?string | |
| 3 | `is_managed_externally` | bool | |
| 4 | `uuid` | string | |
| 5 | `username` | string | always lowercase (saving hook, `User.php:228-231`) |
| 6 | `email` | string | always lowercase |
| 7 | `language` | ?string | |
| 8 | `root_admin` | bool | `isRootAdmin()` |
| 9 | `2fa_enabled` | bool | `filled(mfa_app_secret)` |
| 10 | `2fa` | bool | same value (deprecated) |
| 11 | `created_at` | string | formatTimestamp |
| 12 | `updated_at` | string | formatTimestamp |

Includes (in this order): `servers` → list of `server` (perm `server`), `roles` → list of `role` (perm `role`) (`UserData.php:60-82`).

### 2.2 ServerData — object `server`
`app/Data/Api/Application/ServerData.php:33-100`

```json
{
  "id": 1, "external_id": null, "uuid": "…", "identifier": "abcd1234",
  "name": "…", "description": "",
  "status": null,               // ?string: installing|install_failed|reinstall_failed|suspended|restoring_backup|null
  "suspended": false,           // status == suspended (deprecated)
  "limits": {"memory": 1024, "swap": 0, "disk": 5120, "io": 500, "cpu": 100,
             "threads": null,   // ?string e.g. "0-2"
             "oom_disabled": true, "oom_killer": false},
  "feature_limits": {"databases": 0, "allocations": 0, "backups": 0},   // each ?int
  "user": 1, "node": 1,
  "allocation": 1,              // ?int (primary allocation id)
  "egg": 1,
  "container": {
    "startup_command": "…", "image": "…",
    "installed": 1,             // int 0/1: 1 unless status ∈ {installing, install_failed, reinstall_failed}
    "environment": {"VAR": "value", …, "STARTUP": "<startup>", "P_SERVER_UUID": "<uuid>"}
  },
  "updated_at": "…+00:00", "created_at": "…+00:00"     // NOTE: updated_at BEFORE created_at
}
```
* `identifier` = `uuid_short`. `user` = `owner_id`. `installed` comes from `Server::isInstalled()` (`app/Models/Server.php` `isInstalled`).
* `environment` comes from `EnvironmentService::handle` (`app/Services/Servers/EnvironmentService.php:37-56`). It is built from the egg's variables in DB order (relation `Server::variables`, `Server.php:365`, with no ORDER BY): `env_variable → server_value ?? default_value`, followed by `STARTUP` = `servers.startup` and `P_SERVER_UUID` = uuid. Values may be `null` or `""`. Because `STARTUP` and `P_SERVER_UUID` are always present, this is always a JSON object.

Includes, in order (`ServerData.php:102-169`):

| include | shape | object key | perm |
|---|---|---|---|
| `allocations` | list | `allocation` | `allocation` |
| `user` | item | `user` | `user` |
| `subusers` | list | **`subuser`** | `user` |
| `egg` | item | `egg` | `egg` |
| `variables` | list | **`server_variable`** (ServerVariableData) | `server` |
| `node` | item | `node` | `node` |
| `databases` | list | **`databases`** (ServerDatabaseData) | `server_database` |

The `variables` include reuses the relation already lazy-loaded by `EnvironmentService` (the `server_variables` join is bound to the *real* server id). Eager-loading that relation from a fresh model would join on `server_id IS NULL` (`Server.php:365-377`). The Rust port must emit correct per-server values.

### 2.3 NodeData — object `node`
`app/Data/Api/Application/NodeData.php:26-44`. Attributes are the node model's `toArray()`, with these rules:
* Hidden: `daemon_token`, `daemon_token_id` (`Node.php:125`).
* Casts: `memory`, `disk`, `cpu`, `daemon_listen`, `daemon_connect` and `daemon_sftp` are int; `behind_proxy`, `public` and `maintenance_mode` are bool; `tags` is an array (`Node.php:184-199`). Uncast integer columns (`memory_overallocate`, `disk_overallocate`, `cpu_overallocate`, `upload_size`) come back from PDO as int.
* `created_at` / `updated_at` are replaced **in place** by formatTimestamp.
* `allocated_resources: {memory:int, disk:int, cpu:int}` is appended last. It is the sum over the node's servers (0 when there are none).
* **Any loaded Eloquent relation or extra selected column is also emitted** (see below).

Key order on GET (fresh row from the DB) follows the **physical column order**, which differs by engine. This was derived from the migrations and confirmed against the dumps in `pelican-rs/crates/pelican-db/schema/{mysql,mariadb,sqlite,pgsql}.sql`.

* **MySQL / MariaDB:**
  `id, uuid, public, name, description, fqdn, scheme, behind_proxy, maintenance_mode, memory, memory_overallocate, disk, disk_overallocate, cpu, cpu_overallocate, upload_size, daemon_listen, daemon_sftp, daemon_sftp_alias, daemon_base, created_at, updated_at, tags, daemon_connect, allocated_resources`
* **SQLite** (the panel's default `DB_CONNECTION`, `config/database.php:27`) **and PostgreSQL** (pelican-rs's `pgsql.sql`):
  `id, public, name, fqdn, scheme, memory, memory_overallocate, disk, disk_overallocate, daemon_listen, daemon_sftp, daemon_base, created_at, updated_at, upload_size, behind_proxy, description, maintenance_mode, uuid, tags, cpu, cpu_overallocate, daemon_sftp_alias, daemon_connect, allocated_resources`

Types: `id` int, `uuid` string, `public` bool, `name` string, `description` ?string, `fqdn` string, `scheme` `"http"|"https"`, `behind_proxy` bool, `maintenance_mode` bool, `memory`/`memory_overallocate`/`disk`/`disk_overallocate`/`cpu`/`cpu_overallocate`/`upload_size` int, `daemon_listen`/`daemon_sftp`/`daemon_connect` int, `daemon_sftp_alias` ?string, `daemon_base` string, `tags` list of strings.

Context-dependent extra or missing keys. The Rust port should reproduce these, or consciously drop them as a documented divergence:
1. `GET /nodes/deployable` adds `servers_sum_memory`, `servers_sum_disk`, `servers_sum_cpu` after the table columns (before `allocated_resources`). Each is `null` when the node has no servers. Otherwise it is an int on SQLite, and **a numeric string on MySQL** (SUM returns DECIMAL) **[driver-dependent]** (`app/Services/Deployment/FindViableNodesService.php:24-29`).
2. Nodes loaded through many-to-many relations carry a `pivot` object:
   * via mount (`GET /mounts/{m}/nodes`, `?include=nodes` on mount): `"pivot": {"mount_id": int, "mountable_id": int, "mountable_type": "node"}` (`vendor/.../Relations/MorphToMany.php:181-191`);
   * via database host (`?include=nodes`): `"pivot": {"database_host_id": int, "node_id": int}` (`BelongsToMany.php:1038-1048`).

   It is emitted after the table columns and before `allocated_resources`.
3. `PATCH /nodes/{id}` (success path) and `POST /nodes` emit keys in a different order and/or subset (see §3.2.5 and §3.2.6).

Includes: `allocations` → list `allocation` (perm `allocation`); `servers` → list `server` (perm `server`).

### 2.4 AllocationData — object `allocation`
`app/Data/Api/Application/AllocationData.php:16-40`

| key | type | notes |
|---|---|---|
| `id` | int | |
| `ip` | string | |
| `alias` | ?string | raw `ip_alias` |
| `port` | int | |
| `notes` | ?string | |
| `assigned` | bool | `server_id !== null` |

Includes: `node` → item `node` (perm `node`); `server` → item `server` (perm `server`; `null_resource` when unassigned).

### 2.5 EggData — object `egg`
`app/Data/Api/Application/EggData.php:24-87`

| # | key | type | source |
|---|---|---|---|
| 1 | `id` | int | |
| 2 | `uuid` | string | |
| 3 | `name` | string | |
| 4 | `author` | ?string | |
| 5 | `description` | ?string | |
| 6 | `icon` | ?string | public-disk URL `{APP_URL}/storage/icons/egg/{uuid}.{png|jpg|webp}` if the file exists (`app/Models/Traits/HasIcon.php:30-40`) **[public disk URL config unverified]** |
| 7 | `features` | ?list | |
| 8 | `tags` | list | |
| 9 | `docker_image` | string | first value of `docker_images` or `""` (deprecated) |
| 10 | `docker_images` | object | map display-name → image (a list for legacy rows) |
| 11 | `config` | object | see below |
| 12 | `startup` | string | first value of `startup_commands` or `""` (deprecated) |
| 13 | `startup_commands` | object | map name → command |
| 14 | `script` | object | see below |
| 15 | `created_at` | string | |
| 16 | `updated_at` | string | |

`config` is `{files, startup, logs, stop, file_denylist, extends}`, using values inherited from `config_from` when the egg's own are null:
* `files` = `json_decode(inherit_config_files ?: '{}', true, THROW)`;
* `startup` / `logs` = `json_decode(... ?: '{}', true)` (invalid JSON → `null`);
* `stop` = ?string;
* `file_denylist` = ?list;
* `extends` = `config_from` (?int).

`{}` decoded with assoc=true is an **empty PHP array**, so empty `files`, `startup` and `logs` serialize as **`[]`**, not `{}`.

`script` is `{privileged: bool, install: ?string, entry: string, container: string, extends: ?int}`, using values copied from `copy_script_from` when the egg's own are empty (`app/Models/Egg.php` `getCopyScript*Attribute`).

Includes: `servers` → list `server` (perm `server`); `variables` → list **`egg_variable`** (EggVariableData, perm `egg`).

### 2.6 EggVariableData (egg variables) / ServerVariableData (server variables)
`app/Data/Api/Application/EggVariableData.php`, `ServerVariableData.php`. Both are the Eloquent `toArray()` of `EggVariable` (`app/Models/EggVariable.php:105-115`).
* Casts: `egg_id` int; `user_viewable` and `user_editable` bool; `rules` a list of strings. `created_at`/`updated_at` are `immutable_datetime` and serialize as **`YYYY-MM-DDTHH:MM:SS.000000Z`** (or null).
* Key order on MySQL/MariaDB: `id, egg_id, sort, name, description, env_variable, default_value, user_viewable, user_editable, rules, created_at, updated_at`. On SQLite and PostgreSQL `sort` is the **last** table column (confirmed in the schema dumps).
* ServerVariableData (via `Server::variables`, which selects `egg_variables.*, server_variables.variable_value as server_value`) appends **`server_value`** (?string) after the table columns.
* The default object name is `egg` for EggVariableData and `egg_variable` for ServerVariableData, but they are only ever rendered with an override key: `egg_variable` (egg include), `server_variable` (server include) and `variable` (parent include).
* ServerVariableData include `parent` → item **`variable`**: the *same* variable rendered again with EggVariableData (still including `server_value`), perm `egg`.

### 2.7 ServerDatabaseData — object `server_database`
`app/Data/Api/Application/ServerDatabaseData.php:15-45`

| key | type |
|---|---|
| `id` | int |
| `server` | int (server_id) |
| `host` | int (database_host_id) |
| `database` | string |
| `username` | string |
| `remote` | string |
| `max_connections` | ?int |
| `created_at` | string (ATOM) |
| `updated_at` | string (ATOM) |

Includes:
* `password` → raw item `database_password` `{password: <decrypted plaintext>}`. **No permission check.**
* `host` → item `database_host` (perm `database_host`).

### 2.8 DatabaseHostData — object `database_host`
`app/Data/Api/Application/DatabaseHostData.php:19-45`: `id` int, `name` string, `host` string, `port` int, `username` string, `created_at`, `updated_at` (ATOM). `password` and `max_databases` are never exposed.

Includes: `databases` → list `server_database` (perm `server_database`); `nodes` → list `node` (perm `node`; carries `pivot`, §2.3).

### 2.9 MountData — object `mount`
`app/Data/Api/Application/MountData.php`. The Eloquent `toArray()` of `Mount`, which has no timestamps (`Mount.php:98`). Same order on both engines: `id` int, `uuid` string, `name` string, `description` ?string, `source` string, `target` string, `read_only` bool, `user_mountable` bool.

Includes: `eggs` → list `egg` (perm `egg`); `nodes` → list `node` (perm `node`, with `pivot`); `servers` → list `server` (perm `server`).

### 2.10 RoleData — object `role` / RolePermissionData — object `permissions`
`RoleData.php:14-34`: `id` int, `name` string, `created_at`, `updated_at` (ATOM).

Include `permissions` → list **`permissions`** of `{name: string, created_at, updated_at}` (`RolePermissionData.php:10-28`). **No permission check.**

### 2.11 PluginData — object `plugin`
`app/Data/Api/Application/PluginData.php:14-62`

```json
{"id":"string","name":"string","author":"string","version":"string","description":null,
 "category":"plugin|theme|language","url":null,"update_url":null,
 "namespace":"string","class":"string",
 "panels":["admin","app"] | null,          // explode(',', panels)
 "panel_version":null,
 "composer_packages":{...} | null,         // json_decode
 "meta":{"status":"not_installed|disabled|enabled|errored|incompatible|crashed",
         "status_message":null,"load_order":0,
         "is_compatible":true,"update_available":false,"can_enable":false,"can_disable":false}}
```
* `update_available` triggers an outbound HTTP GET to `update_url` (cached 10 min, `app/Models/Plugin.php:290-358`).
* Plugin rows come from `plugins/*/plugin.json` (a Sushi in-memory model, `Plugin.php:128-212`).
* No includes.

### 2.12 SubuserData — object `server_subuser` (rendered as `subuser` in the server include)
`SubuserData.php:16-40`: `id` int, `user_id` int, `server_id` int, `permissions` list of strings, `created_at`, `updated_at` (formatTimestamp).

Includes: `user` → item `user` (perm `user`); `server` → item `server` (perm `server`).

### 2.13 WebhookConfigurationData — object `webhook`
`WebhookConfigurationData.php:24-63`

| key | type | notes |
|---|---|---|
| `id` | int | |
| `name` | ?string | |
| `description` | ?string | |
| `scope` | `"global"\|"server"` | |
| `server_id` | ?int | |
| `type` | ?string | e.g. `regular` |
| `type_available` | bool | the type's schema is registered (plugin types can disappear) |
| `endpoint` | string | |
| `events` | list | |
| `payload` | object\|list\|null | |
| `headers` | object\|list\|null | |
| `created_at` | ?string | ATOM |
| `updated_at` | ?string | ATOM |

Includes:
* `server` → item `server` (perm `server`; `null_resource` if `server_id` is null).
* `deliveries` → list `webhook_delivery`: the **latest 25** by `id DESC`. **No permission check.**

### 2.14 WebhookDeliveryData — object `webhook_delivery`
`WebhookDeliveryData.php:10-40`: `id` int, `webhook_configuration_id` int, `event` string, `endpoint` string, `payload` (array), `successful` bool (`successful_at !== null`), `successful_at` ?string, `created_at` ?string, `updated_at` ?string (ATOM).

### 2.15 Resources that do not exist in the Application API
There are no Location, Backup, Schedule or ApiKey transformers or endpoints in the application API. Subusers exist only as a server include. (`App\Data\Api\Client\*` are client-API only.)

---

## 3. Endpoints

All paths are relative to `/api/application`. Unless stated otherwise, success responses are `200` with the envelope from §1.8. Every endpoint can additionally return the cross-cutting errors in §1.2's precedence list.

### 3.1 Users — `routes/api-application.php:14-26`

#### 3.1.1 `GET /users` — list
* Perm `user:R` (`GetUsersRequest`). Controller `UserController::index` (`app/Http/Controllers/Api/Application/Users/UserController.php:44-54`).
* QueryBuilder over `users`: filters/sorts per §1.10, default per_page 50.
* 200: paginated list of `user`.

#### 3.1.2 `GET /users/{user:id}`
* Perm `user:R`. → 404 (binding).
* 200: item `user` (`UserController.php:64-69`).

#### 3.1.3 `GET /users/external/{external_id}`
* Perm `user:R` (`GetExternalUserRequest`).
* `User::where('external_id', $externalId)->firstOrFail()` → 404 (`ExternalUserController.php:21-28`).
* 200: item `user`.

#### 3.1.4 `POST /users` — create
* Perm `user:W` (`StoreUserRequest`, `app/Http/Requests/Api/Application/Users/StoreUserRequest.php:33-54`).
* Rules (the subset of `User::getRules()`, `app/Models/User.php:183-205,246-254`):
  * `external_id`: `sometimes|nullable|string|max:255|unique:users,external_id`
  * `is_managed_externally`: `boolean`
  * `email`: `required|email|between:1,255|unique:users,email`
  * `username`: `required|between:1,255|unique:users,username`
  * `password`: `sometimes|nullable|string` (not trimmed)
  * `language`: `string|in:<ICU locales: ResourceBundle::getLocales('')>`
  * `timezone`: `string|in:<DateTimeZone::listIdentifiers()>`
* Display names: `external_id` → "Third Party Identifier", `is_managed_externally` → "Is managed by Third Party?".
* Logic (`UserCreationService::handle`, `app/Services/Users/UserCreationService.php:33-74`):
  1. A non-empty password is bcrypt-hashed.
  2. BEGIN transaction. If the password is empty, it is set to the hash of 30 random chars and a password-reset token is created later.
  3. `root_admin` is not reachable through the API (not in the rules).
  4. `User::forceCreate(validated + uuid=v4)`. The model `saving` hook lowercases username/email (`User.php:228-231`); the `creating` hook defaults `timezone` (`User.php:221-226`); model defaults are `language='en'`, `is_managed_externally=false`, `external_id=null`.
  5. Reset token via the Filament `app` panel password broker (inserts into `password_resets`).
  6. COMMIT.
  7. If `config('panel.email.send_account_created_notification')` (default true), queue the `AccountCreated` notification (email, includes the reset token).
* Events: `eloquent.created: App\Models\User` (webhooks).
* 201:
  ```json
  {"object":"user","attributes":{…UserData…},"meta":{"resource":"<APP_URL>/api/application/users/<id>"}}
  ```
* Errors: 422. Case-variant duplicates can pass the request `unique` rule on case-sensitive collations and then fail model validation after lowercasing → 500 `DataValidationException`.

#### 3.1.5 `PATCH /users/{user:id}` — update
* Perm `user:W` (`UpdateUserRequest`, `UpdateUserRequest.php:22-33`).
* Rules: the same keys as §3.1.4, from `User::getRulesForUpdate($user)`: `unique` rules ignore this user's id (`app/Traits/HasValidation.php:57-80`). **`email` and `username` remain `required`**, so this is effectively a full update of those two fields.
* Logic (`UserController.php:85-94`, `UserUpdateService::handle`, `app/Services/Users/UserUpdateService.php:24-39`):
  1. If the password is non-empty it is hashed, otherwise the key is removed.
  2. `forceFill(validated)->saveOrFail()`.
  3. If a password was set, the `PasswordChanged` event fires → `RevocationListener` queues a `RevokeSftpAccessJob` per node the user has servers on (Wings `POST /api/deauthorize-user`, `DaemonServerRepository::deauthorize`).
  4. `refresh()`.
* 200: item `user`.

#### 3.1.6 `PATCH /users/{user:id}/roles/assign`
* Perm `user:W` (`AssignUserRolesRequest`, which extends StoreUserRequest).
* Rules: `roles: required|array`, `roles.*: integer|exists:roles,id` (`AssignUserRolesRequest.php:8-16`).
* Logic (`UserController.php:103-120`):
  * If the target user **is** a root admin, nothing happens.
  * Otherwise `Role::getRootAdmin()`, which is `findOrCreate('Root Admin','web')` and **may create the role**. For each `role` in the input: skip it if `$role === rootAdminId` (strict int comparison — a **string** `"1"` passes `integer` validation and bypasses this guard), else `$user->assignRole($role)` (spatie: attaches the roles not already held).
* 200: item `user`.

#### 3.1.7 `PATCH /users/{user:id}/roles/remove`
* Same permission, rules and guards; uses `$user->removeRole($role)` (`UserController.php:129-146`).
* 200: item `user`.

#### 3.1.8 `DELETE /users/{user:id}`
* Perm `user:W` (`DeleteUserRequest`).
* Logic (`UserController.php:176-185`):
  * If the target is a root admin → **403 with body `[]`** (`new JsonResponse([], 403)`).
  * Otherwise `$user->delete()`. The model `deleting` hook (`User.php:233-239`) throws DisplayException-400 `Cannot delete a user with active servers attached to their account. Please delete their servers before continuing.` if the user owns servers, and `Cannot delete your own user account.` if the target is the API key's owner. It then fires the `User\Deleting` event (RevocationListener → deauthorize jobs).
* 204 (empty).

### 3.2 Nodes & allocations — `routes/api-application.php:36-52`

#### 3.2.1 `GET /nodes`
* Perm `node:R`. `NodeController::index` (`app/Http/Controllers/Api/Application/Nodes/NodeController.php:43-53`).
* 200: paginated list of `node` (§1.10).

#### 3.2.2 `GET /nodes/deployable`
* Perm `node:R` (`GetDeployableNodesRequest` extends GetNodesRequest).
* Rules (`GetDeployableNodesRequest.php:7-21`): `page: integer`, `memory: required|integer|min:0`, `disk: required|integer|min:0`, `cpu: sometimes|integer|min:0`, `tags: sometimes|array`. These are normally sent as query parameters (`?memory=512&disk=1024&tags[]=eu`).
* Logic (`NodeDeploymentController.php:28-42`, `FindViableNodesService::handle`, `app/Services/Deployment/FindViableNodesService.php:22-34`):
  1. Load all nodes with `public = true`, with `withSum(servers.memory)`, `withSum(servers.disk)`, `withSum(servers.cpu)`.
  2. If `tags` is non-empty, keep nodes sharing **at least one** tag (an intersection, despite the docs saying "all").
  3. Keep `Node::isViable(memory, disk, cpu)` (`app/Models/Node.php:339-366`). For each of memory/disk/cpu: if `node.X > 0 && node.X_overallocate >= 0`, then require `sum + requested <= node.X * (1 + overallocate/100)`. A limit of 0 or an overallocate of `-1` means unlimited.
* `page` is validated but unused. **Not paginated.**
* 200: `{"object":"list","data":[node…]}`. Node attributes include `servers_sum_*` (§2.3).

#### 3.2.3 `GET /nodes/{node:id}`
* Perm `node:R`. 200: item `node`.

#### 3.2.4 `GET /nodes/{node:id}/configuration`
* Perm `node:R` (`GetNodeRequest`). `NodeConfigurationController.php:21-24`.
* 200: **not enveloped**. The raw output of `Node::getConfiguration()` (`app/Models/Node.php:246-272`), with the **unredacted** token:
  ```json
  {"debug":false,"uuid":"<node uuid>","token_id":"<daemon_token_id>","token":"<daemon_token plaintext>",
   "api":{"host":"0.0.0.0","port":<daemon_listen>,
          "ssl":{"enabled":<!behind_proxy && scheme=="https">,
                 "cert":"/etc/letsencrypt/live/<lower(fqdn)>/fullchain.pem",
                 "key":"/etc/letsencrypt/live/<lower(fqdn)>/privkey.pem"},
          "upload_limit":<upload_size>},
   "system":{"data":"<daemon_base>","sftp":{"bind_port":<daemon_sftp>}},
   "allowed_mounts":["<mount.source>",…],
   "remote":"<config('app.url')>"}
  ```

#### 3.2.5 `POST /nodes` — create
* Perm `node:W` (`StoreNodeRequest`, `app/Http/Requests/Api/Application/Nodes/StoreNodeRequest.php:59-100`).
* Rules (from `Node::$validationRules`, `app/Models/Node.php:141-162`):
  * `name`: `required|string|min:1|max:100`
  * `description`: `string|nullable`
  * `public`: `boolean`
  * `fqdn`: `required|string|not_in:0.0.0.0,127.0.0.1,localhost`
  * `scheme`: `required|string|in:http,https`
  * `behind_proxy`: `boolean`
  * `memory`, `disk`, `cpu`: `required|numeric|min:0`
  * `memory_overallocate`, `disk_overallocate`, `cpu_overallocate`: `required|numeric|min:-1`
  * `daemon_base`: `sometimes|required|regex:/^([\/][\d\w.\-\/]+)$/`
  * `daemon_sftp`, `daemon_listen`, `daemon_connect`: `required|numeric|between:1,65535`
  * `daemon_sftp_alias`: `nullable|string`
  * `maintenance_mode`: `boolean`
  * `upload_size`: `integer|min:1`
  * `tags`: `array`
* Display names: `daemon_base` → "Daemon Base Path", `upload_size` → "File Upload Size Limit", `public` → "Node Visibility".
* `validated()` fills in `daemon_base = "/var/lib/pelican/volumes"` when it is absent.
* Logic: `Node::create(validated)` (`NodeController.php:77-89`). The `creating` hook sets `uuid` (v4), `daemon_token` (64 random chars, stored encrypted) and `daemon_token_id` (16 random chars) (`Node.php:203-209`). Model defaults: `public=true, behind_proxy=false, memory/disk/cpu/*_overallocate=0, daemon_sftp=2022, daemon_listen=8080, daemon_connect=8080, maintenance_mode=false, tags=[]` (`Node.php:167-182`). **No Wings call.**
* 201: item `node` + `meta.resource` = `<APP_URL>/api/application/nodes/<id>`.
* **Attribute quirk:** the model is not re-read. Its `toArray()` order is:
  1. the model defaults: `public, behind_proxy, memory, memory_overallocate, disk, disk_overallocate, cpu, cpu_overallocate, daemon_base, daemon_sftp, daemon_listen, daemon_connect, maintenance_mode, tags`;
  2. new keys from the request, in rule order: `name, description?, fqdn, scheme, daemon_sftp_alias?, upload_size?`;
  3. `uuid`, `updated_at`, `created_at`, `id`, `allocated_resources` (all 0).

  Keys the client didn't send that have no default (`description`, `daemon_sftp_alias`, `upload_size`) are **absent** from the response. Uncast values keep the JSON type the client sent.

#### 3.2.6 `PATCH /nodes/{node:id}` — update
* Perm `node:W` (`UpdateNodeRequest`). Rules: the same as §3.2.5 (`getRulesForUpdate`; no unique rules), so all `required` fields are required. **An omitted `daemon_base` is reset to `/var/lib/pelican/volumes`** (inherited `validated()`).
* An extra raw input `reset_secret` (not validated) regenerates `daemon_token`/`daemon_token_id`, but only when it is exactly JSON boolean `true` (`$request->input('reset_secret') === true`, `NodeController.php:100-115`).
* Logic (`NodeUpdateService::handle`, `app/Services/Nodes/NodeUpdateService.php:30-67`):
  1. `data.id = node.id`; when resetting, also set the new tokens.
  2. In a transaction:
     * `updated = node.replicate()` (drops id/created_at/updated_at), `exists = true`;
     * `forceFill(data + {created_at: node.created_at, updated_at: now})->save()`;
     * `node.fqdn = updated.fqdn`;
     * **Wings**: `DaemonSystemRepository::update($updated)` → `POST {old scheme}://{NEW fqdn}:{old daemon_connect}/api/update`, `Authorization: Bearer <OLD token>`, body = `updated.getConfiguration()` (the §3.2.4 shape, with new values).
     * On `ConnectionException`: log it and still commit.
  3. Exceptions are handled by the controller's `catch (Exception)`, which `report`s them:
     * **Wings unreachable (ConnectionException)**: the DB update is **committed**. `ConfigurationNotPersistedException` is thrown and caught; the response is **200** with the *original* node data (fqdn replaced by the new value in memory).
     * **Wings non-2xx (RequestException)**: the transaction is **rolled back**, the exception is caught, and the response is **200** with the original node (fqdn shows the new value, though it was not saved).
     * A model `DataValidationException` is also swallowed → 200 with the original node.
* Success: 200 item `node` built from the replicated model. Key order:
  1. all table columns except `id/created_at/updated_at`, in physical order;
  2. `id, created_at, updated_at`;
  3. **`mounts`** — a list of mount `toArray()` objects, each with a `pivot`. This relation was loaded by `getConfiguration()` and leaks into the output;
  4. `allocated_resources`.
* No events other than eloquent `updated`.

#### 3.2.7 `DELETE /nodes/{node:id}`
* Perm `node:W`. `NodeDeletionService::handle` (`app/Services/Nodes/NodeDeletionService.php:15-24`).
* If the node has any servers → DisplayException-400 `HasActiveServersException`, `A node must have no servers linked to it in order to be deleted.`. Otherwise it is deleted (allocations removed by FK cascade **[unverified cascade]**).
* 204.

#### 3.2.8 `GET /nodes/{node:id}/allocations`
* Perm `allocation:R` (`GetAllocationsRequest`). `AllocationController::index` (`app/Http/Controllers/Api/Application/Nodes/AllocationController.php:43-63`).
* Query: `node.allocations()`.
* Filters: `ip` exact, `port` exact, `ip_alias` partial, and `server_id` callback. If the value is empty, a boolean (including `"true"`/`"false"`), or not all digits, the callback applies `server_id IS NULL` (unassigned only); otherwise `server_id = value`. A comma-separated value becomes an array, which triggers "Array to string conversion" → 500 **[edge]**.
* No sorts. Default per_page 50.
* 200: paginated list of `allocation`.

#### 3.2.9 `POST /nodes/{node:id}/allocations`
* Perm `allocation:W` (`StoreAllocationRequest.php:16-42`).
* Rules:
  * `ip`: `required|string`
  * `alias`: `sometimes|nullable|string|max:255`
  * `ports`: `required|array`
  * `ports.*`: **`string`** — JSON numbers fail with 422.
* Logic (`AssignmentService::handle`, `app/Services/Allocations/AssignmentService.php:45-110`):
  1. If `ip` contains `/`, the mask must be all digits and between 25 and 32, else DisplayException-400 `CidrOutOfRangeException` "CIDR notation only allows masks between /25 and /32.".
  2. `IPTools\Network::parse(ip)`; a failure is DisplayException-400 "Could not parse provided allocation IP address (<ip>): <msg>".
  3. BEGIN. For each IP in the network × each port entry:
     * The entry must be digits or match `^(\d{4,5})-(\d{4,5})$`, else `InvalidPortMappingException` "The mapping provided for <port> was invalid and could not be processed.".
     * A range must not exceed 1000 ports (`TooManyPortsInRangeException`, "Adding more than 1000 ports in a single range at once is not supported.").
     * Every port must be within 1024..65535 (`PortOutOfRangeException`, "Ports in an allocation must be greater than or equal to 1024 and less than or equal to 65535.").
     * `Allocation::create({node_id, ip: normalised string, port, ip_alias: alias, server_id: null, is_locked: false})`.
  4. COMMIT.
  * A duplicate `(node_id, ip, port)` violates the unique index → 500 (whole batch rolled back).
* 204 (empty).

#### 3.2.10 `DELETE /nodes/{node:id}/allocations/{allocation:id}`
* Perm `allocation:W`. Scoped binding: the allocation must belong to the node, else 404.
* `$allocation->delete()`. The `deleting` hook throws DisplayException-400 `ServerUsingAllocationException` "A server is currently assigned to this allocation. An allocation can only be deleted if no server is currently assigned." when it is assigned (`app/Models/Allocation.php:108-110`).
* 204.

### 3.3 Servers — `routes/api-application.php:62-91`

#### 3.3.1 `GET /servers`
* Perm `server:R` (`GetServersRequest`; rule `search: string|max:100` is validated but **unused**). `ServerController::index` (`app/Http/Controllers/Api/Application/Servers/ServerController.php:44-54`).
* 200: paginated list of `server`.

#### 3.3.2 `GET /servers/{server:id}`
* Perm `server:R`. Binding: `Server::resolveRouteBinding` → `where id = ?` (`app/Models/Server.php:458-464`).
* 200: item `server`.

#### 3.3.3 `GET /servers/external/{external_id}`
* Perm `server:R`. `firstOrFail` on `external_id` → 404 (`ExternalServerController.php:21-28`).
* 200: item `server`.

#### 3.3.4 `POST /servers` — create
* Perm `server:W` (`StoreServerRequest`, `app/Http/Requests/Api/Application/Servers/StoreServerRequest.php:21-201`; base rules from `Server::$validationRules`, `app/Models/Server.php:170-192`).
* Rules:

  | field | rules |
  |---|---|
  | `external_id` | `sometimes\|nullable\|string\|between:1,255\|unique:servers` (column `external_id`) |
  | `name` | `required\|string\|min:1\|max:255` |
  | `description` | `nullable\|string` |
  | `user` | `required\|integer\|exists:users,id` |
  | `egg` | `required\|exists:eggs,id` |
  | `docker_image` | `sometimes\|string` |
  | `startup` | `sometimes\|string` |
  | `environment` | `present\|array` (object `ENV_NAME → value`) |
  | `skip_scripts` | `sometimes\|boolean` |
  | `oom_killer` | `sometimes\|boolean` |
  | `limits` | `required\|array` |
  | `limits.memory` | `required\|numeric\|min:0` |
  | `limits.swap` | `required\|numeric\|min:-1` |
  | `limits.disk` | `required\|numeric\|min:0` |
  | `limits.io` | `required\|numeric\|between:0,1000` |
  | `limits.threads` | `nullable\|regex:/^[0-9-,]+$/` |
  | `limits.cpu` | `required\|numeric\|min:0` |
  | `feature_limits` | `required\|array` |
  | `feature_limits.databases` | `present\|nullable\|integer\|min:0` |
  | `feature_limits.allocations` | `sometimes\|nullable\|integer\|min:0` |
  | `feature_limits.backups` | `present\|nullable\|integer\|min:0` |
  | `allocation.default` | when `deploy` is falsy: `required\|integer\|bail\|exists:allocations,id,server_id,NULL` |
  | `allocation.additional.*` | when `deploy` is falsy: `integer\|exists:allocations,id (server_id NULL)` |
  | `deploy` | `sometimes\|required\|array` |
  | `deploy.tags` | `array`, plus `present` when deploy |
  | `deploy.tags.*` | `required_with:deploy.tags\|string\|min:1` |
  | `deploy.dedicated_ip` | `required_with:deploy\|boolean` |
  | `deploy.port_range` | `array`, plus `present` when deploy |
  | `deploy.port_range.*` | `string` |
  | `start_on_completion` | `sometimes\|boolean` |

* Normalised data (`StoreServerRequest::validated`, lines 122-150):
  * `owner_id ← user`, `egg_id ← egg`, `image ← docker_image`;
  * `memory, swap, disk, io, cpu, threads ← limits.*`;
  * `allocation_id ← allocation.default`, `allocation_additional ← allocation.additional`;
  * `database_limit, allocation_limit, backup_limit ← feature_limits.*`;
  * `skip_scripts` (default false), `start_on_completion` (default false), `oom_killer` (null if absent).
* Logic (`ServerCreationService::handle`, `app/Services/Servers/ServerCreationService.php:54-132`):
  1. `Egg::findOrFail(egg_id)`. `image ??= first(egg.docker_images)`, `startup ??= first(egg.startup_commands)`.
  2. **With `deploy`:**
     * `FindViableNodesService(memory, disk, cpu, deploy.tags)` (§3.2.2). If empty → DisplayException-400 `NoViableNodeException` "No nodes satisfying the requirements specified for automatic deployment could be found.".
     * If `port_range` is non-empty: `AllocationSelectionService` (`app/Services/Deployment/AllocationSelectionService.php:55-133`). Ports are digits or `NNNN-NNNNN` ranges (other entries are silently ignored; a range over 1000 ports is DisplayException-400). It picks a **random** unassigned allocation on the viable nodes with matching ports; with `dedicated_ip`, it excludes IPs that already have any assigned allocation. If none → DisplayException-400 `NoViableAllocationException` "No allocations satisfying the requirements for automatic deployment were found.". Then `allocation_id`/`node_id` are set from the chosen allocation.
     * If no `node_id` yet (port_range empty), use the first viable node. **The server is then created with no allocation** (`allocation: null`).
  3. **Without `deploy`:** `node_id = allocation(allocation_id).node_id`.
  4. `VariableValidatorService` at admin level (`app/Services/Servers/VariableValidatorService.php:25-54`):
     * Every variable of the egg is validated with its own `rules` against `environment[ENV] ?? default_value`.
     * A failure is a 422 with field `environment.<ENV>`.
     * Result per variable: `value = environment[ENV] ?? null`.
  5. Transaction (5 attempts):
     * `Server::create(...)` with `uuid` (v4, regenerated until both uuid and the 8-char `uuid_short` are unique), `uuid_short`, `status='installing'`, `description ?? ''`, `skip_scripts`, `oom_killer ?? false`, `database_limit ?? 0`, `allocation_limit ?? 0`, `backup_limit ?? 0`, `docker_labels=null`.
     * If there is an `allocation_id`, set `server_id` and `is_locked=true` on the allocations `[allocation_id, …allocation_additional]` that are still unassigned. **The additional allocations are not checked to be on the same node.**
     * Create a `server_variables` row per egg variable, with `variable_value = provided ?? ''`. **Variables that weren't provided are stored as `""`, not the default.**
  6. **Wings**: `DaemonServerRepository::create(start_on_completion)` → `POST /api/servers` `{"uuid": "<uuid>", "start_on_completion": bool}`. On `ConnectionException`, the server is force-deleted (`ServerDeletionService::withForce()`) and the exception rethrown → 500. A `RequestException` (non-2xx with a valid Wings UA) is **not** caught: 500, and the server **stays** in the DB.
* 201: item `server`. **No `meta.resource`.**

#### 3.3.5 `PATCH /servers/{server:id}/details`
* Perm `server:W` (`UpdateServerDetailsRequest.php:12-41`).
* Rules (via `getRulesForUpdate`):
  * `external_id`: `sometimes|nullable|string|between:1,255|unique:servers,external_id,<id>`
  * `name`: `required|string|min:1|max:255`
  * `user`: `required|integer|exists:users,id`
  * `description`: `nullable|string`
* Display names: `user` → "User ID", `name` → "Server Name".
* `validated()` returns **raw input** for `external_id`, `name`, `owner_id ← user` and `description`.
* Logic (`DetailsModificationService::handle`, `app/Services/Servers/DetailsModificationService.php:33-54`): in a transaction, remember the old owner, then `forceFill({external_id: input or NULL, owner_id, name, description: input ?? ''})->saveOrFail()`. **An omitted `external_id` clears it.**
  * If the owner changed, queue a `RevokeSftpAccessJob(oldOwner.uuid, server)` → Wings `POST /api/deauthorize-user` `{"user": "<old owner uuid>", "servers": ["<server uuid>"]}` (`DaemonServerRepository::deauthorize`).
  * The `Server::saved` hook deletes the subuser entry if the new owner was a subuser (`Server.php:221-227`).
* 200: item `server` (the in-memory model, not refreshed).

#### 3.3.6 `PATCH /servers/{server:id}/build`
* Perm `server:W` (`UpdateServerBuildConfigurationRequest.php:13-122`).
* Rules:
  * `allocation`: `sometimes|nullable|unique:servers,allocation_id,<id>|exists:allocations,id`
  * `oom_killer`: `sometimes|boolean`
  * `limits`: `sometimes|array`
  * `limits.memory`: `required_with:limits|numeric|min:0`
  * `limits.swap`: `required_with:limits|numeric|min:-1`
  * `limits.io`: `required_with:limits|numeric|between:0,1000`
  * `limits.cpu`: `required_with:limits|numeric|min:0`
  * `limits.threads`: `nullable|regex:/^[0-9-,]+$/`
  * `limits.disk`: `required_with:limits|numeric|min:0`
  * `add_allocations`: `bail|array`; `add_allocations.*`: `integer`
  * `remove_allocations`: `bail|array`; `remove_allocations.*`: `integer`
  * `feature_limits`: `required|array`
  * `feature_limits.databases`: `present|nullable|integer|min:0`
  * `feature_limits.allocations`: `sometimes|nullable|integer|min:0`
  * `feature_limits.backups`: `present|nullable|integer|min:0`
* Display names: `add_allocations` → "allocations to add", `remove_allocations` → "allocations to remove", `add_allocations.*` → "allocation to add", `remove_allocations.*` → "allocation to remove", `feature_limits.databases` → "Database Limit", `feature_limits.allocations` → "Allocation Limit", `feature_limits.backups` → "Backup Limit".
* `validated()`:
  * `allocation_id ← allocation`. **If `allocation` is omitted entirely, PHP raises "Undefined array key" → ErrorException → 500.** Clients must always send `allocation`; `null` clears the primary allocation.
  * `database_limit / allocation_limit / backup_limit ← feature_limits.*` (null if missing).
  * `limits.*` are flattened to top-level keys.
* Logic (`BuildModificationService::handle`, `app/Services/Servers/BuildModificationService.php:33-126`), in a transaction:
  1. `add_allocations`: `UPDATE allocations SET server_id = <server> WHERE node_id = server.node_id AND id IN add AND server_id IS NULL`. Wrong-node or taken ids are silently skipped.
  2. `remove_allocations`: considers the allocations on this server with id in `remove \ add`. If they include the current primary, `data.allocation_id` becomes the first remaining non-removed allocation, else `add[0]`, else null. Those rows are released with `{server_id: null, notes: null, is_locked: false, show_port: true}`.
  3. If `allocation_id` is set and differs from the current primary: `server.allocations().findOrFail(allocation_id)` → **404** if it isn't one of the server's allocations.
  4. `forceFill(only(oom_killer, memory, swap, io, cpu, threads, disk, allocation_id) + {database_limit, allocation_limit, backup_limit: value ?? 0})->saveOrFail()`, then `refresh()`.
  5. After commit, **Wings**: `DaemonServerRepository::sync()` → `POST /api/servers/{uuid}/sync` (always, since `build` is non-empty). A `ConnectionException` is logged and ignored; a `RequestException` → **500** (the DB changes are already committed).
* 200: item `server` (refreshed).

#### 3.3.7 `PATCH /servers/{server:id}/startup`
* Perm `server:W` (`UpdateServerStartupRequest.php:18-49`).
* Rules:
  * `startup`: `sometimes|string`
  * `environment`: `present|array`
  * `egg`: `required|exists:eggs,id`
  * `image`: `sometimes|string`
  * `skip_scripts`: **`present|boolean`**
* `validated()` returns `{startup?, environment, skip_scripts, egg_id: egg, docker_image: image|null}`.
* Logic (`StartupModificationService::handle` at admin level, `app/Services/Servers/StartupModificationService.php:30-97`), in a transaction:
  1. If `environment` is non-empty: validate **all** variables of the target egg (`egg_id`, which may be the new egg) as in §3.3.4 step 4 (→ 422). Then `updateOrCreate` a `server_variables` row for **every** egg variable with `variable_value = provided ?? ''`. **Variables omitted from a non-empty `environment` are overwritten with `""`.**
  2. If `egg_id` differs: set `egg_id`, and default `docker_image`/`startup` to the new egg's first entries.
  3. `fill({startup: startup ?? current, skip_scripts, image: docker_image ?? current})->save()`.
  4. Return `fresh()`.
* **No Wings call, and no reinstall**, even when the egg changes.
* 200: item `server`.

#### 3.3.8 `POST /servers/{server:id}/suspend` and `/unsuspend`
* Perm `server:W` (`ServerWriteRequest`). `ServerManagementController.php:45-64`, `SuspensionService::handle` (`app/Services/Servers/SuspensionService.php:27-52`).
  1. If the server is already in the target state: no-op (a Filament flash notification is queued) → 204.
  2. If a transfer is in progress (`server.transfer`: a `server_transfers` row with `successful IS NULL`) → **409** `ConflictHttpException` "Cannot toggle suspension status on a server that is currently being transferred.".
  3. `update({status: 'suspended' | null})`. **Unsuspend sets `status = NULL`.**
  4. **Wings**: `sync()` → `POST /api/servers/{uuid}/sync`. Any failure → 500 (the DB is already updated; no transaction).
* 204.

#### 3.3.9 `POST /servers/{server:id}/reinstall`
* Perm `server:W`. `ReinstallServerService::handle` (`app/Services/Servers/ReinstallServerService.php:26-35`): in a transaction, set `status='installing'`, save, then **Wings** `reinstall()` → `POST /api/servers/{uuid}/reinstall`. Any failure rolls back → 500.
* 204.

#### 3.3.10 `POST /servers/{server:id}/transfer`
* Perm `server:W`. Inline validation (`ServerManagementController.php:89-106`):
  * `node_id`: `required|exists:nodes,id`
  * `allocation_id`: `required|bail|unique:servers|exists:allocations,id` (no server may use it as its primary)
  * `allocation_additional`: `nullable|array`
  * `allocation_additional.*`: `integer|exists:allocations,id`
* Logic (`TransferServerService::handle`, `app/Services/Servers/TransferServerService.php:64-117`):
  1. Load the target node with server sums. If `!isViable(server.memory, server.disk, server.cpu)` → **406** `NodeNotViableException`.
  2. `validateTransferState()`: the server must be installed, not `restoring_backup`, and have no active transfer, else **409** `ServerStateConflictException` (message per `lang/en/exceptions.php:60-68`).
  3. Transaction: create a `server_transfers` row `{server_id, old_node, new_node}`. If the server has a primary allocation, also set `old_allocation`, `new_allocation`, `old_additional_allocations` (current non-primary ids) and `new_additional_allocations`, and assign `server_id` on the requested allocations that are unassigned **on the target node** (`assignAllocationsToServer`).
  4. Create a JWT for the new node (15 min, scope ServerTransfer, subject = server uuid) (`NodeJWTService`).
  5. **Wings (source node)**, via raw `Http::daemon(oldNode)` (**no** `throwIf`, so non-2xx responses are silently ignored): `POST /api/servers/{uuid}/transfer` `{"url": "<newNode connection address>/api/transfers", "token": "Bearer <jwt>", "backups": [], "server": {"uuid": "<uuid>", "start_on_completion": false}}`. `backups` is always empty, because the API passes no backup uuids. A `ConnectionException` → 500 (the transfer row persists).
* 204.

#### 3.3.11 `POST /servers/{server:id}/transfer/cancel`
* Perm `server:W` (`ServerManagementController.php:115-137`).
  * If there is no active transfer: **406 with an empty body** (`new Response('', 406)`).
  * Otherwise set `transfer.successful = true` (sic), save, then **Wings** `DaemonServerRepository::cancelTransfer()`: `DELETE /api/servers/{uuid}/transfer` on the **old** node, then `DELETE /api/transfers/{uuid}` on the **new** node (`app/Repositories/Daemon/DaemonServerRepository.php:106-122`). Errors → 500.
* 204.

#### 3.3.12 `DELETE /servers/{server:id}` and `DELETE /servers/{server:id}/{force?}`
* Perm `server:W`. Force mode is used iff the optional segment is exactly `force`; any other segment means a normal delete (`ServerController.php:97-102`).
* Logic (`ServerDeletionService::handle`, `app/Services/Servers/ServerDeletionService.php:48-109`):
  1. **Wings** `delete()` → `DELETE /api/servers/{uuid}`.
     * A `ConnectionException` aborts with **500** unless force (its code is never 404 here).
     * Any other exception, including `RequestException` for a Wings 404/500 with a valid UA, is reported and ignored.
  2. Transaction:
     * for each database: `DatabaseManagementService::delete` (drops the DB and user on the host; on failure, rethrow unless force, in which case just delete the row);
     * for each backup on a non-Wings backup host: delete it remotely (rethrow unless force);
     * release all allocations (`RELEASE_ATTRIBUTES`);
     * `server.delete()`.
* 204.

#### 3.3.13 Server databases — `routes/api-application.php:82-90`
These endpoints need **`server_database`** permission only, not `server`. `{database:id}` is a scoped binding (`server.databases()`), so a mismatch is 404.

* **`GET /servers/{server:id}/databases`** — `server_database:R`. 200: **unpaginated** list of `server_database` (`DatabaseController.php:38-43`).
* **`GET /servers/{server:id}/databases/{database:id}`** — `server_database:R`. 200: item `server_database`.
* **`POST /servers/{server:id}/databases`** — `server_database:W` (`StoreServerDatabaseRequest.php:23-88`).
  * Rules:
    * `database`: `required|alpha_dash|min:1|max:48|unique:databases,database` (where `server_id = <server>` and `database = <generated name>`)
    * `remote`: `required|string|regex:/^[0-9%.]{1,15}$/`
    * `host`: `required|integer|exists:database_hosts,id`
  * Display names: `host` → "Database Host Server ID", `remote` → "Remote Connection String", `database` → "Database Name".
  * Generated name: `s{server_id}_{substr(database, 0, 48 - len("s{id}_"))}`.
  * Logic (`DatabaseManagementService::create`, `app/Services/Databases/DatabaseManagementService.php:74-109`):
    * If `panel.client_features.databases.enabled` is false → `DatabaseClientFeatureNotEnabledException` (a PanelException) → **500**.
    * If `database_limit` is not null and the count is already at the limit → DisplayException-400 `TooManyDatabasesException` "Operation aborted: creating a new database would put this server over the defined limit.". A limit of **0 means none are allowed**; null means unlimited.
    * `username = u{server_id}_{10 random}`, `password` = 24 random chars including special characters.
    * Transaction: the `(server_id, database)` pair must not exist (else DisplayException-400 `DuplicateDatabaseNameException`); save the row (password encrypted); then, on the remote MySQL host: `CREATE DATABASE IF NOT EXISTS`, `CREATE USER ... IDENTIFIED BY ...` (plus `WITH MAX_USER_CONNECTIONS` if set), `GRANT ... ON db.* TO user@remote`, `FLUSH PRIVILEGES`; then activity `server:database.create` (subject = the database, property `name`).
    * Host errors → 500, rolled back.
  * 201: item `server_database` + `meta.resource` = `<APP_URL>/api/application/servers/<sid>/databases/<did>`.
* **`POST /servers/{server:id}/databases/{database:id}/reset-password`** — `server_database:W`. `rotatePassword`: a new 24-char password; in a transaction, update the row, then `DROP USER IF EXISTS`, `CREATE USER`, `GRANT`, `FLUSH`. No activity log. 204.
* **`DELETE /servers/{server:id}/databases/{database:id}`** — `server_database:W`. In a transaction: `DROP DATABASE IF EXISTS`, `DROP USER IF EXISTS`, `FLUSH`; activity `server:database.delete`; delete the row. 204.

### 3.4 Eggs — `routes/api-application.php:101-108`

* **`GET /eggs`** — `egg:R`. `Egg::all()`: **unpaginated**; filter and sort are ignored. 200: list of `egg` (`EggController.php:38-43`).
* **`GET /eggs/{egg:id}`** — `egg:R`. 200: item `egg`.
* **`GET /eggs/{egg:id}/export`** — `egg:R` (`ExportEggRequest`, rule `format: nullable|string|in:yaml,json`).
  * Format: `EggFormat::tryFrom(format) ?? YAML` (default **yaml**, although the docs say json).
  * 200: `StreamedResponse` download. `Content-Type: application/yaml` or `application/json`; `Content-Disposition: attachment; filename=egg-<kebab name>.<yaml|json>`. The kebab name is `str(name)->kebab()->lower()->trim()`, with characters outside `[\w-]` removed (`Egg.php:361`).
  * Body from `EggExporterService::handle` (`app/Services/Eggs/Sharing/EggExporterService.php:18-63`). Structure, in key order:
    ```
    _comment: "DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL"
    meta: {version: "PLCN_v3", update_url}
    exported_at: <now ATOM>
    name, author, uuid, description
    icon: <data URI | null>
    tags, features, docker_images
    file_denylist: <inherited, empties removed>
    startup_commands
    config: {files, startup, logs, stop}            (inherited raw strings)
    scripts: {installation: {script, container, entrypoint}}
    variables: [each variable toArray() without id, egg_id, created_at, updated_at]
    ```
    * JSON format: `JSON_PRETTY_PRINT|JSON_UNESCAPED_SLASHES`.
    * YAML format: `Yaml::dump(yamlExport(struct), 10, 2, MULTI_LINE_LITERAL_BLOCK|OBJECT_AS_MAP)`. In `yamlExport`, strings that are valid JSON are decoded into structures, CRLF becomes LF, and `description` values longer than 80 characters or containing newlines are word-wrapped at 100.
* **`POST /eggs/import`** — `egg:W`.
  * The **raw request body** is parsed as **YAML** (`fromContent($request->getContent())`; JSON bodies parse because YAML flow syntax accepts JSON) (`EggImporterService.php:41-232`).
  * `meta.version` must be one of `PTDL_v1`, `PTDL_v2`, `PLCN_v1`, `PLCN_v2` or `PLCN_v3`, else DisplayException-400 `InvalidFileUploadException` "The file format is not recognized.". A parse error is DisplayException-400 "File parse failed: …".
  * Normalisation:
    * legacy `image`/`images` → `docker_images`; `startup` → `startup_commands {"Default": …}`;
    * array-valued `config.*` and `scripts.installation.*` are JSON-encoded;
    * the `server.build.*` variable names in `config.files` are rewritten;
    * boolean `default_value`s become `"true"`/`"false"`;
    * reserved env names (`EggVariable::RESERVED_ENV_NAMES`) are prefixed with `SERVER_`, and their references in startup commands are rewritten;
    * a data-URI `image` is treated as `icon`.
  * Transaction:
    * the egg is found by `uuid` (update) or a new one is created (uuid from the file, else v4);
    * `copy_script_from` and `config_from` are reset to null;
    * fill from the file; save;
    * variables are upserted by `env_variable` (`rules` string split on `|`), and variables not in the file are deleted;
    * the egg is refreshed.
  * The icon (data URI) is written to the public disk.
  * 201: item `egg`. **No `meta.resource`.**
* **`DELETE /eggs/{egg:id}`** and **`DELETE /eggs/uuid/{egg:uuid}`** — `egg:W`. The `deleting` hook (`Egg.php:195-200`) throws DisplayException-400 `HasActiveServersException` "An Egg with active servers attached to it cannot be deleted from the Panel." or `HasChildrenException` "This Egg is a parent to one or more other Eggs. Please delete those Eggs before deleting this Egg.". 204.

### 3.5 Database hosts — `routes/api-application.php:118-127`

* **`GET /database-hosts`** — `database_host:R`. Paginated, **default per_page 10**; filters `name`, `host`; sorts `id`, `name`, `host` (`DatabaseHostController.php:39-49`).
* **`GET /database-hosts/{database_host:id}`** — `database_host:R`. Item `database_host`.
* **`POST /database-hosts`** — `database_host:W`.
  * Rules (`DatabaseHost::$validationRules`, `app/Models/DatabaseHost.php:69-77`):
    * `name`: `required|string|max:255`
    * `host`: `required|string`
    * `port`: `required|numeric|between:1,65535`
    * `username`: `required|string|max:32`
    * `password`: `nullable|string`
    * `node_ids`: `nullable|array`
    * `node_ids.*`: `required|integer|exists:nodes,id`
  * Logic (`HostCreationService`, `app/Services/Databases/Hosts/HostCreationService.php:33-52`), in a transaction:
    * create `{password (encrypted), name, host, port, username, max_databases: null}`;
    * `nodes().sync(node_ids ?? [])`;
    * **connection test**: open a PDO connection to `mysql:host;port` with these credentials (database `mysql`). A failure is a `PDOException` → 500 and a rollback.
  * 201: item `database_host` + `meta.resource` = `<APP_URL>/api/application/database-hosts/<id>`.
* **`PATCH /database-hosts/{database_host:id}`** — `database_host:W`. The rules are the same; `name`, `host`, `port` and `username` stay required.
  * Logic (`HostUpdateService`, `HostUpdateService.php:25-43`): an empty `password` is ignored; `update(data)` touches only fillable columns, so **`node_ids` is ignored on update**; then the connection test (500 and rollback on failure).
  * 200: item `database_host`.
* **`DELETE /database-hosts/{database_host:id}`** — `database_host:W`. `$host->delete()` with no guard. If databases still reference the host, the FK violation is a 500 **[FK behaviour per engine unverified]**. 204.

### 3.6 Mounts — `routes/api-application.php:137-155`

* **`GET /mounts`** — `mount:R`. Paginated (50); filters `uuid`, `name`; sorts `id`, `uuid` (`MountController.php:38-48`).
* **`GET /mounts/{mount:id}`** — `mount:R`. Item `mount`.
* **`GET /mounts/{mount:id}/eggs`** — perm **`egg:R`** (it uses `GetEggsRequest`, not the mount permission). Unpaginated list of `egg`.
* **`GET /mounts/{mount:id}/nodes`** — perm **`node:R`**. Unpaginated list of `node`, each with a `pivot` (§2.3).
* **`GET /mounts/{mount:id}/servers`** — perm **`server:R`**. Unpaginated list of `server`.
* **`POST /mounts`** — `mount:W`.
  * Rules (`Mount::getRules`, `app/Models/Mount.php:72-93`):
    * `name`: `required|string|min:2|max:64|unique:mounts,name`
    * `description`: `nullable|string|max:255`
    * `source`: `required|string|not_in:/etc/pelican,/var/lib/pelican/volumes,/srv/daemon-data`
    * `target`: `required|string|not_in:/home/container`
    * `read_only`, `user_mountable`: `sometimes|boolean` (default false)
  * Logic: `fill(validated)`, `uuid = v4`, `saveOrFail`, `fresh()`.
  * 201: item `mount` + `meta.resource` = `<APP_URL>/api/application/mounts/<id>`.
* **`PATCH /mounts/{mount:id}`** — `mount:W`. Rules come from `getRulesForUpdate`, so **`name`, `source` and `target` are required**; the name-unique check ignores this mount. `forceFill(validated)->save()`. 200: item `mount` (not refreshed; the bound model's DB key order).
* **`DELETE /mounts/{mount:id}`** — `mount:W`. If the mount has servers attached → DisplayException-400 `HasActiveServersException` "A mount must have no servers attached to it in order to be deleted.". Otherwise it is deleted (the `mountables` rows cascade). 204.
* **`POST /mounts/{mount:id}/eggs`** — `mount:W`. Rules: `eggs: required|array|exists:eggs,id` (every id must exist), `eggs.*: integer`. Logic: `mount.eggs().attach(ids)`, which inserts `mountables(mount_id, mountable_id, mountable_type='egg')`. **Attaching an already-attached id violates the primary key → 500.** 200: item `mount`.
* **`POST /mounts/{mount:id}/nodes`** — the same with `nodes` / `exists:nodes,id` / type `node`.
* **`POST /mounts/{mount:id}/servers`** — the same with `servers` / `exists:servers,id` / type `server`.
* **`DELETE /mounts/{mount:id}/eggs/{egg_id}`**, **`/nodes/{node_id}`**, **`/servers/{server_id}`** — `mount:W` (`DeleteMountRelationRequest`). `detach(id)`; detaching something not attached is a no-op. A non-numeric id → `TypeError` (the controller param is typed `int`) → 500. 204.

### 3.7 Roles — `routes/api-application.php:165-174`

* **`GET /roles`** — `role:R`. Paginated, **default 10**; filters `id` (partial LIKE), `name`; sorts `id`, `name`.
* **`GET /roles/{role:id}`** — `role:R`. Item `role`.
* **`POST /roles`** — `role:W`. Rule `name: required|string`. `Role::create(['name'])` with `guard_name='web'`. A duplicate name is spatie `RoleAlreadyExists` (InvalidArgumentException) → **500**. 201: item `role` + `meta.resource` = `<APP_URL>/api/application/roles/<id>`.
* **`PATCH /roles/{role:id}`** — `role:W`. Rule `name: required|string`. If the role is `Root Admin` → `PanelException` → **500**. `update(name)`. 200: item `role`.
* **`DELETE /roles/{role:id}`** — `role:W`. Root Admin → **500** `PanelException`. 204.
* There is no endpoint for changing a role's permissions.

### 3.8 Plugins — `routes/api-application.php:184-198`

The plugin subsystem is filesystem- and Composer-based (`app/Services/Helpers/PluginService.php`). See Open Questions about porting it.

* **`GET /plugins`** — `plugin:R`. Paginated, **default 10**; filters `id`, `name`, `author`, `category`; sorts the same four.
* **`GET /plugins/{plugin:id}`** — `plugin:R`. Item `plugin` (string id, lowercase).
* **`POST /plugins/import/file`** — `plugin:W`. Multipart field `plugin` (zip). A missing file is a `PanelException` → **500**.
  * `downloadPluginFromFile` (`PluginService.php:375-493`):
    * size ≤ `PANEL_PLUGIN_MAX_IMPORT_SIZE` (100 MiB), compressed and total uncompressed;
    * no `..` and no absolute entry paths;
    * extract to `plugins/.import-*`;
    * `plugin.json` must be at the root or one level deep;
    * `id` must match `^[a-z0-9][a-z0-9._-]*$`;
    * when replacing an existing plugin, its `meta` is kept and the swap is atomic with rollback.
  * If the plugin is already installed, it is reinstalled with `installPlugin(enable=false)`: composer packages, asset build, migrations, seeder.
  * Errors → 500.
  * 201 with an **empty body** (`new Response('', 201)`).
* **`POST /plugins/import/url`** — `plugin:W` (`ImportFilePluginRequest`; the names are swapped in code). Rule `url: required|string`. Downloads with the Hub credentials (timeout 60 s), checks size, then proceeds as for a file. 201 empty.
* **`PUT /plugins/hub`** — `plugin:W`.
  * Rules: `hub_url: required|string|max:255|url:https|regex:/^\S+$/`; `api_key: required|string|max:128|regex:/^pnl_[A-Za-z0-9]+$/`.
  * Writes `PANEL_PLUGIN_HUB_URL` (with the trailing `/` trimmed) and `PANEL_PLUGIN_HUB_API_KEY` to `.env`, runs `artisan queue:restart`, and clears `plugins.<id>.update` from the cache.
  * 204.
* **`POST /plugins/{plugin:id}/install`** — `plugin:W`. If status ≠ `not_installed` → 500 `PanelException`. `installPlugin(plugin, enable=true)`. Any failure sets status `not_installed` with a message and rethrows (500). 200: item `plugin` (status updated in memory).
* **`POST /plugins/{plugin:id}/update`** — `plugin:W`. If no update is available → 500. `updatePlugin`: downloads from the update feed (sha256 checked), re-reads the rows, and reinstalls if it was installed. 200: item `plugin`.
* **`POST /plugins/{plugin:id}/uninstall`** — `plugin:W`. Rule `delete: boolean`. If not installed → 500. `uninstallPlugin`: roll back migrations, set status `not_installed`, optionally delete the files, rebuild assets, remove composer packages. 200: item `plugin`.
* **`POST /plugins/{plugin:id}/enable`** — `plugin:W`. If `!canEnable()` (status not disabled/crashed, or incompatible) → 500. Status becomes `enabled` in `plugin.json` meta. 200: item `plugin`.
* **`POST /plugins/{plugin:id}/disable`** — `plugin:W`. If `!canDisable()` (status not enabled/incompatible) → 500. Status becomes `disabled`. 200: item `plugin`.

### 3.9 Webhooks — `routes/api-application.php:208-220`

* **`GET /webhooks`** — `webhook:R` (`GetWebhookRequest`).
  * Rules: `per_page: sometimes|integer|min:1|max:100`; `scope: sometimes|enum(global,server)`. Both are read from query or body, and an invalid value is a 422.
  * The page size comes from the **validated** `per_page` (default 50).
  * Filters `name`, `description`, `endpoint`, `type`, `scope`, `server_id` (all partial); sorts `id`, `name`, `type`, `created_at`.
  * 200: paginated list of `webhook`.
* **`GET /webhooks/types`** — `webhook:R`. 200 (**not enveloped**):
  ```json
  {"data":[{"id":"regular","label":"Regular","has_preview":false}]}
  ```
  The list is all registered schemas in registration order. Core registers only `regular` (`app/Providers/Extensions/WebhookServiceProvider.php`); plugins may add more.
* **`GET /webhooks/events`** — `webhook:R`. `?scope=global|server` (default `global`). 200 (**not enveloped**):
  ```json
  {"data":[{"event":"<event id>","label":"<label>"}, …],"meta":{"scope":"global"}}
  ```
  * Server scope: the fixed list `server:file.read` … `server:sftp.denied` plus plugin additions. Label = the part after `server:`, with `.` → ` → `, Title-Cased (`WebhookConfiguration.php:232-297,357-364`).
  * Global scope: `eloquent.{created,updated,deleted}: App\Models\<X>` for every file in `app/Models`, plus custom `App\Events\*` classes, filtered as in `allPossibleAdminOnlyEvents` (`WebhookConfiguration.php:299-336`). Label = the event with `eloquent.` and `App\Models\` stripped, and `App\Events\` → `event: ` (`transformClassName`).
  * **The list is generated dynamically from the filesystem.**
* **`GET /webhooks/{webhook:id}`** — `webhook:R`. Item `webhook`.
* **`POST /webhooks`** — `webhook:W` (`StoreWebhookRequest.php:22-166`).
  * Rules:
    * `name`: `required|string|max:191`
    * `description`: `nullable|string|max:191`
    * `endpoint`: `required|string|url|max:191`
    * `type`: `sometimes|string|in:<registered type ids>`
    * `scope`: `sometimes|enum`
    * `server_id`: `nullable|integer|exists:servers,id`
    * `events`: `required|array|min:1`; `events.*`: `required|string`
    * `payload`: `nullable|array`
    * `headers`: `nullable|array`, and every key must match `/\A[A-Za-z0-9 !#$%&'*+.^_`|~-]+\z/` (else "The headers field contains an invalid header name.")
    * `headers.*`: `string|regex:/\A[^\r\n]*\z/`
    * plus the resolved type schema's `getPayloadRules()` under `payload.*` (none for `regular`).
  * After-validation, with the resolved scope = `scope` input, else `server` if `server_id` is filled, else `global`:
    * scope server without `server_id` → error on `server_id` (validation.required);
    * scope global with `server_id` → error on `server_id` (validation.prohibited);
    * every event must be in `filamentCheckboxList(scope)`, else error `events.<i>` (validation.in).
  * Persisted: the validated data, with `scope ??=` the resolved scope and `type ??=` the input type or `WebhookTypes::detect(endpoint)` (default `regular`), then the schema's `mutateFormDataBeforeSave` (`regular`: spaces in header names → `-`).
  * `fill()->saveOrFail()`. The `saved` hook refreshes the webhook caches.
  * 201: item `webhook` (`fresh()`) + `meta.resource` = `<APP_URL>/api/application/webhooks/<id>`.
* **`PATCH /webhooks/{webhook:id}`** — `webhook:W` (`UpdateWebhookRequest.php`). Partial update: `name`, `endpoint` and `events` become `sometimes`.
  * `prepareForValidation` (runs before authorize):
    * if `payload` is absent and the type changes, merge the stored `payload`;
    * if `events` is absent and the scope changes, merge the stored `events`.
  * Payload rules apply only when `payload` is sent.
  * Scope resolution: the `scope` input, else `server` if `server_id` is filled, else the stored scope. `hasServer` honours an explicit `server_id: null`. Type: the `type` input, else the stored type (never re-detected).
  * Persisted: if `server_id` is sent without `scope`, set the resolved scope; if the resolved scope is global, force `server_id = null`; then `mutateFormDataBeforeSave`.
  * 200: item `webhook` (`fresh()`).
* **`DELETE /webhooks/{webhook:id}`** — `webhook:W`. Deletes (caches updated). 204.
* **`POST /webhooks/{webhook:id}/test`** — `webhook:W`. Rules: `event: sometimes|string`, `data: sometimes|array`.
  * `WebhookConfiguration::run(event, data)` (`WebhookConfiguration.php:451-462`) defaults:
    * server scope: event `server:file.write` with server sample data;
    * otherwise: `eloquent.created: App\Models\Server` with `getWebhookSampleData()`.
  * Queues `ProcessWebhook`. The delivery result is later stored in `webhooks` (see the `deliveries` include).
  * **202**: `{"dispatched":true,"endpoint":"<endpoint>"}`.

---

## 4. Open questions / uncertain framework behaviour

1. **Physical column order.** `NodeData`, `MountData`, `EggVariableData`/`ServerVariableData` mirror Eloquent `toArray()`, so their key order depends on the DB engine. Node and egg-variable order are the same on MySQL and MariaDB, and differ from SQLite and PostgreSQL, which share an order (§2.3, §2.6; confirmed against `crates/pelican-db/schema/*.sql`). Databases migrated from old Pterodactyl or Pelican versions may differ again. **Decision needed:** either emit the order of the engine the differential tests run PHP against (pgsql/sqlite order if PHP runs on PostgreSQL), or select the order per connected engine. In both cases the differential tests should compare these three resources as unordered maps.
2. **Relation/attribute leakage in NodeData:** `pivot` on nodes reached through mounts or database hosts, `mounts` after a successful node PATCH, `servers_sum_*` on deployable nodes, and missing keys in the POST /nodes response. These are accidental. Decide per item whether to replicate them or document them as intentional divergence.
3. **`servers_sum_*` types**: MySQL `SUM()` → DECIMAL → PHP string; SQLite → int; null when there are no servers. Not verified against a live MySQL.
4. **Middleware order (bindings before auth).** Derived from the Laravel 13 `Configuration\Middleware` default `api` group plus append semantics (checked in vendor). It means an unauthenticated request for a missing id gets 404, not 401. Worth one live check.
5. **Validation error `rule` codes** for rule objects and closures (snake-cased FQCNs). Also, whether Laravel 13 merges framework-default validation messages with the panel's partial `lang/en/validation.php` (I believe it does: framework path plus app path). Exact message text parity needs a fixture run.
6. **Non-numeric / negative `per_page`.** The 500 (TypeError, or ErrorException via the warning-to-exception handler) and the "no limit" behaviour follow from reading the code. The exact exception class basename in `code` is unverified.
7. **Session (TransientToken) auth and the CSRF/419 behaviour** when `Referer`/`Origin` matches a stateful domain. These are probably out of scope for the Rust port, but clients sending an `Origin` header that matches APP_URL will see 419 on writes in PHP.
8. **Non-JSON clients** get redirects or HTML instead of JSON errors (§1.12). The Rust port should probably always emit JSON.
9. **`RevocationListener` registration** relies on Laravel's automatic event discovery of `app/Listeners` (it isn't listed in `EventServiceProvider::$listen`). Laravel 13 `Application::configure()` enables discovery by default — believed true, not run-verified.
10. **Webhook event catalogue** (`/webhooks/events`, and validation of `events`) is computed by scanning `app/Models` and `app/Events`. The Rust port needs a static list matching the PHP model and event set at the target version.
11. **Plugins**: PHP-specific (Composer, artisan, Filament assets, `.env` writing). Porting means either a stub (e.g. 501) or a different plugin model. The wire shape of `PluginData` is specified, but behavioural parity is likely infeasible.
12. **Icon URL** (`EggData.icon`) depends on the `public` filesystem disk config (not present in `config/`, so Laravel defaults apply, presumably `APP_URL/storage`).
13. **DB-host delete with dependent databases**, and node delete cascading allocations: these rely on FK definitions spread across migrations. Not fully traced.
14. **Wings error semantics** depend on the UA-header check (§1.14). Rust must replicate the `ConnectionException` vs `RequestException` split to get the same "committed but 500" and "rolled back but 200" outcomes on node update, server build, create, delete and suspend.
15. **Egg import accepts YAML, and JSON-as-YAML.** Symfony YAML parsing of arbitrary JSON (escapes, tabs) may diverge from strict JSON parsing in edge cases.
16. **`formatTimestamp` with null timestamps** → TypeError → 500 (affects users, servers, nodes and subusers rows whose `created_at`/`updated_at` is NULL).
17. **`PATCH /servers/{id}/build` without `allocation`** → 500 (undefined array key). This is very likely an upstream bug; decide whether to replicate it or return a clean 422 or 200.
18. **Startup/creation variable semantics** (unspecified variables stored as `""`) — confirm intended behaviour before replicating.
19. **Role assignment guard bypass** with string ids (`"1"`), and `Role::getRootAdmin()` creating the role as a side effect.
