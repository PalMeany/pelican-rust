# Плагины pelican-rs: руководство автора

Решение — ADR-0018. Плагин pelican-rs — это папка с `plugin.json` (тот же формат, что у Pelican) и WASM-компонентом,
реализующим мир `pelican:plugin@1.0.0` (`crates/pelican-plugins/wit/plugin.wit`). Плагин работает в песочнице: у
него нет доступа к БД, файлам и сети, кроме того, что разрешено в манифесте.

## Быстрый старт (Rust)

```bash
cp -r examples/plugins/hello my-plugin      # Cargo.toml, plugin.json, src/lib.rs, lang/
# поправить id/name/namespace/class в plugin.json и имя крейта
scripts/build-plugin my-plugin              # → my-plugin/<wasm.component>
(cd my-plugin && zip -r ../my-plugin.zip plugin.json *.wasm lang README.md)
```

Затем «Plugins → Import from File» в админке (или `POST /api/application/plugins/import/file`), «Install».
Изменения применяются сразу: рантайм перезагружает плагины после установки, включения и обновления.

Код плагина — реализация трейта `PelicanPlugin` из `pelican-plugin-sdk` (все хуки, кроме `register`, имеют
реализации по умолчанию) и макрос `plugin!(Тип)`:

```rust
use pelican_plugin_sdk::{PelicanPlugin, Registration, Request, Response, Route, RouteScope, json};

struct Ping;
impl PelicanPlugin for Ping {
    fn register() -> Registration {
        Registration {
            routes: vec![Route { scope: RouteScope::Client, method: "GET".into(), path: "/ping".into() }],
            ..Default::default()
        }
    }
    fn handle(_req: Request) -> Response {
        Response::json(200, &json!({"pong": true}))
    }
}
pelican_plugin_sdk::plugin!(Ping);
```

## plugin.json

Поля оригинала (`id`, `name`, `author`, `version`, `category`, `namespace`, `class`, `panels`, `panel_version`,
`update_url`, …) сохраняют смысл; `namespace`/`class` обязательны по формату, но для WASM-плагина не используются.
Раздел `wasm` (оригинал его игнорирует):

```json
"wasm": {
    "component": "plugin.wasm",
    "api": 1,
    "capabilities": {
        "http": ["api.example.com", "*.example.org"],
        "api": { "server": 1, "user": 3 }
    }
}
```

- `http` — хосты для `host::http` (без них исходящие запросы запрещены);
- `api` — права для `host::api` (как у application-ключа: 1 чтение, 2 запись, 3 обе). Панель создаёт плагину
  собственный application-ключ (`Plugin: <id>` в списке API-ключей; его можно отозвать).

`panel_version` (`^1.0`), `update_url` (фид обновлений) и Hub работают как в оригинале.

## Что плагин может зарегистрировать (`register`)

| Поле | Эффект |
|---|---|
| `events` | события в `on_event`: события активности (`server:power.start`), `App\Events\ActivityLogged` (все), `*` |
| `routes` | `web` → `/plugins/<id>/<path>` (сессия), `client` → `/api/client/plugins/<id>/<path>` и `/api/client/servers/{server}/plugins/<id>/<path>` (клиентский API-ключ; `req.server` — привязанный сервер; для JS собственных страниц используйте web-маршруты), `application` → `/api/application/plugins/<id>/api/<path>` (право ключа `plugin`) |
| `pages` | страницы в оболочке панели: admin `/admin/plugins/<id>/page/<page>`, server `/server/{server}/plugins/<id>/<page>`, app `/plugins/<id>/pages/<page>`; тело — HTML-фрагмент из `render_page`; `permission` — право админа (`viewList thing`) или сабюзера (`group.action`) |
| `navigation` | пункты меню admin/server для страниц (`group`: `server`/`user`/`advanced` или своя группа) |
| `tasks` | типы задач расписаний (форма расписания панели и раннер вызывают `run_task`) |
| `features` | server features: Wings получает `listeners` (для яиц, у которых id есть в `features` или `features` пуст), при совпадении строки консоли открывается модалка со страницей `feature-<id>` (зарегистрируйте её как server-страницу, чтобы задать заголовок) |
| `avatars` | провайдеры аватаров (выбор в настройках, URL из `avatar_url`) |
| `admin_permissions` | права ролей админов `<prefix> <model>` |
| `subuser_permissions` | группы прав сабюзеров `<name>.<permission>` |
| `api_resources` | ресурсы прав application-ключей |
| `settings` | форма настроек в списке плагинов (`settings_form`, `settings_data`, `save_settings`) |

`install`/`uninstall` вызываются при установке и удалении (аналог миграций). Ошибка `install` оставляет плагин
`not_installed` с сообщением.

## Возможности хоста (`host::*`)

`log`, `kv_get`/`kv_set` (хранилище плагина в `storage/plugins/<id>/`, переживает обновления), `http`, `api`,
`activity` (запись в журнал активности), `trans` (ключи `<id>::group.key` читаются из `lang/<locale>/<group>.json`
плагина; остальные — переводы панели).

## Ограничения

- Время одного вызова ~30 с, память 128 МиБ; зависание или паника плагина возвращают ошибку, плагин при
  повторной загрузке с ошибкой получает статус `errored`.
- `PANEL_PLUGIN_SAFE_MODE=true` отключает загрузку всех плагинов.
- Без sidecar PHP-плагины оригинала видны и управляются (импорт, статусы), но не исполняются (см. ниже).

## PHP-плагины оригинала: sidecar (ADR-0020)

PHP-плагины исполняет сам оригинал Pelican **той же версии**, что и схема БД, запущенный рядом с pelican-rs
(например, прежняя установка или образ `ghcr.io/pelican-dev/panel`). pelican-rs проксирует ему страницы и
маршруты плагинов и пересылает события; ядро панели остаётся в pelican-rs.

Настройка pelican-rs (`.env`):

```
PANEL_PHP_SIDECAR=http://127.0.0.1:8090     # адрес sidecar, доступный только pelican-rs
PANEL_PHP_SIDECAR_DATA=/pelican-rs-data     # каталог данных pelican-rs, как его видит sidecar (если отличается)
```

Sidecar (оригинал) запускается с тем же `APP_KEY` и БД, `APP_URL` = публичный адрес pelican-rs,
`SESSION_DRIVER=file`, `SESSION_COOKIE=pelican_sidecar_session`, `QUEUE_CONNECTION=sync`, без cron/воркеров
ядра. Его каталог `plugins/` — это `storage/sidecar/plugins` pelican-rs: туда pelican-rs кладёт плагин-мост
`pelican-rs-bridge` и ссылки на PHP-плагины (WASM-плагины оригинал не видит). Пример для Docker:

```bash
docker run -d --name pelican-sidecar --network <сеть pelican-rs> \
  -e APP_KEY=<как у pelican-rs> -e APP_URL=https://panel.example.com -e APP_INSTALLED=true \
  -e DB_CONNECTION=... -e DB_HOST=... -e DB_DATABASE=... -e DB_USERNAME=... -e DB_PASSWORD=... \
  -e SESSION_DRIVER=file -e SESSION_COOKIE=pelican_sidecar_session -e CACHE_STORE=file \
  -e QUEUE_CONNECTION=sync -e CADDY_APP_URL=:80 -e TRUSTED_PROXIES=<адрес pelican-rs> \
  -v /var/lib/pelican-rs:/pelican-rs-data -v /var/lib/pelican-rs/storage/sidecar/plugins:/var/www/html/plugins \
  --entrypoint sh ghcr.io/pelican-dev/panel:<версия> -c "php-fpm -D && exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile"
```

Что работает: страницы и ресурсы Filament из плагинов (в оформлении оригинала, с Livewire), их пункты меню в
админке и панели сервера, маршруты плагинов, слушатели `eloquent.*` и `App\Events\*` (`ActivityLogged` и др.),
типы задач расписаний, права ролей/сабюзеров, ресурсы API-ключей, провайдеры аватаров, языковые плагины
(новые языки и переводы поверх встроенных), установка/обновление/
удаление/включение/выключение (Composer-пакеты, миграции и сидеры плагина — в sidecar). Пункты меню
показываются по `canAccess()` страницы для конкретного пользователя и сервера. Пример —
`examples/php-plugins/greeter`; проверено на плагинах `tickets`, `player-counter`, `robo-avatars`,
`pirate-language` из [pelican-dev/plugins](https://github.com/pelican-dev/plugins).

Render hooks плагинов (`FilamentView::registerRenderHook`: head, начало/конец страницы и body, футер, форма входа)
вставляются в страницы pelican-rs.

Вход, выполненный внутри sidecar (OAuth-провайдер плагина, страница регистрации плагина), передаётся в сессию pelican-rs.

Действия, которые плагины добавляют на консоль (`Console::registerCustomHeaderActions`), и их server features
показываются на консоли pelican-rs; выполняются на служебной странице моста в оформлении Filament.

Ограничения: темы и виджеты плагинов на страницах ядра, действия плагинов на других страницах ядра (кроме консоли),
captcha-провайдеры из PHP-плагинов. Версии sidecar и панели
сверяются: при расхождении PHP-плагины отключаются (ошибка в логе).
