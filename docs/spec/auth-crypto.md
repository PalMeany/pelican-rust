# Auth & Crypto Compatibility Spec

Status: reference contract for the Rust rewrite (`pelican-rs`). It must run against an **existing production
Pelican database and `.env`** (same `APP_KEY`) without migrating or re-encrypting data.

Sources:

* Panel source (read-only): `panel/…` = `/Users/tazik/Desktop/Code/claude/pelican/panel` (git history available).
* Framework/vendor source at the exact locked versions: `vendor/…` = `/Users/tazik/Desktop/Code/claude/pelican/.ref/vendor`
  (laravel/framework **13.35.0**, laravel/sanctum 4.x, laravel/passkeys 0.2 + web-auth/webauthn-lib 5.3,
  spatie/laravel-permission 8.x, filament 5.x, pragmarx/google2fa, s1lentium/iptools).

Citations are `path:line` or `path:start-end`. Statements marked **(framework)** were verified in `vendor/`. Statements
marked **(uncertain)** are inferences that were not fully verified.

---

## 0. Quick reference (the things that break production if wrong)

| Topic | Contract |
|---|---|
| Cipher | `AES-256-CBC`, key = `base64_decode(APP_KEY without "base64:")` (32 bytes). Not configurable via env. |
| Payload | `base64( json{"iv":b64(16B),"value":b64(ciphertext PKCS7),"mac":hex(HMAC-SHA256(key, iv_b64 ‖ value_b64)),"tag":""} )` |
| Previous keys | `APP_PREVIOUS_KEYS` (comma separated, same `base64:` parsing). Decrypt tries current key then each previous key by MAC. Encrypt always uses current key. |
| DB encrypted columns | `nodes.daemon_token`, `databases.password`, `database_hosts.password`, `api_keys.token`, `users.mfa_app_secret` (all **non-serialized** string), `users.mfa_app_recovery_codes` (non-serialized JSON array of bcrypt hashes). Nothing else in the DB is encrypted (S3 secrets, webhook headers, OAuth secrets, mail password are plaintext — DB JSON or `.env`). |
| Passwords | bcrypt `$2y$`, cost `BCRYPT_ROUNDS` (default 12), rehash-on-login when cost differs, `HASH_VERIFY=true` → non-bcrypt hash is an error. PHP truncates input at 72 bytes. |
| API token | Bearer = `identifier(16)` ‖ `token(32)`. Look up `api_keys.identifier = bearer[0..16]`, decrypt `api_keys.token`, compare to `bearer[16..]`. Prefix (`pacc_`/`papp_`/legacy `plcn_`/`peli_`/`ptlc_`/`ptla_`/none) is **cosmetic** – type comes from `key_type`. |
| Daemon → panel | `Authorization: Bearer <daemon_token_id>.<daemon_token>`; `daemon_token` decrypted from DB, `hash_equals`. |
| Panel → daemon | `Authorization: Bearer <daemon_token>` (no id); JWTs HS256 signed with plaintext `daemon_token`. |
| TOTP | RFC6238, SHA1, 6 digits, 30 s, **window ±8 steps**, base32 secret (16 chars new, 32 chars legacy), anti-replay of time-step. 8 recovery codes `xxxxxxxxxx-xxxxxxxxxx` stored as bcrypt hashes (legacy: 10-char codes). |
| Root admin | User has spatie role named exactly `Root Admin` (guard `web`). Global gate-before → allow everything. |
| Morph type | `model_has_roles.model_type = 'user'` (enforced morph map), not the class name. |

---

## 1. Laravel encryption

### 1.1 APP_KEY / config

* `config/app.php` in the panel does **not** define `cipher`, `key` or `previous_keys` (`panel/config/app.php:1-21`).
  Since Laravel 11 the framework's own `config/app.php` is merged underneath the application's file
  (`vendor/laravel/framework/src/Illuminate/Foundation/Bootstrap/LoadConfiguration.php:113-122`, `array_merge($base[$name], $config)`),
  so the effective values are **(framework)** `vendor/laravel/framework/config/app.php:127-135`:
  * `cipher` = `'AES-256-CBC'` (hard-coded, no env override).
  * `key` = `env('APP_KEY')`.
  * `previous_keys` = `array_filter(explode(',', env('APP_PREVIOUS_KEYS', '')))`.
* Key parsing **(framework)** `vendor/laravel/framework/src/Illuminate/Encryption/EncryptionServiceProvider.php:27-82`:
  * Empty key → `MissingAppKeyException`.
  * If the string starts with `base64:` → `base64_decode(rest)` (PHP non-strict base64 decode); otherwise the raw string bytes are the key.
  * Each previous key is parsed the same way.
  * `Encrypter::supported()` requires `mb_strlen($key,'8bit') === 32` for AES-256-CBC, else `RuntimeException` at boot
    (`vendor/laravel/framework/src/Illuminate/Encryption/Encrypter.php:54-82`). The same check applies to every previous key (`Encrypter.php` `previousKeys()`).
* Installers generate `base64:` + base64(32 random bytes) (`panel/docker/entrypoint.sh:44-47`; `p:environment:setup` calls `key:generate`,
  `panel/app/Console/Commands/Environment/AppSettingsCommand.php:35`). Legacy Pterodactyl installs migrated to Pelican use the same format.
  Rust must still accept a raw 32-byte non-prefixed key for parity.

> **Important — several subsystems use the *raw string* `config('app.key')` (including the literal `base64:` prefix) as an HMAC key, others use the *decoded* bytes.** See §1.7.

### 1.2 Encrypt algorithm (framework, `Encrypter.php:104-131`)

```
encrypt(value, serialize = true):
  iv        = random_bytes(16)                        # openssl_cipher_iv_length('aes-256-cbc')
  plaintext = serialize ? php_serialize(value) : value
  value_b64 = openssl_encrypt(plaintext, 'aes-256-cbc', key, options=0, iv)
              # options=0 → output is BASE64 of the ciphertext, PKCS#7 padding
  iv_b64    = base64(iv)
  tag_b64   = base64('')  = ''                         # CBC is not AEAD
  mac       = hex( HMAC_SHA256(key = raw 32-byte key, msg = iv_b64 ‖ value_b64) )   # lowercase hex, 64 chars
  json      = json_encode({"iv":iv_b64,"value":value_b64,"mac":mac,"tag":""}, JSON_UNESCAPED_SLASHES)
  return base64(json)                                   # standard alphabet, with padding
```

* Key order in the JSON is `iv, value, mac, tag`. `tag` is always present and empty for CBC in Laravel ≥ 9.
* `encryptString($v)` ≡ `encrypt($v, false)` (`Encrypter.php:141-144`). The `encrypt()` global helper defaults to **serialize = true**.
* The MAC is computed over the **base64 text** of iv and value (not the raw bytes), concatenated without separator.

### 1.3 Decrypt algorithm (framework, `Encrypter.php:155-307, 317-336`)

```
decrypt(payload, unserialize = true):
  if payload is not a string → DecryptException('The payload is invalid.')
  p = json_decode(base64_decode(payload), assoc)       # non-strict base64 decode
  require p is object with string iv, value, mac; tag (if present) must be string
  require strlen(base64_decode(p.iv, strict)) == 16     else 'The payload is invalid.'
  tag = empty(p.tag) ? null : base64_decode(p.tag)
  CBC: if tag is a (non-null) string → DecryptException('Unable to use tag because the cipher algorithm does not support AEAD.')
  for key in [current, ...previous]:                   # MAC-first; first key whose MAC validates wins
      if hash_equals(hex(HMAC_SHA256(key, p.iv ‖ p.value)), p.mac) and validKey is null: validKey = key
  if validKey is null → DecryptException('The MAC is invalid.')
  plaintext = openssl_decrypt(p.value, 'aes-256-cbc', validKey, 0, base64_decode(p.iv))
  if plaintext === false → DecryptException('Could not decrypt the data.')
  return unserialize ? php_unserialize(plaintext) : plaintext
```

Compatibility notes:

* Old payloads (Laravel < 9 / Pterodactyl era) have **no `tag` key** and may have escaped slashes (`"\/"`) because
  `JSON_UNESCAPED_SLASHES` was not used. Use a real JSON parser; the MAC is over the *decoded* JSON strings so escaping does not matter.
* `Encrypter::appearsEncrypted($v)` (`Encrypter.php:344-360`): strict base64 → JSON object with `iv`, `value`, `mac`. Useful for the
  defensive heuristics in §1.10.
* The MAC comparison is constant-time (`hash_equals`); Rust must also compare in constant time.

### 1.4 PHP `serialize()` formats that can appear inside decrypted plaintext

Only strings were ever encrypted with serialization in this schema, so only this form must be supported for DB data:

```
s:<byte length>:"<raw bytes>";          e.g.  s:5:"hello";   s:32:"AbC…";
```

(Byte length is the UTF-8 byte count; the content is not escaped.) Filament/Livewire also `encrypt()` arrays into component
state (e.g. `vendor/filament/filament/src/Auth/Pages/Login.php` `encrypt(['identifier'=>…,'userKey'=>…])`), but
those never touch the DB.

### 1.5 Eloquent `encrypted` casts (framework)

* `'encrypted'` cast: read = `decrypt($raw, false)` (no unserialize); write = `encrypt($value, false)`
  (`vendor/laravel/framework/src/Illuminate/Database/Eloquent/Concerns/HasAttributes.php:1456-1471`).
* `'encrypted:array'`: write = `json_encode($array)` (flags 0 → escaped slashes `\/`, `\uXXXX` unicode) **then** `encrypt(…, false)`
  (`HasAttributes.php:1117-1131` + `Casts/Json.php`); read = decrypt string then `json_decode(…, true)` (`HasAttributes.php:862-869`).
* `NULL` is never encrypted (write path checks `! is_null($value)`, read path returns null for primitive casts – `HasAttributes.php:857-859, 1129`).
  An empty string `''` stored in an encrypted column is **not** valid and throws `DecryptException` on read.
* Decryption is lazy (only on attribute access); listing models does not decrypt.

### 1.6 Writing from Rust

Rust must produce byte-for-byte compatible payloads so the PHP panel (if run in parallel / rolled back) can read them:
`encryptString` semantics (no serialize), JSON key order `iv,value,mac,tag`, `"tag":""`, unescaped slashes, standard base64 with `=` padding
for both the inner fields and the outer wrapper, MAC lowercase hex.

### 1.7 Every other use of APP_KEY (HMAC / signing)

| Use | Key material | Algorithm / format | Source |
|---|---|---|---|
| Cookie encryption (all cookies except none) | decoded key (Encrypter) | cookie value = `encrypt( hex(HMAC_SHA1(key=raw decoded key, msg=cookieName‖"v2")) ‖ "|" ‖ plainValue , serialize=false)`; on read the 41-char prefix is validated against current+previous keys and stripped; invalid → cookie becomes `null` | `vendor/laravel/framework/src/Illuminate/Cookie/Middleware/EncryptCookies.php` (`encrypt()/decrypt()/validateValue()`), `Cookie/CookieValuePrefix.php:7-27` |
| `X-XSRF-TOKEN` header | decoded key | header is the encrypted `XSRF-TOKEN` cookie; server `decrypt(header, false)` then strips first 41 chars (no prefix validation) and `hash_equals` with session `_token` | `vendor/…/Foundation/Http/Middleware/PreventRequestForgery.php` `getTokenFromRequest()` |
| Session password hash (`password_hash_web`) & remember-me cookie hash | **raw string `config('app.key')` incl. `base64:`** | `hex(HMAC_SHA256(key = APP_KEY string, msg = users.password))`; fallback key `'base-key-for-password-hash-mac'` if app.key null | `vendor/…/Auth/AuthManager.php:135`, `Auth/SessionGuard.php:656-663` |
| Signed URLs (password-reset link, etc.) | **raw string `config('app.key')`** (first element of `[app.key, ...previous_keys]` for signing; all tried for validation) | `signature = hex(HMAC_SHA256(key, full URL with ksort-ed query incl. expires))` | `vendor/…/Routing/RoutingServiceProvider.php:78-82`, `Routing/UrlGenerator.php` `signedRoute()/hasCorrectSignature()` |
| Password-reset token generation | **decoded** key | plain token = `hex(HMAC_SHA256(key, Str::random(40)))` (64 hex chars) | `vendor/…/Auth/Passwords/PasswordBrokerManager.php:85-111`, `DatabaseTokenRepository.php` `createNewToken()` |
| Passkey WebAuthn user handle | `PASSKEYS_USER_HANDLE_SECRET` or **raw string `config('app.key')`** | `HMAC_SHA256(key, "users|" ‖ users.id)` **binary** (32 bytes) | `vendor/laravel/passkeys/config/passkeys.php` (`user_handle_secret`), `vendor/laravel/passkeys/src/PasskeyAuthenticatable.php:42-49` |
| Filament MFA scoped identifiers | none (plain SHA-256) | see §4.4 | |

### 1.8 Inventory of encrypted values in the database

All current writers go through the Eloquent `encrypted` cast (no PHP serialization). **Nothing else is encrypted** — this was verified by
grepping every `encrypt|decrypt|Crypt::|'encrypted'` occurrence in `panel/app`, `panel/database`, `panel/config`, `panel/routes`.

| table.column | Type (current schema) | Current format | Written by | Decrypted / used by |
|---|---|---|---|---|
| `nodes.daemon_token` | `text` (`2020_04_10_141024_store_node_tokens_as_encrypted_value.php:30`) | `encryptString(token)`; token = `Str::random(64)` (`panel/app/Models/Node.php:118-120,193,201-208`), reset in `panel/app/Services/Nodes/NodeUpdateService.php:35-36`. **Legacy rows migrated from Pterodactyl hold a 20-char token** (36-char UUID `daemonSecret` minus the 16-char id, `2020_04_10…:33-40`). Do not assume 64. | Node model `creating`, NodeUpdateService (reset) | `DaemonAuthenticate` (`panel/app/Http/Middleware/Api/Daemon/DaemonAuthenticate.php:41-47`), outgoing `Http::daemon` bearer (`panel/app/Providers/AppServiceProvider.php:68-78`), JWT signing key (`panel/app/Services/Nodes/NodeJWTService.php:76-77`), node `config.yml` generation (`Node.php:251-252`) |
| `nodes.daemon_token_id` | `varchar(16)` unique | **plaintext** `Str::random(16)` | same | lookup key |
| `databases.password` | `text` | `encryptString(Utilities::randomStringWithSpecialCharacters(24))` (`panel/app/Services/Databases/DatabaseManagementService.php:90,140-144`) | Database model cast (`panel/app/Models/Database.php:84`) | MySQL user creation/rotation (`Database.php:145`), JDBC string (`Database.php:112`), API/UI with `database.view-password` |
| `database_hosts.password` | `text`, nullable | `encryptString(pw)` or `NULL` (validation `nullable`, `panel/app/Models/DatabaseHost.php:74,84`) | DatabaseHost cast | DB host connection config (`DatabaseHost.php:112`) |
| `api_keys.token` | `text` (`2018_01_13_142012_SetupTableForKeyEncryption.php:23-25`) | `encryptString(Str::random(32))` | `HasAccessTokens::createToken` (`panel/app/Models/Traits/HasAccessTokens.php:31-44`), `KeyCreationService` (`panel/app/Services/Api/KeyCreationService.php:33-46`), admin `CreateApiKey` (`panel/app/Filament/Admin/Resources/ApiKeys/Pages/CreateApiKey.php:43-62`) | `ApiKey::findToken` (`panel/app/Models/ApiKey.php:197-208`); admin list shows `identifier.token` (`panel/app/Filament/Admin/Resources/ApiKeys/ApiKeyResource.php` column `key`) |
| `users.mfa_app_secret` | `text` nullable (`2025_07_22_091435_update_users_totp.php:18-20`) | `encryptString(base32Secret)` | Filament `saveAppAuthenticationSecret` → `User::update` (`panel/app/Models/User.php:211,543-546`); migration copy (§1.9) | `getAppAuthenticationSecret()` (`User.php:538-541`) → TOTP verification; `RequireTwoFactorAuthentication` uses `filled()` (`panel/app/Http/Middleware/RequireTwoFactorAuthentication.php:40`); API `2fa_enabled` flags (`panel/app/Data/Api/Client/UserData.php:41`, `panel/app/Data/Api/Application/UserData.php:53-54`) |
| `users.mfa_app_recovery_codes` | `text` nullable | `encryptString(json_encode([bcrypt_hash, …]))` (cast `encrypted:array`, `User.php:212`); `[]` after all codes used; `NULL` when disabled | Filament `saveAppAuthenticationRecoveryCodes` (`User.php:564-567`); migration (§1.9) | Filament recovery-code verification (`vendor/filament/filament/src/Auth/MultiFactor/App/AppAuthentication.php` `verifyRecoveryCode()`) |

Not encrypted (plaintext, but sensitive — Rust must treat accordingly):

| Location | Content | Source |
|---|---|---|
| `backup_hosts.configuration` (JSON) | S3 `key`, `secret`, `token`, bucket, endpoint… | cast `array` (`panel/app/Models/BackupHost.php:46-51`); created from env in `2026_01_16_081858_create_backup_hosts_table.php` (`AWS_SECRET_ACCESS_KEY` copied verbatim) |
| `webhook_configurations.headers` / `endpoint` | arbitrary headers (may include auth), Discord webhook URL tokens | cast `array` (`panel/app/Models/WebhookConfiguration.php:91-95`) |
| `users.oauth` (JSON) | provider → provider user id map | cast `array` (`User.php:214`) |
| `users.password` | bcrypt hash | §2 |
| `users.remember_token` | 60-char random | §6.6 |
| `password_resets.token` | bcrypt hash of reset token | §2.4 |
| `passkeys.credential` | WebAuthn public credential JSON | §5 |
| `.env` | `MAIL_PASSWORD`, `OAUTH_*_CLIENT_SECRET`, `CAPTCHA_TURNSTILE_SECRET_KEY`, `DB_PASSWORD`, `REDIS_PASSWORD`, `PANEL_PLUGIN_HUB_API_KEY`… | written by `EnvironmentWriterTrait` (`panel/app/Traits/EnvironmentWriterTrait.php:19-23`) from `panel/app/Filament/Admin/Pages/Settings.php` |
| `settings` table (legacy Pterodactyl key/value) | still exists (`2017_12_12_220426_MigrateSettingsTableToNewFormat.php`), never read by Pelican. Pterodactyl stored `settings::mail:password` here **encrypted with serialize** – irrelevant, ignore. | |

### 1.9 Historical formats that may still exist in old databases

Migrations only run once, so the format of a row depends on which code wrote it and whether the fix-up migration could read it.

1. **2017-11-11** `2017_11_11_161922_Add2FaLastAuthorizationTimeColumn.php:22-33`: `users.totp_secret` → `Crypt::encrypt()` (**serialized**).
2. **2017-11-19** `2017_11_19_122708_MigratePubPrivFormatToSingleKey.php:18-52`: api key `secret` decrypted (serialized) → stored **plaintext** 32 chars as `token CHAR(32)`; undecryptable ones replaced by `Str::random(32)`.
3. **2018-01-13** `2018_01_13_142012_SetupTableForKeyEncryption.php:16-26`: `identifier varchar(16) NULL UNIQUE` added, `token` → `text`. From here Pterodactyl wrote `encrypt($token)` (**serialized**). Keys from step 2 kept a plaintext token and `identifier = NULL` (unreachable by lookup).
4. **2020-04-10** `2020_04_10_141024_store_node_tokens_as_encrypted_value.php:33-40`: `daemonSecret` split → `daemon_token_id` (first 16 chars, plaintext) + `daemon_token = encrypt(rest)` (**serialized**, 20 chars of a UUID).
5. Pterodactyl app code also wrote `databases.password`, `database_hosts.password` with `encrypt()` (**serialized**).
6. **2024-05-31** `2024_05_31_204646_fix_old_encrypted_values.php:13-71`: for every row of `api_keys.token`, `databases.password`,
   `database_hosts.password`, `nodes.daemon_token`, `users.totp_secret`: `encrypt(decrypt($v), false)` i.e. **unserialize and re-encrypt without serialization**.
   Any exception (MAC invalid, value not encrypted, value not unserializable — PHP warnings become `ErrorException` under Laravel) is logged
   and the row is **left unchanged**. Therefore rows that were already non-serialized stay as they were (correct), and rows that were
   plaintext (step 2) or encrypted with a different key stay broken.
7. **Pelican (pre-Filament-v4) code** used the `encrypted` cast for `users.totp_secret` (verified: `git show 1900c04b7^:app/Models/User.php` line 171).
8. **2025-07-22 `update_users_totp`** — three versions shipped over time (same migration name, so each DB ran exactly one of them):
   * commit `1900c04b7` (canary only, 2025-09-08…2025-09-15, not in any tag): copied `$user->totp_secret` **raw ciphertext** through the new
     `encrypted` cast → `mfa_app_secret` is **double-encrypted** (`encryptString(<base64 payload of encryptString(secret)>)`); recovery tokens dropped.
   * commit `d5d50d415` (tags `v1.0.0-beta26` … `v1.0.0-beta38`): only dropped `use_totp/totp_secret/totp_authenticated_at` and `recovery_tokens`
     → **existing TOTP silently removed** (`mfa_app_secret = NULL`).
   * current (`e7907403d`, in `v1.0.0-rc.1`) `panel/database/migrations/2025_07_22_091435_update_users_totp.php:29-64`: for `use_totp = true AND totp_secret NOT NULL`
     copies the ciphertext as-is (`DB::raw('totp_secret')`, single encryption, non-serialized thanks to step 6) and converts
     `recovery_tokens.token` (bcrypt hashes) into `Crypt::encryptString(json_encode([...]))` – same format as the `encrypted:array` cast.
9. `users.totp_secret` original column was `CHAR(16)` plaintext before 2017 (`2016_01_23_203159_add_users.php`). Irrelevant now (column dropped).

### 1.10 Required Rust decrypt behaviour (defensive)

Implement `laravel_decrypt_string(payload) -> Result<Vec<u8>>` exactly per §1.3 and wrap DB reads in a tolerant layer:

1. `NULL` → `None`.
2. Decrypt (current + previous keys). On failure → treat as **corrupt** (log, do not crash the whole request; PHP would throw a 500 when the
   attribute is accessed).
3. If the plaintext matches `^s:(\d+):"(.*)";$` (DOTALL) **and** the byte length equals `$1` → unwrap (row written by legacy serialized `encrypt()` that the
   2024 migration failed to convert, e.g. because the migration ran before an `APP_KEY` was restored). PHP itself would *not* unwrap
   (it would return the serialized string) – this is a deliberate robustness improvement; flag it in logs.
4. For `users.mfa_app_secret` only: if the plaintext `appearsEncrypted()` (strict base64 → JSON with iv/value/mac), decrypt once more
   (double-encryption from canary commit `1900c04b7`). Again PHP would fail here; this is a recovery path.
5. Never rewrite rows automatically unless explicitly decided (keep PHP rollback possible).

---

## 2. Passwords

### 2.1 Hashing configuration (framework defaults; panel has no `config/hashing.php`)

`vendor/laravel/framework/config/hashing.php:18-66`:

| Key | Value |
|---|---|
| `driver` | `env('HASH_DRIVER','bcrypt')` (supported: `bcrypt`, `argon`, `argon2id`) |
| `bcrypt.rounds` | `env('BCRYPT_ROUNDS', 12)` |
| `bcrypt.verify` | `env('HASH_VERIFY', true)` |
| `bcrypt.limit` | `env('BCRYPT_LIMIT', null)` |
| `argon.memory/threads/time` | `env('ARGON_MEMORY',65536)`, `env('ARGON_THREADS',1)`, `env('ARGON_TIME',4)`; `verify` = `HASH_VERIFY` |
| `rehash_on_login` | `true` |

`BcryptHasher` (`vendor/laravel/framework/src/Illuminate/Hashing/BcryptHasher.php`):

* `make`: `password_hash($v, PASSWORD_BCRYPT, ['cost'=>rounds])` → `$2y$12$…` (60 chars). If `limit` set and `strlen > limit` → error.
* `check`: empty/NULL hash → `false`; if `verify` and `password_get_info($hash)['algoName'] !== 'bcrypt'` → **RuntimeException("This password does not use the Bcrypt algorithm.")** (HTTP 500). `password_get_info` only recognises `$2y$` as bcrypt (**uncertain** for `$2a$`/`$2b$`; PHP has never produced them here).
* `needsRehash`: `password_needs_rehash($hash, PASSWORD_BCRYPT, ['cost'=>rounds])` → true for `$2y$10$` legacy Pterodactyl hashes when rounds=12.
* **PHP bcrypt truncates the password at 72 bytes** and treats NUL specially (**uncertain** on NUL). Rust bcrypt implementations that reject >72-byte input must truncate to 72 bytes to remain compatible.

Rust must: verify `$2y$` (and accept `$2a$`/`$2b$` defensively), honour `BCRYPT_ROUNDS` for new hashes, and if `HASH_DRIVER` is set to `argon`/`argon2id`
create/verify `$argon2i$`/`$argon2id$` hashes with the configured params (PHP's `password_hash` encoding).

### 2.2 Where passwords are verified

| Flow | Behaviour | Source |
|---|---|---|
| Web login (Filament) | `EloquentUserProvider::validateCredentials` = `Hash::check`; then **again** in `attemptWhen` which calls `rehashPasswordIfRequired` (rehash with configured cost, `forceFill(['password'=>…])->save()`) before `login()` | `vendor/filament/filament/src/Auth/Pages/Login.php` `authenticate()`; `vendor/laravel/framework/src/Illuminate/Auth/SessionGuard.php:470-491,539-543`; `EloquentUserProvider.php:148-178` |
| Client API change password | `current_password` checked with `Hash::check` in `authorize()` (throws `InvalidPasswordProvidedException`), new password rules `required|string|confirmed|min:8`; `is_managed_externally` → 403 | `panel/app/Http/Requests/Api/Client/Account/UpdatePasswordRequest.php:19-37` |
| Client API change email | requires `password` field checked with `Hash::check` | `panel/app/Http/Requests/Api/Client/Account/UpdateEmailRequest.php` `authorize()` |
| SFTP password auth (daemon → panel) | `password_verify($input, $user->password)` (no algorithm check, no rehash) | `panel/app/Http/Controllers/Api/Remote/SftpAuthenticationController.php:46-51` |
| `logoutOtherDevices` (after API password change, session guard only) | `Hash::check` then **forced** rehash (new salt → new hash) | `SessionGuard.php:760-795`, `panel/app/Http/Controllers/Api/Client/AccountController.php` `updatePassword()` |

### 2.3 Password rules / setting

* Profile page: `Password::default()` = `min:8` (no `Password::defaults()` callback is registered anywhere), must equal confirmation,
  stored with `Hash::make` (`panel/app/Filament/Pages/Auth/EditProfile.php:129-140`; `vendor/laravel/framework/src/Illuminate/Validation/Rules/Password.php:166-173`). Hidden for `is_managed_externally` users.
* Reset page: `Password::default()` (`vendor/filament/filament/src/Auth/Pages/PasswordReset/ResetPassword.php:187-197`).
* API `PATCH /api/application/users/{id}` / store: hashed in `UserUpdateService`/`UserCreationService` (`panel/app/Services/Users/UserUpdateService.php`, `UserCreationService.php`); `PasswordChanged` event (update only) triggers SFTP revocation on all nodes (`panel/app/Listeners/RevocationListener.php`).
* Users created without a password (OAuth, API without password, admin UI) get `Hash::make(Str::random(30))` and an `AccountCreated` mail containing a password-reset link (`UserCreationService::handle`). So `users.password` is always a valid bcrypt hash.
* Usernames and emails are always stored lowercase+trimmed (`panel/app/Models/User.php:228-231,256-268`).

### 2.4 Password reset tokens

* Broker config: `panel/config/auth.php:19-26` → table **`password_resets`** (not Laravel's default `password_reset_tokens`), `expire` 60 min, `throttle` 60 s.
  Framework merges `passwords` key so the app's `users` entry fully replaces the framework one (`LoadConfiguration.php:140-150`).
* Table (`2016_01_23_201433_add_password_resets.php`): `email varchar index`, `token varchar index`, `created_at timestamp NOT NULL`. No id/PK.
* `DatabaseTokenRepository` **(framework)** (`vendor/laravel/framework/src/Illuminate/Auth/Passwords/DatabaseTokenRepository.php`):
  * `create`: delete rows for the email, insert `{email, token: Hash::make(plain), created_at: now}`; plain = `hex(HMAC_SHA256(decoded APP_KEY, Str::random(40)))`.
  * `exists`: row by email, not expired (`created_at + 3600 s` in future), `Hash::check(plain, row.token)`.
  * `recentlyCreatedToken`: row `created_at + 60 s` in future → `RESET_THROTTLED`.
  * Successful reset deletes the row.
* Reset link: `URL::signedRoute('filament.app.auth.password-reset.reset', ['email'=>…, 'token'=>…])` (`vendor/filament/filament/src/Panel/Concerns/HasAuth.php:465-472`); the route requires the `signed` middleware (`vendor/filament/filament/routes/web.php:43-50`). Path is `/password-reset/reset?email=…&token=…&signature=…` on the `app` panel (path ''), request page `/password-reset/request` (Filament defaults `resetPasswordRoutePrefix = 'password-reset'`, slugs `request`/`reset`, `vendor/filament/filament/src/Panel/Concerns/HasAuth.php:65-88`; login slug `/login`).
* Reset action: `forceFill(['password'=>Hash::make(new), 'remember_token'=>Str::random(60)])->save()` and fires `PasswordReset` → activity `auth:password-reset` (`ResetPassword.php:88-106`; `panel/app/Listeners/Auth/PasswordResetListener.php`).
* Rate limits: request page `rateLimit(2)` per IP per 60 s; reset page `rateLimit(2)` per IP plus `RateLimiter` key `filament-reset-password:` . sha1(broker.'|'.email), 2 attempts (`RequestPasswordReset.php:59`, `ResetPassword.php:70,144-160`).
* Admin "send password reset" button and `AccountCreated` mail use the same broker (`panel/app/Filament/Admin/Resources/Users/UserResource.php:215-240`, `panel/app/Notifications/AccountCreated.php:34-35`).
* `config('http.rate_limit.password_reset*')` exists (`panel/config/http.php:20-21`) but is **not referenced** by any route.

---

## 3. API keys

### 3.1 Table `api_keys` (current schema, reconstructed from migrations; authoritative DDL in `pelican-rs/crates/pelican-db/schema/{mysql,mariadb,pgsql,sqlite}.sql`)

| Column | Type | Notes |
|---|---|---|
| `id` | int unsigned AI | |
| `user_id` | int unsigned **NULL**, FK users **ON DELETE CASCADE** (`2018_01_13_145209_AddLastUsedAtColumn.php`) | Model validation requires it; nullable in DDL. |
| `public` | varchar(16) NULL | **Legacy leftover** on MySQL/MariaDB/PostgreSQL (only the SQLite branch of `2017_11_19_122708…:32-38` dropped it). Never read/written by Pelican; Rust must ignore it (and not `SELECT *` into a strict struct without it). |
| `key_type` | tinyint unsigned default 0 | 0 = NONE, 1 = ACCOUNT (client), 2 = APPLICATION (`panel/app/Models/ApiKey.php:63-67`). Very old Pterodactyl DBs may contain 3/4 (daemon key types) – no special handling exists. |
| `identifier` | varchar(16) NULL UNIQUE | plaintext |
| `token` | text | encrypted (§1.8) |
| `allowed_ips` | text NOT NULL, JSON array of strings, default `'[]'` (`2024_06_04_133434_make_allowed_ips_column_non_nullable.php:15-20`) | |
| `memo` | text NULL | "description" |
| `last_used_at` | timestamp NULL | |
| `expires_at` | timestamp NULL (`2023_02_23_191004…`) | Never set by Pelican code; honoured by Sanctum (§3.4). `config('panel.api.key_expire_time')` is unused. |
| `permissions` | json NULL (`2024_11_04_185326_revamp_api_keys_permissions.php:24-27`) | see §3.6 |
| `created_at`, `updated_at` | timestamps | |

Legacy `r_servers, r_nodes, r_allocations, r_users, r_locations, r_nests, r_eggs, r_database_hosts, r_server_databases, r_packs, r_mounts`
were converted into `permissions` and dropped (`2024_11_04_185326_revamp_api_keys_permissions.php:35-62`; earlier drops in
`2020_09_13_110021…`, `2024_03_12_154408…`, `2024_03_14_055537…`).

### 3.2 Generation

* `identifier` = prefix + `Str::random(16 - 5)` → 16 chars total (`ApiKey.php:72,213-228`).
  Current prefixes: `pacc_` (ACCOUNT), `papp_` (APPLICATION). History (git): no prefix (Pterodactyl < 1.8, pure `Str::random(16)`),
  `ptlc_`/`ptla_` (Pterodactyl, 2022-05-22 `b051718af`), `plcn_`/`peli_` (Pelican 2024-04-09 `1660af94a`), `pacc_`/`papp_` (2025-09-04 `3543b4773`).
  **All are valid forever**; nothing parses the prefix.
* `token` = `Str::random(32)` (`ApiKey::KEY_LENGTH`, `ApiKey.php:78`), alphanumeric `[A-Za-z0-9]`.
* Full secret shown once to the user = `identifier . token` (48 chars): profile UI `EditProfile.php:279-293`, admin list `ApiKeyResource.php` (`state(fn => $key->identifier . $key->token)` – admins can re-display application keys at any time).
* Client API `POST /api/client/account/api-keys` returns `meta.secret_token` = **only the 32-char token** (`panel/app/Http/Controllers/Api/Client/ApiKeyController.php:38-56`); the client must concatenate `attributes.identifier` + `secret_token`. Limit `config('panel.api.key_limit')` = `API_KEYS_LIMIT` default 25 account keys (`ApiKeyController.php:40`, `panel/config/panel.php:71-74`). `allowed_ips` max 50, each must satisfy `IPTools\Range::parse($ip)->valid()` (`panel/app/Http/Requests/Api/Client/Account/StoreApiKeyRequest.php`).
* Account keys: `permissions` stays default `'[]'`; `memo` = description or `''` (`HasAccessTokens.php:34-41`).
* Application keys (admin UI): `user_id` = creating admin, `permissions` = `{resource: 0|1|3}` for every resource in `ApiKey::getPermissionList()` (`CreateApiKey.php:43-62`, `ApiKeyResource.php` form options 0 none / 1 read / 3 read+write). `memo` required.

### 3.3 Bearer parsing and verification

Order of evaluation for `/api/client/*` and `/api/application/*` (`panel/bootstrap/app.php:37-53`, `panel/app/Providers/RouteServiceProvider.php:42-52`):

```
api group:   EnsureStatefulRequests → auth:sanctum → IsValidJson → TrackAPIKey → AuthenticateIPAccess
+ application-api: SubstituteBindings → AuthenticateApplicationUser → throttle:api.application
+ client-api:      SubstituteBindings → RequireClientApiKey         → throttle:api.client
```

`auth:sanctum` = `RequestGuard(Laravel\Sanctum\Guard)` **(framework)** `vendor/laravel/sanctum/src/Guard.php`:

1. For each guard in `config('sanctum.guard')` = `['web']` (`panel/config/sanctum.php:37`): if the **session** guard has a user → return user with
   `TransientToken` (cookie auth wins even if a bearer header is also present). Only possible when the request is "stateful" (§3.8), since otherwise no session is started.
2. Else `$request->bearerToken()`: last case-insensitive occurrence of `"Bearer "` in `Authorization`, take the rest, cut at first `,`
   (`vendor/laravel/framework/src/Illuminate/Http/Concerns/InteractsWithInput.php:58-69`).
3. `isValidBearerToken`: if token contains `|` → require `ctype_digit(before '|')` and non-empty rest, else reject; otherwise just non-empty.
4. `ApiKey::findToken($token)` (`panel/app/Models/ApiKey.php:197-208`):
   ```
   identifier = substr(token, 0, 16)
   row = SELECT * FROM api_keys WHERE identifier = ? LIMIT 1
   ok  = row && decrypt(row.token) === substr(token, 16)     # PHP '===' (not constant time); DecryptException → 500
   ```
   No `key_type` filter, no `user` existence check here.
5. `isValidAccessToken`: `sanctum.expiration` is `null` (`panel/config/sanctum.php:50`) → only `expires_at` NULL or in future; provider check skipped (guard config `provider` null).
6. `supportsTokens($user)` – User uses `HasApiTokens` → true. Tokenable = `ApiKey::user()` (`ApiKey.php:146-150`, BelongsTo `user_id`; Sanctum morph columns do not exist).
7. Fires `TokenAuthenticated`, then **updates `last_used_at = now()` on every successful bearer request** (`config('sanctum.last_used_at', true)`), preserving the connection's "modified records" flag (`Guard.php` `updateLastUsedAt`). `updated_at` is also bumped by Eloquent `save()` (**uncertain**: `forceFill(...)->save()` with timestamps enabled → yes, `updated_at` changes).
8. Failure → `AuthenticationException` → JSON 401 (`panel/app/Exceptions/Handler.php:375-382`).

Post-auth middleware:

* `TrackAPIKey`: sets activity-log `api_key_id` = token id if an `ApiKey`, else null (`panel/app/Http/Middleware/Activity/TrackAPIKey.php:18-27`).
* `AuthenticateIPAccess` (§3.5).
* `AuthenticateApplicationUser`: user must satisfy `isAdmin()` (§7.3) else 403 "This account does not have permission to access the API." (`panel/app/Http/Middleware/Api/Application/AuthenticateApplicationUser.php:16-23`). Applies to **both** key types and to session users.
* `RequireClientApiKey`: `key_type === 2` on client routes → 403 "You are attempting to use an application API key on an endpoint that requires a client API key." (`panel/app/Http/Middleware/Api/Client/RequireClientApiKey.php:16-23`). Key types 0/1/3/4 are allowed.
* Rate limits: `api.client` / `api.application` keyed by `user.uuid` (fallback IP), `APP_API_CLIENT_RATELIMIT` (256) per `APP_API_CLIENT_RATELIMIT_PERIOD` (1) minutes, same for application (`RouteServiceProvider.php:73-89`, `panel/config/http.php:13-40`).

### 3.4 Expiry / revocation

* `expires_at` past → token invalid (401). Nothing in Pelican sets it.
* Deleting the user cascades the keys. Client `DELETE /api/client/account/api-keys/{identifier}` deletes only `key_type = 1` of the current user and logs `user:api-key.delete` (`ApiKeyController.php:63-78`).

### 3.5 `allowed_ips` semantics (`panel/app/Http/Middleware/Api/AuthenticateIPAccess.php:23-50`)

* Skipped for session (TransientToken) requests and when the list is empty.
* `$request->ip()` (respects `TRUSTED_PROXIES`, `panel/config/trustedproxy.php`) must be inside **any** entry, parsed by `IPTools\Range::parse` (`vendor/s1lentium/iptools/src/Range.php:43-85`):
  * contains `/` or a space → CIDR / "ip mask" network (first..last address);
  * contains `*` → wildcard, `*` replaced by `0` for start and `255` for end (IPv4-style);
  * contains `-` (not at position 0) → explicit range `a-b`;
  * else single IP.
  * Containment = byte-wise `strcmp` of packed addresses (`inet_pton`), so IPv4 vs IPv6 never match each other.
* Miss → activity `auth:ip-blocked` (actor = user, subjects = user + key, property `identifier`) and 403
  `"This IP address (<ip>) does not have permission to access the API using these credentials."`.
* Note: an invalid stored entry throws inside `Range::parse` → 500 (PHP behaviour).

### 3.6 Application-key permissions (`permissions` JSON)

* Shape: JSON object `{"<resource>": int}`; account keys `[]`; may be `NULL` for legacy rows created before the column existed and not touched by the conversion (treat NULL/`[]` as all-NONE). `ApiKey::getPermission($r)` = `permissions[$r] ?? 0` (`ApiKey.php:155-158`).
* Resource names (`ApiKey::DEFAULT_RESOURCE_NAMES`, `ApiKey.php:160-172` + plugin-registered `registerCustomResourceName`):
  `server`, `node`, `allocation`, `user`, `egg`, `database_host`, `server_database`, `mount`, `role`, `plugin`, `webhook`.
  The legacy conversion mapped `r_servers→server, r_nodes→node, r_allocations→allocation, r_users→user, r_eggs→egg, r_database_hosts→database_host, r_server_databases→server_database, r_mounts→mount` (`2024_11_04…:35-50`).
* Values are bit flags `AdminAcl::NONE=0, READ=1, WRITE=2` (`panel/app/Services/Acl/Api/AdminAcl.php:13-38`); check is `($value & $action) != 0`.
  UI writes 0/1/3; validation accepts 0..3 (`ApiKey.php:116`). Value 2 = write without read.
* Authorization per endpoint (`panel/app/Http/Requests/Api/Application/ApplicationApiRequest.php:35-55`): each request class declares
  `$resource` and `$permission` (READ for GETs, WRITE for mutations, one NONE):
  * Session user (TransientToken): READ → `can('viewList <resource>') && can('view <resource>')`; WRITE → `can('update <resource>')`; NONE → **false**.
    Note: for `database_host` / `server_database` / `webhook` resources these strings don't match any role permission name (roles use `databaseHost`, `database`, `webhook`) — so only root admins pass, except `webhook` which matches. Replicate.
  * Root admin + **ACCOUNT** key → always true.
  * Otherwise → `AdminAcl::check(key, resource, permission)` (so a root admin's APPLICATION key is still limited by its flags; non-root admins with an ACCOUNT key get 403 because account keys have no flags).
* Client API requests (`ClientApiRequest`, `panel/app/Http/Requests/Api/Client/ClientApiRequest.php:14-29`): if the request defines `permission()`, require a `{server}` route param and `user->can(<subuser permission>, $server)` (§7.6); otherwise allowed.

### 3.7 Sanctum integration summary

* `Sanctum::usePersonalAccessTokenModel(ApiKey::class)` (`panel/app/Providers/AppServiceProvider.php:82`); `ApiKey extends Laravel\Sanctum\PersonalAccessToken`, table `api_keys` (Eloquent default name), no `abilities`, no `tokenable_*`, no `name` columns.
* `User` uses `HasAccessTokens` which wraps Sanctum `HasApiTokens`: `tokens()` = all `api_keys` of the user (any type); `apiKeys()` = `key_type = 1` only (`User.php:280-285`; `HasAccessTokens.php:23-26`).
* `currentAccessToken()` is either the `ApiKey` row or `TransientToken` (session).

### 3.8 Browser (session) access to the API — stateful requests & CSRF

* `EnsureStatefulRequests::fromFrontend` (`panel/app/Http/Middleware/EnsureStatefulRequests.php:18-25`) = Sanctum's check **OR** the request carries a
  cookie named `config('session.cookie')` (`pelican_session` by default). Sanctum's check (`vendor/laravel/sanctum/src/Http/Middleware/EnsureFrontendRequestsAreStateful.php`):
  take `Referer` (fallback `Origin`), strip `http://`/`https://`, ensure trailing `/`, match `Str::is` against each `SANCTUM_STATEFUL_DOMAINS` entry + `/*`.
  Default list: `localhost,localhost:3000,127.0.0.1,127.0.0.1:8000,::1,<APP_URL host[:port]>` (`panel/config/sanctum.php:19-23`, `vendor/laravel/sanctum/src/Sanctum.php:45-50`).
* For stateful requests Sanctum forces `session.http_only=true`, `session.same_site=lax` and runs, in order: mark `sanctum` attribute,
  `EncryptCookies`, `AddQueuedCookiesToResponse`, `StartSession`, `Illuminate\Foundation\Http\Middleware\ValidateCsrfToken` (= framework `PreventRequestForgery`, **not** the app subclass, so the `remote/*` excepts do not apply). `authenticate_session` is not configured → no password-hash session check on API calls.
* CSRF rules (Laravel 13 `PreventRequestForgery::handle`, `vendor/laravel/framework/src/Illuminate/Foundation/Http/Middleware/PreventRequestForgery.php`): pass if method is `GET/HEAD/OPTIONS`, **or `Sec-Fetch-Site: same-origin`**, or token matches: `_token` input, else `X-CSRF-TOKEN` header, else `X-XSRF-TOKEN` header (decrypted, prefix stripped) — compared with session `_token` via `hash_equals`. Otherwise 419 `TokenMismatchException`. Every passing response re-sets the `XSRF-TOKEN` cookie (value = session token, encrypted by `EncryptCookies`; `httpOnly=false`, lifetime = session lifetime, path/domain/secure/samesite from session config).
* Consequence to replicate: a bearer-token client that **also** sends a `pelican_session` cookie is treated as stateful and needs CSRF for writes
  (unless `Sec-Fetch-Site: same-origin`); if the session holds a logged-in user, that user (not the token owner) is used.
* CORS: paths `/api/client*`, `/api/application*`, methods GET/POST/PUT/PATCH/DELETE/HEAD, origins `APP_CORS_ALLOWED_ORIGINS` (comma list), credentials allowed (`panel/config/cors.php`).

### 3.9 Daemon (Wings) credentials

* Wings → panel (`/api/remote/*`, `daemon` middleware group, CSRF-excluded `remote/*`): `DaemonAuthenticate` (`panel/app/Http/Middleware/Api/Daemon/DaemonAuthenticate.php:28-50`):
  missing bearer → 401 with `WWW-Authenticate: Bearer`; bearer split on `.` must give exactly 2 non-empty parts else 400;
  `Node WHERE daemon_token_id = part0` → 404 if none; `hash_equals(decrypt(daemon_token), part1)` else 403. Node stored in request attributes.
* Panel → Wings: `Authorization: Bearer <decrypted daemon_token>` (`AppServiceProvider.php:68-78`); TLS verify only in `production`. Responses' `User-Agent` must match `^Pelican Wings/v(\d+\.\d+\.\d+|develop) \(id:(\w*)\)$` and the id must equal `daemon_token_id`, else `ConnectionException` (`panel/app/Repositories/Daemon/DaemonRepository.php:55-67`).
* Node JWTs (websocket, file upload/download, backup download, transfer): HS256 with key = plaintext `daemon_token`; `iss = APP_URL`, `aud = node connection address`, `jti` (claim **and header**) = `sha256(identifiedBy)`, `iat = now`, `nbf = now − 5 min`, `exp` as set, optional `sub` (claim + header), custom claims, `user_uuid`, `scope` (space separated: `backup-download`, `file-download`, `file-upload`, `transfer`, `websocket`), `unique_id = Str::random(16)`. Date claims are **integer** unix seconds (`panel/app/Services/Nodes/NodeJWTService.php:72-110`, `panel/app/Extensions/Lcobucci/JWT/Encoding/TimestampDates.php`, `panel/app/Enums/NodeJwtScope.php`).

---

## 4. Two-factor authentication

### 4.1 Columns (users)

| Column | Type | Meaning |
|---|---|---|
| `mfa_app_secret` | text NULL, encrypted | base32 TOTP secret; **TOTP enabled ⇔ `filled(decrypted)`** |
| `mfa_app_recovery_codes` | text NULL, `encrypted:array` | JSON array of bcrypt hashes of unused recovery codes |
| `mfa_email_enabled` | bool default false | email-code MFA enabled |

Legacy columns (dropped by `2025_07_22…`): `use_totp` tinyint, `totp_secret` text (encrypted), `totp_authenticated_at` timestamptz; table
`recovery_tokens(id, user_id FK cascade, token varchar = bcrypt hash, created_at)` (`2020_07_02_213612_create_user_recovery_tokens_table.php`). A Rust implementation
targeting an **un-migrated** DB is out of scope — require the PHP migrations to have run (check `migrations` table).

### 4.2 Providers & configuration

`panel/app/Providers/Filament/PanelProvider.php:60-63`: `multiFactorAuthentication([AppAuthentication::make()->recoverable(), EmailAuthentication::make()])`
on every panel (`app`, `admin`, `server`). Provider order matters: the **first enabled provider** for a user is challenged → TOTP wins over email
(`vendor/filament/filament/src/Auth/MultiFactor/MultiFactorChallenge.php:38-52`). MFA is **not** "required" at the Filament level; Pelican enforces enrolment itself (§4.6).

### 4.3 TOTP (Filament `AppAuthentication` + `pragmarx/google2fa`)

Source: `vendor/filament/filament/src/Auth/MultiFactor/App/AppAuthentication.php`, `vendor/pragmarx/google2fa/src/Google2FA.php`.

* Secret generation: `generateSecretKey(16)` → 16 chars of RFC 4648 base32 alphabet `A-Z2-7` (80 bits). Legacy Pterodactyl secrets are **32** chars.
  google2fa validates: chars in alphabet (case-insensitive, upper-cased before decode), length ≥ 8, and with Google-Authenticator compatibility enforced the length must be a power of two.
* Algorithm: HOTP(SHA1, key = base32-decode(secret), counter = `floor(time()/30)` packed as 64-bit big-endian), dynamic truncation, `% 10^6`, left-pad to 6 digits (`Google2FA.php:199-330`).
* Window: Filament `codeWindow = 8` → accepts counters `[T-8, T+8]` (±4 min) (`AppAuthentication.php:50`, `Google2FA::findValidOTP`). `config('panel.auth.2fa.window')` (=4) and `2fa.bytes` (=32) in `panel/config/panel.php:7-11` are **unused leftovers**.
* Code comparison: `hash_equals(computed, (string) input)` — input must be exactly the 6-digit string.
* Anti-replay (login challenge, disable, regenerate): cache key `filament.app_authentication_codes.` . md5(secret) stores the last accepted
  time-step **forever**; a new code must have counter `> last` and within window (`AppAuthentication.php:173-229`, `verifyKeyNewer`). Uses a cache lock
  `<key>.lock` (60 s, block 10 s) when the store supports locks. Setup confirmation does **not** use anti-replay.
* Setup: server generates secret + 8 recovery codes, shows QR (`otpauth://totp/<brand>:<email>?secret=…&issuer=<brand>`, brand = `strip_tags(APP_NAME)`, holder = `users.email` — `User.php:548-551`) (**uncertain**: exact URI parameter order from google2fa `getQRCodeUrl`), user confirms with a code, then `saveSecret` + `saveRecoveryCodes` in one transaction (`vendor/filament/filament/src/Auth/MultiFactor/App/Actions/SetUpAppAuthenticationAction.php:87-224`).
* Disable: requires a valid current code (anti-replay) or recovery code; sets secret and codes to `NULL` (`DisableAppAuthenticationAction.php:74-124`).
* Admin/CLI reset: `php artisan p:user:disable2fa` sets `mfa_app_secret = NULL, mfa_app_recovery_codes = NULL, mfa_email_enabled = false` (`panel/app/Console/Commands/User/DisableTwoFactorCommand.php`).

### 4.4 Recovery codes

* Generation: 8 codes, each `Str::random(10) . '-' . Str::random(10)` (21 chars, `[A-Za-z0-9]` + `-`) (`AppAuthentication.php:43,168-171`).
* Storage: `Hash::make(code)` per code (bcrypt, current cost) → array → `encrypted:array` (`AppAuthentication.php:118-130`).
* Legacy codes migrated from `recovery_tokens`: Pterodactyl generated 10 codes of `Str::random(10)` stored with `password_hash` (bcrypt cost 10). Still verified by `Hash::check`.
* Verification (`AppAuthentication.php` `verifyRecoveryCode`): inside a DB transaction with `SELECT … FOR UPDATE` on the user (and a cache lock
  `filament.app_authentication_recovery_codes.` . md5(class.':'.id)), iterate hashes; first match is consumed, the remaining hashes are saved back
  (re-encrypted). Case-sensitive. Regeneration requires a valid TOTP code and replaces all 8.

### 4.5 Email MFA (`vendor/filament/filament/src/Auth/MultiFactor/Email/EmailAuthentication.php`)

* Enabled ⇔ `users.mfa_email_enabled` (`User.php:569-577`).
* Before challenge (and on "resend"): rate-limit `filament-email-authentication:` . scopedId, max 2 per 60 s; code = 6 digits `random_int(0, 999999)` zero-padded; cache
  `filament.email_authentication_codes.` . sha256(scopedId . '|' . sessionId) = `{hash: bcrypt(code), expiresAt}` with TTL **4 min**; send mail notification `VerifyEmailAuthentication`.
* Verify: `Hash::check`, not expired, then cache entry deleted (single use).
* scopedId (`vendor/filament/filament/src/FilamentManager.php:623-636`) = `sha256(strlen(guard).":".guard . strlen(class).":".class . strlen(id).":".id)` with guard `web`, class `App\Models\User`.

### 4.6 Login enforcement & `RequireTwoFactorAuthentication`

* MFA challenge happens **only** in the Filament password login (§6.3). **OAuth login (§6.5) and passkey login (§5) bypass MFA entirely.**
* `RequireTwoFactorAuthentication` (`panel/app/Http/Middleware/RequireTwoFactorAuthentication.php:28-61`), attached only as Filament `authMiddleware` (`PanelProvider.php:78-81`) — **not** on `/api/*`:
  * level = `(int) config('panel.auth.2fa_required')` = `APP_2FA_REQUIRED` (0 none, 1 admins, 2 all) (`panel/config/panel.php:6`; edited via Settings page `panel/app/Filament/Admin/Pages/Settings.php:230-240`).
  * Skip when no user or route name matches `*auth.*` (login, logout, profile `filament.*.auth.profile`).
  * has2fa = `mfa_email_enabled || filled(mfa_app_secret)`. Pass when level 0 or has2fa; pass when level 1 and `!isAdmin()`.
  * Else: if `$request->isJson()` (or path starts with `/api` – never true because `path()` has no leading slash) → `TwoFactorAuthRequiredException` (HTTP 400, "Two-factor authentication is required on this account in order to access this endpoint.") — this hits Livewire JSON updates. Otherwise flash banner `auth.2fa_must_be_enabled` and redirect to profile `?tab=2fa::data::tab`.
* It only checks that 2FA is *configured*, not that it was used in the current session.
* Rate limit for challenge submissions: `filament-multi-factor-challenge:` . scopedId, 5 attempts per 60 s (`MultiFactorChallenge.php:158-181`).

### 4.7 Activity events for 2FA

`TwoFactorListener` maps `ProvidedAuthenticationToken` → `auth:token` / `auth:recovery-token` (`panel/app/Listeners/Auth/TwoFactorListener.php`), but **nothing dispatches that event** (TODO). Historical rows from Pterodactyl contain `auth:checkpoint`, `auth:token`, `auth:recovery-token`, `user:two-factor.create/delete` (translations in `panel/lang/en/activity.php:10-39`).

---

## 5. Passkeys (laravel/passkeys 0.2, webauthn-lib 5.3)

### 5.1 Table `passkeys` (`panel/database/migrations/2026_02_07_103629_create_passkeys_table.php`)

| Column | Type |
|---|---|
| `id` | bigint AI |
| `user_id` | int unsigned FK users ON DELETE CASCADE |
| `name` | text |
| `credential_id` | varchar(512) UNIQUE – base64url **unpadded** of raw credential id |
| `credential` | json – serialized `CredentialRecord` |
| `last_used_at` | timestamp NULL |
| `created_at`, `updated_at` | timestamps |

`credential` JSON keys (`vendor/web-auth/webauthn-lib/src/Denormalizer/PublicKeyCredentialSourceDenormalizer.php:82-94`, same for `CredentialRecordDenormalizer`):
`publicKeyCredentialId` (b64url), `type` (`public-key`), `transports` (array), `attestationType`, `trustPath` (object), `aaguid` (UUID string),
`credentialPublicKey` (b64url COSE key), `userHandle` (b64url of the 32-byte HMAC, §1.7), `counter` (int), `otherUI`, `backupEligible`, `backupStatus`, `uvInitialized`.
Model: `vendor/laravel/passkeys/src/Passkey.php` (cast `credential` json, appended `authenticator` label from AAGUID).

### 5.2 Configuration (effective)

Panel overrides (`panel/config/passkeys.php:7-43`) merged over package defaults (`vendor/laravel/passkeys/config/passkeys.php`, shallow `mergeConfigFrom`):

| Key | Value |
|---|---|
| `relying_party_id` | host of `PASSKEYS_RELYING_PARTY_ID` (bare domain or URL, normalised) else host of `APP_URL` |
| RP name | = RP id (`Passkeys::relyingPartyName()`) |
| `allowed_origins` | normalised `APP_URL` origin + `PASSKEYS_ALLOWED_ORIGINS` (comma list); normalisation: lowercase scheme/host, add `https://` if no scheme, drop default port (`panel/app/Helpers/PasskeyOrigin.php`) |
| `strict_origin` | `PASSKEYS_STRICT_ORIGIN` (false) — when false, `AllowPasskeyOrigin` adds the request's `Origin` (or scheme+host) if its host == RP id == request host and scheme is https (or http on localhost/127.0.0.1/[::1]) (`panel/app/Http/Middleware/AllowPasskeyOrigin.php`) |
| `user_handle_secret` | `PASSKEYS_USER_HANDLE_SECRET` else raw `APP_KEY` string |
| `timeout` | 60000 ms |
| `guard` | `web` |
| `middleware` | `web`, `AddPasskeyErrorMessage`, `AllowPasskeyOrigin` |
| `management_middleware` | `auth` (package default would be `password.confirm`) |
| `throttle` | `throttle:6,1` |
| `redirect` | `/` |

### 5.3 Routes / flows (`vendor/laravel/passkeys/routes/routes.php`)

| Route | Middleware | Behaviour |
|---|---|---|
| `GET /passkeys/login/options` | guest:web, throttle 6/min | `PublicKeyCredentialRequestOptions` (32-byte challenge, rpId, `allowCredentials=[]`, `userVerification=required`, timeout) stored serialized in session `passkey.verification_options`; returns `{options}` |
| `POST /passkeys/login` | guest:web, throttle | body `{credential:{id,rawId,type:'public-key',response}, remember?:bool}`; options pulled from session (once); lookup `credential_id = b64url(rawId)` **FOR UPDATE**; assertion validated (origin, rpId hash, challenge, UV, signature, counter); credential JSON + `last_used_at` updated; `Auth::guard('web')->login(user, remember)`; session regenerated; JSON `{redirect}` or redirect to intended `/` |
| `GET /passkeys/confirm/options`, `POST /passkeys/confirm` | auth:web, throttle | password-confirmation via passkey |
| `GET /user/passkeys/options` | auth:web, auth, throttle | `PublicKeyCredentialCreationOptions`: rp, user `{name: users.email, id: userHandle, displayName: users.username}` (`User.php:579-587`), challenge 32 B, ES256 then RS256, residentKey **required**, userVerification **required**, attestation `none`, excludeCredentials = user's passkeys |
| `POST /user/passkeys` | same | validates attestation, rejects duplicate `credential_id`, inserts row |
| `DELETE /user/passkeys/{passkey}` | auth:web, auth | deletes (Pelican UI deletes via Livewire `auth()->user()->passkeys()->findOrFail($id)->delete()`, `panel/app/Livewire/Passkeys.php`) |

* Passkey login fires `Illuminate\Auth\Events\Login` → activity `auth:success`; no MFA challenge; `Passkeys::authorizeLoginUsing` not set.
* UI hides passkey button/tab when `!$request->isSecure()` (`panel/app/Filament/Pages/Auth/Login.php:79-89`, `EditProfile.php` passkeys tab).
* Passkeys are bound to the RP id: changing `APP_URL` host invalidates all registered passkeys (browser side) — Rust must reproduce the same RP id and user handle derivation.

---

## 6. Sessions & login

### 6.1 Session config (effective)

`panel/config/session.php:5-7` overrides only `driver` (`SESSION_DRIVER`, default **`file`**) and `cookie` (`SESSION_COOKIE`, default **`pelican_session`**);
the rest comes from `vendor/laravel/framework/config/session.php`: `lifetime` `SESSION_LIFETIME`=120 min, `expire_on_close` false, `encrypt` `SESSION_ENCRYPT`=false,
files `storage/framework/sessions`, table `sessions`, `path` `/`, `domain` `SESSION_DOMAIN`, `secure` `SESSION_SECURE_COOKIE` (null), `http_only` true, `same_site` `lax`, `partitioned` false, lottery 2/100,
`serialization` = **`php`** (`vendor/laravel/framework/src/Illuminate/Session/SessionManager.php:204,221`).

* Session id: 40 chars `[A-Za-z0-9]` (`Session/Store.php:33,712-725`). Cookie value is the id encrypted per §1.7 cookie scheme.
* Stored payload = PHP `serialize($attributes)` (file driver: file `storage/framework/sessions/<id>`; database driver: `sessions(id, user_id, ip_address, user_agent, payload=base64(serialize), last_activity)` – `2016_01_23_203947_create_sessions_table.php`, `DatabaseSessionHandler.php:202-217`; redis via cache store).
* Important keys: `_token` (CSRF, 40 chars), `login_web_59ba36addc2b2f9401580f014c7f58ea4e30989d` (= `'login_web_'.sha1('Illuminate\Auth\SessionGuard')`, value = user id),
  `password_hash_web` (HMAC per §1.7, or raw hash when set by Filament profile save), `url.intended`, `passkey.verification_options`, flash data.
* Per decision `docs/decisions/0009-sessions-cache-queue.md` the Rust panel does **not** read/write PHP session payloads or PHP cookies: it stores
  its own (JSON, base64) payload in the existing `sessions` table under its own cookie name; users re-login at cutover. The PHP formats above are
  documented only so nobody tries to parse them by accident (e.g. leftover `sessions` rows from a PHP install using the database driver must be
  ignored/expired, not decoded). The same decision puts cache-backed state (rate limiters, MFA anti-replay steps, email MFA codes) in process memory.

### 6.2 Guards

* `web` = session guard, provider `users` (Eloquent `App\Models\User`) (framework `config/auth.php` merged with panel `config/auth.php`).
* `api` guard (`driver: token`) is declared (`panel/config/auth.php:12-17`) but unused.
* `sanctum` guard registered by Sanctum (§3.3).
* Timebox for login attempts: `auth.timebox_duration` default 200 ms (`Login.php` uses `config('auth.timebox_duration', 200_000)` µs).

### 6.3 Password login flow (Filament `App\Filament\Pages\Auth\Login` → `Filament\Auth\Pages\Login`)

Sources: `panel/app/Filament/Pages/Auth/Login.php`, `vendor/filament/filament/src/Auth/Pages/Login.php` `authenticate()`.

1. `config('auth.disable_password_login')` (`AUTH_DISABLE_PASSWORD_LOGIN`) → only OAuth + passkey buttons; any submission fails with `auth.password_login_disabled` (`Login.php:51-56,154-160`).
2. Rate limit: `rateLimit(5)` → key `livewire-rate-limiter:` . sha1(`App\Filament\Pages\Auth\Login|authenticate|<ip>`), 5 hits / 60 s, every submit counts (`vendor/danharrin/livewire-rate-limiting/src/WithRateLimiting.php`). Message: notification "Too many login attempts…" with seconds.
3. Captcha (if enabled and not in MFA step) — §6.4.
4. Credentials: field `login`; if `FILTER_VALIDATE_EMAIL` → `email`, else `username`; value `mb_strtolower` (`Login.php:162-167`). Plus `password`, `remember`.
5. Within a 200 ms timebox: fire `Attempting`; `retrieveByCredentials` (`WHERE email|username = ?`); `Hash::check`; check `canAccessPanel(panel)`. On failure fire `Failed` (→ activity `auth:fail`) and show `filament-panels::auth/pages/login.messages.failed` ("These credentials do not match our records.") on `data.login`; captcha widget reset.
6. MFA: if the user has an enabled provider, the component stores `encrypt(['identifier'=>id,'userKey'=>scopedId])` and shows the challenge (email code is sent here). The challenge submit re-runs steps 2–5 (password re-validated), then validates the challenge form (TOTP with anti-replay or recovery code, or email code), rate limited 5/min per user.
7. `attemptWhen(credentials, same user && canAccessPanel, remember)` → rehash-if-needed → `login()` (session `login_web_…`, `password_hash_web`, session id regenerated, remember cookie if requested) → `Login` event (→ activity `auth:success`) → `session()->regenerate()`.
8. Response: redirect to intended URL, or the admin panel if the user's customization `redirect_to_admin` is set and they can access it (`panel/app/Http/Responses/LoginResponse.php`).

The `auth.lockout` config (`panel/config/auth.php:7-10`, 3 attempts / 2 min) is a **dead leftover**.

Panel access (`User::canAccessPanel`, `User.php:475-486`): root admin → all; `admin` panel → `isAdmin()`; `app`/`server` → everyone. Server tenant access: `canAccessTenant` (`User.php:523-536`).

### 6.4 Captcha (`panel/app/Extensions/Captcha`)

* Only Cloudflare Turnstile ships. Enabled iff `env('CAPTCHA_TURNSTILE_ENABLED')` truthy (`.../Schemas/Turnstile/TurnstileSchema.php:22-25`); first active schema is used.
* Config: `CAPTCHA_TURNSTILE_SITE_KEY`, `CAPTCHA_TURNSTILE_SECRET_KEY`, `CAPTCHA_TURNSTILE_VERIFY_DOMAIN` (default true) (`Schemas/BaseSchema.php`, `TurnstileSchema.php`).
* Field is required on the login form (not shown during MFA step) (`Login.php:66-69`; `Turnstile/Component.php:13-22`).
* Validation (`TurnstileSchema.php:74-115`): POST JSON `{secret, response}` to `https://challenges.cloudflare.com/turnstile/v0/siteverify` (timeout 15 s, connect 5 s, HTTP errors throw); `success=false` → error mapped from `error-codes[0]`; if verify-domain, response `hostname` must equal the request host. Failure → validation error "Captcha validation failed: <msg>".
* `App\Events\Auth\FailedCaptcha` exists but is never dispatched.

### 6.5 OAuth / Socialite (`panel/app/Extensions/OAuth`, `panel/app/Http/Controllers/Auth/OAuthController.php`)

* Providers registered (`panel/app/Providers/Extensions/OAuthServiceProvider.php:21-43`): `facebook`, `x`, `linkedin`, `google`, `github`, `gitlab`, `bitbucket`, `slack` (Socialite built-ins), `authentik`, `discord`, `steam` (socialiteproviders).
* Per provider `<ID>` = upper-case id (`panel/app/Extensions/OAuth/Schemas/OAuthSchema.php`):
  * enabled: `OAUTH_<ID>_ENABLED` (truthy);
  * `OAUTH_<ID>_CLIENT_ID`, `OAUTH_<ID>_CLIENT_SECRET` (Steam: secret = Web API key, `allowed_hosts` = APP_URL host);
  * `OAUTH_<ID>_SHOULD_CREATE_MISSING_USERS`, `OAUTH_<ID>_SHOULD_LINK_MISSING_USERS` (default false);
  * extras: `OAUTH_AUTHENTIK_BASE_URL`, `OAUTH_AUTHENTIK_DISPLAY_NAME` (Authentik), `OAUTH_AUTHENTIK_DISPLAY_COLOR` (#fd4b2d), `OAUTH_GITLAB_HOST`.
  * `services.<id>.redirect` = `/auth/oauth/callback/<id>` (relative) (`panel/app/Extensions/OAuth/OAuthService.php:34-47`).
* Routes (`panel/routes/auth.php`, `web` + `guest`, prefix `/auth`): `GET /auth/oauth/redirect/{driver}` (disabled → redirect `auth.login`), `GET /auth/oauth/callback/{driver}` (guest middleware removed). `GET /auth/login` redirects to `/login`.
* Callback (`OAuthController.php:41-131`):
  1. provider disabled/unknown → redirect login. `error` query param → report + persistent danger notification.
  2. `Socialite::driver(id)->user()` (state checked in session by Socialite; Steam uses OpenID).
  3. **Logged-in user** → link: `users.oauth[id] = providerUserId` → redirect profile `?tab=oauth::data::tab`.
  4. Else find `User WHERE JSON_CONTAINS(oauth, json(providerUserId), '$."<id>"')` (`whereJsonContains('oauth->id', $id)`) → login.
     Type-sensitive on MySQL: the id is stored with the PHP type Socialite returned (GitHub int, Discord/Google string…). Rust should match both `"123"` and `123`.
  5. Else needs email (else error "No email was linked…"); if provider exposes `email_verified` and it is false → error.
     Existing user with that email → only if `SHOULD_LINK_MISSING_USERS` → link + login. Else no user → only if `SHOULD_CREATE_MISSING_USERS` → `UserCreationService` (username = provider nickname or emailLocalPart+3 random, random password, `oauth = {id: providerUserId}`, AccountCreated mail with reset link).
  6. Login = `auth()->guard()->login($user, remember: true)` → **no MFA, always remember-me** → redirect `/`.
* `users.oauth` JSON: default attribute `'[]'`; values `[]` (empty, JSON array!) or object `{"discord":"…","github":123}`. Unlink removes key (`OAuthService::unlinkUser`, profile action `EditProfile.php` oauth tab).

### 6.6 Remember-me

* Cookie `remember_web_59ba36addc2b2f9401580f014c7f58ea4e30989d`, lifetime 576000 min (400 days) (`SessionGuard.php:62,630-648,902-905`).
* Value (before cookie encryption) = `<user id>|<users.remember_token>|<hex HMAC_SHA256(APP_KEY string, users.password)>`.
* Validation (`SessionGuard.php:215-244`, `EloquentUserProvider.php:68-83`): user by id, `hash_equals(remember_token, token)`, and password hash matches either HMAC form or raw `users.password` (legacy). On success → session login + `Login` event (`auth:success` logged again).
* `remember_token` = `Str::random(60)`, set on first remember login, rotated on logout (if non-empty) and on password reset (`SessionGuard.php:670-691,743-748`).

### 6.7 `is_managed_externally`

`users.is_managed_externally` bool default false (`2025_10_23_073209…`). Effects: profile username/email disabled and password hidden (`EditProfile.php:115-131`); client API username/email/password updates return 403 via `authorize()` false (`UpdateUsernameRequest`, `UpdateEmailRequest`, `UpdatePasswordRequest:30`). Settable only by admins / application API (`StoreUserRequest`, `UpdateUserRequest`). Login is unaffected.

### 6.8 Other limits

* Client API email change: max 3 per 24 h per user (`user:update-email:<uuid>`) (`AccountController.php:25,80-92`).
* SFTP auth (`SftpAuthenticationController.php:22-160`): `ThrottlesLogins` 5 attempts / 1 min, key = lowercase(`<server part of username>|<ip>`) (note: server id, not username); username format `<username>.<server uuid or uuid_short>` (split at last `.`); password via `password_verify`, public key via SHA256 fingerprint against `user_ssh_keys`; access requires owner, `update server` permission, or subuser `file.sftp`. Responses: `{user: uuid, server: uuid, permissions: [...]}` (permissions from §7.7).

---

## 7. Authorization

### 7.1 spatie/laravel-permission tables (`2024_07_19_130942_create_permission_tables.php`; teams disabled)

| Table | Columns |
|---|---|
| `permissions` | `id` bigint, `name`, `guard_name`, timestamps, UNIQUE(name, guard_name) |
| `roles` | `id` bigint, `name`, `guard_name`, timestamps, UNIQUE(name, guard_name) |
| `model_has_permissions` | `permission_id`, `model_type`, `model_id`; PK(permission_id, model_id, model_type) |
| `model_has_roles` | `role_id`, `model_type`, `model_id`; PK(role_id, model_id, model_type) |
| `role_has_permissions` | `permission_id`, `role_id`; PK |
| `node_role` (Pelican) | `node_id` int unsigned FK cascade, `role_id` bigint FK cascade, UNIQUE(node_id, role_id) (`2025_04_08_113556_create_node_role_table.php`) |

* Config: panel overrides only `models.role = App\Models\Role` (`panel/config/permission.php`); everything else is the package default (`vendor/spatie/laravel-permission/config/permission.php`): morph key `model_id`, wildcard permissions **off**, cache key `spatie.permission.cache` (24 h, default store).
* `model_type` = **`user`** because of `Relation::enforceMorphMap` (`panel/app/Providers/AppServiceProvider.php:127-140`, present since 2022).
* `guard_name` = `web` for everything (`Role::DEFAULT_GUARD_NAME`, `panel/app/Models/Role.php:50`; spatie default guard for User resolves to `auth.defaults.guard` = `web`). Permission lookups are `findByName(name, 'web')` – exact, case-sensitive string match (a 2025-11-11 migration normalised casing, `2025_11_11_121905_fix_case_for_role_permissions.php`).
* Users get permissions only through roles in practice (UI writes `role_has_permissions` via `syncPermissions`, `panel/app/Filament/Admin/Resources/Roles/Pages/CreateRole.php:49-73`, `EditRole.php:47-53`), but direct `model_has_permissions` rows are honoured by spatie.
* Concurrent PHP+Rust operation is not a supported mode (decision 0009); if a PHP panel is started again after Rust changed roles/permissions,
  its cached `spatie.permission.cache` (24 h) may be stale — clear the PHP cache (`php artisan permission:cache-reset` / `cache:clear`) on rollback.

### 7.2 Root admin

* Role named exactly **`Root Admin`** (`Role::ROOT_ADMIN`, `Role.php:48,190-201`), created by seeder / `findOrCreate('Root Admin','web')`.
  Legacy `users.root_admin = 1` was converted to this role (`2024_08_01_114538_remove_root_admin_column.php:15-26`); root admins have no other roles (`2025_04_07_062945…`).
* Detection: `User::isRootAdmin()` = `hasRole('Root Admin')` (`User.php:465-468`).
* `Gate::before(fn ($user) => $user->isRootAdmin() ? true : null)` (`AppServiceProvider.php:105`) → every ability allowed.
* Root admin can target everything; non-root admins cannot target root admins (`canTarget`, `User.php:498-516`). API refuses to assign/remove the root role or modify roles of a root admin (`panel/app/Http/Controllers/Api/Application/Users/UserController.php` `assignRoles/removeRoles`). Last root admin protections exist in UI (`isLastRootAdmin`, `User.php:458-463`).

### 7.3 `isAdmin()`

`isRootAdmin() || (roles count ≥ 1 && getAllPermissions() count ≥ 1)` (`User.php:470-473`). Gates the admin panel and the application API.

### 7.4 Admin permission names (spatie `permissions.name`, guard `web`)

Format `"<prefix> <model>"` (`Role::getPermissionList`, `Role.php:121-163`; enums `panel/app/Enums/RolePermissionModels.php`, `RolePermissionPrefixes.php`):

* Prefixes: `viewList`, `view`, `create`, `update`, `delete`.
* Models: `apiKey`, `allocation`, `backupHost`, `databaseHost`, `database`, `egg`, `mount`, `node`, `role`, `server`, `user`, `webhook` → 5 × 12 = 60 names, e.g. `viewList server`, `update databaseHost`.
* Model-specific: `import egg`, `export egg`.
* Special: `view settings`, `update settings`, `view health`, `seeIps activityLog` (renamed from `seeIps activity`, `2025_08_30_111308…`), `view panelLog`, `viewList plugin`, `create plugin`, `update plugin`, `delete plugin`.
* Plugins may add more via `Role::registerCustomPermissions` / `registerCustomDefaultPermissions`.

### 7.5 Gate resolution (exact order — must be replicated)

`Gate::raw` **(framework)** `vendor/laravel/framework/src/Illuminate/Auth/Access/Gate.php:428-453,621-645,781-864`:

1. Global before-callbacks (spatie's, registered via `callAfterResolving(Gate::class)` in `vendor/spatie/laravel-permission/src/PermissionServiceProvider.php:51`, and Pelican's root-admin one). First non-null wins; their relative order is irrelevant because neither ever returns `false`:
   * spatie: `checkPermissionTo(ability)` → `true` if the user has a permission literally named `ability`; `null` otherwise (non-existent permission names count as false→null) (`vendor/spatie/laravel-permission/src/PermissionRegistrar.php:119-130`, `Traits/HasPermissions.php:253-260`).
   * Pelican: root admin → `true`.
   * ⇒ **Any user holding permission `update server` passes `can('update server', $anyServer)` without node restrictions.**
2. Otherwise the model policy (auto-discovered `App\Policies\<Model>Policy`): policy `before()` then method. Ability strings with spaces are used as method names verbatim; `ServerPolicy` defines `__call` returning null so `before()` always runs (`panel/app/Policies/ServerPolicy.php:18-57`).
3. `null` result → denied.

Policy semantics (`panel/app/Policies/*.php`):

* `DefaultAdminPolicies` (`DefaultAdminPolicies.php:10-43`): `viewAny→can('viewList X')`, `view→can('view X', m)`, `create→can('create X')`, `update→can('update X', m)`, `delete/deleteAny→can('delete X')`, `replicate→can('create X')`.
  Used by ApiKey (`apiKey`), BackupHost, DatabaseHost, Egg, Mount, Node, Plugin, Role, Server, User, WebhookConfiguration (`webhook`), Allocation (admin side).
* Node restriction `before()` hooks (return `false` when not targetable): `NodePolicy` (node), `ServerPolicy` (server's node, after subuser checks), `AllocationPolicy` (allocation's node, only outside a server tenant), `DatabaseHostPolicy`/`MountPolicy`/`BackupHostPolicy` (any linked node not targetable → false).
* `UserPolicy`: update/delete also require `canTarget(user)` (non-root cannot edit root admins).
* Server-panel (tenant) policies map to subuser permissions on the current tenant: `ActivityLogPolicy`→`activity.read`; `BackupPolicy`→`backup.read/create/delete`; `DatabasePolicy`→`database.read/create/update/delete`; `FilePolicy`→`file.read` (viewAny), `file.read-content` (view), `file.create/update/delete`; `SchedulePolicy`→`schedule.*`; `SubuserPolicy`→`user.read/create/update/delete`; `AllocationPolicy` (tenant) → `allocation.read/create/update/delete`.

### 7.6 Node restrictions (`node_role`)

`User::accessibleNodes()` (`User.php:335-349`): root admin → all nodes; if none of the user's roles has any `node_role` row → all nodes; else only nodes linked to at least one of the user's roles. `canTarget(Node)` uses this (`User.php:510-513`). `accessibleServers()` (`User.php:306-318`): with `viewList server` → owned ∪ subuser ∪ servers on accessible nodes; else owned ∪ subuser.

### 7.7 Subuser permissions

* Storage: `subusers.permissions` JSON array of strings (cast `array`, `panel/app/Models/Subuser.php:87-94`). A user cannot be a subuser of their own server (rows deleted by `2024_12_02…`, `2026_10_02…`).
* Full built-in list (`panel/app/Enums/SubuserPermission.php:9-63`):
  `websocket.connect` (hidden),
  `control.console`, `control.start`, `control.stop`, `control.restart`,
  `file.read`, `file.read-content`, `file.create`, `file.update`, `file.delete`, `file.archive`, `file.sftp`,
  `backup.read`, `backup.create`, `backup.delete`, `backup.download`, `backup.restore`,
  `schedule.read`, `schedule.create`, `schedule.update`, `schedule.delete`,
  `user.read`, `user.create`, `user.update`, `user.delete`,
  `database.read`, `database.create`, `database.update`, `database.delete`, `database.view-password`,
  `allocation.read`, `allocation.create`, `allocation.update`, `allocation.delete`,
  `activity.read`, `mount.read`, `mount.update`,
  `startup.read`, `startup.update`, `startup.docker-image`,
  `settings.rename`, `settings.description`, `settings.reinstall`, `settings.change-icon`.
  Plus plugin-registered `group.perm` strings (`Subuser::registerCustomPermissions`, `Subuser.php:52-72`). `GET /api/client/permissions` returns `Subuser::allPermissionKeys()`.
* Legacy note: Pterodactyl/old Pelican stored `database.view_password` (underscore); renamed to `database.view-password` in commit `85d625d11` (2025-05-05) **without data migration** → old rows carry a dead permission. Exact string comparison must be kept (or a deliberate normalisation decided — see open questions).
* `Subuser::doesPermissionExist(p)` = contains `.` and is in `allPermissionKeys()` (`Subuser.php:158-165`).
* `User::can($ability, Server)` override (`User.php:447-456`): if ability is a `SubuserPermission` or an existing subuser permission string → `checkPermission` (memoised per request in `Context`), implemented by `hasPermission` (`User.php:409-426`):
  1. `canned('update', $server)` **or** owner → true. `canned('update', $server)` resolves as: root admin → true; else spatie finds no permission
     literally named `update` → null; `ServerPolicy::before` → `false` if the server's node is not targetable (§7.6), else null;
     `DefaultAdminPolicies::update` → `can('update server', $server)` → spatie → true iff the user holds `update server`.
     I.e. **root admin, or (`update server` AND node targetable)**;
  2. `websocket.connect` and `canned('view', $server)` (root admin, or `view server` AND node targetable) → true;
  3. subuser row with that exact string in `permissions` → true; else false.
* `ServerPolicy::before` for subuser abilities: owner → true; subuser with permission → true; then node check.
* `AuthenticateServerAccess` (client API server routes, `panel/app/Http/Middleware/Api/Client/Server/AuthenticateServerAccess.php:32-61`): allowed if owner, `can('update server', $server)` (resolved by the spatie global before-callback, so **node restrictions are not applied here**, unlike the Filament tenant check `canAccessTenant` which uses `canned('view', …)`), or any subuser row; else 404. Server state conflicts (installing/transferring/suspended…) → 409 except on `api:client:server.view`, and admins (`update server`) may still use `api:client:server.ws` and `api:client:server.files.*`.
* Effective permission list sent to Wings (`panel/app/Services/Servers/GetUserPermissionsService.php:19-51`):
  * `isAdmin = user.isAdmin() && (can('view',server) || can('update',server))`;
  * owner & not admin → `['*']`;
  * admin & (owner or `can('update', server)`) → `['*','admin.websocket.errors','admin.websocket.install','admin.websocket.transfer']`;
  * admin (view only) → unique(`['websocket.connect', admin.* ...]` ∪ subuser perms);
  * else subuser perms (or `[]`).

---

## 8. Account-level security events (activity log)

Tables: `activity_logs(id, event, ip, description, actor_type, actor_id, api_key_id, properties json, timestamp)` (batch column dropped
`2025_09_03_080547…`), `activity_log_subjects(id, activity_log_id FK cascade, subject_type, subject_id)`. Morph types use the morph map
(`user`, `api_key`, `server`, …). Default `ip = Request::ip()`, `api_key_id` from `TrackAPIKey`, actor = explicit actor or current guard user
(`panel/app/Services/Activity/ActivityLogService.php:124-225`). `withRequestMetadata()` adds properties `ip` (`getClientIp`) and `useragent`.

| Event | When | Subject / properties | Source |
|---|---|---|---|
| `auth:success` | any `Illuminate\Auth\Events\Login` (password, OAuth, passkey, remember-me cookie re-login) | subject user; props `ip`, `useragent` | `panel/app/Listeners/Auth/AuthenticationListener.php:19-36` (auto-discovered listener) |
| `auth:fail` | `Illuminate\Auth\Events\Failed` (Filament login failures) | subject user if found; props = credentials minus `password`,`token`,`secret` (i.e. `email` or `username`) + `ip`,`useragent` | same; old rows with `password` were scrubbed by `2025_07_19_141511_clear_password_from_failed_auth_logs.php` |
| `auth:password-reset` | `PasswordReset` event | subject user; ip/useragent | `PasswordResetListener.php` |
| `auth:ip-blocked` | API key used from non-allowed IP | actor user; subjects user + api key; prop `identifier` | `AuthenticateIPAccess.php:43-47` |
| `auth:sftp.fail` | SFTP password mismatch | subject user; prop `method=password` | `SftpAuthenticationController.php:48` |
| `server:sftp.denied` | SFTP without permission | actor user; subject server | `SftpAuthenticationController.php:139-146` |
| `user:api-key.create` / `.delete` | profile UI, client API, admin user page | subject key (create); prop `identifier` | `ApiKeyController.php:47-73`, `EditProfile.php:284-322`, `UserResource.php:412` |
| `user:ssh-key.create` / `.delete` | | prop `fingerprint` | `SSHKeyController.php:43,65`, `EditProfile.php:371,413` |
| `user:account.username-changed` / `email-changed` (props `old`,`new`) / `password-changed` | client API only (profile page saves are **not** logged) | | `AccountController.php:60,89,107` |
| `auth:token`, `auth:recovery-token` | defined but never emitted (§4.7) | | `TwoFactorListener.php` |

Notifications/mails: no "new login / new device" notifications exist. Security-related mails: `AccountCreated` (with reset link when no password set), Filament `ResetPassword`, Filament email-MFA code (`VerifyEmailAuthentication`). Password change via API/admin dispatches `PasswordChanged` → `RevokeSftpAccessJob` for each node hosting a server the user owns or is subuser of (`RevocationListener.php`); user deletion (`Deleting`) does the same.

---

## 9. Implementation checklist for pelican-rs

1. Encrypter: AES-256-CBC + HMAC as §1.2/§1.3, current + previous keys, tolerant reader §1.10, exact writer §1.6.
2. Cookie crypto (§1.7) is **not** required: decision 0009 gives the Rust panel its own session format and cookie name, so PHP session,
   XSRF and remember-me cookies are simply not recognised (users log in again). Keep the documented algorithms only as reference.
   `users.remember_token` may still be rotated by Rust's own remember-me implementation (column semantics: opaque 60-char token).
3. bcrypt verify/hash with 72-byte truncation, rehash on login when cost ≠ `BCRYPT_ROUNDS`; argon2 when `HASH_DRIVER` says so.
4. API key auth exactly as §3.3–3.6 including `last_used_at` writes, `expires_at`, IP ranges, client/application separation and ACL bit flags.
5. Daemon auth §3.9 (token length not fixed).
6. TOTP ±8 window, anti-replay store, bcrypt recovery codes consumed atomically; email MFA codes (6 digits, 4 min).
7. Passkeys with identical RP id, origin logic and user-handle HMAC; credential JSON round-trip compatible with webauthn-lib.
8. spatie semantics incl. global-before ordering, `model_type='user'`, guard `web`, `node_role` restrictions, subuser string matching.
9. Activity events in §8 with the same names/properties.

---

## Open questions / uncertain framework behaviour

1. **Double-encrypted `mfa_app_secret`** – only DBs that ran migration `2025_07_22_091435_update_users_totp` from canary commit `1900c04b7` (2025-09-08 … 2025-09-15, not in a tag). PHP cannot use those secrets either. Decide whether Rust silently recovers (§1.10 step 4) or forces re-enrolment.
2. **Silently dropped TOTP** – DBs upgraded through tags `v1.0.0-beta26`…`beta38` lost all legacy TOTP (`mfa_app_secret = NULL`). Nothing to recover; just be aware users may believe 2FA is on.
3. **Serialized leftovers** – rows the 2024-05-31 fix-up could not convert stay `s:N:"…";`-wrapped. PHP would return the wrapped string (broken). Recommended: unwrap + log (§1.10 step 3). Confirm.
4. **Sessions at cutover** – resolved by decision 0009 (own session payload/cookie, re-login required). Open sub-question: should Rust's
   remember-me reuse `users.remember_token` (PHP-compatible semantics, rotating it would invalidate PHP remember cookies on rollback) or keep
   its own token elsewhere (no new tables allowed per 0003)?
5. **`database.view_password` legacy subuser permission** – keep exact PHP behaviour (dead permission) or normalise to `database.view-password`?
6. **MFA bypass via OAuth and passkeys**, and `RequireTwoFactorAuthentication` only checking enrolment (not session MFA) and not covering `/api/*` – replicate as-is or harden? (Changing it changes observable behaviour.)
7. **Gate ordering quirk** – spatie's global before-callback grants `"<verb> <model>"` checks before Pelican's node-restriction policy `before()` runs; e.g. `can('update server', $server)` ignores `node_role`. Replicate (current behaviour) or fix?
8. **Application API with session auth** – resources `database_host`/`server_database` produce permission names (`viewList database_host`) that no role can hold → only root admins pass. Replicate?
9. `ApiKey::findToken` uses non-constant-time `===` and throws (500) on undecryptable tokens; Rust should use constant-time compare and treat decrypt failure as 401 — confirm that diverging here is acceptable.
10. `password_get_info()` on `$2a$`/`$2b$` hashes (would throw under `HASH_VERIFY`) – none expected in practice; Rust can accept them.
11. bcrypt NUL-byte handling in PHP vs Rust crates – untested edge case.
12. Exact `otpauth://` URI parameter set/order emitted by `pragmarx/google2fa-qrcode` (cosmetic; any RFC-compliant URI works with authenticators).
13. Password-reset links (`/password-reset/reset?...&signature=...`, verified Filament defaults) in already-sent emails: decide if Rust must keep links in already-sent emails working (links expire after 60 min anyway; the signed URL uses the raw `APP_KEY` string).
14. Sanctum `updateLastUsedAt` → also bumps `api_keys.updated_at` (Eloquent `save()` with timestamps) – believed true, not runtime-verified.
15. Legacy API key types 3/4 (Pterodactyl daemon keys) and key_type 0 rows authenticate on the client API (only type 2 is blocked). Probably none exist; consider rejecting `key_type ∉ {1,2}`.
16. `permissions` JSON for SQLite/Postgres installs written by the 2024-11-04 migration is a JSON object (grammar `json_encode`s arrays) – verified for MySQL/MariaDB/SQLite/Postgres grammars, but MySQL `JSON` columns re-serialise with spaces; parse, never string-compare.
17. Remember-me / cookie-prefix HMACs use the **decoded** key (cookie prefix) vs the **raw `base64:` string** (password-hash HMAC, signed URLs, passkey user handle). Verified in vendor code; mixing them up breaks interop silently.
