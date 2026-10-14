# Eggs: import / export formats (original Pelican panel)

Status: reference spec, derived from the PHP source in `panel/` (PHP 8.5, Laravel 13, Filament 5).
All paths are relative to `panel/` unless stated otherwise. Framework/library behaviour (Symfony YAML
7.4.20, Laravel JSON casts, HTTP client) was checked against `.ref/vendor` (the exact versions the panel uses).
Line numbers refer to the files as they are at the time of writing.

---

## 1. Source map

| Concern | File |
|---|---|
| Export (JSON / YAML) | `app/Services/Eggs/Sharing/EggExporterService.php` |
| Import (content / upload / URL), version upgrade | `app/Services/Eggs/Sharing/EggImporterService.php` |
| Format enum (`yaml`, `json`) | `app/Enums/EggFormat.php:5-9` |
| Egg model (current export version, inheritance accessors, casts, rules) | `app/Models/Egg.php` |
| Egg variable model (reserved env names, rules, casts) | `app/Models/EggVariable.php` |
| Icon storage helper | `app/Models/Traits/HasIcon.php` |
| Application API endpoints | `app/Http/Controllers/Api/Application/Eggs/EggController.php:78-105`, `routes/api-application.php:101-107` |
| Export request (format param) | `app/Http/Requests/Api/Application/Eggs/ExportEggRequest.php:7-13` |
| Admin UI actions | `app/Filament/Components/Actions/{ExportEggAction,ImportEggAction,UpdateEggAction,UpdateEggBulkAction}.php` |
| Background import job | `app/Jobs/InstallEgg.php` |
| Update check / egg index / normalize commands | `app/Console/Commands/Egg/{CheckEggUpdatesCommand,UpdateEggIndexCommand,NormalizeEggCommand}.php` |
| Scheduling of the above | `app/Console/Kernel.php:47-48`, `docker/crontab:2` |
| Remote egg index URL | `config/panel.php:19-22` (`panel.cdn.egg_index_url`) |
| Installer egg selection | `app/Livewire/Installer/Steps/EggSelectionStep.php`, `app/Livewire/Installer/PanelInstaller.php:244-276` |
| Consumer of stored config (Wings config) | `app/Services/Eggs/EggConfigurationService.php:26-80` |
| Tests / fixtures | `tests/Unit/Console/Commands/Egg/*`, `tests/_fixtures/*.yaml`, `tests/Unit/Services/Eggs/EggExporterServiceTest.php`, `tests/Seeders/EggSeeder.php`, `tests/Integration/Api/Fixtures/Application/StoreEndpointsFixtureTest.php:117-155` (+ snapshot `__snapshots__/StoreEndpointsFixtureTest__test_import_egg__1.json`) |

---

## 2. Format versions

The version marker is `meta.version`. The only *produced* version is `Egg::EXPORT_VERSION = 'PLCN_v3'`
(`app/Models/Egg.php:109`). Accepted versions (`EggImporterService::parse`, `EggImporterService.php:157-164`):

| `meta.version` | Origin | Import treatment |
|---|---|---|
| `PTDL_v1` | Pterodactyl, single `image`/`images` + `startup` string | `convertLegacy()` then `convertToV3()` |
| `PTDL_v2` | Pterodactyl, `docker_images` map + `startup` string | `convertToV3()` |
| `PLCN_v1` | Pelican (pre multi-startup) | `convertToV3()` |
| `PLCN_v2` | Pelican (pre multi-startup) | `convertToV3()` |
| `PLCN_v3` | current: `startup_commands` map | used as-is |
| anything else / missing | — | `InvalidFileUploadException('The file format is not recognized.')` |

There is no code that distinguishes PTDL_v2 / PLCN_v1 / PLCN_v2 from each other; all three are treated
identically (only `startup` → `startup_commands` conversion). Differences such as variable `rules` being a
`|`-separated string vs a list, `tags`, `image`, `field_type` are handled *independently of the version*
(see §5), so a Rust implementation must apply those normalisations to every version.

Both JSON and YAML are supported for every version. The serialization format is chosen by:
- export: explicit `EggFormat` (`json`|`yaml`);
- import from upload: file extension / MIME (§5.1);
- import from URL: URL path extension (§5.1);
- import via API body: **always parsed with the YAML parser** (§5.1).

---

## 3. Database columns involved

`eggs` (relevant columns; casts from `Egg.php:172-185`):

| Column | Type / cast | Notes |
|---|---|---|
| `uuid` | string(36) | match key for import |
| `name`, `author`, `description` | string / string / text nullable | |
| `features` | json nullable, `array` cast | |
| `docker_images` | json nullable, `array` cast | map `label => image` |
| `file_denylist` | json nullable, `array` cast | |
| `update_url` | text nullable | |
| `config_files`, `config_startup`, `config_logs` | text nullable | **stored as JSON text** (not cast) |
| `config_stop` | string(255) nullable | plain string, e.g. `stop` or `^C` |
| `config_from` | int nullable | parent egg for config inheritance |
| `startup_commands` | json, `array` cast | map `name => command` (migration `2025_09_03_090706_support_multiple_startup_commands.php`) |
| `script_install` | text nullable | |
| `script_entry` | string, default `ash`, NOT NULL | |
| `script_container` | string, default `alpine:3.4`, NOT NULL | |
| `script_is_privileged` | bool, DB default `true` | never touched by import/export |
| `copy_script_from` | int nullable | parent egg for script inheritance |
| `force_outgoing_ip` | bool, default false | never touched by import/export |
| `tags` | json, `array` cast, model default `'[]'` | |

The legacy `eggs.image` column (base64 icon, added `2025_10_29_105725_add_egg_image_column.php`) was moved to
files and dropped by `2025_12_13_000000_convert_base64_images_to_files.php`. Icons now live on the `public` disk
at `icons/egg/{uuid}.{png|jpg|webp}` (`HasIcon.php:19-28`, `Egg::RESOURCE_NAME = 'egg'`).

`egg_variables` (`EggVariable.php`): `id, egg_id, sort, name, description, env_variable, default_value,
user_viewable, user_editable, rules, created_at, updated_at`. Casts: `user_viewable`/`user_editable` → bool,
`rules` → array (JSON column since `2024_07_25_072050_convert_rules_to_array.php`), dates immutable.
Unique indexes `(egg_id, env_variable)` and `(egg_id, name)` (`2025_04_01_033956_egg_variable_unique_foreign_key.php`).

`name`, `description`, `env_variable`, `default_value`, `user_viewable`, `user_editable`, `rules` are NOT NULL,
so an imported variable with a null/missing `description` or `default_value` fails at SQL level.

**Physical column order matters for export** (see §4.2, variables): `sort` was added with
`->after('egg_id')` (`2024_04_20_214441_add_egg_var_sort.php`). MySQL/MariaDB honour `after`, so the order is
`id, egg_id, sort, name, …`; SQLite and PostgreSQL ignore `after`, so `sort` is the last column
(`…, rules, created_at, updated_at, sort`). Confirmed by the schema dumps in this repo
(`pelican-rs/crates/pelican-db/schema/{mysql,mariadb}.sql:174-186` vs `pgsql.sql:229-242`, `sqlite.sql:117`).

---

## 4. Export

### 4.1 Entry points

| Entry | Behaviour |
|---|---|
| `GET /api/application/eggs/{egg:id}/export?format=json|yaml` (`EggController::export`, `EggController.php:78-87`) | `format` validated `nullable|string|in:yaml,json` (`ExportEggRequest.php:11`); `EggFormat::tryFrom(input) ?? YAML` → **default YAML** (the doc comment in the request class says JSON; the code wins). Response is a `StreamedResponse` download, filename `egg-{kebab}.{json|yaml}`, header `Content-Type: application/json` or `application/yaml`. |
| Admin "Export" action (`ExportEggAction.php:43-52`) | Two buttons that open the API route above in a new tab with `format=json` / `format=yaml` (relies on the browser session being accepted by the application API, permission `export egg`). |
| `p:egg:check-updates` | Calls the exporter internally to diff against the remote file (§6.3). |
| `p:egg:normalize` | Re-uses `yamlExport()` + the same `Yaml::dump` call, but on a parsed *file*, not on DB data (§7). |

`getKebabName()` (`Egg.php:361-364`): `str($name)->kebab()->lower()->trim()->split('/[^\w\-]/')->join('')`.
Laravel `Str::kebab` = `Str::snake($v, '-')`: if the string is not all-lowercase, `ucwords()`, remove all
whitespace, insert `-` before every uppercase letter that follows any char, lowercase. Then every char that is
not `[A-Za-z0-9_-]` is removed. Examples: `Vanilla Minecraft` → `vanilla-minecraft`;
`Counter-Strike: Global Offensive` → `counter--strike-global-offensive`.

### 4.2 Document structure (exact key order)

Built in `EggExporterService::handle` (`EggExporterService.php:18-57`). The egg is loaded with
`scriptFrom`, `configFrom`, `variables` relations (variables: no ORDER BY → database natural order, in practice
by `id`; **not** sorted by `sort`).

```
_comment            = "DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL"
meta.version        = "PLCN_v3"
meta.update_url     = eggs.update_url (string|null)
exported_at         = Carbon::now()->toAtomString()      e.g. "2026-06-01T01:02:03+00:00" (app tz = UTC)
name                = eggs.name
author              = eggs.author
uuid                = eggs.uuid
description         = eggs.description (string|null)
icon                = "data:{mime};base64,{...}" | null   (§4.5)
tags                = eggs.tags (list)
features            = eggs.features (list|null)           NOTE: own value, NOT inherit_features
docker_images       = eggs.docker_images (map)
file_denylist       = inherit_file_denylist, empty strings removed, re-indexed (list)
startup_commands    = eggs.startup_commands (map name => command)
config.files        = inherit_config_files   (JSON text as stored, string|null)
config.startup      = inherit_config_startup (JSON text, string|null)
config.logs         = inherit_config_logs    (JSON text, string|null)
config.stop         = inherit_config_stop    (string|null)
scripts.installation.script     = copy_script_install   (string|null)
scripts.installation.container  = copy_script_container (string)
scripts.installation.entrypoint = copy_script_entry     (string)
variables           = list of every egg_variables row as toArray() minus id, egg_id, created_at, updated_at
```

Inheritance resolution (exported values are always the *resolved* ones, the parent link itself is not exported):
- `inherit_config_{files,startup,logs,stop}` (`Egg.php:244-287`): own value if `!is_null(own)` **or**
  `config_from` is null; otherwise the parent's own value (single level, no recursion).
- `inherit_file_denylist` (`Egg.php:310-317`): if `config_from` is null → own; otherwise **always the parent's**
  (even when the child has its own list). Different rule from the other accessors.
- `copy_script_{install,entry,container}` (`Egg.php:206-239`): own value if `!empty(own)` **or**
  `copy_script_from` is empty; otherwise parent's. (`empty('')` → inherits.)
- `features` uses the raw column, not `inherit_features` (`EggExporterService.php:36`).

Variable objects: keys are emitted in **physical column order** minus the four removed keys:
- SQLite / PostgreSQL: `name, description, env_variable, default_value, user_viewable, user_editable, rules, sort`
- MySQL / MariaDB: `sort, name, description, env_variable, default_value, user_viewable, user_editable, rules`

Value types: `user_viewable`/`user_editable` bool; `rules` list of strings; `sort` int|null; `default_value`,
`description`, `name`, `env_variable` strings. No `field_type`, no `required`, no `id`.
A Rust port should pick one order; for byte-compatibility with exports produced by an SQLite/Postgres panel
and with the published pelican-eggs files use the SQLite order (it matches `tests/_fixtures/*.yaml`).

### 4.3 JSON encoding

`json_encode($struct, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)` (`EggExporterService.php:60`):
- 4-space indentation, `"key": value`, `\n` newlines, **no trailing newline**.
- `/` not escaped; non-ASCII **is** escaped as `\uXXXX` (no `JSON_UNESCAPED_UNICODE`); invalid UTF-8 → exception (HTTP 500).
- Empty PHP arrays encode as `[]` regardless of intended type (an empty `docker_images`/`startup_commands` map
  becomes `[]`); null → `null`.
- PHP arrays with keys `0..n-1` in order encode as JSON arrays, otherwise objects (e.g. a `docker_images` map
  whose keys happen to be `"0"`, `"1"` becomes a JSON list).
- `config.*` values are the stored JSON **text** emitted as JSON strings (escaped `\n`, `\"`, and `\\/` where the
  stored text contains `\/`), not as nested objects.
- Scripts are emitted verbatim (CRLF kept as `\r\n`).

Expected JSON export of the egg obtained by importing `tests/Unit/Console/Commands/Egg/egg-ptdlv2.json`
on SQLite (derived, with `exported_at` frozen):

```json
{
    "_comment": "DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL",
    "meta": {
        "version": "PLCN_v3",
        "update_url": "https://raw.githubusercontent.com/pelican/panel/refs/heads/main/tests/Unit/Console/Command/Egg/egg-ptdlv2.json"
    },
    "exported_at": "2026-06-01T01:02:03+00:00",
    "name": "Egg Example",
    "author": "example@example.com",
    "uuid": "21f1c702-953a-45ca-ae58-c205e8a4ea7e",
    "description": "Egg for test purposes",
    "icon": null,
    "tags": [
        "test"
    ],
    "features": [
        "eula",
        "java_version",
        "pid_limit"
    ],
    "docker_images": {
        "ghcr.io/pelican-eggs/yolks:java_8": "ghcr.io/pelican-eggs/yolks:java_8",
        "ghcr.io/pelican-eggs/yolks:java_11": "ghcr.io/pelican-eggs/yolks:java_11",
        "ghcr.io/pelican-eggs/yolks:java_16": "ghcr.io/pelican-eggs/yolks:java_16",
        "ghcr.io/pelican-eggs/yolks:java_17": "ghcr.io/pelican-eggs/yolks:java_17"
    },
    "file_denylist": [],
    "startup_commands": {
        "Default": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar minecraft.jar"
    },
    "config": {
        "files": "{\n    \"server.properties\": {\n        \"parser\": \"properties\",\n        \"find\": {\n            \"server-ip\": \"0.0.0.0\",\n            \"server-port\": \"{{server.allocations.default.port}}\"\n        }\n    }\n}",
        "startup": "{\n    \"done\": \")! For help, type \"\n}",
        "logs": "{}",
        "stop": "stop"
    },
    "scripts": {
        "installation": {
            "script": "#!/bin/ash\r\necho test some escaped backslashes (/), then proper backslashes (/)",
            "container": "ghcr.io/pelican-eggs/installers:alpine",
            "entrypoint": "ash"
        }
    },
    "variables": [
        {
            "name": "Modpack Version",
            "description": "",
            "env_variable": "MODPACK_VERSION",
            "default_value": "1.0.0",
            "user_viewable": true,
            "user_editable": true,
            "rules": [
                "required",
                "string",
                "max:20"
            ],
            "sort": null
        }
    ]
}
```

### 4.4 YAML encoding

`Yaml::dump($this->yamlExport($struct), 10, 2, Yaml::DUMP_MULTI_LINE_LITERAL_BLOCK | Yaml::DUMP_OBJECT_AS_MAP)`
(`EggExporterService.php:61`): inline level 10 (block style up to depth 10, then flow), indentation 2.

#### 4.4.1 Pre-transform `yamlExport()` (`EggExporterService.php:81-117`), applied recursively

1. `Collection` → plain array.
2. **Any string that is valid JSON is decoded** (`json_decode($s, true)`, `JSON_ERROR_NONE`) and the result is
   transformed recursively. This is how `config.files/startup/logs` become nested YAML maps. Side effects
   (must be reproduced for compatibility):
   - `"{}"` and `"[]"` → empty array → dumped as `{  }`.
   - any scalar-looking string anywhere in the document is converted: `"25565"` → int `25565`,
     `"1.5"` → float, `"1e5"` → float `100000.0`, `"true"`/`"false"` → bool, `"null"` → null,
     `"\"x\""` → `x`, `" 7"` → `7`. This applies to `default_value`, `name`, `description` (short),
     `config.stop`, script strings, docker image names, etc.
   - `""` is not valid JSON → stays `""`.
3. Other strings: `"\r\n"` → `"\n"` (lone `\r` kept).
4. In arrays, a key named `description` (case-insensitive, at any depth, including variable descriptions)
   whose value is a string containing `"\n"` **or** longer than 80 bytes is replaced by
   `wordwrap($value, 100, "\n")` (PHP wordwrap: break at the last space before column 100, never split a word
   longer than 100) and is **not** passed through steps 2–3 (so CRLF inside such descriptions is kept, and the
   string is then not eligible for a literal block — see below).
5. Everything else unchanged.

#### 4.4.2 Symfony YAML 7.4 dumper rules relevant to eggs

(`vendor/symfony/yaml/Dumper.php:52-145`, `Inline.php::dump`, `Escaper.php`)

- Block mapping: `key: value`; nested maps/lists on the next line indented by 2 more spaces.
- Lists of maps: the item is `-` alone on its line, the map follows indented 2 more:
  ```yaml
  variables:
    -
      name: 'Modpack Version'
  ```
  Lists of scalars: `  - value`.
- Empty array (list or map) → `{  }` (two spaces; `DUMP_EMPTY_ARRAY_AS_SEQUENCE` not set). So an egg with no
  variables exports `variables: {  }`, empty denylist `file_denylist: {  }`.
- `null` → `null`; bools → `true`/`false`; ints bare; floats via `(string)` with `.0` appended for whole numbers.
- Strings, in this order of rules (`Inline.php::dump`):
  1. numeric strings (`is_numeric`) → single-quoted (`'25565'`); (after step 2 above these rarely remain strings);
  2. `''` for empty;
  3. invalid UTF-8 or containing bytes outside `\x00\x07-\x0d\x1B\x20-\xff` → `!!binary base64`;
  4. contains a control char `[\x00-\x1f]`, `\x7f`, U+0085, U+00A0, U+2028, U+2029 → double-quoted with
     escapes (`\n`, `\r`, `\t`, `\"`, `\\`, `\e`, `\N`, `\_`, `\L`, `\P`, `\xNN`) — only reached for strings
     *not* emitted as literal block (see below);
  5. case-insensitive `null`, `~`, `true`, `false`, `y`, `n`, `yes`, `no`, `on`, `off`, **or** contains any of
     whitespace, `'`, `"`, `:`, `{`, `}`, `[`, `]`, `,`, `&`, `*`, `#`, `?`, **or** starts with
     `- ? | < > = ! % @` `` ` `` or a Unicode space → single-quoted (`'` doubled); if the string contains `'`
     and the double-quoted form is shorter, double-quoted is used;
  6. matches `^[0-9]+[_0-9]*$`, `^[+-]?0o[0-7_]+$`, `^0x[0-9a-fA-F_]+$` or the YAML timestamp regex
     → single-quoted (this is why `exported_at` is quoted);
  7. otherwise bare.
- **Literal blocks** (`DUMP_MULTI_LINE_LITERAL_BLOCK`): a map/list value that is a string containing `"\n"` and no
  `"\r"` is emitted as `key: |` + indentation indicator + chomping indicator, then each line prefixed by the
  current indent + 2 spaces; empty lines emitted as truly empty lines. Chomping: `+` if the string ends with
  `"\n\n"`, nothing (clip) if it ends with a single `"\n"`, `-` otherwise. Indentation indicator `2` is added
  when the first non-blank line starts with a space (`Dumper.php:169-183`).
- Map keys are dumped with the same scalar rules (`'Java 21':`, `'ghcr.io/pelican-eggs/yolks:java_8':`,
  `server.properties:` bare).
- Output ends with `"\n"` when the last emitted value is an inline scalar (always the case for a normal egg,
  whose last key is a variable's `sort`). If the last value is a literal block there is no trailing newline.

Expected YAML export of the same egg as §4.3 (SQLite order):

```yaml
_comment: 'DO NOT EDIT: FILE GENERATED AUTOMATICALLY BY PANEL'
meta:
  version: PLCN_v3
  update_url: 'https://raw.githubusercontent.com/pelican/panel/refs/heads/main/tests/Unit/Console/Command/Egg/egg-ptdlv2.json'
exported_at: '2026-06-01T01:02:03+00:00'
name: 'Egg Example'
author: example@example.com
uuid: 21f1c702-953a-45ca-ae58-c205e8a4ea7e
description: 'Egg for test purposes'
icon: null
tags:
  - test
features:
  - eula
  - java_version
  - pid_limit
docker_images:
  'ghcr.io/pelican-eggs/yolks:java_8': 'ghcr.io/pelican-eggs/yolks:java_8'
  'ghcr.io/pelican-eggs/yolks:java_11': 'ghcr.io/pelican-eggs/yolks:java_11'
  'ghcr.io/pelican-eggs/yolks:java_16': 'ghcr.io/pelican-eggs/yolks:java_16'
  'ghcr.io/pelican-eggs/yolks:java_17': 'ghcr.io/pelican-eggs/yolks:java_17'
file_denylist: {  }
startup_commands:
  Default: 'java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar minecraft.jar'
config:
  files:
    server.properties:
      parser: properties
      find:
        server-ip: 0.0.0.0
        server-port: '{{server.allocations.default.port}}'
  startup:
    done: ')! For help, type '
  logs: {  }
  stop: stop
scripts:
  installation:
    script: |-
      #!/bin/ash
      echo test some escaped backslashes (/), then proper backslashes (/)
    container: 'ghcr.io/pelican-eggs/installers:alpine'
    entrypoint: ash
variables:
  -
    name: 'Modpack Version'
    description: ''
    env_variable: MODPACK_VERSION
    default_value: 1.0.0
    user_viewable: true
    user_editable: true
    rules:
      - required
      - string
      - 'max:20'
    sort: null
```

The committed fixture `tests/Unit/Console/Commands/Egg/egg-plcnv3-example.yaml` is the byte-exact output of
`p:egg:normalize` (not of a DB export): it has no `icon` key and `startup_commands` is the **last** key,
because normalize dumps the parsed file array (§7). It is a good byte-level test of the YAML dumper.
`tests/Unit/Services/Eggs/EggExporterServiceTest.php` pins: literal `\n` / `\r\n` escape sequences inside
scripts survive, real CRLF is normalised to LF.

### 4.5 Icon

`getEggIconAsBase64()` (`EggExporterService.php:68-79`): checks `public` disk paths
`icons/egg/{uuid}.png`, `.jpg`, `.webp` **in that order**; first hit →
`"data:" + mime + ";base64," + base64(file)` with mime `image/png` / `image/jpeg` / `image/webp`; none → `null`.
SVG icons are never exported. In YAML the data URI is single-quoted (contains `:`).

---

## 5. Import

### 5.1 Entry points and format detection

| Entry | Code | Format detection | Target egg |
|---|---|---|---|
| `POST /api/application/eggs/import` (raw body) | `EggController::import`, `EggController.php:98-105` | none: `fromContent($body)` with default `EggFormat::YAML` → **JSON bodies are parsed by the YAML parser** (Symfony 7.4 supports multi-line flow mappings and `\/`, `\uXXXX` escapes, so ordinary JSON works). | by uuid / new |
| Admin upload (`ImportEggAction`, file tab) | `EggImporterService::fromFile`, `EggImporterService.php:53-71` | YAML if client extension is `yaml`/`yml` or MIME contains `yaml`, otherwise JSON. Upload error → `'The selected file was not uploaded successfully'`; any parse/import error is re-wrapped as `InvalidFileUploadException('File parse failed: ' . msg)`. | by uuid / new |
| Admin URL import (`ImportEggAction`, url tab) | `ImportEggAction.php:87-101` → `fromUrl` | URLs containing `github.com` get the first `blob` replaced by `raw`. `fromUrl` (`EggImporterService.php:78-93`): extension of `parse_url(path)`: `yaml`/`yml` → YAML, `json` → JSON, else `'Unsupported file format.'`. HTTP GET, timeout 5 s, connect timeout 1 s, non-2xx throws. | by uuid / new |
| Admin "GitHub" tab / installer egg selection | `InstallEgg::dispatch($downloadUrl)` (`ImportEggAction.php:52-57`, `PanelInstaller.php:254-258`) | queued job, `fromUrl($url)`; job `timeout = 15`; exceptions are logged (`'Failed to install egg from URL: …'`) and swallowed (`InstallEgg.php:25-32`). | by uuid / new |
| Update single egg / bulk | `UpdateEggAction.php:39-61`, `UpdateEggBulkAction.php:41-87` | `fromUrl($egg->update_url, $egg)` | **the given egg** |
| Test seeder | `tests/Seeders/EggSeeder.php` | `fromFile` for every file in `tests/_fixtures` | by uuid / new |

Notes:
- The admin import modal on the *edit egg* page (`EditEgg.php:54-58`, `->multiple(false)`) still calls
  `fromFile($file)`/`fromUrl($url)` **without** passing the current egg, so it updates whichever egg has the
  file's uuid (or creates a new egg) — not necessarily the one being edited.
- The URL tab validates `->url()->endsWith(['.json', '.yaml', '.yml'])` (`ImportEggAction.php:165-172`).
- Filament import results notification: names derived from filename after `egg-`, headline-cased.
- Permissions: `import egg` (import/update actions), `export egg` (export). API: application API key with
  `egg` resource write permission (`ImportEggRequest.php:11-13`).

### 5.2 `parse()` pipeline (`EggImporterService.php:146-232`) — exact order

1. Decode: YAML → `Yaml::parse($content)` (no flags; maps → PHP arrays); JSON →
   `json_decode($content, true, 512, JSON_THROW_ON_ERROR)`. Any throwable →
   `InvalidFileUploadException('File parse failed: ' . msg)`.
2. `version = parsed.meta.version ?? ''`, dispatch per §2 (unknown → `'The file format is not recognized.'`).
   - `convertLegacy` (PTDL_v1 only, lines 288-304): `images = parsed.images` if set, else
     `[parsed.image ?? 'nil']`; unset `images`, `image`, top-level `field_type`;
     `docker_images = { img: img for img in images }` (label = image). Note the literal fallback image `'nil'`.
   - `convertToV3` (PTDL_v1/v2, PLCN_v1/v2, lines 310-321): `startup = parsed.startup` (**required** —
     a missing key raises a PHP warning which Laravel turns into an exception), unset `startup`,
     `startup_commands = {"Default": startup}` (appended as the last key; overwrites any existing value).
3. `config` normalisation (lines 166-181), if `config` is an array: for each key, if the value is an
   array/object → `json_encode((array)$v, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR)` (so YAML maps become
   compact JSON text; an empty YAML map `{  }` becomes `[]`). Then, for key `files` only (if string), apply
   the legacy placeholder upgrades as **plain sequential substring replacement**
   (`str_replace` with arrays, `UPGRADE_VARIABLES`, lines 23-32), in this order:

   | from | to |
   |---|---|
   | `server.build.env.SERVER_IP` | `server.allocations.default.ip` |
   | `server.build.default.ip` | `server.allocations.default.ip` |
   | `server.build.env.SERVER_PORT` | `server.allocations.default.port` |
   | `server.build.default.port` | `server.allocations.default.port` |
   | `server.build.env.SERVER_MEMORY` | `server.build.memory_limit` |
   | `server.build.memory` | `server.build.memory_limit` |
   | `server.build.env.` | `server.environment.` |
   | `server.build.environment.` | `server.environment.` |

   (`str_replace` with arrays replaces each pair over the whole string before moving to the next pair. Note
   `server.build.memory` also matches inside `server.build.memory_limit` — both one produced by the previous
   pair from `server.build.env.SERVER_MEMORY` and one already present in the file — producing
   `server.build.memory_limit_limit`. This is the original behaviour.)
4. `scripts.installation` (lines 183-190): array/object values JSON-encoded the same way (normally all strings).
5. Variables bool fix (lines 194-202): for each variable whose `default_value` is a PHP bool →
   `'true'` / `'false'` string. (Ints/floats are left as numbers.)
6. Reserved env names (lines 205-224), `EggVariable::RESERVED_ENV_NAMES` (`EggVariable.php:65`):
   `P_SERVER_UUID, P_SERVER_ALLOCATION_LIMIT, SERVER_MEMORY, SERVER_IP, SERVER_PORT, ENV, HOME, USER, STARTUP,
   MODIFIED_STARTUP, SERVER_UUID, UUID, INTERNAL_IP, HOSTNAME, TERM, LANG, PWD, TZ, TIMEZONE`.
   - Every variable is mapped to a copy with `env_variable = strtoupper(env_variable)`, then partitioned into
     forbidden (in the list) / allowed.
   - **Only if at least one is forbidden**: `variables = allowed ++ forbidden-renamed` where renamed =
     `"SERVER_" + NAME` (forbidden ones move to the end; *all* variables keep the uppercased name). For each
     `startup_commands` entry, regex `/\b(NAME1|NAME2…)\b/` (names `preg_quote`d, case-sensitive) → `SERVER_$1`.
     Config files and scripts are **not** rewritten.
   - If none is forbidden, the uppercasing is discarded (original case kept).
   - `$parsed['variables']` must exist (collect over a missing key is fine here but step 5.3 needs it).
7. Legacy icon (lines 226-229): if top-level `image` is non-empty and starts with `data:` → move it to `icon`
   (appended as last key) and unset `image`. A non-data `image` value is left in place and ignored.

The returned array is what `p:egg:normalize` serialises (§7).

### 5.3 Persisting: `fromParsed()` (`EggImporterService.php:102-137`), single DB transaction

1. `uuid = parsed.uuid ?? Uuid::uuid4()`.
2. Target egg: the egg passed by the caller (update-from-URL), else `Egg::where('uuid', uuid)->first()`, else a
   new `Egg`. **When a target egg is passed, its uuid is overwritten with the file's uuid.**
3. `forceFill`: `uuid`, `author = parsed.author` (null if missing), `copy_script_from = null`,
   `config_from = null` (import always breaks inheritance).
4. For `i in 0..count(parsed.variables)`: unset `variables[i].field_type` (requires a 0-based list and an
   existing `variables` key — a missing key raises an exception).
5. `fillFromParsed` (lines 237-261):
   - if `parsed.icon` is a `data:` URI → `saveEggIconFromBase64` (lines 266-282): must match
     `^data:image/([\w+]+);base64,(.+)$`; extension lowercased; `writeIcon()` (`HasIcon.php:42-69`) normalises
     `jpeg|jpg → jpg`, `png`, `webp`, anything else (e.g. `svg+xml`) throws and is **reported and ignored**;
     writes `icons/egg/{uuid}.{ext}` on the public disk and deletes other `{uuid}.{png,jpg,jpeg,webp,svg}`.
     The file write happens before the DB commit and is not rolled back on failure. A missing/null icon
     leaves any existing icon file untouched.
   - `forceFill` (missing keys become `null` unless stated):

     | Column | Value |
     |---|---|
     | `name` | `parsed.name` |
     | `description` | `parsed.description` |
     | `tags` | `parsed.tags ?? []` |
     | `features` | `parsed.features` |
     | `docker_images` | `parsed.docker_images` |
     | `file_denylist` | `Collection(parsed.file_denylist)->filter(non-empty)` — **keys preserved**, so a list with removed empty entries is stored as a JSON object (`{"1":"x"}`); `null` → `[]` |
     | `update_url` | `parsed.meta.update_url` (overwrites the existing value, null if absent) |
     | `config_files` | `json_encode(json_decode(parsed.config.files), JSON_PRETTY_PRINT)` |
     | `config_startup` | same with `config.startup` |
     | `config_logs` | same with `config.logs` |
     | `config_stop` | `parsed.config.stop` raw |
     | `startup_commands` | `parsed.startup_commands` |
     | `script_install` | `parsed.scripts.installation.script` |
     | `script_entry` | `parsed.scripts.installation.entrypoint` |
     | `script_container` | `parsed.scripts.installation.container` |

     The `config_*` re-encode: `json_decode` **without** assoc (objects stay objects, so `{}` stays `{}`;
     `[]` stays `[]`), then PHP pretty print: 4-space indent, `\n`, **slashes escaped as `\/`**, non-ASCII as
     `\uXXXX`. Input CRLF/indentation is lost. A null/missing value → `json_decode(null)` → `null` →
     stored as the 4-char string `"null"`. Invalid JSON text → stored as `"null"` too.
   - Not touched: `script_is_privileged`, `force_outgoing_ip` (new eggs get DB defaults `true` / `false`).
6. `$egg->save()`.
7. Variables (lines 122-130), for each `parsed.variables[]` in order, unguarded:
   - `rules = is_array(rules) ? rules : explode('|', rules)` — **naive split**, so `regex:/^(a|b)$/` in a string
     rule list is broken into pieces. `rules` key is required (missing → exception).
   - `egg->variables()->updateOrCreate(['env_variable' => v.env_variable], v minus egg_id, env_variable)`:
     match on `(egg_id, env_variable)` (exact match; case-insensitive only under MySQL's default collation);
     all remaining keys of the variable object are written as columns (`name`, `description`,
     `default_value`, `user_viewable`, `user_editable`, `rules`, `sort`, and **any unknown key**, which then
     fails at SQL level; an `id` key would be written as the primary key).
8. Delete stale variables: `egg->variables()->whereNotIn('env_variable', imported names)->delete()`; FK cascade
   deletes the matching `server_variables` rows (servers lose their values).
   Because upserts happen before deletes, renaming/swapping variable `name`s between env vars can violate the
   `(egg_id, name)` unique index mid-import → whole import rolls back.
9. Return `$egg->refresh()`.

API response (`EggController.php:100-104`): HTTP 201 with the `EggData` transformer. Snapshot
`tests/Integration/Api/Fixtures/Application/__snapshots__/StoreEndpointsFixtureTest__test_import_egg__1.json`
shows: YAML `files: {}` and `logs: {}` are returned as `"files": []`, `"logs": []` (stored text `"[]"`),
`features: null`, `tags: []`, `script.privileged: true`.

### 5.4 Validation

- The model declares rules (`Egg.php:139-159`; `EggVariable.php:73-97`: env_variable `alphaDash`,
  `notIn:` reserved names, `name` 1–255, `default_value` `string`, `rules` array of strings, …). They are meant
  to be enforced on `saving` by `ValidationObserver` (`app/Observers/ValidationObserver.php`), attached through
  `#[ObservedBy([ValidationObserver::class])]` **on the `HasValidation` trait** (`app/Traits/HasValidation.php:17`).
- Laravel resolves `ObservedBy` with `ReflectionClass(static::class)->getAttributes()`
  (`vendor/.../Eloquent/Concerns/HasEvents.php:49-64`); PHP does not expose trait-level attributes on the using
  class, so this observer is most likely **never registered** and the importer performs **no model
  validation**. Evidence it is not enforced: the panel's own YAML exporter turns numeric `default_value`
  strings into YAML ints (§4.4.1), which would fail the `string` rule on re-import, yet re-import is a supported
  flow. Treat as uncertain (see open questions) — but compatibility-wise, a Rust importer should **accept**
  numeric/bool `default_value` and coerce to string, and should not reject eggs on model-rule grounds, only on
  DB constraints (NOT NULL `script_entry`/`script_container`/`name`/`author`, unique indexes).
- Coercion of non-string YAML scalars when stored into TEXT columns: Laravel binds PHP ints as integers;
  the DB stores `25565` → reads back `"25565"` (text column) — effectively stringified. Bools other than
  `default_value` (e.g. `user_viewable`) are stored as 0/1 and cast back to bool.

### 5.5 Errors (all `App\Exceptions\Service\InvalidFileUploadException`)

`'The selected file was not uploaded successfully'`, `'File parse failed: …'`, `'Unsupported file format.'`
(URL without/with unknown extension), `'The file format is not recognized.'` (version). Other failures
(SQL, HTTP) propagate as-is from `fromContent`/`fromUrl` (only `fromFile` re-wraps everything).

### 5.6 Lossy / surprising round-trip behaviour (keep for compatibility, document for users)

- JSON → DB: `config.*` text is re-pretty-printed (indentation, `\/`); YAML maps → JSON text with `[]` for
  empty maps.
- DB → YAML → DB: scalar-looking strings change type (ints/floats/bools/null), long descriptions gain
  hard line breaks at ~100 columns, CRLF in scripts becomes LF.
- `config_from` / `copy_script_from` links are dropped on import; inherited values were exported resolved.
- Child-egg `features` inherited from a parent are not exported (raw column).
- Variable `field_type` (Pterodactyl) is dropped; legacy string rules split on every `|`.
- `update_url` follows the file (`meta.update_url`), so updating from a file without it clears the URL.
- Updating egg X from a URL whose file has a different uuid renames X's uuid.

---

## 6. Update-from-URL, update check, remote index

### 6.1 `update_url`

Free-text column (`Egg.php:156`), exported as `meta.update_url`, re-set from `meta.update_url` on import.
Admin form hint: "URLs must point directly to the raw .json file" (`lang/en/admin/egg.php:66-67`), but
`.yaml`/`.yml` work too (extension-based format detection). Stock eggs were re-pointed by migrations
`2024_06_02_205622_update_stock_egg_uuid.php` (name → uuid mapping for 13 stock eggs) and
`2026_01_14_221937_convert_stock_egg_uuids.php` (old Pterodactyl uuids of Forge/Paper/Garry's Mod → Pelican
uuids, `update_url` → `https://raw.githubusercontent.com/pelican-eggs/{repo}/refs/heads/main/…/egg-*.yaml`).

### 6.2 Applying an update

`UpdateEggAction` / `UpdateEggBulkAction`: `EggImporterService::fromUrl($egg->update_url, $egg)` (full import
into that egg, §5.3), then `cache()->forget("eggs.{uuid}.update")`. Bulk action skips eggs with
`update_url === null` and reports updated/failed/skipped names. Errors are reported and shown as a
notification; there is no partial state because of the transaction (except the icon file).

### 6.3 Update check: `p:egg:check-updates` (`CheckEggUpdatesCommand.php`)

Scheduled daily (`Kernel.php:47`) and additionally at `0 0 * * *` in the Docker crontab (`docker/crontab:2`).
For every egg (`Egg::all()`):
1. `update_url` null → comment `"{name}: Skipping (no update url set)"`.
2. Extension of the URL path; empty → error. `isYaml = ext in (yaml, yml)` (anything else is treated as JSON).
3. `local = Yaml::parse(export(YAML))` or `json_decode(export(JSON), true)` — the local egg exported in the same
   format as the remote and parsed back.
4. `remote = GET update_url` (timeout 5 s, connect 1 s); non-2xx → `"HTTP request returned status code N"`;
   body parsed the same way.
5. Remove `exported_at` from both; compare `md5(json_encode(x))` (default flags, key order significant).
6. Prints `"{name}: Up-to-date"` (info) or `"{name}: Found update"` (warn); errors print `"{name}: Error (msg)"`.
7. `cache()->put("eggs.{uuid}.update", bool, now()->addHour())`.

Consequences to preserve or consciously fix: the flag expires after **1 hour** while the check runs once a day,
so the "Update" button (`UpdateEggAction.php:65`, visible iff the cache flag is true) is only visible during
the hour after the check; any key-order or value-type difference (e.g. MySQL variable key order, the extra
`icon: null` key, a missing `icon` in the remote file) yields "Found update".

### 6.4 Remote egg index: `p:egg:update-index` (`UpdateEggIndexCommand.php`)

Scheduled daily (`Kernel.php:48`) and run on demand by the installer's egg step and the import modal when the
cache is empty. GET `config('panel.cdn.egg_index_url')` (env `PANEL_EGG_INDEX_URL`, default
`https://raw.githubusercontent.com/pelican-eggs/pelican-eggs.github.io/refs/heads/main/content/pelican.json`),
timeout 5 s / connect 1 s, non-2xx → print error, exit 1. Expected JSON:

```json
{ "nests": [ { "nest_type": "Minecraft", "Eggs": [ { "egg": { "name": "Paper" }, "download_url": "https://…/egg-paper.yaml" } ] } ] }
```

Stored forever as cache key `eggs.index` = `{ nest_type: { download_url: egg_name } }`. The UI shows one tab
per `nest_type` (`str_slug(label, '_')` id) with checkboxes sorted by egg name; each selected URL becomes an
`InstallEgg` job.

---

## 7. `p:egg:normalize {file} {--delete-original}` (`NormalizeEggCommand.php`)

Description: "Upgrades egg json/yaml to latest format as yaml". Operates on files only (no DB).
1. Read file (failure → error, exit 1); print `Importing {file}`.
2. Extension `yaml|yml` → YAML, `json` → JSON, else `" -> unsupported extension …"`, exit 1.
3. `$importer->parse()` (§5.2, all conversions); `InvalidFileUploadException` → `" -> unsupported file version, is it actually an egg? (…)"`.
4. Requires `meta` (array), `exported_at` key, and `meta.update_url` **string** (null fails) — each with its own error message.
5. `meta.version = PLCN_v3`; `meta.update_url` and the output path get their `.yml|.json|.yaml` extension
   replaced by `.yaml` (regex `^(.*\.)(?:yml|json|yaml)$`).
6. Variables: drop `field_type`; string `rules` → `explode('|')`.
7. If output path == input path and `eggToYaml(array) === original bytes` → `" -> no changes required"`, exit 0,
   file untouched. Otherwise `exported_at = now()->toAtomString()`, print `" -> exporting to {out}"`, write
   `Yaml::dump(yamlExport(array), 10, 2, DUMP_MULTI_LINE_LITERAL_BLOCK | DUMP_OBJECT_AS_MAP)`; write failure →
   `" -> failed to write output file: …"`, exit 1 (original kept).
8. `--delete-original` and output != input → delete input (`" -> deleting input file as requested"`; failure is a warning).

Key order of the output = key order of the input file after conversions (so converted files end with
`startup_commands`, and a migrated legacy `image` ends up as a trailing `icon` key). Tests:
`tests/Unit/Console/Commands/Egg/NormalizerEggCommandTest.php` (upgrade PTDL_v2 JSON → exact YAML fixture,
idempotency, delete-original, no delete on write failure).

---

## 8. Bundled / stock eggs and seeding

- **No egg files ship in the repository** (`database/Seeders/` only contains `DatabaseSeeder.php`, which creates
  the root-admin role and calls plugin seeders — `database/Seeders/DatabaseSeeder.php:14-28`). The historical
  path `database/Seeders/eggs/...` referenced in fixture `update_url`s no longer exists.
- Eggs are obtained at install time from the remote index (installer step "Eggs",
  `EggSelectionStep.php`, queued `InstallEgg` jobs after the admin user is created, `PanelInstaller.php:155,244-276`)
  or later via the admin import modal.
- Data migrations that touch existing stock eggs: `2024_06_02_205622_update_stock_egg_uuid.php`,
  `2024_06_05_220135_update_egg_config_variables.php` and `2024_06_08_020904_refix_egg_variables.php`
  (same placeholder upgrades as `UPGRADE_VARIABLES` applied to stored `config_files`),
  `2026_01_14_221937_convert_stock_egg_uuids.php`.
- Tests seed eggs by importing every file in `tests/_fixtures/` through `fromFile` (`tests/Seeders/EggSeeder.php`,
  invoked from `tests/TestCase.php:51-55`, errors ignored).

## 9. Test fixtures to reuse

| Path (relative to `panel/`) | Content / use |
|---|---|
| `tests/Unit/Console/Commands/Egg/egg-ptdlv2.json` | PTDL_v2 JSON: `startup` string, `rules` as `|` string, `field_type`, CRLF in config JSON text, `\/` escapes, legacy `server.build.default.port` placeholder |
| `tests/Unit/Console/Commands/Egg/egg-plcnv3-example.yaml` | exact `p:egg:normalize` output for the file above (byte-level YAML dumper test) |
| `tests/_fixtures/egg-vanilla-minecraft.yaml` | PLCN_v3 YAML with legacy top-level `image:` data URI (PNG), wrapped multi-line descriptions, multiple docker images, regex rule with `\w`, `sort` values |
| `tests/_fixtures/egg-bungeecord.yaml` | PLCN_v3 YAML, two variables, tags `minecraft`/`proxy`, `image:` data URI |
| `tests/Integration/Api/Fixtures/Application/StoreEndpointsFixtureTest.php:117-155` + snapshot | minimal PLCN_v3 YAML import through the API and expected 201 JSON |

Note the `_fixtures` YAML files use `file_denylist: { }` (one space) and compact list items (`- name: …`), i.e.
they were not produced by the current dumper; they are import fixtures, not export fixtures.

---

## 10. Open questions / uncertain framework behaviour

1. **Model validation on save.** `#[ObservedBy]` is declared on the `HasValidation` trait. Per PHP semantics
   trait attributes are not returned by `ReflectionClass::getAttributes()` of the using class, so the
   `ValidationObserver` is probably never attached and imports are unvalidated. Could not be executed (no PHP
   runtime available). If it *is* active, numeric `default_value`s from YAML would make imports fail with
   `DataValidationException`, and `author` would have to be an e-mail.
2. Whether `Yaml::parse` of arbitrary JSON bodies (API import) is fully equivalent to `json_decode` — Symfony 7.4
   handles multi-line flow collections and JSON escapes, but duplicate keys, very long lines, or `\u` surrogate
   pairs were not verified.
3. Column order of `egg_variables` on MySQL vs SQLite/Postgres is confirmed by the schema dumps, but a database
   created by restoring a dump from another driver, or altered by hand, could differ; the PHP export simply
   follows whatever `SELECT *` returns.
4. Behaviour of `EggFormat::tryFrom(null)` when `format` is omitted: PHP 8.x coerces null for internal enum
   methods with a deprecation, returning null → YAML. A future PHP may throw.
5. `str_replace` placeholder upgrade produces `server.build.memory_limit_limit` for both
   `server.build.env.SERVER_MEMORY` and an already-current `server.build.memory_limit` (derived by reading the
   code, not executed); assumed to be an upstream bug. Reproduce it only if stored `config_files` must be
   byte-compatible.
6. MySQL default collation makes the `updateOrCreate` env_variable match case-insensitive; SQLite/Postgres are
   case-sensitive. The Rust port should choose one (recommend exact match + uppercase normalisation).
7. PHP `json_encode` of floats (`serialize_precision = -1`) and Symfony's float dumping were not exercised;
   eggs normally contain no floats except via the JSON-string auto-decoding in §4.4.1.
